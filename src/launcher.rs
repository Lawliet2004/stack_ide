use std::path::{Path, PathBuf};
use std::time::Duration;

use crossbeam_channel::{Receiver, TryRecvError};
use egui::{Color32, FontId, Key, Modifiers, RichText, Sense, WidgetInfo, WidgetType};

use crate::theme::SemanticPalette;

const MAX_RESULTS: usize = 100;
const PAGE_SIZE: isize = 10;
const ROW_HEIGHT: f32 = 30.0;
const POPUP_WIDTH: f32 = 680.0;
const POPUP_TOP_MARGIN: f32 = 56.0;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CommandId {
    ShowCommandPalette,
    QuickOpen,
    OpenFolder,
    AddFolderToWorkspace,
    OpenFile,
    Save,
    CloseTab,
    NextTab,
    PreviousTab,
    GoToLine,
    GoToSymbol,
    NewTerminal,
    SplitEditorRight,
    SplitEditorDown,
    FocusNextGroup,
    FocusPreviousGroup,
    ToggleTree,
    ToggleGitPanel,
    ToggleProblems,
    ToggleTerminal,
    ToggleOutline,
    FindInFile,
    ReplaceInFile,
    OpenSettings,
    ReloadSettings,
    /// Reload all Lua plugins from the plugin directory.
    ReloadPlugins,
    /// Invoke a plugin-contributed menu item by label.
    InvokePluginMenuItem(String),
    /// Toggle minimap visibility for the focused pane (Ctrl+Shift+\).
    ToggleMinimap,
    SortLinesAscending,
    SortLinesDescending,
    TransformUppercase,
    TransformLowercase,
    TransformTitleCase,
    TransformCamelCase,
    TransformSnakeCase,
    TransformPascalCase,
    TransformKebabCase,
    ToggleUndoHistory,
    ToggleCallHierarchy,
    ToggleTypeHierarchy,
    // ── Git operations ────────────────────────────────────────────────────────
    /// Toggle the inline blame gutter for the active file.
    GitToggleBlame,
    /// Fetch from the default remote.
    GitFetch,
    /// Pull from the default remote.
    GitPull,
    /// Push to the default remote.
    GitPush,
    /// Open the commit-log viewer modal.
    GitShowLog,
    /// Open the tag manager modal.
    GitShowTags,
    /// Open the conflict resolver modal.
    GitShowConflicts,
    /// Save the current working-tree changes to the stash.
    GitStashSave,
    /// Pop the most recent stash entry.
    GitStashPop,
    /// Toggle zen mode (all panels hidden, editor centered).
    ToggleZenMode,
    /// Toggle distraction-free mode (status/tab bar/gutter hidden).
    ToggleDistractionFree,
    /// Open a diff viewer comparing a file against HEAD.
    OpenDiffWithHead,
    /// Open the New Project wizard.
    NewProject,
    /// Run a named task from tasks.toml.
    RunTask(String),
    /// Rerun the last task.
    RerunLastTask,
    /// Terminate the running task.
    TerminateTask,
    /// Open the Environment Variable editor panel.
    EditEnvVars,
    /// Open the Command History browser.
    OpenHistoryBrowser,
    /// Open the theme picker with live preview (Ctrl+Alt+T).
    SelectTheme,
    /// Toggle vim/modal editing for the active editor.
    ToggleVimMode,
    /// Toggle the AI assistant panel.
    ToggleAssistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub id: CommandId,
    pub title: String,
    pub category: String,
    pub shortcut: Option<String>,
}

