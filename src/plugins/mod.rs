pub mod api;
pub mod sandbox;

use mlua::prelude::*;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use api::{NotifyLevel, PluginAction, PluginApiContext};

/// Events the IDE fires into the plugin system.
#[derive(Debug, Clone)]
pub enum PluginEvent {
    FileSaved(PathBuf),
    FileOpened(PathBuf),
    CursorMoved(usize, usize),
    TextChanged(PathBuf),
}

/// A menu item contributed by a plugin.
#[derive(Clone, Debug)]
pub struct PluginMenuItem {
    pub plugin_name: String,
    pub label: String,
    pub callback_name: String,
}

/// A notification queued by a plugin to be shown in the IDE status bar.
#[derive(Debug, Clone)]
pub struct PluginNotification {
    pub message: String,
    pub level: NotifyLevel,
    pub plugin_name: String,
    /// Wall-clock time when this notification was created (for auto-dismiss).
    pub created_at: Instant,
}

/// A single loaded plugin: its isolated Lua VM plus the shared action queue.
struct LoadedPlugin {
    name: String,
    lua: Lua,
    /// Shared write-back queue for this plugin's Lua closures.
    actions: Arc<Mutex<Vec<PluginAction>>>,
    /// Shared context snapshot; refreshed before every call.
    context: Arc<RefCell<PluginApiContext>>,
    hooks: DetectedHooks,
}

/// Which event hooks the plugin registered (detected by inspecting globals
/// after loading).
#[derive(Default)]
struct DetectedHooks {
    on_save: bool,
    on_open: bool,
    on_cursor_move: bool,
    on_text_change: bool,
}

impl DetectedHooks {
    fn detect(lua: &Lua) -> Self {
        let globals = lua.globals();
        Self {
            on_save: globals.get::<_, LuaFunction>("__blue_hook_on_save").is_ok(),
            on_open: globals.get::<_, LuaFunction>("__blue_hook_on_open").is_ok(),
            on_cursor_move: globals
                .get::<_, LuaFunction>("__blue_hook_on_cursor_move")
                .is_ok(),
            on_text_change: globals
                .get::<_, LuaFunction>("__blue_hook_on_text_change")
                .is_ok(),
        }
    }
}

/// The main plugin manager owned by `BlueIdeApp`.
pub struct PluginSystem {
    plugins: Vec<LoadedPlugin>,
    pub menu_items: Vec<PluginMenuItem>,
    pub notifications: Vec<PluginNotification>,
    last_cursor_event: Instant,
    last_text_event: Instant,
    /// Directory from which plugins were last loaded (needed for reload).
    plugin_dir: Option<PathBuf>,
}

