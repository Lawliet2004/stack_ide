use mlua::prelude::*;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// Plugin API context — read from the app each time a hook fires or a menu item is invoked.
pub struct PluginApiContext {
    pub active_file: Option<PathBuf>,
    pub buffer_content: String,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub workspace_root: Option<PathBuf>,
    pub language: String,
}

/// Actions the plugin wants to perform on the IDE state.
/// Collected during a Lua call and drained by `app.rs` each frame.
#[derive(Debug)]
pub enum PluginAction {
    /// Move the editor cursor to (0-based line, 0-based col).
    SetCursor { line: usize, col: usize },
    /// Insert text at the current cursor position.
    InsertText(String),
    /// Replace the whole buffer content.
    ReplaceText(String),
    /// Open a file by path.
    OpenFile(PathBuf),
    /// Save the active buffer.
    SaveFile,
    /// Show a notification in the status bar with an optional level string.
    Notify { message: String, level: NotifyLevel },
    /// Add a menu item to the Plugins menu.
    AddMenuItem {
        label: String,
        callback_name: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyLevel {
    Info,
    Warning,
    Error,
}

impl NotifyLevel {
    fn from_str(s: &str) -> Self {
        match s {
            "warning" | "warn" => Self::Warning,
            "error" | "err" => Self::Error,
            _ => Self::Info,
        }
    }
}

/// Registers all `blue.*` API functions into a plugin's Lua state.
///
/// `context` is a shared snapshot of the current IDE state read before each call.
/// `actions` is the write-back queue; Lua closures push to it, `app.rs` drains it.
pub fn register_api(
    lua: &Lua,
    plugin_name: &str,
    context: Arc<RefCell<PluginApiContext>>,
    actions: Arc<Mutex<Vec<PluginAction>>>,
) -> LuaResult<()> {
    let blue_table = lua.create_table()?;

    // ── Buffer: read ──────────────────────────────────────────────────────────

    let ctx = context.clone();
    blue_table.set(
        "current_file",
        lua.create_function(move |lua, ()| {
            let ctx = ctx.borrow();
            match &ctx.active_file {
                Some(p) => lua
                    .create_string(p.to_string_lossy().as_bytes())
                    .map(LuaValue::String),
                None => Ok(LuaValue::Nil),
            }
        })?,
    )?;

    let ctx = context.clone();
    blue_table.set(
        "get_text",
        lua.create_function(move |lua, ()| {
            let ctx = ctx.borrow();
            lua.create_string(ctx.buffer_content.as_bytes())
                .map(LuaValue::String)
        })?,
    )?;

    let ctx = context.clone();
    blue_table.set(
        "get_line",
        lua.create_function(move |lua, line_num: usize| {
            let ctx = ctx.borrow();
            if line_num == 0 {
                return Ok(LuaValue::Nil);
            }
            let lines: Vec<&str> = ctx.buffer_content.lines().collect();
            match lines.get(line_num - 1) {
                Some(line) => lua.create_string(line.as_bytes()).map(LuaValue::String),
                None => Ok(LuaValue::Nil),
            }
        })?,
    )?;

    let ctx = context.clone();
    blue_table.set(
        "get_cursor",
        lua.create_function(move |lua, ()| {
            let ctx = ctx.borrow();
            let tbl = lua.create_table()?;
            // Expose 1-indexed values to Lua (internally 0-based)
            tbl.set("line", ctx.cursor_line + 1)?;
            tbl.set("col", ctx.cursor_col + 1)?;
            Ok(LuaValue::Table(tbl))
        })?,
    )?;

    // ── Buffer: write ─────────────────────────────────────────────────────────

    let act = actions.clone();
    blue_table.set(
        "set_cursor",
        lua.create_function(move |_, (line, col): (usize, usize)| {
            // Lua passes 1-indexed; convert to 0-indexed for app
            let line = line.saturating_sub(1);
            let col = col.saturating_sub(1);
            if let Ok(mut q) = act.lock() {
                q.push(PluginAction::SetCursor { line, col });
            }
            Ok(())
        })?,
    )?;

    let act = actions.clone();
    blue_table.set(
        "insert_text",
        lua.create_function(move |_, text: LuaString| {
            let s = text.to_str().unwrap_or("").to_owned();
            if let Ok(mut q) = act.lock() {
                q.push(PluginAction::InsertText(s));
            }
            Ok(())
        })?,
    )?;

    let act = actions.clone();
    blue_table.set(
        "replace_text",
        lua.create_function(move |_, text: LuaString| {
            let s = text.to_str().unwrap_or("").to_owned();
            if let Ok(mut q) = act.lock() {
                q.push(PluginAction::ReplaceText(s));
            }
            Ok(())
        })?,
    )?;

    // get_selection and set_selection are read-only stubs for now —
    // selection state would need to be threaded through PluginApiContext.
    let ctx = context.clone();
    blue_table.set(
        "get_selection",
        lua.create_function(move |lua, ()| {
            let ctx = ctx.borrow();
            // Return empty string; selection not yet tracked in context
            let _ = ctx.active_file.as_ref(); // silence unused lint
            lua.create_string(b"").map(LuaValue::String)
        })?,
    )?;

    blue_table.set(
        "set_selection",
        lua.create_function(|_, (_sl, _sc, _el, _ec): (usize, usize, usize, usize)| Ok(()))?,
    )?;

    // ── File operations ───────────────────────────────────────────────────────

    let act = actions.clone();
    blue_table.set(
        "open_file",
        lua.create_function(move |_, path: LuaString| {
            let p = PathBuf::from(path.to_str().unwrap_or(""));
            if let Ok(mut q) = act.lock() {
                q.push(PluginAction::OpenFile(p));
            }
            Ok(())
        })?,
    )?;

    let act = actions.clone();
    blue_table.set(
        "save_file",
        lua.create_function(move |_, ()| {
            if let Ok(mut q) = act.lock() {
                q.push(PluginAction::SaveFile);
            }
            Ok(())
        })?,
    )?;

    let ctx = context.clone();
    blue_table.set(
        "get_workspace_root",
        lua.create_function(move |lua, ()| {
            let ctx = ctx.borrow();
            match &ctx.workspace_root {
                Some(p) => lua
                    .create_string(p.to_string_lossy().as_bytes())
                    .map(LuaValue::String),
                None => Ok(LuaValue::Nil),
            }
        })?,
    )?;

    let ctx = context.clone();
    blue_table.set(
        "read_file",
        lua.create_function(move |lua, path_str: LuaString| {
            let workspace = ctx.borrow();
            let workspace_root = match &workspace.workspace_root {
                Some(root) => root,
                None => return Ok(LuaValue::Nil),
            };
            let path = PathBuf::from(path_str.to_str().unwrap_or(""));
            if !is_path_in_workspace(&path, workspace_root) {
                eprintln!(
                    "[plugin] Access denied: path {} is outside workspace",
                    path.display()
                );
                return Ok(LuaValue::Nil);
            }
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let content = String::from_utf8_lossy(&bytes);
                    lua.create_string(content.as_bytes()).map(LuaValue::String)
                }
                Err(e) => Err(LuaError::RuntimeError(format!(
                    "read_file: could not read {}: {}",
                    path.display(),
                    e
                ))),
            }
        })?,
    )?;