impl CommandSpec {
    pub fn new(
        id: CommandId,
        category: impl Into<String>,
        title: impl Into<String>,
        shortcut: Option<&str>,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            category: category.into(),
            shortcut: shortcut.map(str::to_owned),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherMode {
    Commands,
    Files,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LauncherEvent {
    Execute(CommandId),
    OpenFile(PathBuf),
    Dismissed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RowAction {
    Command(CommandId),
    File(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LauncherRow {
    action: RowAction,
    primary: String,
    secondary: String,
    shortcut: Option<String>,
    score: i64,
}

struct IndexResult {
    generation: u64,
    roots: Vec<PathBuf>,
    scan: WorkspaceScan,
}

pub struct LauncherState {
    mode: Option<LauncherMode>,
    query: String,
    selected: usize,
    request_focus: bool,
    commands: Vec<CommandSpec>,
    files: Vec<WorkspaceFile>,
    rows: Vec<LauncherRow>,
    workspace_roots: Vec<PathBuf>,
    generation: u64,
    index_rx: Option<Receiver<IndexResult>>,
    loading: bool,
    error: Option<String>,
    popup_rect: egui::Rect,
}

impl Default for LauncherState {
    fn default() -> Self {
        Self {
            mode: None,
            query: String::new(),
            selected: 0,
            request_focus: false,
            commands: Vec::new(),
            files: Vec::new(),
            rows: Vec::new(),
            workspace_roots: Vec::new(),
            generation: 0,
            index_rx: None,
            loading: false,
            error: None,
            popup_rect: egui::Rect::NOTHING,
        }
    }
}

impl LauncherState {
    pub fn is_open(&self) -> bool {
        self.mode.is_some()
    }

    pub fn mode(&self) -> Option<LauncherMode> {
        self.mode
    }

    pub fn open_commands(&mut self, commands: Vec<CommandSpec>) {
        self.mode = Some(LauncherMode::Commands);
        self.commands = commands;
        self.query.clear();
        self.selected = 0;
        self.request_focus = true;
        self.error = None;
        self.rebuild_rows();
    }

    pub fn open_files(&mut self, roots: Vec<PathBuf>, context: &egui::Context) {
        self.mode = Some(LauncherMode::Files);
        self.query.clear();
        self.selected = 0;
        self.request_focus = true;
        self.error = None;
        if roots.is_empty() {
            self.workspace_roots.clear();
            self.files.clear();
            self.loading = false;
            self.index_rx = None;
            self.rebuild_rows();
        } else {
            self.refresh_workspace(roots, context.clone());
        }
    }

    pub fn dismiss(&mut self) {
        self.mode = None;
        self.query.clear();
        self.rows.clear();
        self.selected = 0;
        self.request_focus = false;
        self.popup_rect = egui::Rect::NOTHING;
    }

    pub fn set_error(&mut self, message: impl Into<String>) {
        self.error = Some(message.into());
    }

    pub fn refresh_workspace(&mut self, roots: Vec<PathBuf>, context: egui::Context) {
        if self.workspace_roots != roots {
            self.files.clear();
        }
        self.workspace_roots = roots.clone();
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.index_rx = Some(rx);
        self.loading = true;
        self.rebuild_rows();
        std::thread::spawn(move || {
            let scan = collect_workspace_files(&roots);
            let _ = tx.send(IndexResult {
                generation,
                roots,
                scan,
            });
            context.request_repaint();
        });
    }

    pub fn poll_index(&mut self) {
        loop {
            let result = match self.index_rx.as_ref().map(Receiver::try_recv) {
                Some(Ok(result)) => result,
                Some(Err(TryRecvError::Empty)) | None => break,
                Some(Err(TryRecvError::Disconnected)) => {
                    self.index_rx = None;
                    self.loading = false;
                    break;
                }
            };
            if result.generation == self.generation
                && self.workspace_roots == result.roots
            {
                self.files = result.scan.files;
                self.error = result.scan.error;
                self.loading = false;
                self.rebuild_rows();
            }
            self.index_rx = None;
        }
    }

    fn rebuild_rows(&mut self) {
        let query = self.query.trim();
        let mut rows = match self.mode {
            Some(LauncherMode::Commands) => self
                .commands
                .iter()
                .filter_map(|command| {
                    let candidate = format!("{} {}", command.category, command.title);
                    let score = fuzzy_score(&candidate, query)?;
                    Some(LauncherRow {
                        action: RowAction::Command(command.id.clone()),
                        primary: command.title.clone(),
                        secondary: command.category.clone(),
                        shortcut: command.shortcut.clone(),
                        score,
                    })
                })
                .collect::<Vec<_>>(),
            Some(LauncherMode::Files) => self
                .files
                .iter()
                .filter_map(|file| {
                    let score = fuzzy_score(&file.display, query)?;
                    let path = Path::new(&file.display);
                    let primary = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    let secondary = path
                        .parent()
                        .filter(|parent| !parent.as_os_str().is_empty())
                        .map(|parent| parent.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_default();
                    Some(LauncherRow {
                        action: RowAction::File(file.path.clone()),
                        primary,
                        secondary,
                        shortcut: None,
                        score,
                    })
                })
                .collect::<Vec<_>>(),
            None => Vec::new(),
        };
        if !query.is_empty() {
            rows.sort_by(|left, right| {
                right
                    .score
                    .cmp(&left.score)
                    .then_with(|| left.primary.len().cmp(&right.primary.len()))
                    .then_with(|| {
                        left.primary
                            .to_lowercase()
                            .cmp(&right.primary.to_lowercase())
                    })
                    .then_with(|| left.secondary.cmp(&right.secondary))
            });
        }
        rows.truncate(MAX_RESULTS);
        self.rows = rows;
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    pub fn show(
        &mut self,
        context: &egui::Context,
        palette: SemanticPalette,
    ) -> Option<LauncherEvent> {
        let mode = self.mode?;
        self.poll_index();
        let mut event = None;
        let mut dismiss = false;
        context.input_mut(|input| {
            if input.consume_key(Modifiers::NONE, Key::Escape) {
                dismiss = true;
            } else if input.consume_key(Modifiers::NONE, Key::ArrowUp) {
                self.move_selection(-1);
            } else if input.consume_key(Modifiers::NONE, Key::ArrowDown) {
                self.move_selection(1);
            } else if input.consume_key(Modifiers::NONE, Key::PageUp) {
                self.move_selection(-PAGE_SIZE);
            } else if input.consume_key(Modifiers::NONE, Key::PageDown) {
                self.move_selection(PAGE_SIZE);
            } else if input.consume_key(Modifiers::NONE, Key::Enter) {
                event = self.selected_event();
            }
        });
        if dismiss {
            event = Some(LauncherEvent::Dismissed);
        }

        let screen = context.screen_rect();
        let backdrop = egui::Area::new(egui::Id::new("launcher_backdrop"))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(context, |ui| {
                let (rect, response) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter()
                    .rect_filled(rect, 0.0, Color32::from_black_alpha(120));
                response
            });

        let width = POPUP_WIDTH.min((screen.width() - 24.0).max(280.0));
        let position = egui::pos2(
            screen.center().x - width * 0.5,
            screen.top() + POPUP_TOP_MARGIN,
        );
        let area = egui::Area::new(egui::Id::new("launcher_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(position)
            .constrain(true)
            .show(context, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::same(10.0))
                    .show(ui, |ui| {
                        ui.set_width(width);
                        let heading = match mode {
                            LauncherMode::Commands => "Command Palette",
                            LauncherMode::Files => "Quick Open",
                        };
                        ui.label(RichText::new(heading).strong());
                        ui.add_space(4.0);
                        let hint = match mode {
                            LauncherMode::Commands => "Type a command",
                            LauncherMode::Files => "Type a file name or path",
                        };
                        let query_response = ui.add_sized(
                            [width, 30.0],
                            egui::TextEdit::singleline(&mut self.query)
                                .id(egui::Id::new("launcher_query"))
                                .hint_text(hint),
                        );
                        if self.request_focus {
                            query_response.request_focus();
                            self.request_focus = false;
                        }
                        if query_response.changed() {
                            self.selected = 0;
                            self.error = None;
                            self.rebuild_rows();
                        }

                        ui.add_space(6.0);
                        ui.separator();
                        let max_height = (screen.height() - POPUP_TOP_MARGIN - 120.0)
                            .clamp(ROW_HEIGHT * 3.0, ROW_HEIGHT * 12.0);
                        egui::ScrollArea::vertical()
                            .max_height(max_height)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                if self.rows.is_empty() {
                                    ui.add_space(10.0);
                                    ui.label(
                                        RichText::new(self.empty_message(mode))
                                            .color(palette.muted_text),
                                    );
                                    ui.add_space(10.0);
                                } else {
                                    for (index, row) in self.rows.iter().enumerate() {
                                        let response =
                                            launcher_row(ui, row, index == self.selected, palette);
                                        if index == self.selected {
                                            ui.scroll_to_rect(response.rect, None);
                                        }
                                        if response.clicked() {
                                            event = Some(event_for_action(&row.action));
                                        }
                                    }
                                }
                            });

                        ui.separator();
                        ui.horizontal(|ui| {
                            if self.loading {
                                ui.label(
                                    RichText::new("Refreshing workspace files…")
                                        .color(palette.muted_text),
                                );
                                context.request_repaint_after(Duration::from_millis(100));
                            } else if let Some(error) = &self.error {
                                ui.label(RichText::new(error).color(palette.error));
                            } else {
                                ui.label(
                                    RichText::new("↑↓ navigate · Enter select · Esc close")
                                        .color(palette.muted_text),
                                );
                            }
                        });
                    })
                    .response
                    .rect
            });
        self.popup_rect = area.inner;

        if backdrop.inner.clicked()
            && context
                .input(|input| input.pointer.interact_pos())
                .is_none_or(|position| !self.popup_rect.contains(position))
        {
            event = Some(LauncherEvent::Dismissed);
        }
        if matches!(event, Some(LauncherEvent::Dismissed)) {
            self.dismiss();
        }
        event
    }

    fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.rows.len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
    }

    fn selected_event(&self) -> Option<LauncherEvent> {
        self.rows
            .get(self.selected)
            .map(|row| event_for_action(&row.action))
    }

    fn empty_message(&self, mode: LauncherMode) -> &str {
        if let Some(error) = self.error.as_deref() {
            return error;
        }
        match mode {
            LauncherMode::Commands if self.query.trim().is_empty() => "No commands available",
            LauncherMode::Commands => "No matching commands",
            LauncherMode::Files if self.workspace_roots.is_empty() => {
                "Open a folder to search workspace files"
            }
            LauncherMode::Files if self.loading => "Indexing workspace files…",
            LauncherMode::Files if self.query.trim().is_empty() => "No workspace files found",
            LauncherMode::Files => "No matching files",
        }
    }
}

fn event_for_action(action: &RowAction) -> LauncherEvent {
    match action {
        RowAction::Command(command) => LauncherEvent::Execute(command.clone()),
        RowAction::File(path) => LauncherEvent::OpenFile(path.clone()),
    }
}

fn launcher_row(
    ui: &mut egui::Ui,
    row: &LauncherRow,
    selected: bool,
    palette: SemanticPalette,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().max(1.0), ROW_HEIGHT),
        Sense::click(),
    );
    let accessible_label = if row.secondary.is_empty() {
        row.primary.clone()
    } else {
        format!("{} — {}", row.primary, row.secondary)
    };
    let label = accessible_label.clone();
    response.widget_info(move || WidgetInfo::labeled(WidgetType::Button, label.clone()));

    if selected {
        ui.painter().rect_filled(rect, 2.0, palette.selection);
    } else if response.hovered() {
        ui.painter()
            .rect_filled(rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    // File-type icon for file rows (Zed quick-open look).
    let mut text_left = rect.left() + 8.0;
    if let RowAction::File(path) = &row.action {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let icon_rect = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 17.0, rect.center().y),
            egui::vec2(14.0, 14.0),
        );
        crate::file_icons::paint(ui.painter(), icon_rect, &name, palette.muted_text);
        text_left = rect.left() + 30.0;
    }
    let primary_pos = egui::pos2(text_left, rect.center().y);
    ui.painter().text(
        primary_pos,
        egui::Align2::LEFT_CENTER,
        &row.primary,
        FontId::proportional(14.0),
        palette.primary_text,
    );
    if !row.secondary.is_empty() {
        ui.painter().text(
            egui::pos2(rect.left() + rect.width() * 0.42, rect.center().y),
            egui::Align2::LEFT_CENTER,
            &row.secondary,
            FontId::proportional(12.0),
            palette.muted_text,
        );
    }
    if let Some(shortcut) = &row.shortcut {
        ui.painter().text(
            egui::pos2(rect.right() - 8.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            shortcut,
            FontId::monospace(12.0),
            palette.muted_text,
        );
    }
    response
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceFile {
    pub path: PathBuf,
    pub display: String,
}

#[derive(Debug, Default)]
pub struct WorkspaceScan {
    pub files: Vec<WorkspaceFile>,
    pub error: Option<String>,
}

pub fn fuzzy_score(candidate: &str, query: &str) -> Option<i64> {
    let candidate = candidate.replace('\\', "/");
    let candidate_lower = candidate.to_lowercase();
    let query_lower = query.trim().to_lowercase();
    if query_lower.is_empty() {
        return Some(0);
    }

    let candidate_chars = candidate_lower.chars().collect::<Vec<_>>();
    let query_chars = query_lower.chars().collect::<Vec<_>>();
    let basename_start = candidate_chars
        .iter()
        .rposition(|ch| *ch == '/')
        .map_or(0, |index| index + 1);
    let mut score = 0_i64;
    let mut search_from = 0;
    let mut previous_match = None;

    for query_char in query_chars.iter().copied() {
        let relative = candidate_chars[search_from..]
            .iter()
            .position(|candidate_char| *candidate_char == query_char)?;
        let index = search_from + relative;
        score += 10;
        if previous_match == Some(index.saturating_sub(1)) {
            score += 18;
        } else if let Some(previous) = previous_match {
            score -= index.saturating_sub(previous + 1) as i64;
        }
        if index == 0
            || candidate_chars
                .get(index.saturating_sub(1))
                .is_some_and(|ch| matches!(ch, '/' | '_' | '-' | '.' | ' '))
        {
            score += 16;
        }
        if index >= basename_start {
            score += 4;
        }
        previous_match = Some(index);
        search_from = index + 1;
    }

    let basename = candidate_chars[basename_start..].iter().collect::<String>();
    if candidate_lower == query_lower {
        score += 1_000;
    } else if basename == query_lower {
        score += 800;
    } else if basename.starts_with(&query_lower) {
        score += 300;
    }
    score -= candidate_chars.len().saturating_sub(query_chars.len()) as i64;
    Some(score)
}

pub fn collect_workspace_files(roots: &[PathBuf]) -> WorkspaceScan {
        let mut scan = WorkspaceScan::default();
        for root in roots {
            let matcher = crate::workspace::ExcludeMatcher::load_for_root(root);
            let walker = ignore::WalkBuilder::new(root)
                .follow_links(false)
                .filter_entry(|entry| {
                    !(entry.depth() > 0
                        && entry.file_type().is_some_and(|kind| kind.is_dir())
                        && entry
                            .file_name()
                            .to_string_lossy()
                            .eq_ignore_ascii_case("target"))
                })
                .build();

            for entry in walker {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        scan.error.get_or_insert_with(|| error.to_string());
                        continue;
                    }
                };
                let Some(kind) = entry.file_type() else {
                    continue;
                };
                if !kind.is_file() || kind.is_symlink() {
                    continue;
                }
                let path = entry.into_path();
                if matcher.is_excluded(&path, root) {
                    continue;
                }
                let root_name = root.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| root.display().to_string());
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                let display = if roots.len() > 1 {
                    format!("{root_name}/{rel}")
                } else {
                    rel
                };
                scan.files.push(WorkspaceFile { path, display });
            }
        }
        scan.files.sort_by(|left, right| {
            left.display
                .to_lowercase()
                .cmp(&right.display.to_lowercase())
                .then_with(|| left.display.cmp(&right.display))
        });
        scan
    }

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        collect_workspace_files, fuzzy_score, CommandId, CommandSpec, LauncherEvent, LauncherMode,
        LauncherState, RowAction, WorkspaceFile,
    };

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("blue_ide_launcher_{name}_{nonce}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn fuzzy_matching_rewards_basename_boundaries_and_contiguous_runs() {
        let contiguous = fuzzy_score("src/search_panel.rs", "sp").unwrap();
        let scattered = fuzzy_score("src/settings/preferences.rs", "sp").unwrap();
        assert!(contiguous > scattered);
        assert!(fuzzy_score("src/AppState.rs", "appstate").is_some());
        assert!(fuzzy_score("src/editor.rs", "xyz").is_none());
    }