impl PluginSystem {
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
            menu_items: Vec::new(),
            notifications: Vec::new(),
            last_cursor_event: Instant::now(),
            last_text_event: Instant::now(),
            plugin_dir: None,
        }
    }

    // ─── Loading ──────────────────────────────────────────────────────────────

    /// Scans `plugin_dir` for `*.lua` files and loads each one.
    /// Creates the directory (and a README) if it does not exist.
    pub fn load_all(&mut self, plugin_dir: &Path) {
        self.plugin_dir = Some(plugin_dir.to_path_buf());

        if !plugin_dir.exists() {
            if let Err(e) = std::fs::create_dir_all(plugin_dir) {
                eprintln!("[plugins] Failed to create plugin directory: {}", e);
                return;
            }
            let _ = std::fs::write(
                plugin_dir.join("README.md"),
                include_str!("plugins_readme.md"),
            );
        }

        let glob_pattern = format!("{}/*.lua", plugin_dir.display());
        match glob::glob(&glob_pattern) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    self.load_plugin(&entry);
                }
            }
            Err(e) => eprintln!("[plugins] Failed to scan plugin directory: {}", e),
        }
    }

    /// Unloads all plugins and reloads from the last-known directory.
    pub fn reload_all(&mut self) {
        self.plugins.clear();
        self.menu_items.clear();
        if let Some(dir) = self.plugin_dir.clone() {
            self.load_all(&dir);
        }
    }

    fn load_plugin(&mut self, path: &Path) {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[plugin:{}] Failed to read file: {}", name, e);
                return;
            }
        };

        match self.create_plugin(&name, &source) {
            Ok(plugin) => {
                eprintln!(
                    "[plugin:{}] Loaded (hooks: save={} open={} cursor={} text={})",
                    name,
                    plugin.hooks.on_save,
                    plugin.hooks.on_open,
                    plugin.hooks.on_cursor_move,
                    plugin.hooks.on_text_change,
                );
                self.plugins.push(plugin);
            }
            Err(e) => eprintln!("[plugin:{}] Load error: {}", name, e),
        }
    }

    fn create_plugin(&self, name: &str, source: &str) -> Result<LoadedPlugin, String> {
        let lua = Lua::new();

        sandbox::apply_sandbox(&lua).map_err(|e| e.to_string())?;
        sandbox::set_instruction_limit(&lua, 1_000_000)
            .map_err(|e| format!("Failed to set instruction limit: {}", e))?;

        let context = Arc::new(RefCell::new(PluginApiContext {
            active_file: None,
            buffer_content: String::new(),
            cursor_line: 0,
            cursor_col: 0,
            workspace_root: None,
            language: "unknown".to_string(),
        }));

        let actions: Arc<Mutex<Vec<PluginAction>>> = Arc::new(Mutex::new(Vec::new()));

        api::register_api(&lua, name, context.clone(), actions.clone())
            .map_err(|e| format!("Failed to register API: {}", e))?;

        lua.load(source)
            .exec()
            .map_err(|e| format!("Execution error: {}", e))?;

        // After top-level execution, collect any AddMenuItem actions that were
        // pushed synchronously (e.g. blue.show_menu_item("…") at module level).
        let early_actions: Vec<PluginAction> = {
            let mut q = actions.lock().unwrap();
            std::mem::take(&mut *q)
        };
        // We can't mutate self.menu_items here because we're building the plugin;
        // return them in the struct and let the caller drain them.
        // Instead, we harvest them directly:
        let mut harvested_menu: Vec<(String, String)> = Vec::new();
        for action in early_actions {
            if let PluginAction::AddMenuItem {
                label,
                callback_name,
            } = action
            {
                harvested_menu.push((label, callback_name));
            }
        }

        let hooks = DetectedHooks::detect(&lua);

        let plugin = LoadedPlugin {
            name: name.to_string(),
            lua,
            actions,
            context,
            hooks,
        };

        // Register the menu items collected during top-level execution.
        // We return the plugin first so that we can push into self.menu_items after.
        // Trick: stash them back into the action queue so the caller drains them.
        {
            let mut q = plugin.actions.lock().unwrap();
            for (label, callback_name) in harvested_menu {
                q.push(PluginAction::AddMenuItem {
                    label,
                    callback_name,
                });
            }
        }

        Ok(plugin)
    }

    // ─── Event dispatch ───────────────────────────────────────────────────────

    /// Update the shared context snapshot for a plugin just before calling it.
    fn refresh_context(plugin: &LoadedPlugin, snap: &PluginApiContext) {
        let mut ctx = plugin.context.borrow_mut();
        ctx.active_file = snap.active_file.clone();
        ctx.buffer_content = snap.buffer_content.clone();
        ctx.cursor_line = snap.cursor_line;
        ctx.cursor_col = snap.cursor_col;
        ctx.workspace_root = snap.workspace_root.clone();
        ctx.language = snap.language.clone();
    }

    /// Fires an event to all interested plugins.
    /// Returns the union of all `PluginAction`s queued during this round.
    pub fn dispatch_event(
        &mut self,
        event: PluginEvent,
        snap: &PluginApiContext,
    ) -> Vec<PluginAction> {
        match event {
            PluginEvent::FileSaved(path) => {
                self.dispatch_path_hook("on_save", "__blue_hook_on_save", &path, snap)
            }
            PluginEvent::FileOpened(path) => {
                self.dispatch_path_hook("on_open", "__blue_hook_on_open", &path, snap)
            }
            PluginEvent::CursorMoved(line, col) => {
                if self.last_cursor_event.elapsed() < Duration::from_millis(200) {
                    return Vec::new();
                }
                self.last_cursor_event = Instant::now();
                self.dispatch_cursor_hook(line, col, snap)
            }
            PluginEvent::TextChanged(path) => {
                if self.last_text_event.elapsed() < Duration::from_millis(500) {
                    return Vec::new();
                }
                self.last_text_event = Instant::now();
                self.dispatch_path_hook("on_text_change", "__blue_hook_on_text_change", &path, snap)
            }
        }
    }

    fn dispatch_path_hook(
        &mut self,
        hook_name: &str,
        global_key: &str,
        path: &Path,
        snap: &PluginApiContext,
    ) -> Vec<PluginAction> {
        let mut all_actions = Vec::new();
        for plugin in &mut self.plugins {
            // Check the cached hook flag so we skip the globals lookup in the
            // common case (no hook registered).
            let has_hook = match hook_name {
                "on_save" => plugin.hooks.on_save,
                "on_open" => plugin.hooks.on_open,
                "on_text_change" => plugin.hooks.on_text_change,
                _ => false,
            };
            if !has_hook {
                continue;
            }
            Self::refresh_context(plugin, snap);
            let result: LuaResult<()> = (|| {
                let func: LuaFunction = plugin.lua.globals().get(global_key)?;
                let path_str = plugin
                    .lua
                    .create_string(path.to_string_lossy().as_bytes())?;
                func.call::<_, ()>(path_str)
            })();
            if let Err(e) = result {
                eprintln!("[plugin:{}] {} error: {}", plugin.name, hook_name, e);
            }
            let mut q = plugin.actions.lock().unwrap();
            all_actions.extend(std::mem::take(&mut *q));
        }
        all_actions
    }

    fn dispatch_cursor_hook(
        &mut self,
        line: usize,
        col: usize,
        snap: &PluginApiContext,
    ) -> Vec<PluginAction> {
        let mut all_actions = Vec::new();
        for plugin in &mut self.plugins {
            if !plugin.hooks.on_cursor_move {
                continue;
            }
            Self::refresh_context(plugin, snap);
            // Expose 1-indexed to Lua
            let result: LuaResult<()> = plugin
                .lua
                .globals()
                .get::<_, LuaFunction>("__blue_hook_on_cursor_move")
                .and_then(|f| f.call::<_, ()>((line + 1, col + 1)));
            if let Err(e) = result {
                eprintln!("[plugin:{}] on_cursor_move error: {}", plugin.name, e);
            }
            let mut q = plugin.actions.lock().unwrap();
            all_actions.extend(std::mem::take(&mut *q));
        }
        all_actions
    }

    /// Invokes a named callback function in the plugin that owns `label`.
    /// Returns all `PluginAction`s queued during the invocation.
    pub fn invoke_menu_item(&mut self, label: &str, snap: &PluginApiContext) -> Vec<PluginAction> {
        let item = match self.menu_items.iter().find(|i| i.label == label) {
            Some(i) => i.clone(),
            None => {
                eprintln!("[plugins] Menu item not found: {}", label);
                return Vec::new();
            }
        };

        let plugin = match self.plugins.iter_mut().find(|p| p.name == item.plugin_name) {
            Some(p) => p,
            None => {
                eprintln!(
                    "[plugins] Plugin not found for menu item: {}",
                    item.plugin_name
                );
                return Vec::new();
            }
        };

        Self::refresh_context(plugin, snap);

        let result: LuaResult<()> = plugin
            .lua
            .globals()
            .get::<_, LuaFunction>(item.callback_name.as_str())
            .and_then(|f| f.call::<_, ()>(()));
        if let Err(e) = result {
            eprintln!(
                "[plugin:{}] Menu callback '{}' error: {}",
                plugin.name, item.callback_name, e
            );
        }

        let mut q = plugin.actions.lock().unwrap();
        std::mem::take(&mut *q)
    }

    // ─── Action draining ─────────────────────────────────────────────────────

    /// Called by `app.rs` to apply actions produced during plugin calls.
    ///
    /// Returns `AddMenuItem` actions to the caller (since they require access
    /// to `self.menu_items`). Notifications are stored in `self.notifications`.
    pub fn apply_actions(&mut self, actions: Vec<PluginAction>, plugin_name: &str) {
        for action in actions {
            match action {
                PluginAction::Notify { message, level } => {
                    self.notifications.push(PluginNotification {
                        message,
                        level,
                        plugin_name: plugin_name.to_string(),
                        created_at: Instant::now(),
                    });
                }
                PluginAction::AddMenuItem {
                    label,
                    callback_name,
                } => {
                    // Deduplicate: only add if label not already present from this plugin
                    if !self
                        .menu_items
                        .iter()
                        .any(|i| i.plugin_name == plugin_name && i.label == label)
                    {
                        self.menu_items.push(PluginMenuItem {
                            plugin_name: plugin_name.to_string(),
                            label,
                            callback_name,
                        });
                    }
                }
                // Other actions (SetCursor, InsertText, etc.) are handled by app.rs
                _ => {}
            }
        }
    }

    /// Process a batch of actions from a single Lua call, segregating the
    /// notification / menu-item ones (stored here) from the buffer-mutation ones
    /// (returned to the caller for application).
    pub fn split_actions(
        &mut self,
        actions: Vec<PluginAction>,
        plugin_name: &str,
    ) -> Vec<PluginAction> {
        let mut remainder = Vec::new();
        for action in actions {
            match &action {
                PluginAction::Notify { .. } | PluginAction::AddMenuItem { .. } => {
                    // These are stored/handled by the plugin system itself.
                    self.apply_actions(vec![action], plugin_name);
                }
                _ => remainder.push(action),
            }
        }
        remainder
    }

    /// Drain all actions queued inside each plugin's action channel.
    /// Called from `app.rs` every frame to pick up any actions from
    /// synchronous plugin code that wasn't dispatched via an explicit event.
    pub fn drain_pending_actions(&mut self) -> Vec<(String, Vec<PluginAction>)> {
        let mut result = Vec::new();
        for plugin in &mut self.plugins {
            let actions: Vec<PluginAction> = {
                let mut q = plugin.actions.lock().unwrap();
                std::mem::take(&mut *q)
            };
            if !actions.is_empty() {
                result.push((plugin.name.clone(), actions));
            }
        }
        result
    }

    /// Expire notifications older than `ttl`.
    pub fn expire_notifications(&mut self, ttl: Duration) {
        self.notifications.retain(|n| n.created_at.elapsed() < ttl);
    }

    /// Number of loaded plugins.
    pub fn plugin_count(&self) -> usize {
        self.plugins.len()
    }
}

impl Default for PluginSystem {
    fn default() -> Self {
        Self::new()
    }
}