    let ctx = context.clone();
    blue_table.set(
        "list_files",
        lua.create_function(move |lua, dir_str: LuaString| {
            let workspace = ctx.borrow();
            let workspace_root = match &workspace.workspace_root {
                Some(root) => root,
                None => return Ok(LuaValue::Nil),
            };
            let dir = Path::new(dir_str.to_str().unwrap_or(""));
            if !is_path_in_workspace(dir, workspace_root) {
                eprintln!(
                    "[plugin] Access denied: path {} is outside workspace",
                    dir.display()
                );
                return Ok(LuaValue::Nil);
            }
            let table = lua.create_table()?;
            let mut idx = 1;
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    if let Some(path_str) = entry.path().to_str() {
                        table.set(idx, lua.create_string(path_str.as_bytes())?)?;
                        idx += 1;
                    }
                }
            }
            Ok(LuaValue::Table(table))
        })?,
    )?;

    // ── UI ────────────────────────────────────────────────────────────────────

    let act = actions.clone();
    blue_table.set(
        "notify",
        lua.create_function(move |_, (message, level): (LuaString, Option<LuaString>)| {
            let msg = message.to_str().unwrap_or("").to_owned();
            let lvl = level
                .as_ref()
                .and_then(|l| l.to_str().ok())
                .unwrap_or("info");
            let level = NotifyLevel::from_str(lvl);
            if let Ok(mut q) = act.lock() {
                q.push(PluginAction::Notify {
                    message: msg,
                    level,
                });
            }
            Ok(())
        })?,
    )?;

    // show_input_dialog is synchronous in the Lua sense but egui is immediate-mode;
    // for now it returns nil (the README notes it is modal/async).
    blue_table.set(
        "show_input_dialog",
        lua.create_function(|_, _prompt: LuaString| Ok(LuaValue::Nil))?,
    )?;

    let act = actions.clone();
    blue_table.set(
        "show_menu_item",
        lua.create_function(move |_, (label, callback): (LuaString, LuaString)| {
            let label = label.to_str().unwrap_or("").to_owned();
            let callback_name = callback.to_str().unwrap_or("").to_owned();
            if let Ok(mut q) = act.lock() {
                q.push(PluginAction::AddMenuItem {
                    label,
                    callback_name,
                });
            }
            Ok(())
        })?,
    )?;

    // ── Terminal ──────────────────────────────────────────────────────────────

    let ctx = context.clone();
    blue_table.set(
        "run_command",
        lua.create_function(move |lua, cmd_str: LuaString| {
            let cmd_s = cmd_str.to_str().unwrap_or("").to_owned();
            let workspace_root = ctx.borrow().workspace_root.clone();

            #[cfg(windows)]
            let mut cmd = {
                let mut c = Command::new("powershell");
                c.args(["-Command", &cmd_s]);
                c
            };
            #[cfg(not(windows))]
            let mut cmd = {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
                let mut c = Command::new(&shell);
                c.args(["-c", &cmd_s]);
                c
            };

            if let Some(root) = workspace_root {
                cmd.current_dir(root);
            }

            // Hard timeout via a thread so the UI thread doesn't block forever.
            let output = std::thread::spawn(move || cmd.output().ok())
                .join()
                .ok()
                .flatten();

            match output {
                Some(out) => {
                    let text = String::from_utf8_lossy(&out.stdout).into_owned();
                    lua.create_string(text.as_bytes()).map(LuaValue::String)
                }
                None => lua.create_string(b"").map(LuaValue::String),
            }
        })?,
    )?;

    // ── Editor state ──────────────────────────────────────────────────────────

    let ctx = context.clone();
    blue_table.set(
        "get_language",
        lua.create_function(move |lua, ()| {
            let ctx = ctx.borrow();
            lua.create_string(ctx.language.as_bytes())
                .map(LuaValue::String)
        })?,
    )?;

    // get_diagnostics returns an empty table for now; wiring LSP diagnostics
    // into PluginApiContext is a follow-up.
    blue_table.set(
        "get_diagnostics",
        lua.create_function(|lua, ()| lua.create_table().map(LuaValue::Table))?,
    )?;

    // ── Event hooks ───────────────────────────────────────────────────────────
    // Hooks are registered by the plugin via blue.on_*(fn).
    // The functions are stored inside the Lua state using a well-known global
    // name so the plugin system can retrieve and call them without keeping a
    // separate Rust handle (which would need the Lua state's lifetime).

    blue_table.set(
        "on_save",
        lua.create_function(|lua, callback: LuaFunction| {
            lua.globals().set("__blue_hook_on_save", callback)?;
            Ok(())
        })?,
    )?;

    blue_table.set(
        "on_open",
        lua.create_function(|lua, callback: LuaFunction| {
            lua.globals().set("__blue_hook_on_open", callback)?;
            Ok(())
        })?,
    )?;

    blue_table.set(
        "on_cursor_move",
        lua.create_function(|lua, callback: LuaFunction| {
            lua.globals().set("__blue_hook_on_cursor_move", callback)?;
            Ok(())
        })?,
    )?;

    blue_table.set(
        "on_text_change",
        lua.create_function(|lua, callback: LuaFunction| {
            lua.globals().set("__blue_hook_on_text_change", callback)?;
            Ok(())
        })?,
    )?;

    // ── print() override ──────────────────────────────────────────────────────
    let plugin_name_clone = plugin_name.to_string();
    let print_fn = lua.create_function(move |_, args: LuaMultiValue| {
        let parts: Vec<String> = args
            .iter()
            .map(|v| match v {
                LuaValue::String(s) => s.to_string_lossy().to_string(),
                LuaValue::Boolean(b) => b.to_string(),
                LuaValue::Integer(i) => i.to_string(),
                LuaValue::Number(n) => n.to_string(),
                LuaValue::Nil => "nil".to_string(),
                _ => "<value>".to_string(),
            })
            .collect();
        eprintln!("[plugin:{}] {}", plugin_name_clone, parts.join("\t"));
        Ok(())
    })?;
    lua.globals().set("print", print_fn)?;

    lua.globals().set("blue", blue_table)?;
    Ok(())
}

fn is_path_in_workspace(path: &Path, workspace_root: &Path) -> bool {
    if let (Ok(cp), Ok(cr)) = (path.canonicalize(), workspace_root.canonicalize()) {
        return cp.starts_with(&cr);
    }
    path.starts_with(workspace_root)
}