    #[test]
    fn workspace_collection_respects_ignore_rules_and_skips_target() {
        let root = temp_dir("walk");
        fs::create_dir_all(root.join("src/nested")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("src/nested/mod.rs"), "pub mod item;\n").unwrap();
        fs::write(root.join("target/debug/output.bin"), "ignored").unwrap();
        fs::write(root.join("ignored.txt"), "ignored").unwrap();
        fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();

        let result = collect_workspace_files(&[root.clone()]);
        let displays = result
            .files
            .iter()
            .map(|file| file.display.as_str())
            .collect::<Vec<_>>();

        assert!(displays.contains(&"src/main.rs"));
        assert!(displays.contains(&"src/nested/mod.rs"));
        assert!(!displays.iter().any(|path| path.starts_with("target/")));
        assert!(!displays.contains(&"ignored.txt"));
        assert!(result.error.is_none());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn command_filtering_is_typed_and_selection_clamps_to_results() {
        let mut state = LauncherState::default();
        state.open_commands(vec![
            CommandSpec::new(CommandId::QuickOpen, "File", "Quick Open", Some("Ctrl+P")),
            CommandSpec::new(
                CommandId::OpenSettings,
                "Preferences",
                "Open Settings",
                None,
            ),
        ]);

        state.selected = 1;
        state.query = "qopen".to_owned();
        state.rebuild_rows();

        assert_eq!(state.mode(), Some(LauncherMode::Commands));
        assert_eq!(state.rows.len(), 1);
        assert_eq!(state.selected, 0);
        assert_eq!(
            state.selected_event(),
            Some(LauncherEvent::Execute(CommandId::QuickOpen))
        );
    }

    #[test]
    fn file_filtering_preserves_native_paths_and_uses_relative_display_text() {
        let native = std::path::PathBuf::from(r"C:\workspace\src\main.rs");
        let mut state = LauncherState {
            mode: Some(LauncherMode::Files),
            files: vec![WorkspaceFile {
                path: native.clone(),
                display: "src/main.rs".to_owned(),
            }],
            query: "smr".to_owned(),
            ..LauncherState::default()
        };

        state.rebuild_rows();

        assert_eq!(state.rows.len(), 1);
        assert_eq!(state.rows[0].primary, "main.rs");
        assert_eq!(state.rows[0].secondary, "src");
        assert_eq!(state.rows[0].action, RowAction::File(native));
    }

    #[test]
    fn launcher_rows_are_keyboard_navigable_and_escape_dismisses() {
        use egui::{Event, Key, Modifiers, RawInput, Rect, Vec2};

        fn key_event(key: Key) -> Event {
            Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }
        }

        let palette = crate::theme::built_in_theme(crate::settings::Theme::Dark, None).palette;
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(900.0, 700.0));
        let mut state = LauncherState::default();
        state.open_commands(vec![
            CommandSpec::new(CommandId::QuickOpen, "File", "Quick Open", None),
            CommandSpec::new(CommandId::OpenSettings, "Preferences", "Settings", None),
        ]);
        let _ = context.run(
            RawInput {
                screen_rect: Some(screen),
                events: vec![key_event(Key::ArrowDown)],
                ..Default::default()
            },
            |ctx| {
                let _ = state.show(ctx, palette.semantic);
            },
        );
        let mut accepted = None;
        let _ = context.run(
            RawInput {
                screen_rect: Some(screen),
                events: vec![key_event(Key::Enter)],
                ..Default::default()
            },
            |ctx| accepted = state.show(ctx, palette.semantic),
        );
        assert_eq!(
            accepted,
            Some(LauncherEvent::Execute(CommandId::OpenSettings))
        );

        let _ = context.run(
            RawInput {
                screen_rect: Some(screen),
                events: vec![key_event(Key::Escape)],
                ..Default::default()
            },
            |ctx| {
                let _ = state.show(ctx, palette.semantic);
            },
        );
        assert!(!state.is_open());
    }
}
