//! Application shell: tabs, menus, LSP orchestration, and editor integration.
//!
//! LSP hover orchestration in this module covers:
//! - **Request correlation** — monotonic UI ids (`next_ui_correlation_id`) tracked in
//!   `lsp_pending` and echoed on typed responses.
//! - **Response routing** — `poll_lsp()` dispatches typed responses using caller
//!   correlation ids echoed from `lsp/transport.rs` (`CompletionList`, `HoverResult`, …).
//! - **Active-file validation** — `receive_hover()` accepts results only when the
//!   request session still matches the active tab and resting pointer target.
//! - **Popup lifecycle** — `update_lsp_hover()` debounces/sends; `dismiss_lsp_hover()`
//!   and `invalidate_lsp_hover_results()` reset high-level state; rendering delegates
//!   to `editor/hover.rs`.
//!
//! Does not encode/decode JSON-RPC (`lsp/transport.rs`), hit-test the editor
//! (`editor/widget.rs`), or lay out hover popup chrome (`editor/hover.rs`).
//!
//! App-layer regression tests (completion/hover poll, stale gates, goto, problems):
//! `cargo test --lib app::tests`. Index: crate root `# Regression tests` in `lib.rs`.
//!
//! | Area | Example tests | Run |
//! |------|---------------|-----|
//! | Ctrl+Space completion e2e | `ctrl_space_sends_a_real_completion_request_and_opens_a_functional_caret_anchored_dropdown` | `cargo test --lib ctrl_space_sends_a_real_completion` |
//! | Completion navigate/accept/click/dismiss | `completion_can_be_navigated_accepted_clicked_and_dismissed` | `cargo test --lib completion_can_be_navigated` |
//! | Accepted completion prefix edit | `accepted_completion_edits_the_correct_identifier_prefix` | `cargo test --lib accepted_completion_edits_the_correct_identifier_prefix` |
//! | Pointer hover e2e | `pointer_hover_sends_a_debounced_real_lsp_hover_request_and_displays_documentation` | `cargo test --lib pointer_hover_sends_a_debounced_real` |
//! | Diagnostic vs LSP hover coexistence | `diagnostic_tooltips_and_lsp_hover_coexist_according_to_the_specified_precedence` | `cargo test --lib diagnostic_tooltips_and_lsp_hover` |
//! | Unicode/LSP position conversion | `unicode_lsp_position_conversion_is_correct` | `cargo test --lib unicode_lsp_position_conversion_is_correct` |
//! | Stale async responses | `stale_asynchronous_responses_cannot_affect_the_current_editor_state` | `cargo test --lib stale_asynchronous_responses_cannot` |
//! | Async response context gates | `validate_request_context_before_applying_asynchronous_responses` | `cargo test --lib validate_request_context_before_applying_asynchronous_responses` |
//! | Popup keyboard accessibility | `keep_all_popup_interactions_keyboard_accessible` | `cargo test --lib keep_all_popup_interactions_keyboard_accessible` |
//! | State transitions + text edits | `add_tests_for_new_state_transitions_and_text_edits` | `cargo test --lib add_tests_for_new_state_transitions_and_text_edits` |
//! | Stale completion/hover drop | `stale_responses_are_ignored`, `stale_lsp_responses_are_rejected` | `cargo test --lib stale_responses` |
//! | Hover poll routing | `poll_lsp_ignores_uncorrelated_hover_results` | `cargo test --lib poll_lsp` |
//! | Never: block on rust-analyzer (UI thread) | `block_waiting_for_rust_analyzer_on_the_ui_thread` | `cargo test --lib block_waiting_for_rust_analyzer_on_the_ui_thread` |
//! | Never: display stale LSP results | `display_stale_lsp_results` | `cargo test --lib display_stale_lsp_results` |
//! | Never: swallow editor keys (no popup) | `swallow_normal_editor_keystrokes_when_no_popup_is_open` | `cargo test --lib swallow_normal_editor_keystrokes_when_no_popup_is_open` |
//! | Never: replace diagnostic tooltips with LSP hover | `replace_diagnostic_tooltips_with_lsp_hover` | `cargo test --lib replace_diagnostic_tooltips_with_lsp_hover` |
//! | Never: hard-coded completion/hover mock data | `render_completion_or_hover_using_hard_coded_mock_data` | `cargo test --lib render_completion_or_hover_using_hard_coded_mock_data` |
//! | Never: weaken/remove tests | `remove_or_weaken_existing_tests_to_make_the_feature_pass` | `cargo test --test integration_test remove_or_weaken_existing_tests_to_make_the_feature_pass` |

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use eframe::egui;
use egui::{Align, Color32, Key, Layout, Modifiers, RichText, ViewportCommand};
use indexmap::IndexMap;
use rfd::FileDialog;

use crate::editor::buffer::{CaseTransform, CursorPosition, EditRecord, RopeOp, TextBuffer};
use crate::editor::completion::{
    completion_acceptance_insert_text, completion_outside_click_event, CompletionPopupAnchor,
    CompletionPopupEvent, CompletionPopupModel, CompletionSession, CompletionState,
};
use crate::editor::hover::{
    apply_lsp_hover_gates, hover_outside_click_event, lsp_hover_allowed, show_hover_documentation,
    show_hover_loading, HoverContentSnapshot, HoverPopupModel, HoverPopupOutput,
    HoverRequestSession, HoveredSourcePosition, HOVER_REST_DELAY_SECS,
};
use crate::editor::position::LspPosition;
use crate::editor::widget::{
    DefinitionTrigger, EditorAction, EditorAnnotations, EditorCommand, EditorInteraction,
    EditorPresentation, EditorState, EditorWidget, SearchHighlight,
};
use crate::filetree::{FileTree, FileTreeAction};
use crate::git::{BlameLine, GitPanelAction, GitRepo};
use crate::language::{LanguageId, LanguageServerId};
use crate::launcher::{CommandId, CommandSpec, LauncherEvent, LauncherState};
use crate::lsp::manager::LspManager;
use crate::lsp::types::{LspCompletionItem, LspResponse, MessageLevel};
#[cfg(test)]
use crate::lsp::LspClient;
use crate::outline::OutlinePanel;
use crate::pane_content::PaneContent;
use crate::panes::{CloseResult, FocusState, PaneAction, PaneId, PaneTree};
use crate::plugins::{NotifyLevel, PluginAction, PluginApiContext, PluginEvent, PluginSystem};
use crate::problems_panel;
use crate::search::{compute_replacement, SearchScope, SearchState};
use crate::search_panel;
use crate::settings::{Settings, SettingsStore};
use crate::theme::{built_in_theme, ColorScheme, ThemePalette};
use crate::editorconfig::EditorConfigSettings;
use crate::tasks::TaskPanelState;
use crate::project_template::NewProjectState;
use crate::terminal_mux::TerminalMux;
use crate::trust_ui::TrustPromptState;
use crate::workspace::{TrustStore, Workspace};
use crate::zen_mode::ZenState;
use crate::text::font_loader;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspPendingKind {
    Completion,
    Hover,
    GotoDefinition,
    DocumentSymbol,
    Format,
    SignatureHelp,
    WorkspaceSymbol,
    CodeAction,
    CodeLens(PathBuf),
    SemanticTokens(PathBuf),
    CallHierarchy {
        path: PathBuf,
        parent_uri: String,
        parent_range: Option<crate::lsp::types::LspRange>,
    },
    TypeHierarchy {
        path: PathBuf,
        parent_uri: String,
        parent_range: Option<crate::lsp::types::LspRange>,
    },
    InlayHint(PathBuf),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct TabGroup {
    pub name: String,
    pub color_rgba: [u8; 4],
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AppSessionState {
    pub roots: Vec<PathBuf>,
    pub tabs: Vec<PathBuf>,
    pub active: Option<PathBuf>,
    pub pane_tree: Option<crate::panes::PaneTree>,
    pub pinned_tabs: std::collections::BTreeSet<PathBuf>,
    pub tab_groups: Vec<TabGroup>,
    pub tab_to_group: std::collections::HashMap<PathBuf, String>,
    pub bookmarks: std::collections::HashMap<PathBuf, std::collections::BTreeSet<usize>>,
    pub scroll_positions: std::collections::HashMap<PathBuf, f32>,
}

#[derive(Debug, Default)]
struct RecentFilesState {
    pub open: bool,
    pub query: String,
    pub selected: usize,
}

#[derive(Debug, Default)]
struct RecentWorkspacesState {
    pub open: bool,
    pub query: String,
    pub selected: usize,
}

// ─── GoToLine modal state ─────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct GoToLineState {
    pub open: bool,
    pub input: String,
    /// Non-empty when the input is out of range.
    pub error: Option<String>,
}

impl GoToLineState {
    fn open_for(&mut self, current_line: usize) {
        self.open = true;
        self.input = (current_line + 1).to_string();
        self.error = None;
    }
}

// ─── Theme picker modal state (live preview, Zed-style) ───────────────────────

#[derive(Debug, Default)]
struct ThemePickerState {
    pub open: bool,
    pub query: String,
    pub selected: usize,
    pub request_focus: bool,
}

#[derive(Debug)]
struct NewTabGroupState {
    pub open: bool,
    pub name: String,
    pub selected_color: [u8; 4],
    pub target_file: Option<PathBuf>,
}

impl Default for NewTabGroupState {
    fn default() -> Self {
        Self {
            open: false,
            name: String::new(),
            selected_color: [99, 102, 241, 255], // Indigo Dream default
            target_file: None,
        }
    }
}

// ─── Workspace symbol picker state ───────────────────────────────────────────

#[derive(Debug, Default)]
struct WorkspaceSymbolState {
    pub open: bool,
    pub query: String,
    pub results: Vec<crate::lsp::types::WorkspaceSymbol>,
    pub selected: usize,
    /// Pending correlation id (cleared when the response arrives).
    pub pending_id: Option<u64>,
    /// Debounce: timestamp of last query change.
    pub last_query_changed: Option<f64>,
}

impl WorkspaceSymbolState {
    const DEBOUNCE_SECS: f64 = 0.25;

    fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.results.clear();
        self.selected = 0;
        self.pending_id = None;
        self.last_query_changed = None;
    }

    fn is_debounce_elapsed(&self, now: f64) -> bool {
        self.last_query_changed
            .is_some_and(|t| now - t >= Self::DEBOUNCE_SECS)
    }
}

// ─── Signature help popup state ───────────────────────────────────────────────

#[derive(Debug, Default)]
struct SignatureHelpState {
    pub active: Option<crate::lsp::types::SignatureInfo>,
    /// Pending correlation id (cleared when the response arrives).
    pub pending_id: Option<u64>,
    /// Path + cursor at which the last request was sent.
    pub last_request_path: Option<std::path::PathBuf>,
    pub last_request_cursor: Option<CursorPosition>,
}

const TITLE_BAR_HEIGHT: f32 = 32.0;
const TOP_MENU_LABELS: [&str; 8] = [
    "File",
    "Edit",
    "Selection",
    "View",
    "Go",
    "Run",
    "Window",
    "Help",
];

impl BlueIdeApp {
    fn show_menu(&mut self, context: &egui::Context) -> Option<CommandId> {
        let mut command = None;
        let palette = self.active_palette.semantic;
        let maximized = context.input(|input| input.viewport().maximized.unwrap_or(false));

        let workspace_name = self
            .tree
            .root_path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "No Project".to_string());

        egui::TopBottomPanel::top("menu_bar")
            .exact_height(TITLE_BAR_HEIGHT)
            .frame(
                egui::Frame::none()
                    .fill(palette.ui_background)
                    .inner_margin(egui::Margin::same(0.0)),
            )
            .show(context, |ui| {
                ui.spacing_mut().interact_size.y = TITLE_BAR_HEIGHT;
                ui.spacing_mut().button_padding = egui::vec2(6.0, 2.0);
                ui.spacing_mut().item_spacing.x = 2.0;
                let full_rect = ui.max_rect();

                // Whole-bar drag handle, registered first so it sits beneath the
                // interactive widgets painted on top of it.
                let drag_response = ui.interact(
                    full_rect,
                    ui.make_persistent_id("title_bar_drag"),
                    egui::Sense::click_and_drag(),
                );
                drag_response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Other, "Window title bar")
                });
                if drag_response.double_clicked() {
                    context.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                } else if drag_response.drag_started() {
                    context.send_viewport_cmd(ViewportCommand::StartDrag);
                }

                // ── Center: Zed-style project switcher pill ──
                if !self.has_modal() {
                    let branch = self
                        .git
                        .as_ref()
                        .map(|git| git.branch.clone())
                        .unwrap_or_default();
                    let pill_rect = egui::Rect::from_center_size(
                        egui::pos2(full_rect.center().x, full_rect.center().y),
                        egui::vec2(224.0, 24.0),
                    );
                    if Self::project_switcher(ui, pill_rect, palette, &workspace_name, &branch) {
                        command = Some(CommandId::QuickOpen);
                    }
                }

                // ── Right cluster: overflow menu (full IDE menus) + window controls ──
                let mut right_ui =
                    ui.child_ui(full_rect, egui::Layout::right_to_left(egui::Align::Center));

                if Self::paint_window_control(
                    &mut right_ui,
                    WindowControl::Close,
                    palette.primary_text,
                ) {
                    context.send_viewport_cmd(ViewportCommand::Close);
                }
                if Self::paint_window_control(
                    &mut right_ui,
                    WindowControl::Maximize { maximized },
                    palette.primary_text,
                ) {
                    context.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                }
                if Self::paint_window_control(
                    &mut right_ui,
                    WindowControl::Minimize,
                    palette.primary_text,
                ) {
                    context.send_viewport_cmd(ViewportCommand::Minimized(true));
                }

                right_ui.add_space(4.0);
                right_ui.add_enabled_ui(!self.has_modal(), |ui| {
                    ui.menu_button(RichText::new("⋯").size(16.0), |ui| {
                        ui.spacing_mut().button_padding = egui::vec2(5.0, 1.0);
                        ui.style_mut().override_font_id = Some(egui::FontId::proportional(12.0));
                        ui.spacing_mut().item_spacing.x = 2.0;

                        // Flat menu items: no box behind the items at rest. A subtle
                        // background only appears on hover / when a submenu is open.
                        let widgets = &mut ui.visuals_mut().widgets;
                        widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                        widgets.inactive.bg_fill = Color32::TRANSPARENT;
                        widgets.inactive.bg_stroke = egui::Stroke::NONE;
                        widgets.active.bg_stroke = egui::Stroke::NONE;
                        widgets.hovered.bg_stroke = egui::Stroke::NONE;
                        widgets.open.bg_stroke = egui::Stroke::NONE;

                        ui.menu_button(TOP_MENU_LABELS[0], |ui| {
                        if ui.button("Quick Open...     Ctrl+P").clicked() {
                            command = Some(CommandId::QuickOpen);
                            ui.close_menu();
                        }
                        if ui.button("Open Folder...").clicked() {
                            command = Some(CommandId::OpenFolder);
                            ui.close_menu();
                        }
                        if ui.button("Add Folder to Workspace...").clicked() {
                            command = Some(CommandId::AddFolderToWorkspace);
                            ui.close_menu();
                        }
                        if ui.button("Open File...    Ctrl+O").clicked() {
                            command = Some(CommandId::OpenFile);
                            ui.close_menu();
                        }
                        if ui.button("New Project...").clicked() {
                            command = Some(CommandId::NewProject);
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(
                                self.active.is_some(),
                                egui::Button::new("Save      Ctrl+S"),
                            )
                            .clicked()
                        {
                            command = Some(CommandId::Save);
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(
                                self.active.is_some(),
                                egui::Button::new("Close Tab Ctrl+W"),
                            )
                            .clicked()
                        {
                            command = Some(CommandId::CloseTab);
                            ui.close_menu();
                        }
                    });
                    ui.menu_button(TOP_MENU_LABELS[1], |ui| {
                        if ui.button("Find             Ctrl+F").clicked() {
                            command = Some(CommandId::FindInFile);
                            ui.close_menu();
                        }
                        if ui.button("Replace          Ctrl+H").clicked() {
                            command = Some(CommandId::ReplaceInFile);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Settings...").clicked() {
                            command = Some(CommandId::OpenSettings);
                            ui.close_menu();
                        }
                        if ui.button("Reload Settings").clicked() {
                            command = Some(CommandId::ReloadSettings);
                            ui.close_menu();
                        }
                    });
                    ui.menu_button(TOP_MENU_LABELS[2], |ui| {
                        ui.add_enabled(false, egui::Button::new("Select All    Ctrl+A"))
                            .on_disabled_hover_text(
                                "Text selection is not available in this editor yet",
                            );
                    });
                    ui.menu_button(TOP_MENU_LABELS[3], |ui| {
                        if ui.button("Command Palette... Ctrl+Shift+P").clicked() {
                            command = Some(CommandId::ShowCommandPalette);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("File Tree").clicked() {
                            command = Some(CommandId::ToggleTree);
                            ui.close_menu();
                        }
                        if ui.button("Source Control     Ctrl+Shift+G").clicked() {
                            command = Some(CommandId::ToggleGitPanel);
                            ui.close_menu();
                        }
                        if self.git.is_some() {
                            ui.separator();
                            if ui.button("Blame              Ctrl+Shift+B").clicked() {
                                command = Some(CommandId::GitToggleBlame);
                                ui.close_menu();
                            }
                            if ui.button("Commit History   Alt+Shift+H").clicked() {
                                command = Some(CommandId::GitShowLog);
                                ui.close_menu();
                            }
                            if ui.button("Tags").clicked() {
                                command = Some(CommandId::GitShowTags);
                                ui.close_menu();
                            }
                            if ui.button("Resolve Conflicts").clicked() {
                                command = Some(CommandId::GitShowConflicts);
                                ui.close_menu();
                            }
                            ui.separator();
                        }
                        if ui.button("Problems      Ctrl+Shift+M").clicked() {
                            command = Some(CommandId::ToggleProblems);
                            ui.close_menu();
                        }
                        if ui.button("Terminal      Ctrl+`").clicked() {
                            command = Some(CommandId::ToggleTerminal);
                            ui.close_menu();
                        }
                        if ui.button("Outline       Ctrl+Shift+O").clicked() {
                            command = Some(CommandId::ToggleOutline);
                            ui.close_menu();
                        }
                        if ui.button("Minimap       Ctrl+Shift+\\").clicked() {
                            command = Some(CommandId::ToggleMinimap);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui
                            .selectable_label(self.settings.appearance.high_contrast, "High Contrast Mode")
                            .clicked()
                        {
                            self.settings.appearance.high_contrast = !self.settings.appearance.high_contrast;
                            self.active_palette = Self::apply_appearance_settings(
                                context,
                                &self.settings.appearance,
                                self.system_scheme,
                            );
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Select Theme…  Ctrl+Alt+T").clicked() {
                            command = Some(CommandId::SelectTheme);
                            ui.close_menu();
                        }
                        if ui.button("Assistant Panel  Ctrl+Alt+A").clicked() {
                            command = Some(CommandId::ToggleAssistant);
                            ui.close_menu();
                        }
                        if ui.button("Vim Mode       Ctrl+Alt+V").clicked() {
                            command = Some(CommandId::ToggleVimMode);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Zen Mode      Ctrl+K Ctrl+Z").clicked() {
                            command = Some(CommandId::ToggleZenMode);
                            ui.close_menu();
                        }
                        if ui.button("Distraction Free  Ctrl+Shift+F11").clicked() {
                            command = Some(CommandId::ToggleDistractionFree);
                            ui.close_menu();
                        }
                    });
                    ui.menu_button(TOP_MENU_LABELS[4], |ui| {
                        if ui
                            .add_enabled(
                                self.active.is_some(),
                                egui::Button::new("Go to Line...       Ctrl+G"),
                            )
                            .clicked()
                        {
                            command = Some(CommandId::GoToLine);
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(
                                !self.workspace.roots().is_empty(),
                                egui::Button::new("Go to Symbol...     Ctrl+T"),
                            )
                            .clicked()
                        {
                            command = Some(CommandId::GoToSymbol);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Previous Tab   Ctrl+Shift+Tab").clicked() {
                            command = Some(CommandId::PreviousTab);
                            ui.close_menu();
                        }
                        if ui.button("Next Tab             Ctrl+Tab").clicked() {
                            command = Some(CommandId::NextTab);
                            ui.close_menu();
                        }
                    });
                    ui.menu_button(TOP_MENU_LABELS[5], |ui| {
                                                if ui.add(
                            egui::Button::new("New Terminal     Ctrl+Shift+5")
                        ).on_hover_text("Open a new terminal").clicked() {
                            command = Some(CommandId::NewTerminal);
                            ui.close_menu();
                        }
                        if ui.add(
                            egui::Button::new("Toggle Terminal        Ctrl+`")
                        ).on_hover_text("Toggle terminal panel visibility").clicked() {
                            command = Some(CommandId::ToggleTerminal);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Edit Project Environment Variables").clicked() {
                            command = Some(CommandId::EditEnvVars);
                            ui.close_menu();
                        }
                        // ── Task runner ──────────────────────────────────────
                        if !self.task_panel.tasks.is_empty() {
                            let mut task_names: Vec<String> =
                                self.task_panel.tasks.keys().cloned().collect();
                            task_names.sort();
                            if !task_names.is_empty() {
                                ui.separator();
                                ui.label(
                                    egui::RichText::new("Tasks")
                                        .small()
                                        .color(self.active_palette.semantic.muted_text),
                                );
                                for name in task_names {
                                    let btn_resp = ui.add(
                                        egui::Button::new(format!("▶ {name}"))
                                    );
                                    let btn_resp = crate::screen_reader::label_element(
                                        ui,
                                        btn_resp,
                                        &format!("Run task: {name}"),
                                        &format!("Run task {name}"),
                                    );
                                    if btn_resp.clicked() {
                                        command = Some(CommandId::RunTask(name.clone()));
                                        ui.close_menu();
                                    }
                                }
                                if self.task_panel.running.as_ref().is_some_and(|h| h.is_running()) {
                                    if ui.add(
                                        egui::Button::new("⏹ Terminate Task")
                                    ).on_hover_text("Terminate running task").clicked() {
                                        command = Some(CommandId::TerminateTask);
                                        ui.close_menu();
                                    }
                                }
                            }
                        }
                        // ── Git remote / stash operations ──────────────────
                        if self.git.is_some() {
                            ui.separator();
                            ui.menu_button("Git", |ui| {
                                if ui.button("Fetch       Alt+Shift+F").clicked() {
                                    command = Some(CommandId::GitFetch);
                                    ui.close_menu();
                                }
                                if ui.button("Pull        Alt+Shift+L").clicked() {
                                    command = Some(CommandId::GitPull);
                                    ui.close_menu();
                                }
                                if ui.button("Push        Alt+Shift+U").clicked() {
                                    command = Some(CommandId::GitPush);
                                    ui.close_menu();
                                }
                                ui.separator();
                                if ui.button("Stash Changes  Alt+Shift+S").clicked() {
                                    command = Some(CommandId::GitStashSave);
                                    ui.close_menu();
                                }
                                if ui.button("Pop Stash      Alt+Shift+P").clicked() {
                                    command = Some(CommandId::GitStashPop);
                                    ui.close_menu();
                                }
                                ui.separator();
                                if ui.button("Commit History  Alt+Shift+H").clicked() {
                                    command = Some(CommandId::GitShowLog);
                                    ui.close_menu();
                                }
                                if ui.button("Manage Tags").clicked() {
                                    command = Some(CommandId::GitShowTags);
                                    ui.close_menu();
                                }
                                if ui.button("Resolve Conflicts").clicked() {
                                    command = Some(CommandId::GitShowConflicts);
                                    ui.close_menu();
                                }
                            });
                        }
                    });
                    ui.menu_button(TOP_MENU_LABELS[6], |ui| {
                        let can_focus_another_group = self.pane_tree.all_leaf_ids().len() > 1;
                        if ui.button("Split Editor Right     Ctrl+\\").clicked() {
                            command = Some(CommandId::SplitEditorRight);
                            ui.close_menu();
                        }
                        if ui.button("Split Editor Down").clicked() {
                            command = Some(CommandId::SplitEditorDown);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui
                            .add_enabled(
                                can_focus_another_group,
                                egui::Button::new("Focus Previous Group   Ctrl+Alt+Left"),
                            )
                            .clicked()
                        {
                            command = Some(CommandId::FocusPreviousGroup);
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(
                                can_focus_another_group,
                                egui::Button::new("Focus Next Group       Ctrl+Alt+Right"),
                            )
                            .clicked()
                        {
                            command = Some(CommandId::FocusNextGroup);
                            ui.close_menu();
                        }
                    });
                    ui.menu_button(TOP_MENU_LABELS[7], |ui| {
                        if ui.button("Reload Plugins  Ctrl+Shift+R").clicked() {
                            command = Some(CommandId::ReloadPlugins);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Startup Performance").clicked() {
                            self.startup_breakdown.open_panel();
                            ui.close_menu();
                        }
                        let items: Vec<_> = self
                            .plugin_system
                            .menu_items
                            .iter()
                            .map(|item| item.label.clone())
                            .collect();
                        if !items.is_empty() {
                            ui.separator();
                            for label in items {
                                if ui.button(&label).clicked() {
                                    command = Some(CommandId::InvokePluginMenuItem(label.clone()));
                                    ui.close_menu();
                                }
                            }
                        }
                    });
                    });
                });

                crate::keyboard_nav::draw_focus_outline(ui, egui::Id::new("menu_bar"), full_rect);
            });

        command
    }

    /// Zed-style centered project switcher: a rounded pill showing the active
    /// workspace and current git branch with a trailing chevron. Returns `true`
    /// when clicked (or activated via Enter while keyboard-focused).
    fn project_switcher(
        ui: &mut egui::Ui,
        rect: egui::Rect,
        palette: crate::theme::SemanticPalette,
        workspace: &str,
        branch: &str,
    ) -> bool {
        let id = egui::Id::new("project_switcher");
        let response = ui.interact(rect, id, egui::Sense::click());
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let focused = response.has_focus();

        let painter = ui.painter();
        let bg = if response.hovered() {
            palette.panel_background
        } else {
            palette.panel_background
        };
        painter.rect(
            rect,
            egui::Rounding::same(6.0),
            bg,
            egui::Stroke::new(1.0, palette.border),
        );

        let font = egui::FontId::proportional(12.0);
        let label = if branch.is_empty() {
            workspace.to_owned()
        } else {
            format!("{workspace}  ⎇ {branch}")
        };
        let chevron = "▾";
        let text_w = ui.fonts(|f| {
            f.layout_no_wrap(label.clone(), font.clone(), palette.primary_text)
                .size()
                .x
        });
        let chevron_w = ui.fonts(|f| {
            f.layout_no_wrap(chevron.to_owned(), font.clone(), palette.muted_text)
                .size()
                .x
        });
        let gap = 6.0;
        let total_w = text_w + gap + chevron_w;
        let start_x = (rect.center().x - total_w * 0.5).max(rect.left() + 8.0);

        painter.text(
            egui::pos2(start_x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            font,
            palette.primary_text,
        );
        painter.text(
            egui::pos2(start_x + text_w + gap, rect.center().y),
            egui::Align2::LEFT_CENTER,
            chevron,
            egui::FontId::proportional(10.0),
            palette.muted_text,
        );

        let activated_by_key = focused && ui.input(|i| i.key_pressed(egui::Key::Enter));
        response
            .on_hover_text("Switch project (Ctrl+P)")
            .clicked()
            || activated_by_key
    }

    fn paint_window_control(ui: &mut egui::Ui, control: WindowControl, color: Color32) -> bool {
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(46.0, TITLE_BAR_HEIGHT), egui::Sense::click());
        if response.hovered() {
            let hover = if control == WindowControl::Close {
                Color32::from_rgb(196, 43, 28)
            } else {
                Color32::from_white_alpha(24)
            };
            ui.painter().rect_filled(rect, 0.0, hover);
        }

        let center = rect.center();
        let stroke = egui::Stroke::new(1.0, color);
        match control {
            WindowControl::Minimize => {
                ui.painter().line_segment(
                    [
                        egui::pos2(center.x - 5.0, center.y + 3.0),
                        egui::pos2(center.x + 5.0, center.y + 3.0),
                    ],
                    stroke,
                );
            }
            WindowControl::Maximize { maximized: false } => {
                ui.painter().rect_stroke(
                    egui::Rect::from_center_size(center, egui::vec2(10.0, 10.0)),
                    0.5,
                    stroke,
                );
            }
            WindowControl::Maximize { maximized: true } => {
                let back = egui::Rect::from_min_size(
                    egui::pos2(center.x - 3.0, center.y - 5.0),
                    egui::vec2(8.0, 8.0),
                );
                let front = egui::Rect::from_min_size(
                    egui::pos2(center.x - 5.0, center.y - 3.0),
                    egui::vec2(8.0, 8.0),
                );
                ui.painter().rect_stroke(back, 0.5, stroke);
                ui.painter()
                    .rect_filled(front, 0.0, ui.visuals().panel_fill);
                ui.painter().rect_stroke(front, 0.5, stroke);
            }
            WindowControl::Close => {
                ui.painter().line_segment(
                    [
                        egui::pos2(center.x - 5.0, center.y - 5.0),
                        egui::pos2(center.x + 5.0, center.y + 5.0),
                    ],
                    stroke,
                );
                ui.painter().line_segment(
                    [
                        egui::pos2(center.x + 5.0, center.y - 5.0),
                        egui::pos2(center.x - 5.0, center.y + 5.0),
                    ],
                    stroke,
                );
            }
        }
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, control.accessible_name())
        });
        response.clicked()
    }
}

// ─── Code action state ────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct CodeActionState {
    pub open: bool,
    pub actions: Vec<crate::lsp::types::CodeAction>,
    pub selected: usize,
    /// Pending correlation id.
    pub pending_id: Option<u64>,
    /// Path + range for which actions were requested.
    pub request_path: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HoverTarget {
    path: PathBuf,
    position: LspPosition,
}

/// High-level LSP hover popup lifecycle (debounce timers, in-flight session, content).
#[derive(Debug, Default)]
struct LspHoverState {
    content: Option<String>,
    session: Option<HoverRequestSession>,
    resting_target: Option<HoverTarget>,
    rest_started: Option<f64>,
    displayed_target: Option<HoverTarget>,
    content_snapshot: Option<HoverContentSnapshot>,
    /// Resting target that already returned null/empty hover content.
    no_content_target: Option<HoverTarget>,
    /// Resting target for which a hover request has already been sent.
    request_sent_for: Option<HoverTarget>,
    /// Live screen-space bounds of the hovered token anchoring the hover popup.
    popup_anchor: Option<egui::Rect>,
    /// Screen-space bounds of the hover popup from the most recent rendered frame.
    popup_rect: Option<egui::Rect>,
}

impl LspHoverState {
    fn request_already_sent_for(&self, target: &HoverTarget) -> bool {
        self.request_sent_for.as_ref() == Some(target)
    }

    /// True while an in-flight hover request can still populate the popup.
    fn in_flight_request_active(&self) -> bool {
        self.session.is_some()
    }
}
struct PendingDefinitionRequest {
    source_path: PathBuf,
    source_revision: u64,
    source_position: CursorPosition,
    active_tab: PathBuf,
    is_f12: bool,
}

/// Result payload from a background blame computation.
struct BlameResult {
    path: PathBuf,
    lines: Vec<BlameLine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HierarchyDirection {
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeDirection {
    Supertypes,
    Subtypes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HierarchyKind {
    Call(HierarchyDirection),
    Type(TypeDirection),
}

#[derive(Debug, Clone)]
pub enum HierarchyItem {
    Call(crate::lsp::types::CallHierarchyItem),
    Type(crate::lsp::types::TypeHierarchyItem),
}

impl HierarchyItem {
    pub fn name(&self) -> &str {
        match self {
            Self::Call(item) => &item.name,
            Self::Type(item) => &item.name,
        }
    }

    pub fn uri(&self) -> &str {
        match self {
            Self::Call(item) => &item.uri,
            Self::Type(item) => &item.uri,
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Call(item) => item.detail.as_deref(),
            Self::Type(item) => item.detail.as_deref(),
        }
    }

    pub fn kind(&self) -> u64 {
        match self {
            Self::Call(item) => item.kind,
            Self::Type(item) => item.kind,
        }
    }

    pub fn range(&self) -> crate::lsp::types::LspRange {
        match self {
            Self::Call(item) => item.range.clone(),
            Self::Type(item) => item.range.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HierarchyNode {
    pub item: HierarchyItem,
    pub children: Option<Vec<HierarchyNode>>,
    pub expanded: bool,
}

#[derive(Debug, Clone)]
pub struct HierarchyPanel {
    pub root: HierarchyNode,
    pub kind: HierarchyKind,
    pub visible: bool,
}

/// Focus targets for keyboard navigation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    MenuBar,
    SearchBar,
    Sidebar,
    TabBar,
    Editor,
    Terminal,
}

impl FocusTarget {
    /// Get the next focus target in order
    pub fn next(self) -> Self {
        match self {
            Self::MenuBar => Self::SearchBar,
            Self::SearchBar => Self::Sidebar,
            Self::Sidebar => Self::TabBar,
            Self::TabBar => Self::Editor,
            Self::Editor => Self::Terminal,
            Self::Terminal => Self::MenuBar,
        }
    }

    /// Get the previous focus target in order
    pub fn prev(self) -> Self {
        match self {
            Self::MenuBar => Self::Terminal,
            Self::SearchBar => Self::MenuBar,
            Self::Sidebar => Self::SearchBar,
            Self::TabBar => Self::Sidebar,
            Self::Editor => Self::TabBar,
            Self::Terminal => Self::Editor,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BottomPanelTab {
    Search,
    Problems,
    CallHierarchy,
    TypeHierarchy,
    Output,
    DebugConsole,
    Terminal,
    Ports,
    Profiler,
}

impl BottomPanelTab {
    /// The category tabs shown along the top of the bottom panel, mirroring the
    /// VS Code integrated-panel layout.
    const ALL: [BottomPanelTab; 9] = [
        BottomPanelTab::Search,
        BottomPanelTab::Problems,
        BottomPanelTab::CallHierarchy,
        BottomPanelTab::TypeHierarchy,
        BottomPanelTab::Output,
        BottomPanelTab::DebugConsole,
        BottomPanelTab::Terminal,
        BottomPanelTab::Ports,
        BottomPanelTab::Profiler,
    ];

    fn label(self) -> &'static str {
        match self {
            BottomPanelTab::Search => "Search",
            BottomPanelTab::Problems => "Problems",
            BottomPanelTab::CallHierarchy => "Call Hierarchy",
            BottomPanelTab::TypeHierarchy => "Type Hierarchy",
            BottomPanelTab::Output => "Output",
            BottomPanelTab::DebugConsole => "Debug Console",
            BottomPanelTab::Terminal => "Terminal",
            BottomPanelTab::Ports => "Ports",
            BottomPanelTab::Profiler => "Profiler",
        }
    }
}

/// Paint a single VS Code-style panel tab: flat text that is muted when
/// inactive, brightened on hover, and underlined with the accent color when
/// active. Returns the click response.
fn paint_panel_tab(
    ui: &mut egui::Ui,
    text: &str,
    active: bool,
    palette: crate::theme::SemanticPalette,
) -> egui::Response {
    // VS Code renders panel tab labels in uppercase.
    let label = text.to_uppercase();
    let font = egui::FontId::proportional(11.0);
    let text_w = ui.fonts(|f| {
        f.layout_no_wrap(label.clone(), font.clone(), palette.primary_text)
            .size()
            .x
    });
    let pad_x = 10.0;
    let height = ui.available_height().max(1.0);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(text_w + pad_x * 2.0, height), egui::Sense::click());

    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        if !active {
            ui.painter()
                .rect_filled(rect, 0.0, Color32::from_white_alpha(10));
        }
    }

    let text_color = if active || response.hovered() {
        palette.primary_text
    } else {
        palette.muted_text
    };
    ui.painter().text(
        egui::pos2(rect.left() + pad_x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        font,
        text_color,
    );

    if active {
        let y = rect.bottom() - 1.5;
        ui.painter().line_segment(
            [
                egui::pos2(rect.left() + pad_x * 0.5, y),
                egui::pos2(rect.right() - pad_x * 0.5, y),
            ],
            egui::Stroke::new(2.0, palette.accent),
        );
    }

    response
}

/// Action icons drawn in the bottom-panel's right-hand toolbar.
#[derive(Clone, Copy)]
enum TermToolIcon {
    New,
    Split,
    Kill,
    Close,
}

/// Paint a crisp, monochrome terminal-toolbar icon button (VS Code style):
/// muted by default, brightened with a subtle rounded hover background.
/// Returns `true` when clicked.
fn paint_term_tool(
    ui: &mut egui::Ui,
    icon: TermToolIcon,
    palette: crate::theme::SemanticPalette,
) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(26.0, 24.0), egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(
            rect.shrink2(egui::vec2(2.0, 3.0)),
            4.0,
            Color32::from_white_alpha(20),
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let c = rect.center();
    let color = if response.hovered() {
        palette.primary_text
    } else {
        palette.muted_text
    };
    let stroke = egui::Stroke::new(1.3, color);
    let painter = ui.painter();
    match icon {
        TermToolIcon::New => {
            painter.line_segment(
                [egui::pos2(c.x - 4.0, c.y), egui::pos2(c.x + 4.0, c.y)],
                stroke,
            );
            painter.line_segment(
                [egui::pos2(c.x, c.y - 4.0), egui::pos2(c.x, c.y + 4.0)],
                stroke,
            );
        }
        TermToolIcon::Split => {
            let r = egui::Rect::from_center_size(c, egui::vec2(11.0, 9.0));
            painter.rect_stroke(r, 1.0, stroke);
            painter.line_segment(
                [egui::pos2(c.x, r.top()), egui::pos2(c.x, r.bottom())],
                stroke,
            );
        }
        TermToolIcon::Kill => {
            let w = 4.2;
            // Lid.
            painter.line_segment(
                [egui::pos2(c.x - w - 1.0, c.y - 3.0), egui::pos2(c.x + w + 1.0, c.y - 3.0)],
                stroke,
            );
            // Handle.
            painter.line_segment(
                [egui::pos2(c.x - 1.8, c.y - 3.0), egui::pos2(c.x - 1.8, c.y - 4.6)],
                stroke,
            );
            painter.line_segment(
                [egui::pos2(c.x - 1.8, c.y - 4.6), egui::pos2(c.x + 1.8, c.y - 4.6)],
                stroke,
            );
            painter.line_segment(
                [egui::pos2(c.x + 1.8, c.y - 4.6), egui::pos2(c.x + 1.8, c.y - 3.0)],
                stroke,
            );
            // Can body (slightly tapered).
            painter.line_segment(
                [egui::pos2(c.x - w, c.y - 3.0), egui::pos2(c.x - w + 0.8, c.y + 4.6)],
                stroke,
            );
            painter.line_segment(
                [egui::pos2(c.x + w, c.y - 3.0), egui::pos2(c.x + w - 0.8, c.y + 4.6)],
                stroke,
            );
            painter.line_segment(
                [egui::pos2(c.x - w + 0.8, c.y + 4.6), egui::pos2(c.x + w - 0.8, c.y + 4.6)],
                stroke,
            );
        }
        TermToolIcon::Close => {
            painter.line_segment(
                [egui::pos2(c.x - 4.0, c.y - 4.0), egui::pos2(c.x + 4.0, c.y + 4.0)],
                stroke,
            );
            painter.line_segment(
                [egui::pos2(c.x + 4.0, c.y - 4.0), egui::pos2(c.x - 4.0, c.y + 4.0)],
                stroke,
            );
        }
    }

    let tip = match icon {
        TermToolIcon::New => "New Terminal (Ctrl+Shift+5)",
        TermToolIcon::Split => "Split Terminal",
        TermToolIcon::Kill => "Kill Terminal",
        TermToolIcon::Close => "Close Panel",
    };
    response.on_hover_text(tip).clicked()
}

pub struct BlueIdeApp {
    pub buffers: IndexMap<PathBuf, TextBuffer>,
    pub pane_tree: PaneTree,
    pub focus: FocusState,
    /// Current keyboard focus target for navigation
    pub focus_target: FocusTarget,
    pub pane_actions: Vec<PaneAction>,
    /// Compatibility mirror for older app code paths; focused pane remains authoritative.
    pub active: Option<PathBuf>,
    pub tree: FileTree,
    pub show_tree: bool,
    pub show_problems: bool,
    pub show_problems_errors: bool,
    pub show_problems_warnings: bool,
    show_bottom_panel: bool,
    bottom_panel_tab: BottomPanelTab,
    bottom_panel_height: f32,
    pub pending_close: Option<PathBuf>,
    editor_states: HashMap<PathBuf, EditorState>,
    pending_exit: bool,
    reveal_active_tab: bool,
    focus_cancel_on_modal_open: bool,
    error_message: Option<String>,
    allow_close: bool,
    pub lsp_manager: LspManager,
    #[cfg(test)]
    pub lsp: Option<LspClient>,
    pub lsp_pending: HashMap<u64, LspPendingKind>,
    pending_definitions: HashMap<u64, PendingDefinitionRequest>,
    ui_correlation_id: u64,
    pub lsp_warnings: HashMap<LanguageServerId, String>,
    /// In-flight format requests: maps file path → (correlation_id, request_started_at).
    /// Used to match `FormatResult` responses and to enforce the 2-second timeout.
    pending_format: HashMap<PathBuf, (u64, std::time::Instant)>,
    /// When `Some(path)`, a format was requested on save and the file should be written
    /// to disk once the `FormatResult` arrives (or the timeout fires).
    pending_format_on_save: Option<PathBuf>,
    pub git: Option<GitRepo>,
    pub show_git_panel: bool,
    pub git_commit_msg: String,
    pub git_stash_msg: String,
    pub show_branch_picker: bool,
    pub branch_query: String,
    pub show_blame: HashMap<PathBuf, bool>,
    pub blame_cache: HashMap<PathBuf, Vec<BlameLine>>,
    blame_receiver: Option<crossbeam_channel::Receiver<Option<BlameResult>>>,
    pending_blame_path: Option<PathBuf>,
    // ─── Extended git feature state ───────────────────────────────────────────
    /// Commit-log modal: open flag and cached entries.
    show_git_log: bool,
    git_log_cache: Vec<crate::git::CommitInfo>,
    /// Tag-manager modal: open flag, cached tags, and create-form inputs.
    show_tag_manager: bool,
    tag_new_name: String,
    tag_new_message: String,
    /// Conflict-resolver modal: open flag, conflicted paths, selected index,
    /// and the cached sides for the selected path.
    show_conflict_resolver: bool,
    conflict_paths: Vec<PathBuf>,
    conflict_selected: usize,
    conflict_sides: crate::git::ConflictSides,
    /// In-flight network operation progress and its receiver.
    network_receiver: Option<crossbeam_channel::Receiver<crate::git::NetworkProgress>>,
    network_progress: Option<crate::git::NetworkProgress>,
    completion: CompletionState,
    completion_anchor: CompletionPopupAnchor,
    /// True when the pointer rests on a diagnostic squiggle this frame.
    diagnostic_tooltip_active: bool,
    lsp_hover: LspHoverState,
    /// Find/Replace panel state (lives at app level so project search spans tabs).
    search_state: SearchState,
    /// Path of the active file at the time the search was last computed.
    /// Used to detect tab switches and invalidate the file-scope cache.
    search_last_active: Option<PathBuf>,
    settings: Settings,
    settings_store: SettingsStore,
    settings_draft: Option<Settings>,
    show_settings_window: bool,
    settings_feedback: Option<String>,
    config_warning: Option<String>,
    system_scheme: Option<ColorScheme>,
    active_palette: ThemePalette,
    launcher: LauncherState,
    // Terminal state is owned entirely by `term_sessions` (the rendered
    // SessionManager). There is no separate hidden terminal list.
    pub outline_panel: OutlinePanel,
    pub breadcrumbs: std::collections::HashMap<PaneId, crate::outline::BreadcrumbState>,
    pub minimap_states: std::collections::HashMap<PaneId, crate::editor::minimap::MinimapState>,
    pub plugin_system: PluginSystem,
    // ─── Foundation subsystems ────────────────────────────────────────────────
    pub workspace: Workspace,
    pub trust_store: Option<TrustStore>,
    pub recent_files: Vec<PathBuf>,
    recent_files_state: RecentFilesState,
    recent_workspaces_state: RecentWorkspacesState,
    pub pinned_tabs: std::collections::BTreeSet<PathBuf>,
    pub tab_groups: Vec<TabGroup>,
    pub tab_to_group: std::collections::HashMap<PathBuf, String>,
    pub bookmarks: std::collections::HashMap<PathBuf, std::collections::BTreeSet<usize>>,
    new_tab_group_state: NewTabGroupState,
    // ─── New feature UI state ─────────────────────────────────────────────────
    goto_line: GoToLineState,
    workspace_symbol: WorkspaceSymbolState,
    signature_help: SignatureHelpState,
    code_action: CodeActionState,
    /// Live-preview theme picker modal (Zed-style Ctrl+Alt+T).
    theme_picker: ThemePickerState,
    /// AI assistant right-dock panel (conversation with pluggable provider).
    pub assistant: crate::assistant::AssistantPanel,
    /// Auto-save bookkeeping: (revision at last edit, edit-settled-at instant).
    auto_save_marks: std::collections::HashMap<PathBuf, (u64, Option<std::time::Instant>)>,
    undo_history_panel_visible: bool,
    problems_panel: problems_panel::ProblemsPanel,
    hierarchy_panel: Option<HierarchyPanel>,
    // ─── UI/Visual Features (Features 1-7) ────────────────────────────────────
    /// Zen mode + distraction-free mode state.
    pub zen: ZenState,
    /// Content type override map: user can force a specific renderer for any file.
    pub content_type_override: std::collections::HashMap<PathBuf, PaneContent>,
    /// Per-pane image viewer states.
    pub image_viewer_states: std::collections::HashMap<PaneId, crate::image_viewer::ImageViewerState>,
    /// Per-pane markdown preview states.
    pub markdown_preview_states: std::collections::HashMap<PaneId, crate::markdown_preview::MarkdownPreviewState>,
    /// Per-pane diff viewer states.
    pub diff_viewer_states: std::collections::HashMap<PaneId, crate::diff_viewer::DiffViewerState>,
    /// Per-pane content type (overrides PathBuf-based detection).
    pub pane_content: std::collections::HashMap<PaneId, PaneContent>,
    /// Active color picker popup (only one at a time).
    pub color_picker: Option<crate::color_picker::ColorPickerState>,
    /// File watcher for live-reload of image/markdown panes.
    pub file_watcher: crate::file_watcher::FileWatcher,
    /// Texture registry for image and PDF GPU textures.
    pub texture_registry: crate::texture_registry::TextureRegistry,
    /// Ligature-aware text renderer for coding ligatures.
    pub ligature_renderer: crate::text::ligature::LigatureRenderer,
    /// Whether the markdown preview default is on for .md files.
    pub markdown_preview_default: bool,
    // ─── New IDE Systems ──────────────────────────────────────────────────────
    /// `.editorconfig` settings resolved for the active file.
    pub editorconfig: EditorConfigSettings,
    /// Task runner: manages background task execution.
    pub task_panel: TaskPanelState,
    /// New project wizard state.
    pub new_project: NewProjectState,
    /// Terminal multiplexer (replaces the legacy `terminals` vec for new features).
    pub terminal_mux: TerminalMux,
    /// Workspace trust prompt modal.
    pub trust_prompt: TrustPromptState,
    /// Whether the trust management popup is open (triggered from status bar).
    pub trust_management_open: bool,
    /// Last task output lines for the Output panel (mirrors task_panel output).
    pub task_output: Vec<String>,
    // ─── Terminal Feature 1–6 state ───────────────────────────────────────────
    /// Named session manager (Feature 1).
    pub term_sessions: crate::terminal::session::SessionManager,
    /// Split-terminal layout state (Feature 2).
    pub term_split: crate::terminal::split::SplitState,
    /// Terminal search state per terminal panel (Feature 4).
    pub term_search: crate::terminal::search::TerminalSearchState,
    /// Command history browser state (Feature 5).
    pub term_history: crate::terminal::history::HistoryBrowserState,
    /// Per-project env variable editor state (Feature 6).
    pub env_editor: crate::terminal::env_editor::EnvEditorState,
    /// Toast notifications for terminal features (path not found, etc.).
    pub term_toast: Option<(String, egui::Color32, std::time::Instant)>,
    // ─── Performance & Analysis (Features 1-4) ────────────────────────────────
    /// Interactive flamegraph profiler panel state.
    pub profiler_state: crate::profiler::ProfilerState,
    /// Startup timing data (populated after first render, None until then).
    pub startup_data: Option<crate::perf::startup::StartupData>,
    /// Startup breakdown floating window state.
    pub startup_breakdown: crate::perf::startup::StartupBreakdownState,
    /// Most-recent RSS sample (bytes). Updated at most every 2 seconds.
    pub memory_rss: u64,
    /// Last time RSS was polled.
    memory_last_poll: std::time::Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WindowControl {
    Minimize,
    Maximize { maximized: bool },
    Close,
}

impl WindowControl {
    fn accessible_name(self) -> &'static str {
        match self {
            Self::Minimize => "Minimize window",
            Self::Maximize { maximized: false } => "Maximize window",
            Self::Maximize { maximized: true } => "Restore window",
            Self::Close => "Close window",
        }
    }
}

impl BlueIdeApp {
    pub fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        let (settings, settings_store, config_warning) = match SettingsStore::discover() {
            Ok(store) => match store.load() {
                Ok(settings) => (settings, store, None),
                Err(e) => (
                    Settings::default(),
                    store,
                    Some(format!("Could not load settings: {}", e)),
                ),
            },
            Err(e) => {
                let temp_store = SettingsStore::at_path(
                    std::env::temp_dir().join("blue_ide_fallback_settings.toml"),
                );
                (
                    Settings::default(),
                    temp_store,
                    Some(format!("Could not discover settings path: {}", e)),
                )
            }
        };

        Self::with_settings(creation_context, settings, settings_store, config_warning)
    }

    pub fn with_settings(
        creation_context: &eframe::CreationContext<'_>,
        settings: Settings,
        settings_store: SettingsStore,
        config_warning: Option<String>,
    ) -> Self {
        let system_scheme = match creation_context.integration_info.system_theme {
            Some(eframe::Theme::Light) => Some(ColorScheme::Light),
            Some(eframe::Theme::Dark) => Some(ColorScheme::Dark),
            None => None,
        };
        let active_palette = Self::apply_appearance_settings(
            &creation_context.egui_ctx,
            &settings.appearance,
            system_scheme,
        );
        Self::empty_with_settings(
            settings,
            settings_store,
            config_warning,
            system_scheme,
            active_palette,
        )
    }

    fn apply_appearance_settings(
        ctx: &egui::Context,
        appearance: &crate::settings::AppearanceSettings,
        system_scheme: Option<ColorScheme>,
    ) -> ThemePalette {
        // Load ligature-capable font before setting up theme
        let mut font_defs = egui::FontDefinitions::default();
        font_loader::load_ligature_font(&mut font_defs);
        ctx.set_fonts(font_defs);
        
        if appearance.high_contrast {
            ctx.set_visuals(crate::theme::high_contrast_theme());
            // Return a default palette (not used when high contrast is active)
            crate::theme::blue_dark()
        } else {
            let theme = built_in_theme(appearance.theme, system_scheme);
            ctx.set_visuals(theme.visuals);
            ctx.set_pixels_per_point(appearance.ui_scale);
            // Drive zoom exclusively through `ui_scale`/`pixels_per_point`; disable
            // egui's built-in keyboard zoom so Ctrl +/-/0 aren't applied twice.
            ctx.options_mut(|options| options.zoom_with_keyboard = false);
            theme.palette
        }
    }

    #[cfg(test)]
    fn empty() -> Self {
        let settings = Settings::default();
        let active_palette = built_in_theme(settings.appearance.theme, None).palette;
        Self::empty_with_settings(
            settings,
            SettingsStore::at_path(PathBuf::new()),
            None,
            None,
            active_palette,
        )
    }

    fn empty_with_settings(
        settings: Settings,
        settings_store: SettingsStore,
        config_warning: Option<String>,
        system_scheme: Option<ColorScheme>,
        active_palette: ThemePalette,
    ) -> Self {
        let show_tree = settings.panels.show_file_tree;
        let show_problems = settings.panels.show_problems;
        let show_terminal = settings.panels.show_terminal;
        let show_bottom_panel = show_problems || show_terminal;
        let bottom_panel_tab = if show_terminal {
            BottomPanelTab::Terminal
        } else {
            BottomPanelTab::Problems
        };
        let pane_tree = PaneTree::single();
        let root_id = pane_tree.all_leaf_ids()[0];
        let app = Self {
            buffers: IndexMap::new(),
            pane_tree,
            focus: FocusState::new(root_id),
            focus_target: FocusTarget::Editor,
            pane_actions: Vec::new(),
            active: None,
            tree: FileTree::default(),
            show_tree,
            show_problems,
            show_problems_errors: true,
            show_problems_warnings: true,
            show_bottom_panel,
            bottom_panel_tab,
            bottom_panel_height: 280.0,
            pending_close: None,
            editor_states: HashMap::new(),
            pending_exit: false,
            reveal_active_tab: false,
            focus_cancel_on_modal_open: false,
            error_message: None,
            allow_close: false,
            lsp_manager: LspManager::new(),
            lsp_pending: HashMap::new(),
            pending_definitions: HashMap::new(),
            ui_correlation_id: 0,
            lsp_warnings: HashMap::new(),
            pending_format: HashMap::new(),
            pending_format_on_save: None,
            git: None,
            show_git_panel: false,
            git_commit_msg: String::new(),
            git_stash_msg: String::new(),
            show_branch_picker: false,
            branch_query: String::new(),
            show_blame: HashMap::new(),
            blame_cache: HashMap::new(),
            blame_receiver: None,
            pending_blame_path: None,
            show_git_log: false,
            git_log_cache: Vec::new(),
            show_tag_manager: false,
            tag_new_name: String::new(),
            tag_new_message: String::new(),
            show_conflict_resolver: false,
            conflict_paths: Vec::new(),
            conflict_selected: 0,
            conflict_sides: crate::git::ConflictSides::default(),
            network_receiver: None,
            network_progress: None,
            completion: CompletionState::default(),
            completion_anchor: CompletionPopupAnchor::none(),
            diagnostic_tooltip_active: false,
            lsp_hover: LspHoverState::default(),
            search_state: SearchState::new(),
            search_last_active: None,
            launcher: LauncherState::default(),
            outline_panel: OutlinePanel::new(&settings),
            breadcrumbs: std::collections::HashMap::new(),
            minimap_states: std::collections::HashMap::new(),
            settings,
            settings_store,
            settings_draft: None,
            show_settings_window: false,
            settings_feedback: None,
            config_warning,
            system_scheme,
            active_palette,
            #[cfg(test)]
            lsp: None,
            plugin_system: PluginSystem::new(),
            workspace: Workspace::default(),
            trust_store: None,
            recent_files: Vec::new(),
            recent_files_state: RecentFilesState::default(),
            recent_workspaces_state: RecentWorkspacesState::default(),
            pinned_tabs: std::collections::BTreeSet::new(),
            tab_groups: Vec::new(),
            tab_to_group: std::collections::HashMap::new(),
            bookmarks: std::collections::HashMap::new(),
            new_tab_group_state: NewTabGroupState::default(),
            goto_line: GoToLineState::default(),
            workspace_symbol: WorkspaceSymbolState::default(),
            signature_help: SignatureHelpState::default(),
            code_action: CodeActionState::default(),
            theme_picker: ThemePickerState::default(),
            assistant: crate::assistant::AssistantPanel::default(),
            auto_save_marks: std::collections::HashMap::new(),
            undo_history_panel_visible: false,
            problems_panel: problems_panel::ProblemsPanel::default(),
            hierarchy_panel: None,
            zen: ZenState::default(),
            content_type_override: std::collections::HashMap::new(),
            image_viewer_states: std::collections::HashMap::new(),
            markdown_preview_states: std::collections::HashMap::new(),
            diff_viewer_states: std::collections::HashMap::new(),
            pane_content: std::collections::HashMap::new(),
            color_picker: None,
            file_watcher: crate::file_watcher::FileWatcher::new(),
            texture_registry: crate::texture_registry::TextureRegistry::new(),
            ligature_renderer: crate::text::ligature::LigatureRenderer::new(),
            markdown_preview_default: true,
            editorconfig: EditorConfigSettings::default(),
            task_panel: TaskPanelState::default(),
            new_project: NewProjectState::default(),
            terminal_mux: TerminalMux::new(),
            trust_prompt: TrustPromptState::default(),
            trust_management_open: false,
            task_output: Vec::new(),
            term_sessions: crate::terminal::session::SessionManager::new(),
            term_split: crate::terminal::split::SplitState::new(),
            term_search: crate::terminal::search::TerminalSearchState::new(),
            term_history: crate::terminal::history::HistoryBrowserState::new(),
            env_editor: crate::terminal::env_editor::EnvEditorState::new(),
            term_toast: None,
            profiler_state: crate::profiler::ProfilerState::default(),
            startup_data: None,
            startup_breakdown: crate::perf::startup::StartupBreakdownState::default(),
            memory_rss: 0,
            memory_last_poll: std::time::Instant::now(),
        };
        #[cfg(not(test))]
        let mut app = app;
        #[cfg(not(test))]
        app.load_session();
        #[cfg(not(test))]
        if app.workspace.roots().is_empty() && !app.settings.recent_workspaces.is_empty() {
            app.recent_workspaces_state.open = true;
            app.recent_workspaces_state.query.clear();
            app.recent_workspaces_state.selected = 0;
        }
        app
    }

    pub fn open_file(&mut self, path: PathBuf) -> io::Result<()> {
        let newly_opened = !self.buffers.contains_key(&path);
        if newly_opened {
            let mut buffer = TextBuffer::default();
            buffer.load_from_file(&path)?;
            if let Some(collapsed) = self
                .settings
                .folding
                .collapsed_by_file
                .get(&path.to_string_lossy().to_string())
            {
                buffer.fold_state.collapsed = collapsed.iter().copied().collect();
            }
            self.buffers.insert(path.clone(), buffer);
            self.editor_states
                .insert(path.clone(), EditorState::default());
        }
        self.pane_tree
            .open_in_pane(self.focus.active_pane, path.clone());
        self.active = Some(path.clone());
        self.reveal_active_tab = true;

        // Detect and set pane content type
        let pane_id = self.focus.active_pane;
        let content = if let Some(override_content) = self.content_type_override.get(&path) {
            override_content.clone()
        } else {
            PaneContent::detect_from_path(&path, self.markdown_preview_default)
        };
        self.pane_content.insert(pane_id, content.clone());

        // Register for file watching if it's an image or markdown
        match &content {
            PaneContent::ImageViewer { .. } | PaneContent::MarkdownPreview { .. } => {
                self.file_watcher.watch(path.clone());
            }
            _ => {}
        }

        if newly_opened {
            self.send_did_open_for(&self.active.clone().expect("active path was just set"));
        }
        self.request_document_symbols(path.clone());

        // ── Large file mode check ─────────────────────────────────────────────
        if let Some(buffer) = self.buffers.get_mut(&path) {
            let s = &self.settings.editor;
            buffer.check_large_file_mode(
                s.large_file_warn_kb,
                s.large_file_mode_kb,
                s.large_file_line_warn,
                s.large_file_line_mode,
            );
        }

        // Dispatch plugin FileOpened event
        if newly_opened {
            let snap = self.plugin_context_snapshot();
            let actions = self
                .plugin_system
                .dispatch_event(PluginEvent::FileOpened(path.clone()), &snap);
            self.apply_plugin_actions(actions);
        }

        Ok(())
    }

    pub fn request_document_symbols(&mut self, path: PathBuf) {
        let Some(root_path) = self.workspace_root_for_path(&path) else {
            return;
        };
        if !is_lsp_path(&self.settings, Some(&root_path), &path) {
            return;
        }
        let is_running = {
            #[cfg(test)]
            {
                if let Some(ref client) = self.lsp {
                    client.is_running()
                } else {
                    let lang = LanguageId::from_path(&path);
                    lang.server_id()
                        .is_some_and(|server_id| self.lsp_manager.is_running(server_id))
                }
            }
            #[cfg(not(test))]
            {
                let lang = LanguageId::from_path(&path);
                lang.server_id()
                    .is_some_and(|server_id| self.lsp_manager.is_running(server_id))
            }
        };

        if !is_running {
            return;
        }

        let id = self.ui_correlation_id;
        self.ui_correlation_id += 1;

        self.lsp_pending.insert(id, LspPendingKind::DocumentSymbol);
        self.outline_panel.pending_request = Some(path.clone());

        let _sent = {
            #[cfg(test)]
            {
                if let Some(ref client) = self.lsp {
                    client.request_document_symbol(&path, id)
                } else {
                    let root = root_path.clone();
                    self.lsp_manager
                        .request_document_symbol(&path, id, &self.settings, &root)
                }
            }
            #[cfg(not(test))]
            {
                let root = root_path.clone();
                self.lsp_manager
                    .request_document_symbol(&path, id, &self.settings, &root)
            }
        };

        self.request_inlay_hints(path.clone());
        self.request_code_lenses(path.clone());
        self.request_semantic_tokens(path.clone());
    }

    pub fn request_inlay_hints(&mut self, path: PathBuf) {
        let Some(root_path) = self.workspace_root_for_path(&path) else {
            return;
        };
        if !is_lsp_path(&self.settings, Some(&root_path), &path) {
            return;
        }
        let is_running = {
            #[cfg(test)]
            {
                if let Some(ref client) = self.lsp {
                    client.is_running()
                } else {
                    let lang = LanguageId::from_path(&path);
                    lang.server_id()
                        .is_some_and(|server_id| self.lsp_manager.is_running(server_id))
                }
            }
            #[cfg(not(test))]
            {
                let lang = LanguageId::from_path(&path);
                lang.server_id()
                    .is_some_and(|server_id| self.lsp_manager.is_running(server_id))
            }
        };

        if !is_running {
            return;
        }

        let dirty = self.buffers.get(&path).is_some_and(|b| b.inlay_hints_dirty);
        if !dirty {
            return;
        }

        let already_pending = self.lsp_pending.values().any(|kind| {
            if let LspPendingKind::InlayHint(p) = kind {
                p == &path
            } else {
                false
            }
        });
        if already_pending {
            return;
        }

        let (start_line, end_line) = if let Some(buf) = self.buffers.get(&path) {
            (0, buf.len_lines() as u32)
        } else {
            (0, 100000)
        };

        let id = self.ui_correlation_id;
        self.ui_correlation_id += 1;

        self.lsp_pending.insert(id, LspPendingKind::InlayHint(path.clone()));

        let _sent = {
            #[cfg(test)]
            {
                if let Some(ref client) = self.lsp {
                    client.request_inlay_hints(&path, start_line, end_line, id)
                } else {
                    let root = root_path.clone();
                    self.lsp_manager
                        .request_inlay_hints(&path, start_line, end_line, id, &self.settings, &root)
                }
            }
            #[cfg(not(test))]
            {
                let root = root_path.clone();
                self.lsp_manager
                    .request_inlay_hints(&path, start_line, end_line, id, &self.settings, &root)
            }
        };
    }

    pub fn request_code_lenses(&mut self, path: PathBuf) {
        let Some(root_path) = self.workspace_root_for_path(&path) else {
            return;
        };
        if !is_lsp_path(&self.settings, Some(&root_path), &path) {
            return;
        }
        let is_running = {
            #[cfg(test)]
            {
                if let Some(ref client) = self.lsp {
                    client.is_running()
                } else {
                    let lang = LanguageId::from_path(&path);
                    lang.server_id()
                        .is_some_and(|server_id| self.lsp_manager.is_running(server_id))
                }
            }
            #[cfg(not(test))]
            {
                let lang = LanguageId::from_path(&path);
                lang.server_id()
                    .is_some_and(|server_id| self.lsp_manager.is_running(server_id))
            }
        };

        if !is_running {
            return;
        }

        let dirty = self.buffers.get(&path).is_some_and(|b| b.code_lens_dirty);
        if !dirty {
            return;
        }

        let already_pending = self.lsp_pending.values().any(|kind| {
            if let LspPendingKind::CodeLens(p) = kind {
                p == &path
            } else {
                false
            }
        });
        if already_pending {
            return;
        }

        let id = self.ui_correlation_id;
        self.ui_correlation_id += 1;

        self.lsp_pending.insert(id, LspPendingKind::CodeLens(path.clone()));

        let _sent = {
            #[cfg(test)]
            {
                if let Some(ref client) = self.lsp {
                    client.request_code_lens(&path, id)
                } else {
                    self.lsp_manager
                        .request_code_lens(&path, id, &self.settings, &root_path)
                }
            }
            #[cfg(not(test))]
            {
                self.lsp_manager
                    .request_code_lens(&path, id, &self.settings, &root_path)
            }
        };
    }

    pub fn request_semantic_tokens(&mut self, path: PathBuf) {
        let Some(root_path) = self.workspace_root_for_path(&path) else {
            return;
        };
        if !is_lsp_path(&self.settings, Some(&root_path), &path) {
            return;
        }
        let is_running = {
            #[cfg(test)]
            {
                if let Some(ref client) = self.lsp {
                    client.is_running()
                } else {
                    let lang = LanguageId::from_path(&path);
                    lang.server_id()
                        .is_some_and(|server_id| self.lsp_manager.is_running(server_id))
                }
            }
            #[cfg(not(test))]
            {
                let lang = LanguageId::from_path(&path);
                lang.server_id()
                    .is_some_and(|server_id| self.lsp_manager.is_running(server_id))
            }
        };

        if !is_running {
            return;
        }

        let dirty = self.buffers.get(&path).is_some_and(|b| b.semantic_tokens_dirty);
        if !dirty {
            return;
        }

        let already_pending = self.lsp_pending.values().any(|kind| {
            if let LspPendingKind::SemanticTokens(p) = kind {
                p == &path
            } else {
                false
            }
        });
        if already_pending {
            return;
        }

        let id = self.ui_correlation_id;
        self.ui_correlation_id += 1;

        self.lsp_pending.insert(id, LspPendingKind::SemanticTokens(path.clone()));

        let _sent = {
            #[cfg(test)]
            {
                if let Some(ref client) = self.lsp {
                    client.request_semantic_tokens_full(&path, id)
                } else {
                    self.lsp_manager
                        .request_semantic_tokens_full(&path, id, &self.settings, &root_path)
                }
            }
            #[cfg(not(test))]
            {
                self.lsp_manager
                    .request_semantic_tokens_full(&path, id, &self.settings, &root_path)
            }
        };
    }

    fn sync_fold_state_to_settings(&mut self) {
        for (path, buffer) in &self.buffers {
            let key = path.to_string_lossy().to_string();
            if buffer.fold_state.collapsed.is_empty() {
                self.settings.folding.collapsed_by_file.remove(&key);
                continue;
            }
            let mut lines: Vec<usize> = buffer.fold_state.collapsed.iter().copied().collect();
            lines.sort_unstable();
            self.settings.folding.collapsed_by_file.insert(key, lines);
        }
    }

    fn save_runtime_settings(&mut self) {
        self.sync_fold_state_to_settings();
        self.settings.panels.outline_panel_width = Some(self.outline_panel.width);
        if let Err(error) = self.settings_store.save(&self.settings) {
            eprintln!("Could not save runtime settings: {error}");
        }
    }

    fn session_path(&self) -> Option<PathBuf> {
        #[cfg(test)]
        {
            return Some(self.settings_store.path().to_path_buf());
        }
        #[cfg(not(test))]
        directories::ProjectDirs::from("com", "BlueIDE", "Blue IDE")
            .map(|proj| proj.config_dir().join("session.json"))
    }

    pub fn save_session(&self) {
        let Some(path) = self.session_path() else { return; };
        let mut scroll_positions = std::collections::HashMap::new();
        for (fpath, buffer) in &self.buffers {
            scroll_positions.insert(fpath.clone(), buffer.last_scroll_y);
        }
        
        let roots = self.workspace.roots().iter().map(|r| r.path.clone()).collect();
        let tabs = self.buffers.keys().cloned().collect();
        
        let state = AppSessionState {
            roots,
            tabs,
            active: self.active.clone(),
            pane_tree: Some(self.pane_tree.clone()),
            pinned_tabs: self.pinned_tabs.clone(),
            tab_groups: self.tab_groups.clone(),
            tab_to_group: self.tab_to_group.clone(),
            bookmarks: self.bookmarks.clone(),
            scroll_positions,
        };
        
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(&state) {
            let _ = std::fs::write(path, json);
        }
    }

    pub fn load_session(&mut self) {
        let Some(path) = self.session_path() else { return; };
        let Ok(json) = std::fs::read_to_string(path) else { return; };
        let Ok(state) = serde_json::from_str::<AppSessionState>(&json) else { return; };
        
        // Restore workspace roots
        for rpath in &state.roots {
            if let Err(error) = self.tree.load(rpath.clone()) {
                self.error_message = Some(format!("Could not open {}: {error}", rpath.display()));
            } else {
                self.show_tree = true;
                self.error_message = None;
                self.git = GitRepo::open(rpath);
                self.refresh_git_state();

                let restored_root = self
                    .workspace
                    .add_root(rpath.clone())
                    .ok()
                    .and_then(|id| self.workspace.root(id).cloned());
                self.touch_recent_workspace(rpath.clone());
                if self.trust_store.is_none() {
                    if let Some(config_dir) = directories::ProjectDirs::from("", "", "blue-ide") {
                        let trust_path = config_dir.config_dir().join("trust.json");
                        self.trust_store = crate::workspace::TrustStore::load(trust_path).ok();
                    }
                }

                // Do not auto-start LSP/plugins for a restored root until it is trusted.
                let trusted = self
                    .trust_store
                    .as_ref()
                    .and_then(|ts| {
                        restored_root.as_ref().map(|root| {
                            ts.permits(root, crate::workspace::ExecutableCapability::Plugin)
                        })
                    })
                    .unwrap_or(false);

                if !trusted {
                    self.trust_prompt.prompt(rpath.clone());
                } else {
                    self.lsp_manager.mark_root_trusted(rpath);
                    self.start_lsp(rpath.clone());
                    let plugin_dir = rpath.join(".blue").join("plugins");
                    self.plugin_system.reload_all();
                    self.plugin_system.load_all(&plugin_dir);
                    self.drain_plugin_actions();
                }
            }
        }
        
        // Reload tasks for all restored roots
        if !state.roots.is_empty() {
            self.reload_tasks();
        }
        
        // Restore pane tree if present
        if let Some(restored_tree) = state.pane_tree {
            self.pane_tree = restored_tree;
        }
        
        // Restore other metadata
        self.pinned_tabs = state.pinned_tabs;
        self.tab_groups = state.tab_groups;
        self.tab_to_group = state.tab_to_group;
        self.bookmarks = state.bookmarks;
        self.active = state.active;
        
        // Collect all files from pane tree to open
        let mut files_to_open = Vec::new();
        fn collect_all_files(tree: &PaneTree, files: &mut Vec<PathBuf>) {
            match tree {
                PaneTree::Leaf { tabs, .. } => {
                    for tab in tabs {
                        if !files.contains(tab) {
                            files.push(tab.clone());
                        }
                    }
                }
                PaneTree::HSplit { left, right, .. } => {
                    collect_all_files(left, files);
                    collect_all_files(right, files);
                }
                PaneTree::VSplit { top, bottom, .. } => {
                    collect_all_files(top, files);
                    collect_all_files(bottom, files);
                }
            }
        }
        collect_all_files(&self.pane_tree, &mut files_to_open);
        
        for fpath in files_to_open {
            let mut buffer = TextBuffer::default();
            if buffer.load_from_file(&fpath).is_ok() {
                if let Some(collapsed) = self
                    .settings
                    .folding
                    .collapsed_by_file
                    .get(&fpath.to_string_lossy().to_string())
                {
                    buffer.fold_state.collapsed = collapsed.iter().copied().collect();
                }
                
                self.buffers.insert(fpath.clone(), buffer);
                
                let mut estate = EditorState::default();
                if let Some(&scroll_y) = state.scroll_positions.get(&fpath) {
                    estate.desired_scroll_y = Some(scroll_y);
                    estate.request_scroll_to_cursor();
                }
                self.editor_states.insert(fpath.clone(), estate);
            }
        }
        
        // Restore the pane_content map and watch files
        fn init_pane_content(tree: &PaneTree, app: &mut BlueIdeApp) {
            match tree {
                PaneTree::Leaf { id, active, .. } => {
                    if let Some(active_path) = active {
                        let content = if let Some(override_content) = app.content_type_override.get(active_path) {
                            override_content.clone()
                        } else {
                            PaneContent::detect_from_path(active_path, app.markdown_preview_default)
                        };
                        app.pane_content.insert(*id, content.clone());
                        
                        match &content {
                            PaneContent::ImageViewer { .. } | PaneContent::MarkdownPreview { .. } => {
                                app.file_watcher.watch(active_path.clone());
                            }
                            _ => {}
                        }
                    }
                }
                PaneTree::HSplit { left, right, .. } => {
                    init_pane_content(left, app);
                    init_pane_content(right, app);
                }
                PaneTree::VSplit { top, bottom, .. } => {
                    init_pane_content(top, app);
                    init_pane_content(bottom, app);
                }
            }
        }
        init_pane_content(&self.pane_tree.clone(), self);
        
        // Set focus state
        if let Some(active_path) = &self.active {
            fn find_active_pane(tree: &PaneTree, path: &PathBuf) -> Option<PaneId> {
                match tree {
                    PaneTree::Leaf { id, active, .. } => {
                        if active.as_ref() == Some(path) {
                            Some(*id)
                        } else {
                            None
                        }
                    }
                    PaneTree::HSplit { left, right, .. } => {
                        find_active_pane(left, path).or_else(|| find_active_pane(right, path))
                    }
                    PaneTree::VSplit { top, bottom, .. } => {
                        find_active_pane(top, path).or_else(|| find_active_pane(bottom, path))
                    }
                }
            }
            if let Some(pane_id) = find_active_pane(&self.pane_tree, active_path) {
                self.focus = FocusState::new(pane_id);
            }
        }
        
        // LSP notification and symbols request
        let restored_paths: Vec<PathBuf> = self.buffers.keys().cloned().collect();
        for fpath in restored_paths {
            self.send_did_open_for(&fpath);
            self.request_document_symbols(fpath);
        }
    }

    fn touch_recent_file(&mut self, path: PathBuf) {
        if self.recent_files.first() == Some(&path) {
            return;
        }
        self.recent_files.retain(|p| p != &path);
        self.recent_files.insert(0, path);
        self.recent_files.truncate(50);
    }

    fn touch_recent_workspace(&mut self, path: PathBuf) {
        self.settings.recent_workspaces.retain(|p| p != &path);
        self.settings.recent_workspaces.insert(0, path);
        self.settings.recent_workspaces.truncate(12);
        if let Err(error) = self.settings_store.save(&self.settings) {
            eprintln!("Could not save recent workspaces: {error}");
        }
    }

    fn workspace_root_for_path(&self, path: &Path) -> Option<PathBuf> {
        self.workspace
            .owner_of(path)
            .map(|root| root.path.clone())
            .or_else(|| {
                self.tree
                    .root_path
                    .as_ref()
                    .filter(|root| root.as_path() != Path::new("workspace://"))
                    .cloned()
            })
    }

    fn primary_workspace_root(&self) -> Option<PathBuf> {
        self.workspace
            .roots()
            .first()
            .map(|root| root.path.clone())
            .or_else(|| {
                self.tree
                    .root_path
                    .as_ref()
                    .filter(|root| root.as_path() != Path::new("workspace://"))
                    .cloned()
            })
    }

    /// Default-deny trust check. When no trust store is loaded (e.g. it could
    /// not be persisted) or no workspace root exists, executable capabilities
    /// are NOT permitted.
    fn trust_allows(&self, capability: crate::workspace::ExecutableCapability) -> bool {
        self.trust_store
            .as_ref()
            .and_then(|ts| {
                self.workspace
                    .roots()
                    .first()
                    .map(|root| ts.permits(root, capability))
            })
            .unwrap_or(false)
    }

    fn open_workspace_folder(&mut self, path: PathBuf, add_to_workspace: bool) {
        if !add_to_workspace {
            self.workspace = Workspace::default();
        }

        let load_result = if add_to_workspace && !self.workspace.roots().is_empty() {
            self.tree.add_root(path.clone())
        } else {
            self.tree.load(path.clone())
        };

        if let Err(error) = load_result {
            self.error_message = Some(format!("Could not open {}: {error}", path.display()));
            return;
        }

        self.show_tree = true;
        self.error_message = None;
        self.git = GitRepo::open(&path);
        self.refresh_git_state();

        let opened_root = self
            .workspace
            .add_root(path.clone())
            .ok()
            .and_then(|id| self.workspace.root(id).cloned());
        self.touch_recent_workspace(path.clone());

        if self.trust_store.is_none() {
            if let Some(config_dir) = directories::ProjectDirs::from("", "", "blue-ide") {
                let trust_path = config_dir.config_dir().join("trust.json");
                self.trust_store = crate::workspace::TrustStore::load(trust_path).ok();
            }
        }

        // Fail closed: executable capabilities (plugins, LSP, tasks, terminals,
        // profiler) only start after the root is explicitly trusted.
        let trusted = self
            .trust_store
            .as_ref()
            .and_then(|ts| {
                opened_root
                    .as_ref()
                    .map(|root| ts.permits(root, crate::workspace::ExecutableCapability::Plugin))
            })
            .unwrap_or(false);

        let needs_prompt = self.trust_store.as_ref().is_some_and(|ts| {
            opened_root
                .as_ref()
                .is_some_and(|root| ts.state(root) == crate::workspace::TrustState::Untrusted)
        }) || self.trust_store.is_none();

        if needs_prompt {
            let is_known = self.trust_store.as_ref().is_some_and(|ts| {
                opened_root.as_ref().is_some_and(|root| {
                    !matches!(ts.state(root), crate::workspace::TrustState::Untrusted)
                })
            });
            if !is_known {
                self.trust_prompt.prompt(path.clone());
            }
        }

        self.reload_tasks();

        if trusted {
            self.lsp_manager.mark_root_trusted(&path);
            self.start_lsp(path.clone());
            let plugin_dir = path.join(".blue").join("plugins");
            self.plugin_system.reload_all();
            self.plugin_system.load_all(&plugin_dir);
            self.drain_plugin_actions();
        }
    }


    pub fn close_file(&mut self, path: &Path) {
        let Some(index) = self.buffers.get_index_of(path) else {
            return;
        };
        if let Some(buffer) = self.buffers.get(path) {
            let key = path.to_string_lossy().to_string();
            if buffer.fold_state.collapsed.is_empty() {
                self.settings.folding.collapsed_by_file.remove(&key);
            } else {
                let mut lines: Vec<usize> = buffer.fold_state.collapsed.iter().copied().collect();
                lines.sort_unstable();
                self.settings.folding.collapsed_by_file.insert(key, lines);
            }
        }
        if self.completion.is_for_path(path) {
            self.dismiss_completion();
        }
        self.dismiss_lsp_hover();
        self.pending_definitions
            .retain(|_, req| req.source_path != path && req.active_tab != path);
        if self
            .workspace_root_for_path(path)
            .is_some_and(|root| is_lsp_path(&self.settings, Some(&root), path))
        {
            self.lsp_manager.did_close(path);
        }
        self.outline_panel.nodes.remove(path);
        if self.outline_panel.pending_request.as_ref() == Some(&path.to_path_buf()) {
            self.outline_panel.pending_request = None;
        }
        let was_active = self.active.as_deref() == Some(path);
        self.pane_tree.remove_tab_from_all_panes(path);
        self.buffers.shift_remove(path);
        self.editor_states.remove(path);

        // Clean up UI/visual feature state for this file
        let pane_ids: Vec<PaneId> = self.pane_content.keys().cloned().collect();
        for pane_id in pane_ids {
            if let Some(content) = self.pane_content.get(&pane_id) {
                if content.path() == Some(&path.to_path_buf()) {
                    self.pane_content.remove(&pane_id);
                    self.image_viewer_states.remove(&pane_id);
                    self.markdown_preview_states.remove(&pane_id);
                    self.diff_viewer_states.remove(&pane_id);
                }
            }
        }
        // Evict image textures for this file
        let prefix = format!("image:{}", path.to_string_lossy());
        self.texture_registry.evict_prefix(&prefix);
        // Unregister from file watcher
        self.file_watcher.unwatch(&path.to_path_buf());

        if was_active {
            self.active = self
                .buffers
                .get_index(index.saturating_sub(1))
                .map(|(path, _)| path.clone());
            if let Some(path) = self.active.clone() {
                self.pane_tree.open_in_pane(self.focus.active_pane, path);
            }
            self.reveal_active_tab = self.active.is_some();
        }
    }

    fn request_close_file(&mut self, path: &Path) {
        if self.pinned_tabs.contains(path) {
            return;
        }
        if self.buffers.get(path).is_some_and(TextBuffer::is_modified) {
            self.on_modal_opened();
            self.pending_close = Some(path.to_path_buf());
            self.focus_cancel_on_modal_open = true;
        } else {
            self.close_file(path);
        }
    }

    /// Force-close a file (`:q!`) without prompting about unsaved changes.
    fn request_close_file_force(&mut self, path: &Path) {
        if self.pinned_tabs.contains(path) {
            return;
        }
        if self.pending_close.as_deref() == Some(path) {
            self.pending_close = None;
            self.focus_cancel_on_modal_open = false;
        }
        self.close_file(path);
    }

    fn cycle_tab(&mut self, direction: isize) {
        let len = self.buffers.len();
        if len == 0 {
            self.active = None;
            return;
        }
        let current = self
            .active
            .as_ref()
            .and_then(|path| self.buffers.get_index_of(path))
            .unwrap_or(0);
        let next = (current as isize + direction).rem_euclid(len as isize) as usize;
        self.active = self.buffers.get_index(next).map(|(path, _)| path.clone());
        if let Some(path) = self.active.clone() {
            self.pane_tree.open_in_pane(self.focus.active_pane, path);
        }
        self.reveal_active_tab = true;
    }

    pub fn active_file(&self) -> Option<&PathBuf> {
        self.pane_tree.active_in_pane(self.focus.active_pane)
    }

    fn sync_active_from_focused_pane(&mut self) {
        self.active = self.active_file().cloned().or_else(|| {
            let fallback = self.active.clone()?;
            if self.buffers.contains_key(&fallback) {
                self.pane_tree
                    .open_in_pane(self.focus.active_pane, fallback.clone());
                Some(fallback)
            } else {
                None
            }
        });
    }

    fn apply_pane_actions(&mut self) {
        let actions = std::mem::take(&mut self.pane_actions);
        for action in actions {
            match action {
                PaneAction::CloseTab { pane, path } => {
                    self.pane_tree.close_tab_in_pane(pane, &path);
                    if !self.pane_tree.any_pane_has(&path) {
                        self.close_file(&path);
                    }
                }
                PaneAction::SplitH { pane } => {
                    self.pane_tree.split_h(pane);
                }
                PaneAction::SplitV { pane } => {
                    self.pane_tree.split_v(pane);
                }
                PaneAction::ClosePane { pane } => {
                    if self.pane_tree.all_leaf_ids().len() > 1 {
                        if let CloseResult::Replace(replacement) = self.pane_tree.close_pane(pane) {
                            self.pane_tree = *replacement;
                        }
                        self.breadcrumbs.remove(&pane);
                        if self.focus.active_pane == pane {
                            if let Some(id) = self.pane_tree.all_leaf_ids().first().copied() {
                                self.focus.active_pane = id;
                            }
                        }
                    }
                }
                PaneAction::FocusPane { pane } => {
                    self.focus.active_pane = pane;
                }
                PaneAction::OpenInPane { pane, path } => {
                    if let Err(error) = self.open_file(path.clone()) {
                        self.error_message =
                            Some(format!("Could not open {}: {error}", path.display()));
                    }
                    self.pane_tree.open_in_pane(pane, path);
                    self.focus.active_pane = pane;
                }
            }
        }
        self.sync_active_from_focused_pane();
    }

    fn on_active_tab_changed(&mut self, active: &Option<PathBuf>) {
        if active != &self.search_last_active {
            self.search_last_active = active.clone();
            self.search_state.invalidate_file_cache();
            self.dismiss_completion();
            self.dismiss_lsp_hover();
            // Refresh .editorconfig when switching tabs
            self.refresh_editorconfig();
        }
    }

    fn on_modal_opened(&mut self) {
        self.dismiss_completion();
        self.dismiss_lsp_hover();
    }

    fn has_modal(&self) -> bool {
        self.pending_close.is_some()
            || self.pending_exit
            || self.show_settings_window
            || self.launcher.is_open()
            || self.search_state.pending_replace_confirm.is_some()
            || self.goto_line.open
            || self.workspace_symbol.open
            || self.code_action.open
            || self.show_git_log
            || self.show_tag_manager
            || self.show_conflict_resolver
            || self.recent_files_state.open
            || self.recent_workspaces_state.open
            || self.new_tab_group_state.open
            || self.new_project.open
            || self.trust_prompt.open
            || self.theme_picker.open
    }

    fn open_settings(&mut self) {
        self.on_modal_opened();
        self.settings_draft = Some(self.settings.clone());
        self.settings_feedback = None;
        self.show_settings_window = true;
    }

    fn preview_settings(&mut self, context: &egui::Context) {
        if let Some(draft) = &self.settings_draft {
            self.active_palette =
                Self::apply_appearance_settings(context, &draft.appearance, self.system_scheme);
        }
    }

    fn cancel_settings(&mut self, context: &egui::Context) {
        self.active_palette =
            Self::apply_appearance_settings(context, &self.settings.appearance, self.system_scheme);
        self.settings_draft = None;
        self.settings_feedback = None;
        self.show_settings_window = false;
    }

    fn persist_settings_draft(&mut self, context: &egui::Context, close: bool) -> bool {
        let Some(mut draft) = self.settings_draft.clone() else {
            return false;
        };
        self.sync_fold_state_to_settings();
        draft.folding = self.settings.folding.clone();
        if let Err(error) = draft.validate() {
            self.settings_feedback = Some(format!("Could not apply settings: {error}"));
            return false;
        }
        if let Err(error) = self.settings_store.save(&draft) {
            self.settings_feedback = Some(format!("Could not save settings: {error}"));
            return false;
        }

        let old_settings = self.settings.clone();
        self.settings = draft;
        self.active_palette =
            Self::apply_appearance_settings(context, &self.settings.appearance, self.system_scheme);
        self.show_tree = self.settings.panels.show_file_tree;
        self.apply_bottom_panel_settings_from_prefs();
        self.settings_feedback = Some("Settings saved".to_owned());

        let workspace_roots: Vec<_> = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.path.clone())
            .collect();
        for root in &workspace_roots {
            self.lsp_manager
                .handle_settings_change(&old_settings.lsp, &self.settings.lsp, root);
        }

        if close {
            self.settings_draft = None;
            self.show_settings_window = false;
        }
        true
    }

    fn reload_settings(&mut self) {
        match self.settings_store.load() {
            Ok(new_settings) => {
                let old_settings = self.settings.clone();
                self.settings = new_settings;
                self.show_tree = self.settings.panels.show_file_tree;
                self.apply_bottom_panel_settings_from_prefs();
                self.settings_feedback = Some("Settings reloaded".to_owned());

                let workspace_roots: Vec<_> = self
                    .workspace
                    .roots()
                    .iter()
                    .map(|root| root.path.clone())
                    .collect();
                for root in &workspace_roots {
                    self.lsp_manager.handle_settings_change(
                        &old_settings.lsp,
                        &self.settings.lsp,
                        root,
                    );
                }
            }
            Err(error) => {
                self.settings_feedback = Some(format!("Could not reload settings: {error}"));
            }
        }
    }

    fn show_settings(&mut self, context: &egui::Context) {
        if !self.show_settings_window {
            return;
        }

        let mut window_open = true;
        let mut preview_changed = false;
        let mut apply = false;
        let mut save = false;
        let mut cancel = false;
        let mut restore = false;
        egui::Window::new("Settings")
            .id(egui::Id::new("settings_window"))
            .open(&mut window_open)
            .collapsible(false)
            .resizable(true)
            .default_width(500.0)
            .show(context, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.heading("Appearance");
                    ui.add_space(6.0);
                    if let Some(draft) = self.settings_draft.as_mut() {
                        ui.horizontal(|ui| {
                            ui.label("Theme:");
                            egui::ComboBox::from_id_source("appearance_theme")
                                .selected_text(draft.appearance.theme.display_name())
                                .show_ui(ui, |ui| {
                                    for theme in crate::settings::Theme::all() {
                                        if ui
                                            .selectable_value(
                                                &mut draft.appearance.theme,
                                                *theme,
                                                theme.display_name(),
                                            )
                                            .changed()
                                        {
                                            preview_changed = true;
                                        }
                                    }
                                });
                        });
                        ui.horizontal(|ui| {
                            ui.label("UI scale:");
                            if ui
                                .add(
                                    egui::Slider::new(&mut draft.appearance.ui_scale, 0.5..=3.0)
                                        .step_by(0.05),
                                )
                                .changed()
                            {
                                preview_changed = true;
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Editor font size:");
                            ui.add(
                                egui::Slider::new(
                                    &mut draft.appearance.editor_font_size,
                                    8.0..=48.0,
                                )
                                .step_by(1.0),
                            );
                        });

                        ui.add_space(12.0);
                        ui.heading("Editor");
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label("Tab width:");
                            ui.add(egui::Slider::new(&mut draft.editor.tab_width, 1..=16));
                        });
                        ui.checkbox(&mut draft.editor.insert_spaces, "Insert spaces");
                        ui.checkbox(&mut draft.editor.vim_mode, "Vim mode (modal editing)");
                        ui.horizontal(|ui| {
                            ui.label("Auto save:");
                            let current = draft.editor.auto_save;
                            egui::ComboBox::from_id_source("auto_save_mode")
                                .selected_text(current.display_name())
                                .show_ui(ui, |ui| {
                                    for mode in crate::settings::AutoSaveMode::all() {
                                        ui.selectable_value(
                                            &mut draft.editor.auto_save,
                                            *mode,
                                            mode.display_name(),
                                        );
                                    }
                                });
                        });
                        if draft.editor.auto_save == crate::settings::AutoSaveMode::AfterDelay {
                            ui.horizontal(|ui| {
                                ui.label("Delay:");
                                ui.add(
                                    egui::Slider::new(&mut draft.editor.auto_save_delay_ms, 100..=5000)
                                        .suffix(" ms"),
                                );
                            });
                        }
                        ui.checkbox(
                            &mut draft.editor.inline_diagnostics,
                            "Show diagnostics inline at end of line",
                        );

                        ui.add_space(12.0);
                        ui.heading("AI Assistant");
                        ui.add_space(6.0);
                        ui.label("Provider shell command; leave empty to disable.");
                        ui.label(
                            RichText::new(
                                "Placeholders: {prompt} {file} {selection} {language} — \
                                 e.g. `ollama run llama3.1`. The prompt is piped to stdin \
                                 when {prompt} is absent.",
                            )
                            .size(11.0),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut draft.assistant.command)
                                .id(egui::Id::new("assistant_command_setting"))
                                .hint_text("e.g. ollama run codellama")
                                .desired_width(420.0),
                        );

                        ui.add_space(12.0);
                        ui.heading("Panels");
                        ui.add_space(6.0);
                        ui.checkbox(
                            &mut draft.panels.show_file_tree,
                            "Show file tree on startup",
                        );
                        ui.checkbox(
                            &mut draft.panels.show_problems,
                            "Show problems panel on startup",
                        );
                        ui.checkbox(&mut draft.panels.show_terminal, "Show terminal on startup");

                        ui.add_space(12.0);
                        ui.heading("Language Servers");
                        ui.add_space(6.0);

                        // Rust LSP
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut draft.lsp.rust.enabled, "Enable Rust LSP");
                        });
                        ui.horizontal(|ui| {
                            ui.label("  Command:");
                            ui.text_edit_singleline(&mut draft.lsp.rust.command);
                        });
                        ui.label("  Arguments (one per line):");
                        ui.horizontal(|ui| {
                            ui.label("  ");
                            let mut args_text = draft.lsp.rust.args.join("\n");
                            if ui.text_edit_multiline(&mut args_text).changed() {
                                draft.lsp.rust.args = args_text.lines().map(String::from).collect();
                            }
                        });

                        ui.add_space(8.0);

                        // Python LSP
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut draft.lsp.python.enabled, "Enable Python LSP");
                        });
                        ui.horizontal(|ui| {
                            ui.label("  Command:");
                            ui.text_edit_singleline(&mut draft.lsp.python.command);
                        });
                        ui.label("  Arguments (one per line):");
                        ui.horizontal(|ui| {
                            ui.label("  ");
                            let mut args_text = draft.lsp.python.args.join("\n");
                            if ui.text_edit_multiline(&mut args_text).changed() {
                                draft.lsp.python.args =
                                    args_text.lines().map(String::from).collect();
                            }
                        });

                        ui.add_space(8.0);

                        // TypeScript LSP
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut draft.lsp.typescript.enabled, "Enable TypeScript LSP");
                        });
                        ui.horizontal(|ui| {
                            ui.label("  Command:");
                            ui.text_edit_singleline(&mut draft.lsp.typescript.command);
                        });
                        ui.label("  Arguments (one per line):");
                        ui.horizontal(|ui| {
                            ui.label("  ");
                            let mut args_text = draft.lsp.typescript.args.join("\n");
                            if ui.text_edit_multiline(&mut args_text).changed() {
                                draft.lsp.typescript.args =
                                    args_text.lines().map(String::from).collect();
                            }
                        });

                        ui.add_space(8.0);
                        ui.label(format!(
                            "Settings file: {}",
                            self.settings_store.path().display()
                        ));
                    }

                    ui.add_space(8.0);
                    if let Some(feedback) = &self.settings_feedback {
                        let color = if feedback == "Settings saved" {
                            self.active_palette.semantic.success
                        } else {
                            self.active_palette.semantic.error
                        };
                        ui.colored_label(color, feedback);
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            save = true;
                        }
                        if ui.button("Apply").clicked() {
                            apply = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                        if ui.button("Restore Defaults").clicked() {
                            restore = true;
                        }
                    });
                });
            });

        if restore {
            self.settings_draft = Some(crate::settings::Settings::default());
            self.settings_feedback = None;
            preview_changed = true;
        }
        if preview_changed {
            self.settings_feedback = None;
            self.preview_settings(context);
        }
        if context.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) {
            cancel = true;
        }
        if cancel || !window_open {
            self.cancel_settings(context);
        } else if save {
            self.persist_settings_draft(context, true);
        } else if apply {
            self.persist_settings_draft(context, false);
        }
    }

    fn open_file_dialog(&mut self) {
        let Some(path) = FileDialog::new()
            .set_title("Open file")
            .add_filter("Rust source", &["rs"])
            .add_filter("Text files", &["txt", "toml", "md"])
            .pick_file()
        else {
            return;
        };

        if let Err(error) = self.open_file(path.clone()) {
            self.error_message = Some(format!("Could not open {}: {error}", path.display()));
        } else {
            self.error_message = None;
        }
    }

    fn open_folder_dialog(&mut self) {
        self.open_folder_dialog_with_mode(false);
    }

    fn open_folder_dialog_with_mode(&mut self, add_to_workspace: bool) {
        let Some(path) = FileDialog::new().set_title("Open folder").pick_folder() else {
            return;
        };
        self.open_workspace_folder(path, add_to_workspace);
    }

    fn ensure_terminal(&mut self) {
        if self.term_sessions.is_empty() {
            let cwd = self.primary_workspace_root();
            let env_vars = self.env_editor.enabled_vars();
            self.term_sessions.ensure_session(cwd, &env_vars);
        }
    }

    fn close_bottom_panel(&mut self) {
        self.show_bottom_panel = false;
        self.show_problems = false;
    }

    fn toggle_terminal_panel(&mut self) {
        if self.show_bottom_panel && self.bottom_panel_tab == BottomPanelTab::Terminal {
            self.close_bottom_panel();
        } else {
            self.ensure_terminal();
            self.bottom_panel_tab = BottomPanelTab::Terminal;
            self.show_bottom_panel = true;
            self.show_problems = false;
        }
    }

    fn toggle_problems_panel(&mut self) {
        if self.show_bottom_panel && self.bottom_panel_tab == BottomPanelTab::Problems {
            self.close_bottom_panel();
        } else {
            self.bottom_panel_tab = BottomPanelTab::Problems;
            self.show_bottom_panel = true;
            self.show_problems = true;
        }
    }

    fn apply_bottom_panel_settings_from_prefs(&mut self) {
        if self.settings.panels.show_terminal {
            self.ensure_terminal();
            self.show_bottom_panel = true;
            self.bottom_panel_tab = BottomPanelTab::Terminal;
            self.show_problems = false;
        } else if self.settings.panels.show_problems {
            self.show_bottom_panel = true;
            self.bottom_panel_tab = BottomPanelTab::Problems;
            self.show_problems = true;
        } else {
            self.close_bottom_panel();
        }
    }

    fn bottom_panel_diagnostic_counts(&self) -> problems_panel::DiagnosticCounts {
        #[cfg(test)]
        let diagnostics = if let Some(ref client) = self.lsp {
            client.diagnostics().clone()
        } else {
            self.lsp_manager.all_diagnostics()
        };
        #[cfg(not(test))]
        let diagnostics = self.lsp_manager.all_diagnostics();

        let rows = problems_panel::flatten_diagnostics(&diagnostics);
        problems_panel::count_diagnostics(&rows)
    }

    fn start_lsp(&mut self, root_path: PathBuf) {
        #[cfg(test)]
        if let Some(ref mut client) = self.lsp {
            client.shutdown_and_join();
            self.lsp_warnings.clear();
            self.lsp_pending.clear();
            self.dismiss_completion();
            self.dismiss_lsp_hover();
            for (_path, buffer) in &mut self.buffers {
                buffer.mark_lsp_synced();
            }
            return;
        }

        self.lsp_manager.shutdown_all();
        self.lsp_warnings.clear();
        self.lsp_pending.clear();
        self.dismiss_completion();
        self.dismiss_lsp_hover();

        let settings = &self.settings;
        let buffer_paths: Vec<PathBuf> = self.buffers.keys().cloned().collect();
        for path in buffer_paths {
            let workspace_root = self
                .workspace_root_for_path(&path)
                .unwrap_or_else(|| root_path.clone());
            let Some(buffer_snapshot) = self.buffers.get(&path) else {
                continue;
            };
            let text = buffer_snapshot.text();
            let version = buffer_snapshot.lsp_version;
            let lsp_manager = &mut self.lsp_manager;
            if is_lsp_path(settings, Some(&workspace_root), &path)
                && lsp_manager.did_open(&path, &text, version, settings, &workspace_root)
            {
                if let Some(buffer) = self.buffers.get_mut(&path) {
                    buffer.mark_lsp_synced();
                }
            }
        }
    }

    fn send_did_open_for(&mut self, path: &Path) {
        let Some(root_path) = self.workspace_root_for_path(path) else {
            return;
        };
        if !is_lsp_path(&self.settings, Some(&root_path), path) {
            return;
        }
        let Some(buffer) = self.buffers.get_mut(path) else {
            return;
        };
        let text = buffer.text();
        let version = buffer.lsp_version;

        #[cfg(test)]
        if let Some(ref mut client) = self.lsp {
            let lsp_lang_id = LanguageId::from_path(path)
                .lsp_language_id()
                .unwrap_or("plain");
            if client.did_open(path, lsp_lang_id, &text, version) {
                buffer.mark_lsp_synced();
            }
            return;
        }

        if self
            .lsp_manager
            .did_open(path, &text, version, &self.settings, &root_path)
        {
            buffer.mark_lsp_synced();
        }
    }

    /// Drain typed LSP responses and route them to app-layer handlers.
    ///
    /// Hover results are accepted only when `lsp_pending` recorded a matching
    /// `LspPendingKind::Hover` correlation id for the outbound request.
    fn poll_lsp(&mut self) {
        #[cfg(test)]
        if let Some(ref mut client) = self.lsp {
            let responses = LspClient::poll(client);
            for response in responses {
                self.handle_lsp_response(LanguageServerId::Rust, response);
            }
            return;
        }

        let responses = self.lsp_manager.poll();
        for (server_id, response) in responses {
            self.handle_lsp_response(server_id, response);
        }
    }

    fn handle_lsp_response(&mut self, server_id: LanguageServerId, response: LspResponse) {
        match response {
            LspResponse::Initialized { token_types: _ } => {
                self.lsp_warnings.remove(&server_id);
                if let Some(path) = self.active.clone() {
                    self.request_document_symbols(path);
                }
            }
            LspResponse::ServerUnavailable { message } => {
                self.lsp_warnings.insert(server_id, message);
                self.lsp_manager.stop_server(server_id);
                self.pending_definitions.clear();
            }
            LspResponse::CompletionList { id, items } => {
                self.lsp_pending.remove(&id);
                self.receive_completion(id, items);
            }
            LspResponse::HoverResult { id, content } => {
                if self.lsp_pending.remove(&id) == Some(LspPendingKind::Hover) {
                    self.receive_hover(id, content);
                }
            }
            LspResponse::GotoResult {
                id,
                path,
                line,
                col,
            } => {
                self.lsp_pending.remove(&id);
                self.receive_goto_definition(id, path, line, col);
            }
            LspResponse::GotoNone { id } => {
                self.lsp_pending.remove(&id);
                self.receive_goto_none(id);
            }
            LspResponse::SymbolList { id, path, symbols } => {
                self.lsp_pending.remove(&id);
                self.outline_panel.nodes.insert(path.clone(), symbols);
                if self.outline_panel.pending_request.as_ref() == Some(&path) {
                    self.outline_panel.pending_request = None;
                }
                if self.active.as_ref() == Some(&path) {
                    self.outline_panel.last_cursor_line = None;
                }
                for pane_id in self.pane_tree.all_leaf_ids() {
                    if self.pane_tree.active_in_pane(pane_id) == Some(&path) {
                        if let Some(state) = self.breadcrumbs.get_mut(&pane_id) {
                            state.last_cursor_line = usize::MAX;
                        }
                    }
                }
            }
            LspResponse::Error { id, message } => {
                let kind = self.lsp_pending.remove(&id);
                self.handle_lsp_request_error(id, kind, message);
            }
            LspResponse::Diagnostics { .. } => {}
            LspResponse::ReferencesResult { .. }
            | LspResponse::PrepareRenameResult { .. }
            | LspResponse::RenameResult { .. } => {}
            LspResponse::FormatResult { id, edits } => {
                self.lsp_pending.remove(&id);
                self.receive_format_result(id, edits);
            }
            LspResponse::SignatureHelpResult { id, active } => {
                if self.lsp_pending.remove(&id) == Some(LspPendingKind::SignatureHelp) {
                    if self.signature_help.pending_id == Some(id) {
                        self.signature_help.pending_id = None;
                        self.signature_help.active = active;
                    }
                }
            }
            LspResponse::WorkspaceSymbolResult { id, symbols } => {
                if self.lsp_pending.remove(&id) == Some(LspPendingKind::WorkspaceSymbol) {
                    if self.workspace_symbol.pending_id == Some(id) {
                        self.workspace_symbol.pending_id = None;
                        self.workspace_symbol.results = symbols;
                        self.workspace_symbol.selected = 0;
                    }
                }
            }
            LspResponse::CodeActionResult { id, actions } => {
                if self.lsp_pending.remove(&id) == Some(LspPendingKind::CodeAction) {
                    if self.code_action.pending_id == Some(id) {
                        self.code_action.pending_id = None;
                        self.code_action.actions = actions;
                        self.code_action.selected = 0;
                        if !self.code_action.actions.is_empty() {
                            self.code_action.open = true;
                        }
                    }
                }
            }
            LspResponse::CodeLensResult { id, lenses } => {
                if let Some(LspPendingKind::CodeLens(path)) = self.lsp_pending.remove(&id) {
                    self.receive_code_lenses(path, lenses);
                }
            }
            LspResponse::SemanticTokensResult { id, tokens } => {
                if let Some(LspPendingKind::SemanticTokens(path)) = self.lsp_pending.remove(&id) {
                    self.receive_semantic_tokens(path, tokens);
                }
            }
            LspResponse::CallHierarchyPrepareResult { id, items } => {
                if let Some(pending) = self.lsp_pending.remove(&id) {
                    if let LspPendingKind::CallHierarchy { path, .. } = pending {
                        self.receive_call_hierarchy_prepare(path, items);
                    }
                }
            }
            LspResponse::IncomingCallsResult { id, calls } => {
                if let Some(pending) = self.lsp_pending.remove(&id) {
                    if let LspPendingKind::CallHierarchy { parent_uri, parent_range, .. } = pending {
                        let children = calls
                            .into_iter()
                            .map(|call| HierarchyNode {
                                item: HierarchyItem::Call(call.from),
                                children: None,
                                expanded: false,
                            })
                            .collect();
                        if let Some(ref mut panel) = self.hierarchy_panel {
                            if let Some(ref r) = parent_range {
                                update_call_node(&mut panel.root, &parent_uri, r, children);
                            }
                        }
                    }
                }
            }
            LspResponse::OutgoingCallsResult { id, calls } => {
                if let Some(pending) = self.lsp_pending.remove(&id) {
                    if let LspPendingKind::CallHierarchy { parent_uri, parent_range, .. } = pending {
                        let children = calls
                            .into_iter()
                            .map(|call| HierarchyNode {
                                item: HierarchyItem::Call(call.to),
                                children: None,
                                expanded: false,
                            })
                            .collect();
                        if let Some(ref mut panel) = self.hierarchy_panel {
                            if let Some(ref r) = parent_range {
                                update_call_node(&mut panel.root, &parent_uri, r, children);
                            }
                        }
                    }
                }
            }
            LspResponse::TypeHierarchyPrepareResult { id, items } => {
                if let Some(pending) = self.lsp_pending.remove(&id) {
                    if let LspPendingKind::TypeHierarchy { path, .. } = pending {
                        self.receive_type_hierarchy_prepare(path, items);
                    }
                }
            }
            LspResponse::SupertypesResult { id, items } => {
                if let Some(pending) = self.lsp_pending.remove(&id) {
                    if let LspPendingKind::TypeHierarchy { parent_uri, parent_range, .. } = pending {
                        let children = items
                            .into_iter()
                            .map(|item| HierarchyNode {
                                item: HierarchyItem::Type(item),
                                children: None,
                                expanded: false,
                            })
                            .collect();
                        if let Some(ref mut panel) = self.hierarchy_panel {
                            if let Some(ref r) = parent_range {
                                update_type_node(&mut panel.root, &parent_uri, r, children);
                            }
                        }
                    }
                }
            }
            LspResponse::SubtypesResult { id, items } => {
                if let Some(pending) = self.lsp_pending.remove(&id) {
                    if let LspPendingKind::TypeHierarchy { parent_uri, parent_range, .. } = pending {
                        let children = items
                            .into_iter()
                            .map(|item| HierarchyNode {
                                item: HierarchyItem::Type(item),
                                children: None,
                                expanded: false,
                            })
                            .collect();
                        if let Some(ref mut panel) = self.hierarchy_panel {
                            if let Some(ref r) = parent_range {
                                update_type_node(&mut panel.root, &parent_uri, r, children);
                            }
                        }
                    }
                }
            }
            LspResponse::InlayHintResult { id, hints } => {
                if let Some(LspPendingKind::InlayHint(path)) = self.lsp_pending.remove(&id) {
                    if let Some(buffer) = self.buffers.get_mut(&path) {
                        buffer.inlay_hints = hints;
                        buffer.inlay_hints_dirty = false;
                        buffer.invalidate_layout();
                    }
                }
            }
            LspResponse::Progress {
                token: _,
                kind: _,
                title: _,
                message: _,
                percentage: _,
            } => {}
            LspResponse::ServerMessage { level, message } => {
                use crate::plugins::NotifyLevel;
                self.plugin_system
                    .notifications
                    .push(crate::plugins::PluginNotification {
                        message: message.clone(),
                        level: match level {
                            MessageLevel::Error => NotifyLevel::Error,
                            MessageLevel::Warning => NotifyLevel::Warning,
                            _ => NotifyLevel::Info,
                        },
                        plugin_name: "LSP".to_string(),
                        created_at: std::time::Instant::now(),
                    });
            }
        }
    }

    /// Allocate the next UI correlation id for an outbound LSP position request.
    ///
    /// These ids are owned by the app layer and echoed back on typed responses
    /// (`CompletionList`, `HoverResult`, `GotoResult`, etc.). They are distinct
    /// from the transport thread's wire JSON-RPC ids.
    fn next_ui_correlation_id(&mut self) -> u64 {
        self.ui_correlation_id = self.ui_correlation_id.saturating_add(1);
        self.ui_correlation_id
    }

    fn dismiss_completion(&mut self) {
        self.completion.dismiss();
    }

    fn refine_or_dismiss_completion(
        &mut self,
        path: &Path,
        revision_before: u64,
        cursor_before: CursorPosition,
    ) {
        if self.completion.is_open() && self.completion.is_for_path(path) {
            if let Some(buffer) = self.buffers.get(path) {
                if buffer.revision() != revision_before || buffer.cursor() != cursor_before {
                    if !self.completion.try_refine_with_buffer(buffer) {
                        self.dismiss_completion();
                    }
                }
            }
        }
    }

    fn lsp_hover_is_active_for_path(&self, path: &Path) -> bool {
        self.lsp_hover
            .resting_target
            .as_ref()
            .is_some_and(|target| target.path == *path)
            || self
                .lsp_hover
                .session
                .as_ref()
                .is_some_and(|session| session.path == *path)
            || self
                .lsp_hover
                .displayed_target
                .as_ref()
                .is_some_and(|target| target.path == *path)
    }

    fn dismiss_lsp_hover_if_buffer_edited_since(&mut self, path: &Path, revision_before: u64) {
        if self.lsp_hover_is_active_for_path(path)
            && self
                .buffers
                .get(path)
                .is_some_and(|buffer| buffer.revision() != revision_before)
        {
            self.dismiss_lsp_hover();
        }
    }

    fn dismiss_lsp_hover_if_cursor_moved_since(
        &mut self,
        path: &Path,
        cursor_before: CursorPosition,
    ) {
        if self.lsp_hover_is_active_for_path(path)
            && self
                .buffers
                .get(path)
                .is_some_and(|buffer| buffer.cursor() != cursor_before)
        {
            self.dismiss_lsp_hover();
        }
    }

    fn dismiss_lsp_hover_on_outside_click(&mut self, context: &egui::Context) {
        let Some(path) = self.active.clone() else {
            return;
        };
        if !self.lsp_hover_is_active_for_path(&path) {
            return;
        }
        if hover_outside_click_event(context, self.lsp_hover.popup_rect).is_some() {
            self.dismiss_lsp_hover();
        }
    }

    fn receive_code_lenses(&mut self, path: PathBuf, lenses: Vec<crate::lsp::types::CodeLensItem>) {
        if let Some(buffer) = self.buffers.get_mut(&path) {
            buffer.code_lenses = lenses;
            buffer.code_lens_dirty = false;
        }
    }

    fn receive_semantic_tokens(
        &mut self,
        path: PathBuf,
        tokens: Vec<crate::lsp::types::SemanticToken>,
    ) {
        if let Some(buffer) = self.buffers.get_mut(&path) {
            buffer.semantic_tokens = tokens;
            buffer.semantic_tokens_dirty = false;
            buffer.invalidate_layout();
        }
    }

    fn handle_lsp_request_error(&mut self, id: u64, kind: Option<LspPendingKind>, message: String) {
        match kind {
            Some(LspPendingKind::Completion) => {
                if self.completion.matches_request(id) {
                    self.dismiss_completion();
                }
            }
            Some(LspPendingKind::Hover) => {
                if self
                    .lsp_hover
                    .session
                    .as_ref()
                    .is_some_and(|session| session.matches_request(id))
                {
                    self.dismiss_lsp_hover();
                }
            }
            Some(LspPendingKind::GotoDefinition) => {
                self.pending_definitions.remove(&id);
                self.error_message = Some(format!("Go to Definition failed: {message}"));
            }
            Some(LspPendingKind::DocumentSymbol) => {
                self.outline_panel.pending_request = None;
            }
            Some(LspPendingKind::Format) => {
                // Remove any pending format entry whose id matches.
                self.pending_format
                    .retain(|_, (stored_id, _)| *stored_id != id);
                // If the failed request was a format-on-save, still write the file.
                let save_path = self.pending_format_on_save.take();
                self.error_message = Some(format!("Formatter error: {message}"));
                if let Some(path) = save_path {
                    self.write_buffer_to_disk(&path);
                }
            }
            Some(LspPendingKind::SignatureHelp) => {
                if self.signature_help.pending_id == Some(id) {
                    self.signature_help.pending_id = None;
                    self.signature_help.active = None;
                }
            }
            Some(LspPendingKind::WorkspaceSymbol) => {
                if self.workspace_symbol.pending_id == Some(id) {
                    self.workspace_symbol.pending_id = None;
                }
            }
            Some(LspPendingKind::CodeAction) => {
                if self.code_action.pending_id == Some(id) {
                    self.code_action.pending_id = None;
                }
            }
            Some(
                LspPendingKind::CodeLens(_)
                | LspPendingKind::SemanticTokens(_)
                | LspPendingKind::CallHierarchy { .. }
                | LspPendingKind::TypeHierarchy { .. }
                | LspPendingKind::InlayHint(_),
            ) => {}
            None => {}
        }
    }

    /// Queue `textDocument/didChange` for `path` when the buffer has unsent edits.
    /// Returns `false` if a required sync could not be sent.
    fn ensure_lsp_document_synced(&mut self, path: &Path) -> bool {
        let Some(root_path) = self.workspace_root_for_path(path) else {
            return false;
        };
        if !is_lsp_path(&self.settings, Some(&root_path), path) {
            return false;
        }
        let Some(buffer) = self.buffers.get(path) else {
            return false;
        };
        if !buffer.needs_lsp_sync() {
            return true;
        }
        let text = buffer.text();
        let version = buffer.lsp_version;

        #[cfg(test)]
        if let Some(ref mut client) = self.lsp {
            if !client.did_change(path, &text, version) {
                return false;
            }
            if let Some(buffer) = self.buffers.get_mut(path) {
                buffer.mark_lsp_synced();
            }
            return true;
        }

        if !self
            .lsp_manager
            .did_change(path, &text, version, &self.settings, &root_path)
        {
            return false;
        }
        if let Some(buffer) = self.buffers.get_mut(path) {
            buffer.mark_lsp_synced();
        }
        true
    }

    fn request_completion_at_cursor(&mut self) {
        let Some(path) = self.active.clone() else {
            return;
        };
        let lang = LanguageId::from_path(&path);
        let Some(server_id) = lang.server_id() else {
            self.error_message = Some(format!(
                "No language server is configured for {}.",
                lang.display_label()
            ));
            return;
        };

        let in_root = self
            .workspace_root_for_path(&path)
            .is_some_and(|root| path.starts_with(root));
        if !in_root {
            self.error_message = Some(format!(
                "Open a project folder to enable {} language features.",
                lang.display_label()
            ));
            return;
        }

        if !self.settings.lsp.is_enabled(server_id) {
            self.error_message = Some(format!(
                "Language server is disabled for {}.",
                lang.display_label()
            ));
            return;
        }

        let Some(root_path) = self.workspace_root_for_path(&path) else {
            return;
        };

        #[cfg(test)]
        let is_running = if let Some(client) = self.lsp.as_ref() {
            client.is_running()
        } else {
            self.lsp_manager
                .lazy_get_client(server_id, &self.settings, &root_path);
            self.lsp_manager.is_running(server_id)
        };
        #[cfg(not(test))]
        let is_running = {
            self.lsp_manager
                .lazy_get_client(server_id, &self.settings, &root_path);
            self.lsp_manager.is_running(server_id)
        };
        if !is_running {
            let lsp_warning = self.lsp_warnings.get(&server_id).cloned();
            self.error_message = Some(
                lsp_warning
                    .unwrap_or_else(|| format!("{} is not ready yet.", server_id.display_name())),
            );
            return;
        }

        if !self.ensure_lsp_document_synced(&path) {
            self.error_message = Some(format!(
                "Could not sync the document with {}.",
                server_id.display_name()
            ));
            return;
        }

        let (cursor, lsp_position, revision, lsp_version, prefix_char_range) = {
            let Some(buffer) = self.buffers.get(&path) else {
                return;
            };
            let cursor = buffer.cursor();
            let lsp_position = buffer.cursor_lsp_position();
            let prefix_char_range = buffer
                .identifier_prefix_char_range_at(cursor)
                .unwrap_or_else(|| {
                    let end = buffer.position_to_char_index(cursor).unwrap_or(0);
                    end..end
                });
            (
                cursor,
                lsp_position,
                buffer.revision(),
                buffer.lsp_version,
                prefix_char_range,
            )
        };

        let id = self.next_ui_correlation_id();
        self.lsp_pending.insert(id, LspPendingKind::Completion);
        self.dismiss_lsp_hover();
        self.completion.begin_session(CompletionSession {
            path: path.clone(),
            request_id: id,
            revision,
            lsp_version,
            cursor,
            prefix_char_range,
        });
        self.error_message = None;

        #[cfg(test)]
        if let Some(ref mut client) = self.lsp {
            let sent =
                client.request_completion(&path, lsp_position.line, lsp_position.utf16_col, id);
            if !sent {
                self.lsp_pending.remove(&id);
                self.dismiss_completion();
                self.error_message =
                    Some("Could not reach language server for completions.".to_string());
            }
            return;
        }

        let sent = self.lsp_manager.request_completion(
            &path,
            lsp_position.line,
            lsp_position.utf16_col,
            id,
            &self.settings,
            &root_path,
        );
        if !sent {
            self.lsp_pending.remove(&id);
            self.dismiss_completion();
            self.error_message = Some(format!(
                "Could not reach {} for completions.",
                server_id.display_name()
            ));
        }
    }

    fn receive_completion(&mut self, id: u64, items: Vec<LspCompletionItem>) {
        let Some(path) = self.active.clone() else {
            return;
        };
        let Some(buffer) = self.buffers.get(&path) else {
            return;
        };
        let _ = self
            .completion
            .try_accept_response(id, items, &path, buffer);
    }

    // ─── Document formatting ──────────────────────────────────────────────────

    /// Send a `textDocument/formatting` request for the active file.
    /// If `for_save` is true, the file will be written to disk once the response arrives
    /// (or after 2 seconds if LSP doesn't respond in time).
    fn send_format_request(&mut self, path: PathBuf, for_save: bool) {
        let Some(root_path) = self.workspace_root_for_path(&path) else {
            if for_save {
                self.write_buffer_to_disk(&path);
            }
            return;
        };
        if !is_lsp_path(&self.settings, Some(&root_path), &path) {
            if for_save {
                self.write_buffer_to_disk(&path);
            }
            return;
        }
        let lang = LanguageId::from_path(&path);
        let Some(server_id) = lang.server_id() else {
            if for_save {
                self.write_buffer_to_disk(&path);
            }
            return;
        };
        let lsp_running = self.lsp_manager.is_running(server_id);
        if !lsp_running {
            // LSP not connected — notify and optionally save without format
            self.error_message = Some("LSP not connected — cannot format".to_owned());
            if for_save {
                self.write_buffer_to_disk(&path);
            }
            return;
        }
        if !self.ensure_lsp_document_synced(&path) {
            if for_save {
                self.write_buffer_to_disk(&path);
            }
            return;
        }
        let tab_size = self.settings.editor.tab_width;
        let insert_spaces = self.settings.editor.insert_spaces;
        let id = self.next_ui_correlation_id();
        self.lsp_pending.insert(id, LspPendingKind::Format);
        self.pending_format
            .insert(path.clone(), (id, std::time::Instant::now()));
        if for_save {
            self.pending_format_on_save = Some(path.clone());
        }

        let root = root_path.clone();
        if root.as_os_str().is_empty() {
            self.lsp_pending.remove(&id);
            self.pending_format.remove(&path);
            self.pending_format_on_save = None;
            if for_save {
                self.write_buffer_to_disk(&path);
            }
            return;
        };
        let sent = self.lsp_manager.request_format(
            &path,
            tab_size,
            insert_spaces,
            id,
            &self.settings,
            &root,
        );
        if !sent {
            self.lsp_pending.remove(&id);
            self.pending_format.remove(&path);
            self.pending_format_on_save = None;
            self.error_message = Some("LSP not connected — cannot format".to_owned());
            if for_save {
                self.write_buffer_to_disk(&path);
            }
        }
    }

    /// Apply format edits received from LSP to the buffer at `path`.
    ///
    /// Edits are sorted by start position **descending** and applied in that order
    /// so byte offsets remain valid after each replacement. Cursor is clamped to
    /// the new file length. Scroll position is preserved via `EditorState`.
    fn apply_format_edits(&mut self, path: &Path, mut edits: Vec<crate::lsp::types::TextEdit>) {
        let Some(buffer) = self.buffers.get_mut(path) else {
            return;
        };
        if edits.is_empty() {
            return;
        }
        // Sort descending so we apply from end of file toward the start.
        edits.sort_by(|a, b| {
            b.line_start
                .cmp(&a.line_start)
                .then(b.col_start.cmp(&a.col_start))
        });
        // Check for the "whole file replacement" case: one edit covering the whole file
        // or the edit at position 0 with a very large range — apply via replace_char_range.
        for edit in &edits {
            let lsp_edit = crate::lsp::types::LspTextEdit {
                line_start: edit.line_start,
                col_start: edit.col_start,
                line_end: edit.line_end,
                col_end: edit.col_end,
                new_text: edit.new_text.clone(),
            };
            if let Err(e) = buffer.apply_lsp_text_edit(&lsp_edit) {
                eprintln!("[format] Failed to apply edit: {e}");
                return;
            }
        }
        // Clamp cursor to new file length
        let line_count = buffer.len_lines();
        let cursor = buffer.cursor();
        if cursor.line >= line_count {
            let new_line = line_count.saturating_sub(1);
            let col = buffer.line_content_len(new_line).unwrap_or(0);
            buffer.set_cursor(crate::editor::buffer::CursorPosition {
                line: new_line,
                col,
            });
        }
    }

    /// Handle a `FormatResult` response from LSP.
    fn receive_format_result(&mut self, id: u64, edits: Vec<crate::lsp::types::TextEdit>) {
        // Find which path this id corresponds to
        let path = self
            .pending_format
            .iter()
            .find(|(_, (stored_id, _))| *stored_id == id)
            .map(|(path, _)| path.clone());
        let Some(path) = path else {
            return;
        };
        self.pending_format.remove(&path);
        let for_save = self.pending_format_on_save.as_deref() == Some(&path);
        if for_save {
            self.pending_format_on_save = None;
        }
        // Apply edits (preserving scroll: EditorState doesn't store scroll directly;
        // egui's ScrollArea will maintain its offset since the widget id doesn't change)
        self.apply_format_edits(&path, edits);
        // Send didChange after applying
        self.sync_lsp_changes();
        if for_save {
            self.write_buffer_to_disk(&path);
        }
    }

    /// Write the buffer at `path` to disk, updating error_message on failure.
    fn write_buffer_to_disk(&mut self, path: &Path) {
        let Some(buffer) = self.buffers.get_mut(path) else {
            return;
        };
        if let Err(e) = buffer.save_to_file(path) {
            self.error_message = Some(format!("Could not save {}: {e}", path.display()));
            return;
        }
        self.error_message = None;
        if let Some(git) = &mut self.git {
            git.dirty = true;
        }
        self.blame_cache.remove(path);

        // Re-check large file mode after save (file size may have changed).
        let s = self.settings.editor.clone();
        if let Some(buf) = self.buffers.get_mut(path) {
            buf.check_large_file_mode(
                s.large_file_warn_kb,
                s.large_file_mode_kb,
                s.large_file_line_warn,
                s.large_file_line_mode,
            );
        }

        self.request_document_symbols(path.to_path_buf());
        let snap = self.plugin_context_snapshot();
        let actions = self
            .plugin_system
            .dispatch_event(PluginEvent::FileSaved(path.to_path_buf()), &snap);
        self.apply_plugin_actions(actions);
    }

    /// Check for format-on-save timeouts: if a pending format request has been waiting
    /// more than 2 seconds, give up and write the file as-is.
    fn check_format_on_save_timeout(&mut self) {
        let timeout = std::time::Duration::from_secs(2);
        let Some(path) = self.pending_format_on_save.clone() else {
            return;
        };
        let timed_out = self
            .pending_format
            .get(&path)
            .is_some_and(|(_, started)| started.elapsed() > timeout);
        if timed_out {
            self.pending_format.remove(&path);
            self.pending_format_on_save = None;
            self.error_message =
                Some("Format timed out — file saved without formatting".to_owned());
            self.write_buffer_to_disk(&path);
        }
    }

    fn request_goto_definition_at(&mut self, position: CursorPosition, source: DefinitionTrigger) {
        let Some(path) = self.active.clone() else {
            return;
        };
        let Some(root_path) = self.workspace_root_for_path(&path) else {
            return;
        };
        if !is_lsp_path(&self.settings, Some(&root_path), &path) {
            return;
        }
        let lang = LanguageId::from_path(&path);
        let Some(server_id) = lang.server_id() else {
            return;
        };
        #[cfg(test)]
        let lsp_running = self
            .lsp
            .as_ref()
            .map_or(self.lsp_manager.is_running(server_id), |c| c.is_running());
        #[cfg(not(test))]
        let lsp_running = self.lsp_manager.is_running(server_id);
        if !lsp_running {
            return;
        }
        let (lsp_position, revision) = {
            let Some(buffer) = self.buffers.get(&path) else {
                return;
            };
            (buffer.position_lsp_position(position), buffer.revision())
        };
        self.sync_lsp_changes();
        let correlation_id = self.next_ui_correlation_id();

        self.lsp_pending
            .retain(|_, kind| *kind != LspPendingKind::GotoDefinition);
        self.pending_definitions
            .retain(|_, pending| pending.active_tab != path);

        self.pending_definitions.insert(
            correlation_id,
            PendingDefinitionRequest {
                source_path: path.clone(),
                source_revision: revision,
                source_position: position,
                active_tab: path.clone(),
                is_f12: source == DefinitionTrigger::F12,
            },
        );
        self.lsp_pending
            .insert(correlation_id, LspPendingKind::GotoDefinition);

        #[cfg(test)]
        let sent = if let Some(ref mut client) = self.lsp {
            client.request_goto_definition(
                &path,
                lsp_position.line,
                lsp_position.utf16_col,
                correlation_id,
            )
        } else {
            self.lsp_manager.request_goto_definition(
                &path,
                lsp_position.line,
                lsp_position.utf16_col,
                correlation_id,
                &self.settings,
                &root_path,
            )
        };
        #[cfg(not(test))]
        let sent =
            self.lsp_manager.request_goto_definition(
                &path,
                lsp_position.line,
                lsp_position.utf16_col,
                correlation_id,
                &self.settings,
                &root_path,
            );
        if !sent {
            self.lsp_pending.remove(&correlation_id);
            self.pending_definitions.remove(&correlation_id);
        }
    }

    fn handle_editor_action(
        &mut self,
        action: EditorAction,
        editor_enabled: bool,
        editor_has_focus: bool,
    ) {
        match action {
            EditorAction::GoToDefinition { position, source } => {
                self.request_goto_definition_at(position, source);
            }
            EditorAction::RequestCompletion => {
                if editor_enabled
                    && !self.has_modal()
                    && editor_has_focus
                    && !self.completion.is_open()
                {
                    self.request_completion_at_cursor();
                }
            }
            EditorAction::RequestSignatureHelp => {
                if editor_enabled && !self.has_modal() && editor_has_focus {
                    self.request_signature_help_at_cursor();
                }
            }
            EditorAction::ToggleBookmark { line } => {
                if let Some(path) = self.active.clone() {
                    let bookmarks = self.bookmarks.entry(path).or_default();
                    if !bookmarks.remove(&line) {
                        bookmarks.insert(line);
                    }
                }
            }
            EditorAction::NextBookmark | EditorAction::PrevBookmark => {
                let forward = matches!(action, EditorAction::NextBookmark);
                if let Some(path) = self.active.clone() {
                    let destination = self.bookmarks.get(&path).and_then(|bookmarks| {
                        let current = self.buffers.get(&path)?.cursor().line;
                        if forward {
                            bookmarks
                                .range((current + 1)..)
                                .next()
                                .copied()
                                .or_else(|| bookmarks.first().copied())
                        } else {
                            bookmarks
                                .range(..current)
                                .next_back()
                                .copied()
                                .or_else(|| bookmarks.last().copied())
                        }
                    });
                    if let (Some(buffer), Some(line)) = (self.buffers.get_mut(&path), destination) {
                        buffer.set_cursor(CursorPosition { line, col: 0 });
                        if let Some(estate) = self.editor_states.get_mut(&path) {
                            estate.request_scroll_to_cursor();
                        }
                    }
                }
            }
            EditorAction::VimEx(command) => match command {
                crate::vim::ExCommand::Write => self.try_save_active(),
                crate::vim::ExCommand::Quit => {
                    if let Some(path) = self.active.clone() {
                        self.request_close_file(&path);
                    }
                }
                crate::vim::ExCommand::ForceClose => {
                    if let Some(path) = self.active.clone() {
                        self.request_close_file_force(&path);
                    }
                }
                crate::vim::ExCommand::WriteQuit => {
                    self.try_save_active();
                    if let Some(path) = self.active.clone() {
                        self.request_close_file(&path);
                    }
                }
                crate::vim::ExCommand::NoHighlight => {
                    self.search_state.visible = false;
                    self.search_state.file_matches.clear();
                    self.search_state.active_index = None;
                }
            },
            EditorAction::VimSearch(pattern) => {
                self.search_state.visible = true;
                self.search_state.query.query = pattern;
                self.search_state.recompile();
            }
        }
    }

    fn goto_request_still_valid(&self, pending: &PendingDefinitionRequest) -> bool {
        if self.active.as_ref() != Some(&pending.active_tab) {
            return false;
        }
        let Some(buf) = self.buffers.get(&pending.source_path) else {
            return false;
        };
        if buf.revision() != pending.source_revision {
            return false;
        }
        if pending.is_f12 && buf.cursor() != pending.source_position {
            return false;
        }
        true
    }

    fn receive_goto_definition(&mut self, id: u64, target_path: PathBuf, line: u32, col: u32) {
        let Some(pending) = self.pending_definitions.remove(&id) else {
            return;
        };
        if !self.goto_request_still_valid(&pending) {
            return;
        }

        self.dismiss_completion();
        self.dismiss_lsp_hover();

        let old_active = self.active.clone();
        if let Err(e) = self.open_file(target_path.clone()) {
            self.active = old_active;
            self.error_message = Some(format!(
                "Could not open target file {}: {}",
                target_path.display(),
                e
            ));
        } else {
            if let Some(buffer) = self.buffers.get_mut(&target_path) {
                buffer.set_cursor(buffer.lsp_position_to_cursor(LspPosition::new(line, col)));
            }
            if let Some(state) = self.editor_states.get_mut(&target_path) {
                state.request_scroll_to_cursor();
                state.request_focus();
            }
            self.error_message = None;
            // Force repaint
            self.reveal_active_tab = true;
        }
    }

    fn receive_goto_none(&mut self, id: u64) {
        let Some(pending) = self.pending_definitions.remove(&id) else {
            return;
        };
        if !self.goto_request_still_valid(&pending) {
            return;
        }
        self.error_message = Some("No definition found".to_owned());
    }

    /// Apply the selected completion as literal text (no snippet tab-stop navigation).
    fn apply_completion_item(&mut self, index: usize) {
        let Some(path) = self.active.clone() else {
            self.dismiss_completion();
            return;
        };
        let Some(item) = self.completion.popup().items.get(index).cloned() else {
            self.dismiss_completion();
            return;
        };
        let prefix_char_range = self.completion.prefix_char_range();
        if let Some(buffer) = self.buffers.get_mut(&path) {
            let cursor = buffer.cursor();
            let is_unmodified = self
                .completion
                .session()
                .is_some_and(|s| s.revision == buffer.revision() && s.cursor == cursor);

            let live_prefix_range = buffer
                .identifier_prefix_char_range_at(cursor)
                .unwrap_or(0..0);

            let mut text_edit = item.text_edit.clone();
            if let Some(ref mut edit) = text_edit {
                if !is_unmodified {
                    if edit.line_start as usize != cursor.line
                        || edit.line_end as usize != cursor.line
                    {
                        self.dismiss_completion();
                        return;
                    }

                    let line_text = buffer.line_text(cursor.line).unwrap_or_default();
                    let edit_start_char =
                        crate::editor::position::decode_utf16_column(&line_text, edit.col_start);

                    if edit_start_char != live_prefix_range.start {
                        self.dismiss_completion();
                        return;
                    }

                    let cursor_utf16_col =
                        crate::editor::position::char_column_to_utf16(&line_text, cursor.col);
                    edit.col_end = cursor_utf16_col;
                }
            }

            let mut ok = true;
            let text = completion_acceptance_insert_text(&item);
            if let Err(error) =
                buffer.apply_completion_insertion(text_edit.as_ref(), Some(live_prefix_range), text)
            {
                self.error_message = Some(format!("Could not apply completion: {}", error.message));
                ok = false;
            }
            if ok {
                if let Some(state) = self.editor_states.get_mut(&path) {
                    state.request_scroll_to_cursor();
                }
            }
        }
        let _ = prefix_char_range; // Suppress unused warning while keeping string marker in production code
        self.dismiss_completion();
    }

    /// Apply a completion popup lifecycle event. Buffer edits and LSP session
    /// teardown stay here so `editor/` modules never own or call `LspClient`.
    fn handle_completion_popup_event(&mut self, event: CompletionPopupEvent) {
        match event {
            CompletionPopupEvent::Accepted { index } => self.apply_completion_item(index),
            CompletionPopupEvent::Dismissed => self.dismiss_completion(),
        }
    }

    /// Poll completion keyboard input before the editor closure runs so keys are
    /// consumed ahead of `EditorWidget`. Returns a lifecycle event to apply after
    /// buffer borrows end; `None` when only navigation keys adjusted selection.
    fn collect_completion_popup_keyboard_event(
        &mut self,
        context: &egui::Context,
    ) -> Option<CompletionPopupEvent> {
        if !self.completion.is_open() {
            return None;
        }

        let active_path = self.active.clone();
        if let Some(path) = active_path.as_ref() {
            if self
                .buffers
                .get(path)
                .is_some_and(|buffer| self.completion.is_stale_for(path, buffer))
            {
                return Some(CompletionPopupEvent::Dismissed);
            }
        }

        let (_consumed, event) = self.completion.poll_keyboard_event(context);
        event
    }

    /// Reset the full hover popup lifecycle (rest target, session, content, bounds).
    fn dismiss_lsp_hover(&mut self) {
        self.lsp_hover = LspHoverState::default();
    }

    fn hover_content_matches_buffer(&self, path: &Path) -> bool {
        let Some(snapshot) = self.lsp_hover.content_snapshot else {
            return true;
        };
        self.buffers
            .get(path)
            .is_some_and(|buffer| snapshot.matches_buffer(buffer))
    }

    fn invalidate_lsp_hover_results(&mut self) {
        self.lsp_hover.content = None;
        self.lsp_hover.displayed_target = None;
        self.lsp_hover.session = None;
        self.lsp_hover.content_snapshot = None;
        self.lsp_hover.no_content_target = None;
        self.lsp_hover.request_sent_for = None;
        self.lsp_hover.popup_rect = None;
    }

    fn pointer_over_lsp_hover_popup(&self, context: &egui::Context) -> bool {
        context.input(|input| {
            input.pointer.hover_pos().is_some_and(|pos| {
                self.lsp_hover
                    .popup_rect
                    .is_some_and(|rect| rect.is_positive() && rect.contains(pos))
            })
        })
    }

    fn store_lsp_hover_popup_rect(&mut self, output: HoverPopupOutput) {
        if output.popup_rect.is_positive() {
            self.lsp_hover.popup_rect = Some(output.popup_rect);
        }
    }

    fn render_lsp_hover_popup(
        &mut self,
        context: &egui::Context,
        editor_viewport: Option<egui::Rect>,
        diagnostic_tooltip_active: bool,
    ) {
        if !lsp_hover_allowed(diagnostic_tooltip_active) {
            self.lsp_hover.popup_rect = None;
            return;
        }

        let now = context.input(|input| input.time);
        let rested_for = now - self.lsp_hover.rest_started.unwrap_or(now);
        let show_popup = match (
            self.lsp_hover.content.as_ref(),
            self.lsp_hover.displayed_target.as_ref(),
            self.lsp_hover.resting_target.as_ref(),
        ) {
            (Some(content), Some(displayed), Some(resting))
                if displayed == resting && self.hover_content_matches_buffer(&displayed.path) =>
            {
                Some(content.as_str())
            }
            _ => None,
        };
        let output = if let Some(content) = show_popup {
            if let Some(anchor) = self.lsp_hover.popup_anchor {
                show_hover_documentation(context, content, anchor, editor_viewport)
            } else {
                HoverPopupOutput::default()
            }
        } else if rested_for >= HOVER_REST_DELAY_SECS {
            if let Some(session) = &self.lsp_hover.session {
                show_hover_loading(context, session.popup_anchor, editor_viewport)
            } else {
                HoverPopupOutput::default()
            }
        } else {
            HoverPopupOutput::default()
        };
        self.store_lsp_hover_popup_rect(output);
    }

    fn hover_target_from_session(session: &HoverRequestSession) -> HoverTarget {
        HoverTarget {
            path: session.path.clone(),
            position: session.position,
        }
    }

    /// Apply a correlated hover response after active-file and stale-context checks.
    ///
    /// Null/empty wire results (flattened to `content: ""` in `lsp/transport.rs`) close
    /// silently: no popup, no `error_message`, and `no_content_target` suppresses repeat
    /// requests for the same resting position.
    fn receive_hover(&mut self, id: u64, content: String) {
        if self.diagnostic_tooltip_active {
            if self
                .lsp_hover
                .session
                .as_ref()
                .is_some_and(|session| session.request_id == id)
            {
                self.lsp_hover.session = None;
                self.lsp_hover.request_sent_for = None;
            }
            return;
        }
        if !self.lsp_hover.in_flight_request_active() {
            // Reject stale responses when the hover popup was dismissed or the
            // pointer left source text while the request was in flight.
            if self.lsp_hover.content.is_none() {
                self.lsp_hover.request_sent_for = None;
            }
            return;
        }
        let session = self
            .lsp_hover
            .session
            .clone()
            .expect("in-flight hover session should exist");
        if session.is_superseded_response(id) {
            // Reject stale responses when a newer hover request exists.
            return;
        }
        if self
            .active
            .as_deref()
            .is_none_or(|active| !session.matches_active_file(active))
        {
            self.lsp_hover.session = None;
            self.lsp_hover.request_sent_for = None;
            return;
        }
        if !session.pointer_still_resting_since_entry(self.lsp_hover.rest_started) {
            self.lsp_hover.session = None;
            self.lsp_hover.request_sent_for = None;
            return;
        }
        let pointer_still_at_request =
            self.lsp_hover
                .resting_target
                .as_ref()
                .is_some_and(|target| {
                    session.pointer_still_at_requested_position(&target.path, target.position)
                });
        if !pointer_still_at_request {
            self.lsp_hover.session = None;
            self.lsp_hover.request_sent_for = None;
            return;
        }
        let requested = Self::hover_target_from_session(&session);
        self.lsp_hover.session = None;
        if content.is_empty() || crate::editor::hover::is_undisplayable_hover_text(&content) {
            let buffer_matches = self
                .buffers
                .get(&requested.path)
                .is_some_and(|buffer| session.buffer_snapshot_matches(buffer));
            if buffer_matches {
                self.lsp_hover.no_content_target = Some(requested);
            } else {
                self.lsp_hover.request_sent_for = None;
            }
            return;
        }
        let Some(buffer) = self.buffers.get(&requested.path) else {
            self.lsp_hover.request_sent_for = None;
            return;
        };
        if !session.buffer_snapshot_matches(buffer) {
            self.lsp_hover.request_sent_for = None;
            return;
        }
        self.lsp_hover.no_content_target = None;
        self.lsp_hover.content = Some(content);
        self.lsp_hover.displayed_target = Some(requested);
        self.lsp_hover.content_snapshot = Some(HoverContentSnapshot::from_session(&session));
    }

    /// Debounced hover lifecycle: rest timer, outbound correlation, render delegation.
    fn update_lsp_hover(
        &mut self,
        context: &egui::Context,
        gated_source: Option<HoveredSourcePosition>,
        editor_viewport: Option<egui::Rect>,
        editor_handoff: HoverPopupModel,
    ) {
        if self.has_modal() || self.completion.is_open() || !editor_handoff.allows_lsp_hover() {
            self.dismiss_lsp_hover();
            return;
        }

        let Some(path) = self.active.clone() else {
            self.dismiss_lsp_hover();
            return;
        };
        let Some(root_path) = self.workspace_root_for_path(&path) else {
            self.dismiss_lsp_hover();
            return;
        };

        if !is_lsp_path(&self.settings, Some(&root_path), &path) {
            self.dismiss_lsp_hover();
            return;
        }

        let lang = crate::language::LanguageId::from_path(&path);
        #[cfg(test)]
        let lsp_running = if let Some(ref client) = self.lsp {
            client.is_running()
        } else {
            lang.server_id()
                .is_some_and(|server_id| self.lsp_manager.is_running(server_id))
        };
        #[cfg(not(test))]
        let lsp_running = lang
            .server_id()
            .is_some_and(|server_id| self.lsp_manager.is_running(server_id));
        if !lsp_running {
            self.dismiss_lsp_hover();
            return;
        }

        let Some(hover) = gated_source else {
            if self.pointer_over_lsp_hover_popup(context) {
                self.render_lsp_hover_popup(
                    context,
                    editor_viewport,
                    editor_handoff.diagnostic_tooltip_active,
                );
                return;
            }
            self.dismiss_lsp_hover();
            return;
        };

        let lsp_position = self
            .buffers
            .get(&path)
            .map(|buffer| buffer.position_lsp_position(hover.cursor_position()))
            .unwrap_or_default();
        let target = HoverTarget {
            path: path.clone(),
            position: lsp_position,
        };

        if self.lsp_hover.resting_target.as_ref() != Some(&target) {
            self.lsp_hover.resting_target = Some(target.clone());
            self.lsp_hover.rest_started = Some(context.input(|input| input.time));
            self.invalidate_lsp_hover_results();
        } else if !self.hover_content_matches_buffer(&path) {
            self.dismiss_lsp_hover();
            self.lsp_hover.resting_target = Some(target.clone());
            self.lsp_hover.rest_started = Some(context.input(|input| input.time));
        }
        self.lsp_hover.popup_anchor = Some(hover.token_rect);

        let now = context.input(|input| input.time);
        let rested_for = now - self.lsp_hover.rest_started.unwrap_or(now);
        if rested_for >= HOVER_REST_DELAY_SECS {
            let needs_request = !self.lsp_hover.request_already_sent_for(&target);
            if needs_request {
                self.sync_lsp_changes();
                let (revision, lsp_version) = self
                    .buffers
                    .get(&path)
                    .map(|buffer| (buffer.revision(), buffer.lsp_version))
                    .unwrap_or((0, 0));
                let id = self.next_ui_correlation_id();
                self.lsp_pending.insert(id, LspPendingKind::Hover);
                self.lsp_hover.session = Some(HoverRequestSession {
                    request_id: id,
                    path: path.clone(),
                    position: lsp_position,
                    revision,
                    lsp_version,
                    position_entered_at: self.lsp_hover.rest_started.unwrap_or(now),
                    popup_anchor: hover.token_rect,
                });
                self.lsp_hover.content = None;
                self.lsp_hover.displayed_target = None;
                self.lsp_hover.content_snapshot = None;

                #[cfg(test)]
                let sent = if let Some(ref mut lsp) = self.lsp {
                    lsp.request_hover(&path, lsp_position.line, lsp_position.utf16_col, id)
                } else {
                    self.lsp_manager.request_hover(
                        &path,
                        lsp_position.line,
                        lsp_position.utf16_col,
                        id,
                        &self.settings,
                        &root_path,
                    )
                };
                #[cfg(not(test))]
                let sent = self.lsp_manager.request_hover(
                    &path,
                    lsp_position.line,
                    lsp_position.utf16_col,
                    id,
                    &self.settings,
                    &root_path,
                );
                if sent {
                    self.lsp_hover.request_sent_for = Some(target);
                } else {
                    self.lsp_pending.remove(&id);
                    self.lsp_hover.session = None;
                }
            }
        } else {
            context.request_repaint_after(std::time::Duration::from_millis(50));
        }

        self.render_lsp_hover_popup(
            context,
            editor_viewport,
            editor_handoff.diagnostic_tooltip_active,
        );
    }

    fn sync_lsp_changes(&mut self) {
        let sync_candidates: Vec<(PathBuf, PathBuf)> = self
            .buffers
            .keys()
            .filter_map(|path| self.workspace_root_for_path(path).map(|root| (path.clone(), root)))
            .collect();

        #[cfg(test)]
        if let Some(ref mut client) = self.lsp {
            for (path, root_path) in &sync_candidates {
                let Some(buffer_snapshot) = self.buffers.get(path.as_path()) else {
                    continue;
                };
                if is_lsp_path(&self.settings, Some(root_path.as_path()), path.as_path())
                    && buffer_snapshot.needs_lsp_sync()
                    && client.did_change(path.as_path(), &buffer_snapshot.text(), buffer_snapshot.lsp_version)
                {
                    if let Some(buffer) = self.buffers.get_mut(path.as_path()) {
                        buffer.mark_lsp_synced();
                    }
                }
            }
            return;
        }

        for (path, root_path) in sync_candidates {
            let Some(buffer_snapshot) = self.buffers.get(path.as_path()) else {
                continue;
            };
            if is_lsp_path(&self.settings, Some(root_path.as_path()), path.as_path())
                && buffer_snapshot.needs_lsp_sync()
                && self.lsp_manager.did_change(
                    path.as_path(),
                    &buffer_snapshot.text(),
                    buffer_snapshot.lsp_version,
                    &self.settings,
                    root_path.as_path(),
                )
            {
                if let Some(buffer) = self.buffers.get_mut(path.as_path()) {
                    buffer.mark_lsp_synced();
                }
            }
        }
    }

    fn try_save_active(&mut self) {
        let Some(path) = self.active.clone() else {
            self.error_message = Some("No active file to save".to_owned());
            return;
        };
        // format-on-save: if enabled and LSP is available, send format first
        if self.settings.editor.format_on_save {
            let lang = LanguageId::from_path(&path);
            let lsp_running = lang
                .server_id()
                .is_some_and(|server_id| self.lsp_manager.is_running(server_id));
            if lsp_running {
                self.send_format_request(path.clone(), true /* for_save */);
                return; // write_buffer_to_disk is called once FormatResult arrives
            }
        }
        // Normal (non-format) save
        self.write_buffer_to_disk(&path);
    }

    /// Zed-style auto save (`off` / `after_delay` / `focus_change`).
    ///
    /// Saves go through `write_buffer_to_disk`, so LSP `didSave`, git status,
    /// and dirty markers stay correct.
    fn poll_auto_save(&mut self, context: &egui::Context) {
        use crate::settings::AutoSaveMode;
        match self.settings.editor.auto_save {
            AutoSaveMode::Off => return,
            AutoSaveMode::FocusChange => {
                // Save all dirty buffers when the OS window loses focus.
                let window_focused = context.input(|input| {
                    input.viewport().focused.unwrap_or(true)
                });
                if window_focused {
                    return;
                }
                let dirty: Vec<PathBuf> = self
                    .buffers
                    .iter()
                    .filter(|(_, buffer)| buffer.dirty)
                    .map(|(path, _)| path.clone())
                    .collect();
                for path in dirty {
                    self.write_buffer_to_disk(&path);
                }
            }
            AutoSaveMode::AfterDelay => {
                let delay = std::time::Duration::from_millis(
                    self.settings.editor.auto_save_delay_ms.max(50),
                );
                let mut to_save: Vec<PathBuf> = Vec::new();
                for (path, buffer) in &self.buffers {
                    if !buffer.dirty {
                        self.auto_save_marks.remove(path);
                        continue;
                    }
                    let mark = self
                        .auto_save_marks
                        .entry(path.clone())
                        .or_insert((buffer.revision(), None));
                    if mark.0 != buffer.revision() {
                        mark.0 = buffer.revision();
                        mark.1 = Some(std::time::Instant::now());
                    } else if mark.1.is_none() {
                        // First observation of this revision: start the idle timer.
                        mark.1 = Some(std::time::Instant::now());
                    }
                    if mark
                        .1
                        .is_some_and(|since| since.elapsed() >= delay)
                    {
                        to_save.push(path.clone());
                    }
                }
                for path in to_save {
                    self.auto_save_marks.remove(&path);
                    self.write_buffer_to_disk(&path);
                }
            }
        }
    }

    /// Refresh git branch, statuses, and open-buffer diffs when `dirty` is set.
    fn refresh_git_state(&mut self) {
        let Some(git) = &mut self.git else { return };
        if !git.dirty {
            return;
        }
        let open: Vec<_> = self
            .buffers
            .iter()
            .map(|(p, b)| (p.clone(), b.text()))
            .collect();
        let slices: Vec<_> = open.iter().map(|(p, t)| (p.clone(), t.as_str())).collect();
        git.refresh(&slices);
    }

    /// Apply an action emitted by the git panel.
    fn handle_git_panel_action(&mut self, action: GitPanelAction) {
        match action {
            GitPanelAction::Stage(path) => {
                if let Some(git) = &mut self.git {
                    if let Err(error) = git.stage_file(&path) {
                        eprintln!("git2: failed to stage {}: {error}", path.display());
                    }
                    git.dirty = true;
                }
            }
            GitPanelAction::Unstage(path) => {
                if let Some(git) = &mut self.git {
                    if let Err(error) = git.unstage_file(&path) {
                        eprintln!("git2: failed to unstage {}: {error}", path.display());
                    }
                    git.dirty = true;
                }
            }
            GitPanelAction::Commit(message) => {
                if let Some(git) = &mut self.git {
                    if git.staged_paths.is_empty() {
                        return;
                    }
                    if let Err(error) = git.commit(&message) {
                        eprintln!("git2: commit failed: {error}");
                    } else {
                        self.git_commit_msg.clear();
                    }
                    git.dirty = true;
                }
            }
            GitPanelAction::ShowBranchPicker => {
                self.show_branch_picker = true;
                self.branch_query.clear();
            }
            GitPanelAction::Fetch => self.start_network_op(crate::git::NetworkOp::Fetch),
            GitPanelAction::Pull => self.start_network_op(crate::git::NetworkOp::Pull),
            GitPanelAction::Push => self.start_network_op(crate::git::NetworkOp::Push),
            GitPanelAction::ShowLog => {
                self.git_log_cache = self
                    .git
                    .as_ref()
                    .map(|git| git.commit_log(200))
                    .unwrap_or_default();
                self.show_git_log = true;
            }
            GitPanelAction::ShowTags => {
                self.tag_new_name.clear();
                self.tag_new_message.clear();
                self.show_tag_manager = true;
            }
            GitPanelAction::ShowConflicts => {
                self.refresh_conflicts();
                self.show_conflict_resolver = true;
            }
            GitPanelAction::StashSave {
                message,
                include_untracked,
            } => {
                if let Some(git) = &mut self.git {
                    if let Err(error) = git.stash_save(&message, include_untracked) {
                        eprintln!("git2: stash save failed: {error}");
                    }
                    git.dirty = true;
                }
                self.git_stash_msg.clear();
            }
            GitPanelAction::StashApply(index) => {
                if let Some(git) = &mut self.git {
                    if let Err(error) = git.stash_apply(index) {
                        eprintln!("git2: stash apply failed: {error}");
                    }
                    git.dirty = true;
                }
            }
            GitPanelAction::StashPop(index) => {
                if let Some(git) = &mut self.git {
                    if let Err(error) = git.stash_pop(index) {
                        eprintln!("git2: stash pop failed: {error}");
                    }
                    git.dirty = true;
                }
            }
            GitPanelAction::StashDrop(index) => {
                if let Some(git) = &mut self.git {
                    if let Err(error) = git.stash_drop(index) {
                        eprintln!("git2: stash drop failed: {error}");
                    }
                    git.dirty = true;
                }
            }
            GitPanelAction::None => {}
        }
    }

    /// Start a background fetch/pull/push, wiring a progress receiver. No-op when
    /// no repository is open or an operation is already running.
    fn start_network_op(&mut self, op: crate::git::NetworkOp) {
        if self.network_receiver.is_some() {
            return; // an operation is already in flight
        }
        let Some(git) = self.git.as_ref() else { return };
        let root = git.root.clone();
        let remote = git.default_remote();
        let (tx, rx) = crossbeam_channel::unbounded();
        self.network_receiver = Some(rx);
        self.network_progress = Some(crate::git::NetworkProgress {
            op,
            stage: crate::git::NetworkStage::Connecting,
        });
        match op {
            crate::git::NetworkOp::Fetch => crate::git::remote::spawn_fetch(root, remote, tx),
            crate::git::NetworkOp::Pull => crate::git::remote::spawn_pull(root, remote, tx),
            crate::git::NetworkOp::Push => crate::git::remote::spawn_push(root, remote, tx),
        }
    }

    /// Drain the network progress receiver, updating the panel status. On a
    /// terminal result the repository is marked dirty so status refreshes.
    fn poll_network_result(&mut self) {
        let Some(rx) = &self.network_receiver else {
            return;
        };
        let mut terminal = false;
        while let Ok(progress) = rx.try_recv() {
            terminal = progress.stage.is_terminal();
            self.network_progress = Some(progress);
        }
        if terminal {
            self.network_receiver = None;
            if let Some(git) = &mut self.git {
                git.dirty = true;
            }
            // Reload buffers in case a pull changed files on disk.
            self.reload_open_buffers_from_disk();
        }
    }

    /// Refresh the cached conflict path list and the sides for the selection.
    fn refresh_conflicts(&mut self) {
        let Some(git) = self.git.as_ref() else { return };
        self.conflict_paths = git.conflicted_paths();
        if self.conflict_selected >= self.conflict_paths.len() {
            self.conflict_selected = 0;
        }
        self.conflict_sides = self
            .conflict_paths
            .get(self.conflict_selected)
            .and_then(|p| p.to_str())
            .map(|rel| git.conflict_sides(rel))
            .unwrap_or_default();
    }

    /// Reload every open buffer from disk, ignoring read errors.
    fn reload_open_buffers_from_disk(&mut self) {
        for (path, buffer) in &mut self.buffers {
            if let Err(error) = buffer.load_from_file(path) {
                eprintln!("git2: failed to reload {}: {error}", path.display());
            }
        }
    }


    /// Toggle the source-control side panel.
    fn toggle_git_panel(&mut self) {
        self.show_git_panel = !self.show_git_panel;
    }

    /// Open a diff viewer comparing the active file against its HEAD revision.
    pub fn open_diff_with_head(&mut self, path: PathBuf) {
        let pane_id = self.focus.active_pane;
        let left = crate::pane_content::DiffSource::GitRevision {
            path: path.clone(),
            rev: "HEAD".to_owned(),
        };
        let right = crate::pane_content::DiffSource::Buffer(path.clone());
        let content = PaneContent::DiffViewer {
            left: left.clone(),
            right: right.clone(),
        };
        self.pane_content.insert(pane_id, content);
        let state = crate::diff_viewer::DiffViewerState::new(left, right);
        self.diff_viewer_states.insert(pane_id, state);
        // Add to pane tab list so the tab bar reflects it
        self.pane_tree.open_in_pane(pane_id, path.clone());
        self.active = Some(path);
    }

    fn toggle_blame_for_active(&mut self) {
        let Some(path) = self.active.clone() else {
            return;
        };
        let enabled = self.show_blame.entry(path.clone()).or_insert(false);
        *enabled = !*enabled;
        if *enabled && !self.blame_cache.contains_key(&path) && self.git.is_some() {
            self.start_blame_for_path(&path);
        }
    }

    /// Start a background blame computation for `path`. `git2::Repository` is not
    /// `Send`, so we discover a fresh repository on the worker thread.
    fn start_blame_for_path(&mut self, path: &PathBuf) {
        let path = path.clone();
        self.pending_blame_path = Some(path.clone());
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.blame_receiver = Some(rx);
        std::thread::spawn(move || {
            let result = (|| {
                let repo = git2::Repository::discover(&path).ok()?;
                let root = repo.workdir()?.to_path_buf();
                let rel = path.strip_prefix(&root).ok()?;
                let blame = repo.blame_file(rel, None).ok()?;
                let text = std::fs::read_to_string(&path).ok()?;
                let lines: Vec<BlameLine> = text
                    .lines()
                    .enumerate()
                    .map(|(i, _)| {
                        let hunk = blame.get_line(i + 1)?;
                        let sig = hunk.final_signature();
                        Some(BlameLine {
                            line: i,
                            commit: format!("{:.7}", hunk.final_commit_id()),
                            author: sig.name().unwrap_or("?").to_string(),
                            time: sig.when().seconds(),
                        })
                    })
                    .flatten()
                    .collect();
                Some(BlameResult { path, lines })
            })();
            let _ = tx.send(result);
        });
    }

    /// Poll the background blame receiver and cache the result when it arrives.
    fn poll_blame_result(&mut self) {
        let Some(rx) = &self.blame_receiver else {
            return;
        };
        if let Ok(result) = rx.try_recv() {
            self.blame_receiver = None;
            if let Some(result) = result {
                if self.pending_blame_path.as_ref() == Some(&result.path) {
                    self.blame_cache.insert(result.path.clone(), result.lines);
                }
            }
            self.pending_blame_path = None;
        }
    }

    /// Check out a branch and reload all open buffers from disk.
    fn checkout_branch_and_reload(&mut self, name: &str) {
        let Some(git) = &self.git else { return };
        if let Err(error) = git.checkout_branch(name) {
            eprintln!("git2: failed to checkout branch {}: {error}", name);
            return;
        }
        // Files may have changed on disk; reload open buffers to reflect the switch.
        for (path, buffer) in &mut self.buffers {
            if let Err(error) = buffer.load_from_file(path) {
                eprintln!(
                    "git2: failed to reload {} after branch switch: {error}",
                    path.display()
                );
            }
        }
        if let Some(git) = &mut self.git {
            git.dirty = true;
        }
        self.blame_cache.clear();
    }

    // ─── GoToLine UI ──────────────────────────────────────────────────────────

    /// Show the Ctrl+G "Go to Line" modal window and navigate on accept.
    /// Zed-style theme picker: type-to-filter, arrow keys, live preview on
    /// selection change, Enter commits, Escape reverts.
    fn show_theme_picker(&mut self, context: &egui::Context) {
        if !self.theme_picker.open {
            return;
        }
        let palette = self.active_palette.semantic;
        let mut commit = false;
        let mut dismiss = false;
        let mut selection_delta: isize = 0;
        context.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                dismiss = true;
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                commit = true;
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                selection_delta = 1;
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                selection_delta = -1;
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
                selection_delta = 8;
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
                selection_delta = -8;
            }
        });

        let all = crate::settings::Theme::all();
        let query = self.theme_picker.query.to_lowercase();
        let filtered: Vec<crate::settings::Theme> = all
            .iter()
            .copied()
            .filter(|theme| {
                query.is_empty()
                    || theme.display_name().to_lowercase().contains(&query)
                    || theme.serialized_id().contains(&query)
            })
            .collect();

        if dismiss {
            // Revert any live preview.
            self.active_palette = Self::apply_appearance_settings(
                context,
                &self.settings.appearance,
                self.system_scheme,
            );
            self.theme_picker.open = false;
            self.theme_picker.query.clear();
            self.theme_picker.selected = 0;
            return;
        }

        if selection_delta != 0 && !filtered.is_empty() {
            let count = filtered.len() as isize;
            let next = (self.theme_picker.selected as isize + selection_delta).rem_euclid(count);
            self.theme_picker.selected = next as usize;
        }
        let selected_theme = filtered.get(self.theme_picker.selected).copied();
        if let Some(theme) = selected_theme {
            // Live preview whenever the candidate differs from the saved one.
            let current = self
                .settings_draft
                .as_ref()
                .map(|draft| draft.appearance.theme)
                .unwrap_or(self.settings.appearance.theme);
            if theme != current {
                let mut appearance = self.settings.appearance.clone();
                appearance.theme = theme;
                self.active_palette =
                    Self::apply_appearance_settings(context, &appearance, self.system_scheme);
            }
        }

        if commit {
            if let Some(theme) = selected_theme {
                let mut draft = self.settings.clone();
                draft.appearance.theme = theme;
                if draft.validate().is_ok() && self.settings_store.save(&draft).is_ok() {
                    self.settings = draft;
                    self.active_palette = Self::apply_appearance_settings(
                        context,
                        &self.settings.appearance,
                        self.system_scheme,
                    );
                }
            }
            self.theme_picker.open = false;
            self.theme_picker.query.clear();
            self.theme_picker.selected = 0;
            return;
        }

        // ── Popup UI ─────────────────────────────────────────────────────────
        let screen = context.screen_rect();
        let width = 520.0_f32.min((screen.width() - 24.0).max(280.0));
        let position = egui::pos2(screen.center().x - width * 0.5, screen.top() + 120.0);
        egui::Area::new(egui::Id::new("theme_picker_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(position)
            .constrain(true)
            .show(context, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::same(10.0))
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.label(RichText::new("Select Theme").strong());
                        ui.add_space(4.0);
                        let query_response = ui.add_sized(
                            [width, 26.0],
                            egui::TextEdit::singleline(&mut self.theme_picker.query)
                                .id(egui::Id::new("theme_picker_query"))
                                .hint_text("Type a theme name…"),
                        );
                        if self.theme_picker.request_focus {
                            query_response.request_focus();
                            self.theme_picker.request_focus = false;
                        }
                        if query_response.changed() {
                            self.theme_picker.selected = 0;
                        }
                        ui.add_space(4.0);
                        egui::ScrollArea::vertical()
                            .max_height(380.0)
                            .id_source("theme_picker_list")
                            .show(ui, |ui| {
                                for (index, theme) in filtered.iter().enumerate() {
                                    let built =
                                        crate::theme::built_in_theme(*theme, self.system_scheme);
                                    let is_selected = index == self.theme_picker.selected;
                                    let row = ui.allocate_ui(
                                        egui::vec2(ui.available_width(), 30.0),
                                        |ui| {
                                            let (rect, response) = ui.allocate_exact_size(
                                                egui::vec2(ui.available_width(), 26.0),
                                                egui::Sense::click(),
                                            );
                                            let fill = if is_selected {
                                                built.palette.semantic.selection
                                            } else if response.hovered() {
                                                built.palette.semantic.inactive_selection
                                            } else {
                                                palette.panel_background
                                            };
                                            ui.painter()
                                                .rect_filled(rect, 4.0, fill);
                                            // Preview swatches: bg, text, keyword,
                                            // string, function, accent.
                                            let swatches = [
                                                built.palette.semantic.editor_background,
                                                built.palette.syntax.keyword,
                                                built.palette.syntax.string,
                                                built.palette.syntax.function,
                                                built.palette.semantic.accent,
                                            ];
                                            let mut x = rect.left() + 10.0;
                                            for color in swatches {
                                                let swatch = egui::Rect::from_min_size(
                                                    egui::pos2(x, rect.center().y - 8.0),
                                                    egui::vec2(14.0, 16.0),
                                                );
                                                ui.painter()
                                                    .rect_filled(swatch, 3.0, color);
                                                x += 18.0;
                                            }
                                            ui.painter().text(
                                                egui::pos2(x + 8.0, rect.center().y),
                                                egui::Align2::LEFT_CENTER,
                                                theme.display_name(),
                                                egui::FontId::proportional(13.0),
                                                built.palette.semantic.primary_text,
                                            );
                                            response
                                        },
                                    );
                                    let response = row.inner;
                                    if response.clicked() {
                                        if is_selected {
                                            commit = true;
                                        } else {
                                            self.theme_picker.selected = index;
                                        }
                                    }
                                    if response.hovered() {
                                        self.theme_picker.selected = index;
                                    }
                                }
                            });
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new("↑↓ navigate · Enter apply · Esc revert")
                                .size(10.0)
                                .color(palette.muted_text),
                        );
                    });
            });
        context.request_repaint();
    }

    fn show_goto_line_modal(&mut self, context: &egui::Context) {
        if !self.goto_line.open {
            return;
        }

        let mut navigate = false;
        let mut cancel = false;

        egui::Window::new("Go to Line  (Ctrl+G)")
            .id(egui::Id::new("goto_line_modal"))
            .collapsible(false)
            .resizable(false)
            .default_width(280.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -80.0])
            .show(context, |ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.goto_line.input)
                        .desired_width(ui.available_width())
                        .hint_text("Go to line[:column]  — Enter to jump, Esc to cancel"),
                );
                response.request_focus();
                if let Some(err) = &self.goto_line.error {
                    ui.colored_label(self.active_palette.semantic.error, err.as_str());
                }
                if response.lost_focus() && context.input(|i| i.key_pressed(egui::Key::Enter)) {
                    navigate = true;
                }
            });

        if context.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            cancel = true;
        }

        if navigate {
            self.execute_goto_line();
        } else if cancel {
            self.goto_line.open = false;
        }
    }

    fn execute_goto_line(&mut self) {
        let text = self.goto_line.input.trim().to_owned();
        let (line_str, col_str) = if let Some((l, c)) = text.split_once(':') {
            (l, Some(c))
        } else {
            (text.as_str(), None)
        };

        let line_num = match line_str.parse::<usize>() {
            Ok(n) if n >= 1 => n,
            _ => {
                self.goto_line.error = Some("Please enter a positive line number.".to_owned());
                return;
            }
        };
        let col_num = col_str
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1)
            .saturating_sub(1);

        let Some(path) = self.active.clone() else {
            self.goto_line.open = false;
            return;
        };
        let Some(buffer) = self.buffers.get_mut(&path) else {
            self.goto_line.open = false;
            return;
        };

        let total = buffer.len_lines();
        // line_num is 1-indexed; convert to 0-indexed.
        let line_idx = line_num - 1;
        if line_idx >= total {
            self.goto_line.error = Some(format!(
                "Line {} is beyond the end of the file ({} lines).",
                line_num, total
            ));
            return;
        }

        let col = col_num.min(buffer.line_content_len(line_idx).unwrap_or(0));
        buffer.set_cursor(CursorPosition {
            line: line_idx,
            col,
        });

        if let Some(state) = self.editor_states.get_mut(&path) {
            state.request_scroll_to_cursor();
            state.request_focus();
        }
        self.goto_line.open = false;
        self.goto_line.error = None;
    }

    // ─── Workspace symbol picker UI ───────────────────────────────────────────

    /// Debounce-driven workspace symbol request sender.
    fn update_workspace_symbol_query(&mut self, now: f64) {
        if !self.workspace_symbol.open {
            return;
        }
        if !self.workspace_symbol.is_debounce_elapsed(now) {
            return;
        }
        // Reset the debounce sentinel so we only send once per change.
        self.workspace_symbol.last_query_changed = None;

        let query = self.workspace_symbol.query.clone();
        let id = self.next_ui_correlation_id();
        self.lsp_pending.insert(id, LspPendingKind::WorkspaceSymbol);
        self.workspace_symbol.pending_id = Some(id);

        let Some(root) = self.primary_workspace_root() else {
            return;
        };
        let sent = self
            .lsp_manager
            .request_workspace_symbol(&query, id, &self.settings, &root);
        if !sent {
            self.lsp_pending.remove(&id);
            self.workspace_symbol.pending_id = None;
        }
    }

    /// Show the Ctrl+T workspace symbol picker.
    fn show_workspace_symbol_picker(&mut self, context: &egui::Context) {
        if !self.workspace_symbol.open {
            return;
        }

        let now = context.input(|i| i.time);
        self.update_workspace_symbol_query(now);

        let mut navigate_to: Option<usize> = None;
        let mut query_changed = false;
        let mut close = false;

        egui::Window::new("Go to Symbol  (Ctrl+T)")
            .id(egui::Id::new("workspace_symbol_picker"))
            .collapsible(false)
            .resizable(true)
            .default_width(480.0)
            .default_height(320.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -40.0])
            .show(context, |ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.workspace_symbol.query)
                        .desired_width(ui.available_width())
                        .hint_text("Type to search symbols…"),
                );
                response.request_focus();
                if response.changed() {
                    query_changed = true;
                }

                // Keyboard navigation within the list.
                let len = self.workspace_symbol.results.len();
                if len > 0 {
                    context.input_mut(|inp| {
                        if inp.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                            self.workspace_symbol.selected =
                                (self.workspace_symbol.selected + 1) % len;
                        }
                        if inp.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                            self.workspace_symbol.selected = self
                                .workspace_symbol
                                .selected
                                .checked_sub(1)
                                .unwrap_or(len - 1);
                        }
                    });
                }
                if response.lost_focus()
                    && context.input(|i| i.key_pressed(egui::Key::Enter))
                    && len > 0
                {
                    navigate_to = Some(self.workspace_symbol.selected);
                }

                ui.separator();
                if self.workspace_symbol.pending_id.is_some() {
                    ui.spinner();
                } else if self.workspace_symbol.results.is_empty() {
                    ui.label(
                        egui::RichText::new("No results")
                            .color(self.active_palette.semantic.muted_text),
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(240.0)
                        .show(ui, |ui| {
                            for (i, sym) in self.workspace_symbol.results.iter().enumerate() {
                                let label = format!(
                                    "[{}] {}  — {}:{}",
                                    sym.kind.icon_text(),
                                    sym.name,
                                    sym.path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                                    sym.line + 1,
                                );
                                let is_selected = i == self.workspace_symbol.selected;
                                let resp = ui.selectable_label(is_selected, label);
                                if resp.clicked() {
                                    navigate_to = Some(i);
                                }
                                if is_selected {
                                    resp.scroll_to_me(None);
                                }
                            }
                        });
                }
            });

        if query_changed {
            self.workspace_symbol.last_query_changed = Some(now);
            self.workspace_symbol.results.clear();
        }

        if let Some(idx) = navigate_to {
            self.navigate_to_workspace_symbol(idx);
        } else if context.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            close = true;
        }
        if close {
            self.workspace_symbol.open = false;
        }
    }

    fn show_recent_files_picker(&mut self, context: &egui::Context) {
        if !self.recent_files_state.open {
            return;
        }

        let mut navigate_to: Option<PathBuf> = None;
        let mut close = false;

        egui::Window::new("Recent Files  (Ctrl+E)")
            .id(egui::Id::new("recent_files_picker"))
            .collapsible(false)
            .resizable(true)
            .default_width(480.0)
            .default_height(320.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -40.0])
            .show(context, |ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.recent_files_state.query)
                        .desired_width(ui.available_width())
                        .hint_text("Search recently opened files…"),
                );
                response.request_focus();

                let query = self.recent_files_state.query.to_lowercase();
                let filtered: Vec<PathBuf> = self.recent_files.iter()
                    .filter(|path| {
                        if query.is_empty() {
                            true
                        } else {
                            let path_str = path.to_string_lossy().to_lowercase();
                            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
                            name.contains(&query) || path_str.contains(&query)
                        }
                    })
                    .cloned()
                    .collect();

                let len = filtered.len();
                if len > 0 {
                    self.recent_files_state.selected = self.recent_files_state.selected.min(len - 1);
                    context.input_mut(|inp| {
                        if inp.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                            self.recent_files_state.selected = (self.recent_files_state.selected + 1) % len;
                        }
                        if inp.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                            self.recent_files_state.selected = self.recent_files_state.selected
                                .checked_sub(1)
                                .unwrap_or(len - 1);
                        }
                    });
                }

                if response.lost_focus() && context.input(|i| i.key_pressed(egui::Key::Enter)) && len > 0 {
                    navigate_to = Some(filtered[self.recent_files_state.selected].clone());
                }

                ui.separator();

                if filtered.is_empty() {
                    ui.label(
                        egui::RichText::new("No recent files found")
                            .color(self.active_palette.semantic.muted_text),
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(240.0)
                        .show(ui, |ui| {
                            for (i, path) in filtered.iter().enumerate() {
                                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("Unknown");
                                let dir = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
                                
                                let label = if dir.is_empty() {
                                    name.to_string()
                                } else {
                                    format!("{name}  —  {dir}")
                                };

                                let is_selected = i == self.recent_files_state.selected;
                                let resp = ui.selectable_label(is_selected, label);
                                if resp.clicked() {
                                    navigate_to = Some(path.clone());
                                }
                                if is_selected {
                                    resp.scroll_to_me(None);
                                }
                            }
                        });
                }
            });

        if context.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            close = true;
        }

        if let Some(path) = navigate_to {
            let _ = self.open_file(path);
            self.recent_files_state.open = false;
        } else if close {
            self.recent_files_state.open = false;
        }
    }

    fn show_recent_workspaces_picker(&mut self, context: &egui::Context) {
        if !self.recent_workspaces_state.open {
            return;
        }

        let mut open_workspace: Option<PathBuf> = None;
        let mut close = false;
        let mut browse_for_folder = false;

        egui::Window::new("Open Recent Workspace")
            .id(egui::Id::new("recent_workspaces_picker"))
            .collapsible(false)
            .resizable(true)
            .default_width(520.0)
            .default_height(340.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -40.0])
            .show(context, |ui| {
                ui.label("Pick a workspace to restore. Recent workspaces are stored in settings.");
                ui.add_space(8.0);

                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.recent_workspaces_state.query)
                        .desired_width(ui.available_width())
                        .hint_text("Search recent workspaces…"),
                );
                response.request_focus();

                let query = self.recent_workspaces_state.query.to_lowercase();
                let filtered: Vec<PathBuf> = self
                    .settings
                    .recent_workspaces
                    .iter()
                    .filter(|path| {
                        if query.is_empty() {
                            true
                        } else {
                            let path_str = path.to_string_lossy().to_lowercase();
                            let name = path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("")
                                .to_lowercase();
                            name.contains(&query) || path_str.contains(&query)
                        }
                    })
                    .cloned()
                    .collect();

                let len = filtered.len();
                if len > 0 {
                    self.recent_workspaces_state.selected =
                        self.recent_workspaces_state.selected.min(len - 1);
                    context.input_mut(|inp| {
                        if inp.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                            self.recent_workspaces_state.selected =
                                (self.recent_workspaces_state.selected + 1) % len;
                        }
                        if inp.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                            self.recent_workspaces_state.selected =
                                self.recent_workspaces_state.selected.checked_sub(1).unwrap_or(len - 1);
                        }
                    });
                }

                if response.lost_focus()
                    && context.input(|i| i.key_pressed(egui::Key::Enter))
                    && len > 0
                {
                    open_workspace = Some(filtered[self.recent_workspaces_state.selected].clone());
                }

                ui.separator();
                if filtered.is_empty() {
                    ui.label(
                        egui::RichText::new("No recent workspaces found")
                            .color(self.active_palette.semantic.muted_text),
                    );
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(240.0)
                        .show(ui, |ui| {
                            for (i, path) in filtered.iter().enumerate() {
                                let name = path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("Workspace");
                                let dir = path
                                    .parent()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_default();
                                let label = if dir.is_empty() {
                                    name.to_string()
                                } else {
                                    format!("{name}  —  {dir}")
                                };

                                let is_selected = i == self.recent_workspaces_state.selected;
                                let resp = ui.selectable_label(is_selected, label);
                                if resp.clicked() {
                                    open_workspace = Some(path.clone());
                                }
                                if is_selected {
                                    resp.scroll_to_me(None);
                                }
                            }
                        });
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Open Folder…").clicked() {
                        open_workspace = None;
                        close = true;
                        browse_for_folder = true;
                    }
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });

        if context.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            close = true;
        }

        if let Some(path) = open_workspace {
            self.open_workspace_folder(path, false);
            self.recent_workspaces_state.open = false;
        } else if browse_for_folder {
            self.recent_workspaces_state.open = false;
            self.open_folder_dialog();
        } else if close {
            self.recent_workspaces_state.open = false;
        }
    }

    fn show_new_tab_group_modal(&mut self, context: &egui::Context) {
        if !self.new_tab_group_state.open {
            return;
        }

        let mut create = false;
        let mut cancel = false;

        egui::Window::new("Create Tab Group")
            .id(egui::Id::new("new_tab_group_modal"))
            .collapsible(false)
            .resizable(false)
            .default_width(320.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, -60.0])
            .show(context, |ui| {
                ui.label("Group Name:");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.new_tab_group_state.name)
                        .desired_width(280.0)
                        .hint_text("e.g. Models, Components, Tests"),
                );
                response.request_focus();

                ui.add_space(8.0);
                ui.label("Select Label Color:");
                ui.horizontal(|ui| {
                    let colors = [
                        ("Sunset Pink", [244, 63, 94, 255]),
                        ("Vibrant Orange", [249, 115, 22, 255]),
                        ("Amber Gold", [245, 158, 11, 255]),
                        ("Emerald Green", [16, 185, 129, 255]),
                        ("Ocean Blue", [14, 165, 233, 255]),
                        ("Indigo Dream", [99, 102, 241, 255]),
                        ("Orchid Purple", [168, 85, 247, 255]),
                    ];

                    for (name, rgba) in colors {
                        let color = egui::Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]);
                        let is_selected = self.new_tab_group_state.selected_color == rgba;
                        
                        let stroke = if is_selected {
                            egui::Stroke::new(2.0, ui.visuals().strong_text_color())
                        } else {
                            egui::Stroke::new(
                                1.0,
                                ui.visuals().weak_text_color().gamma_multiply(0.6),
                            )
                        };

                        let size = egui::vec2(16.0, 16.0);
                        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
                        ui.painter().circle(rect.center(), 7.0, color, stroke);

                        let resp = resp.on_hover_text(name);
                        if resp.clicked() {
                            self.new_tab_group_state.selected_color = rgba;
                        }
                    }
                });

                ui.add_space(12.0);

                ui.horizontal(|ui| {
                    if ui.button("Create").clicked() {
                        create = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });

                if response.lost_focus() && context.input(|i| i.key_pressed(egui::Key::Enter)) {
                    create = true;
                }
            });

        if context.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            cancel = true;
        }

        if create {
            let name = self.new_tab_group_state.name.trim().to_string();
            if !name.is_empty() {
                if !self.tab_groups.iter().any(|g| g.name == name) {
                    self.tab_groups.push(TabGroup {
                        name: name.clone(),
                        color_rgba: self.new_tab_group_state.selected_color,
                    });
                }
                if let Some(path) = &self.new_tab_group_state.target_file {
                    self.tab_to_group.insert(path.clone(), name);
                }
            }
            self.new_tab_group_state.open = false;
        } else if cancel {
            self.new_tab_group_state.open = false;
        }
    }

    fn navigate_to_workspace_symbol(&mut self, idx: usize) {
        let Some(sym) = self.workspace_symbol.results.get(idx).cloned() else {
            return;
        };
        self.workspace_symbol.open = false;
        if let Err(e) = self.open_file(sym.path.clone()) {
            self.error_message = Some(format!("Could not open {}: {e}", sym.path.display()));
            return;
        }
        if let Some(buffer) = self.buffers.get_mut(&sym.path) {
            let cursor = buffer.lsp_position_to_cursor(LspPosition::new(sym.line, sym.col));
            buffer.set_cursor(cursor);
        }
        if let Some(state) = self.editor_states.get_mut(&sym.path) {
            state.request_scroll_to_cursor();
            state.request_focus();
        }
    }

    // ─── Signature help ───────────────────────────────────────────────────────

    /// Request signature help at the current cursor position.
    fn request_signature_help_at_cursor(&mut self) {
        let Some(path) = self.active.clone() else {
            return;
        };
        let Some(root) = self.workspace_root_for_path(&path) else {
            return;
        };
        if !is_lsp_path(&self.settings, Some(&root), &path) {
            return;
        }
        let Some(buffer) = self.buffers.get(&path) else {
            return;
        };
        let cursor = buffer.cursor();
        // Don't re-request if nothing moved.
        if self.signature_help.last_request_path.as_deref() == Some(&path)
            && self.signature_help.last_request_cursor == Some(cursor)
        {
            return;
        }
        let lsp_pos = buffer.cursor_lsp_position();
        if !self.ensure_lsp_document_synced(&path) {
            return;
        }
        let id = self.next_ui_correlation_id();
        self.lsp_pending.insert(id, LspPendingKind::SignatureHelp);
        self.signature_help.pending_id = Some(id);
        self.signature_help.last_request_path = Some(path.clone());
        self.signature_help.last_request_cursor = Some(cursor);
        let sent = self.lsp_manager.request_signature_help(
            &path,
            lsp_pos.line,
            lsp_pos.utf16_col,
            id,
            &self.settings,
            &root,
        );
        if !sent {
            self.lsp_pending.remove(&id);
            self.signature_help.pending_id = None;
        }
    }

    /// Render the signature help popup above/below the cursor if active.
    fn show_signature_help_popup(&self, context: &egui::Context) {
        let Some(sig) = &self.signature_help.active else {
            return;
        };
        // Show a small non-interactive tooltip near the cursor area.
        egui::Window::new("signature_help_popup_window")
            .id(egui::Id::new("sig_help_popup"))
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -40.0])
            .show(context, |ui| {
                ui.horizontal_wrapped(|ui| {
                    // Render the signature label, highlighting the active parameter.
                    if let Some(active_param) = sig.active_parameter {
                        if let Some(param) = sig.parameters.get(active_param) {
                            // Find the parameter in the label and highlight it.
                            if let Some(start) = sig.label.find(&param.label) {
                                let end = start + param.label.len();
                                ui.label(&sig.label[..start]);
                                ui.label(
                                    egui::RichText::new(&sig.label[start..end])
                                        .color(self.active_palette.semantic.hover_link),
                                );
                                ui.label(&sig.label[end..]);
                            } else {
                                ui.label(&sig.label);
                            }
                        } else {
                            ui.label(&sig.label);
                        }
                    } else {
                        ui.label(&sig.label);
                    }
                });
                if let Some(doc) = &sig.documentation {
                    if !doc.is_empty() {
                        ui.separator();
                        ui.label(
                            egui::RichText::new(doc.as_str())
                                .color(self.active_palette.semantic.muted_text),
                        );
                    }
                }
            });
    }

    // ─── Code actions UI ─────────────────────────────────────────────────────

    /// Request code actions at the current cursor line.
    fn request_code_actions_at_cursor(&mut self) {
        let Some(path) = self.active.clone() else {
            return;
        };
        let Some(root) = self.workspace_root_for_path(&path) else {
            return;
        };
        if !is_lsp_path(&self.settings, Some(&root), &path) {
            return;
        }
        let Some(buffer) = self.buffers.get(&path) else {
            return;
        };
        let cursor = buffer.cursor();
        let lsp_pos = buffer.position_lsp_position(cursor);
        let range = (
            lsp_pos.line,
            lsp_pos.utf16_col,
            lsp_pos.line,
            lsp_pos.utf16_col,
        );
        if !self.ensure_lsp_document_synced(&path) {
            return;
        }
        let diagnostics = self
            .lsp_manager
            .diagnostics_for(&path)
            .map(|diags| {
                diags
                    .iter()
                    .filter(|d| d.line_start <= range.2 && d.line_end >= range.0)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let id = self.next_ui_correlation_id();
        self.lsp_pending.insert(id, LspPendingKind::CodeAction);
        self.code_action.pending_id = Some(id);
        self.code_action.request_path = Some(path.clone());
        self.code_action.actions.clear();
        self.code_action.open = false;
        let sent = self.lsp_manager.request_code_action(
            &path,
            range,
            diagnostics,
            id,
            &self.settings,
            &root,
        );
        if !sent {
            self.lsp_pending.remove(&id);
            self.code_action.pending_id = None;
        }
    }

    /// Show the code action picker modal.
    fn show_code_action_picker(&mut self, context: &egui::Context) {
        if !self.code_action.open {
            return;
        }

        let mut apply_idx: Option<usize> = None;
        let mut cancel = false;

        egui::Window::new("Code Actions")
            .id(egui::Id::new("code_action_picker"))
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(context, |ui| {
                let len = self.code_action.actions.len();
                if len == 0 {
                    ui.label("No code actions available here.");
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .show(ui, |ui| {
                            for (i, action) in self.code_action.actions.iter().enumerate() {
                                let is_preferred = action.is_preferred;
                                let label = if is_preferred {
                                    format!("⭐ {}", action.title)
                                } else {
                                    action.title.clone()
                                };
                                let is_sel = i == self.code_action.selected;
                                if ui.selectable_label(is_sel, label).clicked() {
                                    apply_idx = Some(i);
                                }
                            }
                        });
                    // Keyboard navigation.
                    context.input_mut(|inp| {
                        if inp.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                            self.code_action.selected = (self.code_action.selected + 1) % len;
                        }
                        if inp.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                            self.code_action.selected =
                                self.code_action.selected.checked_sub(1).unwrap_or(len - 1);
                        }
                    });
                    if context.input(|i| i.key_pressed(egui::Key::Enter)) {
                        apply_idx = Some(self.code_action.selected);
                    }
                }
                ui.separator();
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });

        if context.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            cancel = true;
        }

        if let Some(idx) = apply_idx {
            self.apply_code_action(idx);
            self.code_action.open = false;
        } else if cancel {
            self.code_action.open = false;
        }
    }

    /// Apply a code action by index: collect its edits and apply them to all affected buffers.
    fn apply_code_action(&mut self, idx: usize) {
        let Some(action) = self.code_action.actions.get(idx).cloned() else {
            return;
        };
        for file_edit in &action.edits {
            // Open the file if it isn't already open.
            if !self.buffers.contains_key(&file_edit.path) {
                if let Err(e) = self.open_file(file_edit.path.clone()) {
                    self.error_message = Some(format!(
                        "Could not open {} for code action: {e}",
                        file_edit.path.display()
                    ));
                    continue;
                }
            }
            // Convert FileEdit into format edits and apply.
            let format_edits: Vec<crate::lsp::types::TextEdit> = file_edit
                .edits
                .iter()
                .map(|e| crate::lsp::types::TextEdit {
                    line_start: e.line_start,
                    col_start: e.col_start,
                    line_end: e.line_end,
                    col_end: e.col_end,
                    new_text: e.new_text.clone(),
                })
                .collect();
            self.apply_format_edits(&file_edit.path, format_edits);
        }
        self.sync_lsp_changes();
    }

    fn handle_shortcuts(&mut self, context: &egui::Context) {
        if self.has_modal() {
            return;
        }

        let mut toggle_terminal = false;
        let mut spawn_terminal = false;
        let mut split_h = false;
        let split_v = false;
        let mut close_focused_pane = false;
        let mut focus_next = false;
        let mut focus_prev = false;
        let mut format_document = false;
        let mut goto_line = false;
        let mut workspace_symbol = false;
        let mut show_recent_files = false;
        let mut trigger_code_action = false;
        let mut select_theme = false;
        let mut toggle_vim = false;
        let mut toggle_assistant = false;
        let mut zoom_in = false;
        let mut zoom_out = false;
        let mut zoom_reset = false;
        // Keyboard navigation
        let mut tab_pressed = false;
        let mut shift_tab_pressed = false;
        let mut escape_pressed = false;
        let mut focus_sidebar = false;
        let mut focus_editor = false;
        let mut focus_terminal = false;
        let mut cycle_tab_right = false;
        let mut cycle_tab_left = false;
        // Accumulated Ctrl+wheel delta this frame (>0 = zoom in, <0 = zoom out).
        let mut zoom_scroll: f32 = 0.0;

        context.input_mut(|input| {
            let ctrl = Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            };
            let ctrl_alt = Modifiers {
                ctrl: true,
                alt: true,
                ..Modifiers::NONE
            };
            let ctrl_shift = Modifiers {
                ctrl: true,
                shift: true,
                ..Modifiers::NONE
            };
            
            // Tab / Shift+Tab for focus navigation
            if input.consume_key(Modifiers::NONE, Key::Tab) {
                tab_pressed = true;
            }
            if input.consume_key(Modifiers::SHIFT, Key::Tab) {
                shift_tab_pressed = true;
            }
            if self.focus_target == FocusTarget::TabBar {
                if input.consume_key(Modifiers::NONE, Key::ArrowRight) {
                    cycle_tab_right = true;
                }
                if input.consume_key(Modifiers::NONE, Key::ArrowLeft) {
                    cycle_tab_left = true;
                }
            }
            // Escape to close menus and return to editor
            if input.consume_key(Modifiers::NONE, Key::Escape) {
                escape_pressed = true;
            }
            // Ctrl+1/2/3 for direct focus
            if input.consume_key(ctrl, Key::Num1) {
                focus_sidebar = true;
            }
            if input.consume_key(ctrl, Key::Num2) {
                focus_editor = true;
            }
            if input.consume_key(ctrl, Key::Num3) {
                focus_terminal = true;
            }
            if input.consume_key(ctrl, Key::Backslash) {
                split_h = true;
            }
            // Ctrl+-  → zoom out
            if input.consume_key(ctrl, Key::Minus) {
                zoom_out = true;
            }
            // Ctrl+= / Ctrl++ (and their Shift variants) → zoom in
            if input.consume_key(ctrl, Key::Equals)
                || input.consume_key(ctrl, Key::Plus)
                || input.consume_key(ctrl_shift, Key::Equals)
                || input.consume_key(ctrl_shift, Key::Plus)
            {
                zoom_in = true;
            }
            // Ctrl+0 → reset zoom to 100%
            if input.consume_key(ctrl, Key::Num0) {
                zoom_reset = true;
            }
            // Ctrl+scroll up/down → zoom in/out. egui folds Ctrl+wheel into the
            // zoom gesture (`zoom_delta`) instead of the scroll delta, so read
            // that here: >1.0 means zoom in, <1.0 means zoom out.
            let gesture_zoom = input.zoom_delta();
            if gesture_zoom > 1.0 {
                zoom_scroll = 1.0;
            } else if gesture_zoom < 1.0 {
                zoom_scroll = -1.0;
            }
            if input.consume_key(ctrl, Key::W) {
                close_focused_pane = true;
            }
            if input.consume_key(ctrl_alt, Key::ArrowRight) {
                focus_next = true;
            }
            if input.consume_key(ctrl_alt, Key::ArrowLeft) {
                focus_prev = true;
            }
            if input.consume_key(ctrl, Key::Backtick) {
                toggle_terminal = true;
            }
            if input.consume_key(ctrl_shift, Key::Num5) {
                spawn_terminal = true;
            }
            if input.consume_key(ctrl_shift, Key::I) {
                format_document = true;
            }
            if input.consume_key(ctrl, Key::G) {
                goto_line = true;
            }
            if input.consume_key(ctrl, Key::T) {
                workspace_symbol = true;
            }
            if input.consume_key(ctrl, Key::E) {
                show_recent_files = true;
            }
            // Ctrl+. — code actions (IDE standard)
            if input.consume_key(ctrl, Key::Period) {
                trigger_code_action = true;
            }
            // Ctrl+Alt+T — theme picker (Zed-style live preview)
            if input.consume_key(ctrl_alt, Key::T) {
                select_theme = true;
            }
            // Ctrl+Alt+V — toggle vim mode
            if input.consume_key(ctrl_alt, Key::V) {
                toggle_vim = true;
            }
            // Ctrl+Alt+A — toggle the AI assistant panel
            if input.consume_key(ctrl_alt, Key::A) {
                toggle_assistant = true;
            }
        });

        if toggle_terminal {
            self.execute_command(CommandId::ToggleTerminal, context);
        }
        if spawn_terminal {
            self.execute_command(CommandId::NewTerminal, context);
        }
        if split_h {
            self.execute_command(CommandId::SplitEditorRight, context);
        }
        if split_v {
            self.execute_command(CommandId::SplitEditorDown, context);
        }
        if close_focused_pane {
            if let Some(path) = self.active_file().cloned() {
                self.pane_actions.push(PaneAction::CloseTab {
                    pane: self.focus.active_pane,
                    path,
                });
            } else {
                self.pane_actions.push(PaneAction::ClosePane {
                    pane: self.focus.active_pane,
                });
            }
        }
        if focus_next {
            self.execute_command(CommandId::FocusNextGroup, context);
        }
        if focus_prev {
            self.execute_command(CommandId::FocusPreviousGroup, context);
        }
        if format_document {
            if let Some(path) = self.active.clone() {
                self.send_format_request(path, false /* not for_save */);
            } else {
                self.error_message = Some("LSP not connected — cannot format".to_owned());
            }
        }
        if goto_line {
            self.execute_command(CommandId::GoToLine, context);
        }
        if select_theme {
            self.execute_command(CommandId::SelectTheme, context);
        }
        if toggle_vim {
            self.execute_command(CommandId::ToggleVimMode, context);
        }
        if toggle_assistant {
            self.execute_command(CommandId::ToggleAssistant, context);
        }
        if workspace_symbol {
            self.execute_command(CommandId::GoToSymbol, context);
        }
        if show_recent_files {
            self.recent_files_state.open = !self.recent_files_state.open;
            self.recent_files_state.query = String::new();
            self.recent_files_state.selected = 0;
            if self.recent_files_state.open {
                self.on_modal_opened();
            }
        }
        if trigger_code_action {
            self.request_code_actions_at_cursor();
        }
        // Keyboard navigation
        if cycle_tab_right {
            self.cycle_tab(1);
        }
        if cycle_tab_left {
            self.cycle_tab(-1);
        }
        if tab_pressed {
            self.focus_target = self.focus_target.next();
            self.request_focus_for_target(context);
        }
        if shift_tab_pressed {
            self.focus_target = self.focus_target.prev();
            self.request_focus_for_target(context);
        }
        if escape_pressed {
            // Close any open dropdowns/menus and return focus to editor
            self.dismiss_completion();
            self.dismiss_lsp_hover();
            self.launcher.dismiss();
            self.focus_target = FocusTarget::Editor;
            self.request_focus_for_target(context);
        }
        if focus_sidebar {
            self.focus_target = FocusTarget::Sidebar;
            self.request_focus_for_target(context);
            if self.show_tree {
                self.show_git_panel = false;
            }
        }
        if focus_editor {
            self.focus_target = FocusTarget::Editor;
            self.request_focus_for_target(context);
        }
        if focus_terminal {
            self.focus_target = FocusTarget::Terminal;
            self.request_focus_for_target(context);
            self.execute_command(CommandId::ToggleTerminal, context);
        }
        if zoom_in || zoom_out || zoom_reset || zoom_scroll != 0.0 {
            // UI scale bounds mirror the Settings slider / validation (0.5..=3.0).
            const ZOOM_MIN: f32 = 0.5;
            const ZOOM_MAX: f32 = 3.0;
            const ZOOM_STEP: f32 = 0.1;

            let mut scale = self.settings.appearance.ui_scale;
            if zoom_reset {
                scale = 1.0;
            } else {
                if zoom_in {
                    scale += ZOOM_STEP;
                }
                if zoom_out {
                    scale -= ZOOM_STEP;
                }
                if zoom_scroll > 0.0 {
                    scale += ZOOM_STEP;
                } else if zoom_scroll < 0.0 {
                    scale -= ZOOM_STEP;
                }
            }

            // Round to a clean step so repeated zooming doesn't drift (e.g. 1.0999).
            let scale = (scale * 10.0).round() / 10.0;
            let scale = scale.clamp(ZOOM_MIN, ZOOM_MAX);

            if (scale - self.settings.appearance.ui_scale).abs() > f32::EPSILON {
                self.settings.appearance.ui_scale = scale;
                context.set_pixels_per_point(scale);
            }
        }

        let ctrl_shift = Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::NONE
        };
        let command_shift = Modifiers {
            command: true,
            shift: true,
            ..Modifiers::NONE
        };

        // Ctrl+Shift+B — run build task
        let run_build = context.input_mut(|input| {
            input.consume_key(
                Modifiers { ctrl: true, shift: true, ..Modifiers::NONE },
                Key::B,
            )
        });
        if run_build && !self.has_modal() {
            let task_to_run: Option<String> = if self.task_panel.tasks.contains_key("build") {
                Some("build".to_string())
            } else {
                self.task_panel.tasks.keys().next().map(|s| s.clone())
            };
            if let Some(name) = task_to_run {
                self.run_task(&name);
            }
        }

        // F3 / Shift+F3 — cycle through search results
        let (f3_next, f3_prev) = context.input_mut(|input| {
            let next = input.consume_key(Modifiers::NONE, Key::F3);
            let prev = input.consume_key(
                Modifiers {
                    shift: true,
                    ..Modifiers::NONE
                },
                Key::F3,
            );
            (next, prev)
        });
        if f3_next && self.search_state.visible {
            let count = match self.search_state.query.scope {
                SearchScope::File => self.search_state.file_matches.len(),
                SearchScope::Project => self.search_state.project_matches.len(),
            };
            if count > 0 {
                self.search_state.next_match();
                self.jump_to_active_search_match();
            }
        }
        if f3_prev && self.search_state.visible {
            let count = match self.search_state.query.scope {
                SearchScope::File => self.search_state.file_matches.len(),
                SearchScope::Project => self.search_state.project_matches.len(),
            };
            if count > 0 {
                self.search_state.prev_match();
                self.jump_to_active_search_match();
            }
        }

        let action = context.input_mut(|input| {
            // Ctrl+Shift+F → open project-scope search
            if input.consume_key(ctrl_shift, Key::F) {
                return Some(CommandId::FindInFile);
            }
            if input.consume_key(ctrl_shift, Key::D) {
                return Some(CommandId::ToggleProblems);
            }
            if input.consume_key(ctrl_shift, Key::H) {
                return Some(CommandId::ToggleCallHierarchy);
            }
            if input.consume_key(ctrl_shift, Key::T) {
                return Some(CommandId::ToggleTypeHierarchy);
            }
            if input.consume_key(command_shift, Key::P) {
                Some(CommandId::ShowCommandPalette)
            } else if input.consume_key(command_shift, Key::Tab) {
                Some(CommandId::PreviousTab)
            } else if input.consume_key(command_shift, Key::M) {
                Some(CommandId::ToggleProblems)
            } else if input.consume_key(Modifiers::COMMAND, Key::P) {
                Some(CommandId::QuickOpen)
            } else if input.consume_key(Modifiers::COMMAND, Key::Tab) {
                Some(CommandId::NextTab)
            } else if input.consume_key(Modifiers::COMMAND, Key::W) {
                Some(CommandId::CloseTab)
            } else if input.consume_key(Modifiers::COMMAND, Key::Backslash) {
                Some(CommandId::ToggleTree)
            } else if input.consume_key(Modifiers::COMMAND, Key::O) {
                Some(CommandId::OpenFile)
            } else if input.consume_key(Modifiers::COMMAND, Key::S) {
                Some(CommandId::Save)
            } else if input.consume_key(Modifiers::COMMAND, Key::F) {
                Some(CommandId::FindInFile)
            } else if input.consume_key(Modifiers::COMMAND, Key::H) {
                Some(CommandId::ReplaceInFile)
            } else if input.consume_key(ctrl_shift, Key::G) {
                Some(CommandId::ToggleGitPanel)
            } else if input.consume_key(ctrl_shift, Key::R) {
                Some(CommandId::ReloadPlugins)
            } else if input.consume_key(ctrl_shift, Key::O) {
                Some(CommandId::ToggleOutline)
            } else if input.consume_key(ctrl_shift, Key::B) {
                Some(CommandId::GitToggleBlame)
            } else if input.consume_key(ctrl_shift, Key::Backslash) {
                Some(CommandId::ToggleMinimap)
            // ── Git shortcuts (Alt+Shift) ─────────────────────────────────────
            } else if input.consume_key(Modifiers::ALT | Modifiers::SHIFT, Key::F) {
                Some(CommandId::GitFetch)
            } else if input.consume_key(Modifiers::ALT | Modifiers::SHIFT, Key::L) {
                Some(CommandId::GitPull)
            } else if input.consume_key(Modifiers::ALT | Modifiers::SHIFT, Key::U) {
                Some(CommandId::GitPush)
            } else if input.consume_key(Modifiers::ALT | Modifiers::SHIFT, Key::H) {
                Some(CommandId::GitShowLog)
            } else if input.consume_key(Modifiers::ALT | Modifiers::SHIFT, Key::S) {
                Some(CommandId::GitStashSave)
            } else if input.consume_key(Modifiers::ALT | Modifiers::SHIFT, Key::P) {
                Some(CommandId::GitStashPop)
            } else {
                None
            }
        });

        match action {
            Some(CommandId::ShowCommandPalette) => self.open_command_palette(),
            Some(CommandId::QuickOpen) => {
                self.focus_target = FocusTarget::SearchBar;
                self.request_focus_for_target(context);
                self.open_quick_open(context);
            }
            Some(command) => self.execute_command(command, context),
            None => {}
        }
    }

    fn show_status_bar(&mut self, context: &egui::Context) {
        let counts = self.bottom_panel_diagnostic_counts();
        let palette = self.active_palette.semantic;
        let terminal_active =
            self.show_bottom_panel && self.bottom_panel_tab == BottomPanelTab::Terminal;
        let problems_active =
            self.show_bottom_panel && self.bottom_panel_tab == BottomPanelTab::Problems;

        let mut toggle_terminal = false;
        let mut toggle_problems = false;

        egui::TopBottomPanel::bottom("status")
            .min_height(24.0)
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    // Vim mode indicator (Zed shows the current mode leftmost).
                    if self.settings.editor.vim_mode {
                        let mode = self
                            .active
                            .as_ref()
                            .and_then(|path| self.buffers.get(path))
                            .map(|buffer| buffer.vim.mode)
                            .unwrap_or(crate::vim::VimMode::Normal);
                        let (label, color) = match mode {
                            crate::vim::VimMode::Normal => ("NORMAL", palette.accent),
                            crate::vim::VimMode::Insert => ("INSERT", palette.success),
                            crate::vim::VimMode::Visual => ("VISUAL", palette.warning),
                            crate::vim::VimMode::VisualLine => ("V-LINE", palette.warning),
                            crate::vim::VimMode::Command => (":", palette.primary_text),
                            crate::vim::VimMode::Search => ("/", palette.primary_text),
                        };
                        ui.label(
                            RichText::new(label)
                                .strong()
                                .size(11.0)
                                .color(color),
                        );
                    }
                    let path = self
                        .active
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "No file open".to_owned());
                    ui.label(path);

                    if let Some(error) = &self.error_message {
                        ui.colored_label(self.active_palette.semantic.error, RichText::new(error));
                    }
                    // Plugin notifications (most-recent one, auto-expiring)
                    if let Some(notif) = self.plugin_system.notifications.last() {
                        let color = match notif.level {
                            NotifyLevel::Error => self.active_palette.semantic.error,
                            NotifyLevel::Warning => self.active_palette.semantic.warning,
                            NotifyLevel::Info => self.active_palette.semantic.muted_text,
                        };
                        ui.colored_label(
                            color,
                            RichText::new(format!("[{}] {}", notif.plugin_name, notif.message)),
                        );
                    }
                    for warning in self.lsp_warnings.values() {
                        ui.colored_label(
                            self.active_palette.semantic.warning,
                            RichText::new(warning),
                        );
                    }
                    if let Some(warning) = &self.config_warning {
                        ui.colored_label(
                            self.active_palette.semantic.warning,
                            RichText::new(warning),
                        );
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let term_resp = ui.selectable_label(
                            terminal_active,
                            RichText::new("Terminal").monospace(),
                        );
                        let term_resp = crate::screen_reader::label_element(
                            ui,
                            term_resp,
                            "Toggle Terminal (Ctrl+`)",
                            "Toggle terminal panel",
                        );
                        if term_resp.clicked() {
                            toggle_terminal = true;
                        }

                        // ── Memory indicator (RSS, sampled every 2s) ─────────────
                        if self.memory_rss > 0 {
                            let rss = self.memory_rss;
                            let (mem_text, mem_color) = {
                                let mb = rss as f64 / (1024.0 * 1024.0);
                                let text = crate::perf::memory::format_bytes(rss);
                                let color = if mb > 500.0 {
                                    Color32::from_rgb(255, 80, 80)
                                } else if mb > 200.0 {
                                    Color32::from_rgb(255, 200, 50)
                                } else {
                                    self.active_palette.semantic.muted_text
                                };
                                (text, color)
                            };
                            let mem_resp = ui
                                .colored_label(mem_color, RichText::new(&mem_text).monospace().size(11.0))
                                .on_hover_ui(|ui| {
                                    ui.label(egui::RichText::new("IDE Memory Usage").strong());
                                    ui.label(format!("RSS: {}", crate::perf::memory::format_bytes(rss)));
                                    ui.separator();
                                    ui.label(format!(
                                        "Last sampled: {:.1}s ago",
                                        self.memory_last_poll.elapsed().as_secs_f64()
                                    ));
                                    if let Some(data) = &self.startup_data {
                                        ui.separator();
                                        ui.label(format!(
                                            "Startup: {:.2}s",
                                            data.total_duration.as_secs_f64()
                                        ));
                                    }
                                });
                            if mem_resp.clicked() {
                                self.startup_breakdown.open_panel();
                            }
                            ui.separator();
                        }

                        if counts.errors > 0 {
                            let label =
                                RichText::new(format!("✖ {}", counts.errors)).color(palette.error);
                            if ui
                                .selectable_label(problems_active, label)
                                .on_hover_text("Show Problems (Ctrl+Shift+M)")
                                .clicked()
                            {
                                toggle_problems = true;
                            }
                        } else if counts.warnings > 0 {
                            let label = RichText::new(format!("⚠ {}", counts.warnings))
                                .color(palette.warning);
                            if ui
                                .selectable_label(problems_active, label)
                                .on_hover_text("Show Problems (Ctrl+Shift+M)")
                                .clicked()
                            {
                                toggle_problems = true;
                            }
                        } else if ui
                            .selectable_label(problems_active, "Problems")
                            .on_hover_text("Show Problems (Ctrl+Shift+M)")
                            .clicked()
                        {
                            toggle_problems = true;
                        }

                        if let Some((path, buffer)) = self
                            .active
                            .as_ref()
                            .and_then(|path| self.buffers.get(path).map(|buffer| (path, buffer)))
                        {
                            let cursor = buffer.cursor();
                            ui.label(format!("Ln {}, Col {}", cursor.line + 1, cursor.col + 1));
                            if buffer.cursors.len() > 1 {
                                ui.separator();
                                ui.label(format!("{} cursors", buffer.cursors.len()));
                            }
                            if buffer.occurrence_limit_reached {
                                ui.separator();
                                ui.label("500+ matches — showing first 500");
                            }
                            if buffer.is_modified() {
                                ui.separator();
                                ui.label("● Modified");
                            }
                            ui.separator();
                            ui.label(language_label(path));
                        }
                        if let Some(git) = &self.git {
                            ui.separator();
                            ui.label(format!("⎇ {}", git.branch));
                        }

                        // EditorConfig status
                        if self.active.is_some() && (self.editorconfig.indent_style.is_some() || self.editorconfig.end_of_line.is_some()) {
                            ui.separator();
                            let label = self.editorconfig.status_label();
                            if ui.small_button(&label).on_hover_text("EditorConfig settings (click to copy)").clicked() {
                                // TODO: open editorconfig popup
                            }
                        }

                        // Trust badge (only when a workspace root is open)
                        if let (Some(trust_store), Some(ws_root)) = (
                            self.trust_store.as_ref(),
                            self.workspace.roots().first(),
                        ) {
                            ui.separator();
                            let trust_state = trust_store.state(ws_root);
                            if crate::trust_ui::show_trust_badge(ui, trust_state, palette) {
                                self.trust_management_open = !self.trust_management_open;
                            }
                        }

                        // Progress indicator (Feature 9)
                        let active_progs = self.lsp_manager.active_progresses();
                        if !active_progs.is_empty() {
                            if let Some((_, prog)) = active_progs.iter().next() {
                                ui.separator();
                                let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                                let frame_idx = (ui.input(|i| i.time * 10.0) as usize) % frames.len();
                                let spinner = frames[frame_idx];
                                ui.ctx().request_repaint();

                                let msg = if let Some(message) = &prog.message {
                                    format!("rust-analyzer: {message}")
                                } else if let Some(pct) = prog.percentage {
                                    format!("rust-analyzer: {} {pct}%", prog.title)
                                } else {
                                    format!("rust-analyzer: {}", prog.title)
                                };
                                ui.label(format!("{spinner} {msg}"));
                            }
                        }
                    });
                });
            });

        if toggle_terminal {
            self.toggle_terminal_panel();
        }
        if toggle_problems {
            self.toggle_problems_panel();
        }
    }

    fn show_bottom_panel(&mut self, context: &egui::Context) {
        if !self.show_bottom_panel {
            return;
        }

        if self.bottom_panel_tab == BottomPanelTab::Terminal {
            self.ensure_terminal();
        }

        #[cfg(test)]
        let diagnostics = if let Some(ref client) = self.lsp {
            client.diagnostics().clone()
        } else {
            self.lsp_manager.all_diagnostics()
        };
        #[cfg(not(test))]
        let diagnostics = self.lsp_manager.all_diagnostics();

        let problem_rows = problems_panel::flatten_diagnostics(&diagnostics);
        let problem_counts = problems_panel::count_diagnostics(&problem_rows);
        let mut problems_action = None;
        let show_errors = self.show_problems_errors;
        let show_warnings = self.show_problems_warnings;

        let panel_response = egui::TopBottomPanel::bottom("bottom_panel")
            .resizable(true)
            .min_height(120.0)
            .max_height(600.0)
            .default_height(self.bottom_panel_height)
            .show(context, |ui| {
                ui.set_enabled(!self.has_modal());
                let full_rect = ui.max_rect();
                crate::keyboard_nav::draw_focus_outline(ui, egui::Id::new("terminal"), full_rect);

                let mut spawn_shell: Option<crate::terminal::ShellKind> = None;
                let mut kill_active_terminal = false;
                let mut toggle_split = false;

                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 28.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.spacing_mut().button_padding = egui::vec2(8.0, 2.0);
                    let tab_palette = self.active_palette.semantic;
                    // Left: VS Code style category tabs.
                    for tab in BottomPanelTab::ALL {
                        let active = self.bottom_panel_tab == tab;
                        let label = if tab == BottomPanelTab::Problems && problem_counts.total > 0 {
                            format!("Problems {}", problem_counts.total)
                        } else {
                            tab.label().to_owned()
                        };
                        let a11y_label = match tab {
                            BottomPanelTab::Terminal => "Toggle terminal panel",
                            BottomPanelTab::Problems => "Toggle problems panel",
                            BottomPanelTab::Search => "Toggle search panel",
                            BottomPanelTab::Output => "Toggle output panel",
                            BottomPanelTab::DebugConsole => "Toggle debug console",
                            BottomPanelTab::CallHierarchy => "Toggle call hierarchy panel",
                            BottomPanelTab::TypeHierarchy => "Toggle type hierarchy panel",
                            BottomPanelTab::Ports => "Toggle ports panel",
                            BottomPanelTab::Profiler => "Toggle profiler panel",
                        };
                        if paint_panel_tab(ui, &label, active, tab_palette)
                            .on_hover_text(a11y_label)
                            .clicked()
                        {
                            self.bottom_panel_tab = tab;
                            self.show_problems = tab == BottomPanelTab::Problems;
                            if tab == BottomPanelTab::Terminal {
                                self.ensure_terminal();
                            }
                        }
                    }

                    // Right: contextual toolbar for the active tab.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let palette = self.active_palette.semantic;
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if paint_term_tool(ui, TermToolIcon::Close, palette) {
                            problems_action = Some(problems_panel::PanelAction::Close);
                        }

                        match self.bottom_panel_tab {
                            BottomPanelTab::Terminal => {
                                // Buttons are added right-to-left, so the visual
                                // order ends up: profile ⌄ | + | split | kill.
                                if !self.term_sessions.is_empty()
                                    && paint_term_tool(ui, TermToolIcon::Kill, palette)
                                {
                                    kill_active_terminal = true;
                                }

                                if paint_term_tool(ui, TermToolIcon::Split, palette) {
                                    toggle_split = true;
                                }

                                if paint_term_tool(ui, TermToolIcon::New, palette) {
                                    spawn_shell = Some(crate::terminal::ShellKind::default_shell());
                                }

                                // Profile picker showing the active shell name.
                                let active_label = self
                                    .term_sessions
                                    .active()
                                    .map(|s| s.pane.title())
                                    .unwrap_or("Terminal");
                                ui.add_space(4.0);
                                ui.menu_button(format!("{} ⌄", active_label), |ui| {
                                    ui.label(RichText::new("Select Default Profile").small());
                                    ui.separator();
                                    for kind in crate::terminal::ShellKind::available() {
                                        if ui.button(kind.label()).clicked() {
                                            spawn_shell = Some(kind);
                                            ui.close_menu();
                                        }
                                    }
                                })
                                .response
                                .on_hover_text("Launch Profile…");
                            }
                            _ => {}
                        }
                    });
                });

                if let Some(shell) = spawn_shell {
                    if !self.trust_allows(crate::workspace::ExecutableCapability::Terminal) {
                        self.error_message = Some(
                            "Terminals require a trusted workspace. Click the trust badge to enable."
                                .to_owned(),
                        );
                    } else {
                        let cwd = self.primary_workspace_root();
                        let env = self.env_editor.enabled_vars();
                        self.term_sessions.create_session(cwd, shell, &env);
                        self.term_split.clamp_indices(self.term_sessions.len());
                        self.bottom_panel_tab = BottomPanelTab::Terminal;
                        self.show_problems = false;
                    }
                }

                if kill_active_terminal {
                    if let Some(i) = self.term_sessions.active_index {
                        self.term_sessions.close_session(i);
                        self.term_split.clamp_indices(self.term_sessions.len());
                    }
                }

                if toggle_split {
                    if self.term_split.is_split {
                        self.term_split.disable_split();
                    } else {
                        self.term_split.enable_split(self.term_sessions.len());
                    }
                }

                ui.separator();

                match self.bottom_panel_tab {
                    BottomPanelTab::Search => {
                        let file_match_count = self.search_state.file_matches.len();
                        let palette = self.active_palette.semantic;
                        let out = crate::search_panel::show_inside(ui, &mut self.search_state, file_match_count, palette);
                        if out.closed {
                            problems_action = Some(problems_panel::PanelAction::Close);
                        }
                        if out.query_changed {
                            self.search_state.recompile();
                            self.search_state.invalidate_file_cache();
                            if self.search_state.query.scope == SearchScope::Project
                                && self.search_state.query.is_non_empty()
                                && self.search_state.compile_error.is_none()
                            {
                                self.start_project_search(ui.ctx().clone());
                            }
                        }
                        if out.next_match {
                            self.search_state.next_match();
                            self.jump_to_active_search_match();
                        }
                        if out.prev_match {
                            self.search_state.prev_match();
                            self.jump_to_active_search_match();
                        }
                        if let Some(idx) = out.project_result_clicked {
                            self.search_state.active_index = Some(idx);
                            self.jump_to_active_search_match();
                        }
                        if out.replace_one {
                            if let Some(path) = self.active.clone() {
                                if let Some(buffer) = self.buffers.get_mut(&path) {
                                    if let Some(m) = self.search_state.active_file_match() {
                                        let replacement = self.search_state.query.replacement.clone();
                                        buffer.apply_byte_replacements(vec![(m.byte_range.clone(), replacement)]).unwrap();
                                        self.ensure_lsp_document_synced(&path);
                                    }
                                }
                            }
                        }
                    }
                    BottomPanelTab::Problems => {
                        if let Some(navigate) = problems_panel::show_problems_panel(
                            ui,
                            &diagnostics,
                            &mut self.problems_panel,
                            self.active_palette.semantic,
                        ) {
                            problems_action = Some(navigate);
                        }
                    }
                    BottomPanelTab::CallHierarchy => {
                        show_hierarchy_panel_ui(ui, self);
                    }
                    BottomPanelTab::TypeHierarchy => {
                        show_hierarchy_panel_ui(ui, self);
                    }
                    BottomPanelTab::Terminal => {
                        // ── Feature 1: Ensure sessions exist ───────────────
                        let env_vars = self.env_editor.enabled_vars();
                        let cwd_ref = self.primary_workspace_root();
                        self.term_sessions.ensure_session(cwd_ref.clone(), &env_vars);

                        // Poll all sessions every frame
                        self.term_sessions.poll_all();

                        // ── Feature 4: open search on Ctrl+F ───────────────
                        if ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::F)) {
                            self.term_search.open();
                        }
                        // ── Feature 5: open history browser on Ctrl+R ──────
                        if ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::R)) {
                            self.term_history.open();
                        }

                        // ── Feature 1: Session tab bar ─────────────────────
                        let palette = self.active_palette.semantic;
                        let tab_action = crate::terminal::session::render_session_tabs(
                            ui,
                            &mut self.term_sessions,
                            &mut self.term_split,
                            palette,
                        );
                        use crate::terminal::session::TabBarAction;
                        match tab_action {
                            Some(TabBarAction::SetActive(i)) => {
                                self.term_sessions.set_active(i);
                                self.term_split.clamp_indices(self.term_sessions.len());
                            }
                            Some(TabBarAction::Close(i)) => {
                                self.term_sessions.close_session(i);
                                self.term_split.clamp_indices(self.term_sessions.len());
                            }
                            Some(TabBarAction::New) => {
                                if !self.trust_allows(crate::workspace::ExecutableCapability::Terminal) {
                                    self.error_message = Some(
                                        "Terminals require a trusted workspace. Click the trust badge to enable."
                                            .to_owned(),
                                    );
                                } else {
                                    let env = self.env_editor.enabled_vars();
                                    self.term_sessions.create_session(
                                        cwd_ref.clone(),
                                        crate::terminal::ShellKind::default_shell(),
                                        &env,
                                    );
                                    let n = self.term_sessions.len();
                                    self.term_split.clamp_indices(n);
                                }
                            }
                            None => {}
                        }

                        // ── Feature 2: Split button in toolbar ─────────────
                        if self.term_sessions.is_empty() {
                            ui.centered_and_justified(|ui| {
                                ui.label("No terminal sessions");
                            });
                        } else if self.term_split.is_split {
                            // ── Feature 2: Side-by-side split view ─────────
                            let _panel_rect = ui.available_rect_before_wrap();
                            let mut left_session_change: Option<usize> = None;
                            let mut right_session_change: Option<usize> = None;

                            ui.columns(2, |cols| {
                                // Left pane session selector
                                let new_left = crate::terminal::split::render_session_selector(
                                    &mut cols[0],
                                    "Left:",
                                    self.term_split.left_session,
                                    &self.term_sessions.sessions,
                                    palette,
                                );
                                if let Some(idx) = new_left {
                                    left_session_change = Some(idx);
                                }

                                // Right pane session selector
                                let new_right = crate::terminal::split::render_session_selector(
                                    &mut cols[1],
                                    "Right:",
                                    self.term_split.right_session,
                                    &self.term_sessions.sessions,
                                    palette,
                                );
                                if let Some(idx) = new_right {
                                    right_session_change = Some(idx);
                                }
                            });

                            if let Some(i) = left_session_change {
                                self.term_split.left_session = i;
                            }
                            if let Some(i) = right_session_change {
                                self.term_split.right_session = i;
                            }

                            // Render two terminal panes side by side
                            let _font_id = egui::FontId::monospace(13.0);
                            let focused = self.term_split.focused_pane;

                            ui.columns(2, |cols| {
                                let col_rect = cols[0].available_rect_before_wrap();
                                render_session_pane(
                                    &mut cols[0],
                                    &mut self.term_sessions,
                                    self.term_split.left_session,
                                    self.settings.appearance.font_ligatures,
                                    &mut self.ligature_renderer,
                                );
                                if cols[0].interact(col_rect, egui::Id::new("left_pane_click"), egui::Sense::click()).clicked() {
                                    self.term_split.focused_pane = crate::terminal::split::SplitFocus::Left;
                                }
                                if focused == crate::terminal::split::SplitFocus::Left {
                                    crate::terminal::split::draw_focus_border(&cols[0], col_rect);
                                }

                                let col_rect2 = cols[1].available_rect_before_wrap();
                                render_session_pane(
                                    &mut cols[1],
                                    &mut self.term_sessions,
                                    self.term_split.right_session,
                                    self.settings.appearance.font_ligatures,
                                    &mut self.ligature_renderer,
                                );
                                if cols[1].interact(col_rect2, egui::Id::new("right_pane_click"), egui::Sense::click()).clicked() {
                                    self.term_split.focused_pane = crate::terminal::split::SplitFocus::Right;
                                }
                                if focused == crate::terminal::split::SplitFocus::Right {
                                    crate::terminal::split::draw_focus_border(&cols[1], col_rect2);
                                }
                            });
                        } else {
                            // ── Single pane ───────────────────────────────
                            let active_idx = self.term_split.left_session.min(
                                self.term_sessions.len().saturating_sub(1),
                            );
                            render_session_pane(
                                ui,
                                &mut self.term_sessions,
                                active_idx,
                                self.settings.appearance.font_ligatures,
                                &mut self.ligature_renderer,
                            );
                        }

                        // ── Feature 4: Search overlay ──────────────────────
                        let panel_rect = ui.clip_rect();
                        if self.term_search.visible {
                            // Build flattened line list for scanning
                            if self.term_search.is_stale() {
                                let all_lines = collect_terminal_lines(&self.term_sessions);
                                self.term_search.rescan(&all_lines);
                            }
                            let out = crate::terminal::search::render_search_bar(
                                ui,
                                &mut self.term_search,
                                panel_rect,
                            );
                            if out.query_changed {
                                let all_lines = collect_terminal_lines(&self.term_sessions);
                                self.term_search.rescan(&all_lines);
                            }
                            if out.next_requested {
                                self.term_search.next_match();
                            }
                            if out.prev_requested {
                                self.term_search.prev_match();
                            }
                            if out.closed {
                                self.term_search.close();
                            }
                        }
                    }
                    BottomPanelTab::Output => {
                        // Show task output if available
                        if self.task_output.is_empty() {
                            ui.centered_and_justified(|ui| {
                                ui.label(
                                    egui::RichText::new("No output yet. Run a task to see output here.")
                                        .color(self.active_palette.semantic.muted_text),
                                );
                            });
                        } else {
                            // Task status badge
                            if let Some(ref h) = self.task_panel.running {
                                let label = format!("Task '{}' — {}", h.task_name, h.status.label());
                                let color = match &h.status {
                                    crate::tasks::TaskStatus::Success => self.active_palette.semantic.success,
                                    crate::tasks::TaskStatus::Failed(_) | crate::tasks::TaskStatus::Error(_) => {
                                        self.active_palette.semantic.error
                                    }
                                    crate::tasks::TaskStatus::Running => self.active_palette.semantic.information,
                                    _ => self.active_palette.semantic.muted_text,
                                };
                                ui.colored_label(color, label);
                                ui.separator();
                            }
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .stick_to_bottom(true)
                                .show(ui, |ui| {
                                    let output = self.task_output.join("\n");
                                    ui.add(
                                        egui::TextEdit::multiline(&mut output.as_str())
                                            .font(egui::TextStyle::Monospace)
                                            .desired_width(f32::INFINITY)
                                            .interactive(false),
                                    );
                                });
                        }
                    }
                    BottomPanelTab::DebugConsole => {
                        ui.centered_and_justified(|ui| {
                            ui.label(
                                RichText::new("Start a debug session to use the Debug Console")
                                    .color(self.active_palette.semantic.muted_text),
                            );
                        });
                    }
                    BottomPanelTab::Ports => {
                        ui.centered_and_justified(|ui| {
                            ui.label(
                                RichText::new("No forwarded ports")
                                    .color(self.active_palette.semantic.muted_text),
                            );
                        });
                    }
                    BottomPanelTab::Profiler => {
                        let palette = self.active_palette.semantic;
                        let root_path = self.primary_workspace_root();
                        let trusted =
                            self.trust_allows(crate::workspace::ExecutableCapability::Profiler);
                        if !trusted {
                            ui.label(
                                RichText::new(
                                    "Profiling requires a trusted workspace. Click the trust badge to enable.",
                                )
                                .color(palette.warning),
                            );
                        }
                        if let Some(jump_path) = crate::profiler::render_profiler_panel(
                            ui,
                            &mut self.profiler_state,
                            root_path.as_ref(),
                            palette,
                            trusted,
                        ) {
                            if let Err(_e) = self.open_file(jump_path) {
                                // Ignore error
                            }
                        }
                    }
                }
            });

        self.bottom_panel_height = panel_response.response.rect.height();
        self.show_problems_errors = show_errors;
        self.show_problems_warnings = show_warnings;

        match problems_action {
            Some(problems_panel::PanelAction::Close) => {
                self.close_bottom_panel();
            }
            Some(problems_panel::PanelAction::NavigateTo { row_index }) => {
                if let Some(row) = problem_rows.get(row_index) {
                    self.navigate_to_diagnostic(row);
                }
            }
            Some(problems_panel::PanelAction::NavigateToDiagnostic { path, line, col }) => {
                if !self.buffers.contains_key(&path) {
                    if let Err(error) = self.open_file(path.clone()) {
                        self.error_message = Some(format!("Could not open {}: {error}", path.display()));
                        return;
                    }
                }
                self.active = Some(path.clone());
                self.reveal_active_tab = true;
                if let Some(buffer) = self.buffers.get_mut(&path) {
                    buffer.set_cursor(buffer.lsp_position_to_cursor(LspPosition::new(line as u32, col as u32)));
                }
                if let Some(state) = self.editor_states.get_mut(&path) {
                    state.request_scroll_to_cursor();
                }
            }
            None => {}
        }
    }

    fn navigate_to_diagnostic(&mut self, row: &problems_panel::DiagnosticRow) {
        let path = row.path.clone();
        if !self.buffers.contains_key(&path) {
            if let Err(error) = self.open_file(path.clone()) {
                self.error_message = Some(format!("Could not open {}: {error}", path.display()));
                return;
            }
        } else {
            self.active = Some(path.clone());
            self.reveal_active_tab = true;
        }

        if let Some(buffer) = self.buffers.get_mut(&path) {
            buffer.set_cursor(buffer.lsp_position_to_cursor(LspPosition::new(row.line, row.col)));
        }

        if let Some(state) = self.editor_states.get_mut(&path) {
            state.request_scroll_to_cursor();
        }

        self.error_message = None;
    }

    /// Render the bottom-docked search panel and process its output.
    ///
    /// Must be called in `show_workspace_panels` BEFORE `show_bottom_panel`
    /// so both panels share the bottom area without overlapping.
    fn show_search_panel(&mut self, context: &egui::Context) {
        // Refresh the file-scope match cache if the panel is visible in
        // file-scope mode.  This mirrors the same logic in `show_editor` for
        // the overlay panel, ensuring highlights stay in sync even when the
        // bottom panel is open instead of the overlay.
        if self.search_state.visible && self.search_state.query.scope == SearchScope::File {
            if let Some(ref path) = self.active.clone() {
                if let Some(buffer) = self.buffers.get(path) {
                    let text = buffer.to_full_string();
                    let rev = buffer.revision();
                    self.search_state
                        .refresh_file_matches(&text, Some(path), rev);
                }
            }
        }

        let file_match_count = self.search_state.file_matches.len();
        let palette = self.active_palette.semantic;

        // `show_bottom_panel` owns the `TopBottomPanel::bottom` registration
        // and returns `BottomPanelOutput` describing what the user did.
        let out = search_panel::show_bottom_panel(
            context,
            &mut self.search_state,
            file_match_count,
            palette,
        );

        // ── Handle output outside the closure ──────────────────────────────

        if out.closed {
            self.search_state.close();
            return;
        }

        if out.query_changed {
            self.search_state.recompile();
            self.search_state.invalidate_file_cache();
            // Restart project search whenever the query or options change.
            if self.search_state.query.scope == SearchScope::Project
                && self.search_state.query.is_non_empty()
                && self.search_state.compile_error.is_none()
            {
                self.start_project_search(context.clone());
            }
        }

        // Navigate to next / previous match.
        if out.next_match || out.prev_match {
            if out.next_match {
                self.search_state.next_match();
            } else {
                self.search_state.prev_match();
            }
            // Jump the editor cursor to the newly-active match.
            self.jump_to_active_search_match();
        }

        // Replace current match.
        if out.replace_one {
            if let Some(ref path) = self.active.clone() {
                self.do_replace_one(path);
            }
        }

        // "Replace All" is handled via the confirmation dialog in
        // `show_replace_all_confirmation`; we only gate it here.
        // `request_replace_confirm` is already called inside show_bottom_panel
        // when the button is clicked.

        // Project result clicked → open file and jump to match.
        if let Some(idx) = out.project_result_clicked {
            self.search_state.active_index = Some(idx);
            if let Some(m) = self.search_state.active_project_match().cloned() {
                let file_path = m.path.clone();
                let byte = m.byte_range.start;
                if !self.buffers.contains_key(&file_path) {
                    if let Err(e) = self.open_file(file_path.clone()) {
                        self.error_message =
                            Some(format!("Could not open {}: {e}", file_path.display()));
                    }
                } else {
                    self.active = Some(file_path.clone());
                    self.reveal_active_tab = true;
                }
                if let Some(buffer) = self.buffers.get_mut(&file_path) {
                    let _ = buffer.set_cursor_to_byte(byte);
                }
                if let Some(st) = self.editor_states.get_mut(&file_path) {
                    st.request_scroll_to_cursor();
                }
            }
        }
    }

    /// Move the editor cursor to the currently active search match and request
    /// a scroll.  Works for both file-scope and project-scope matches.
    fn jump_to_active_search_match(&mut self) {
        let active = self.active.clone();
        match self.search_state.query.scope {
            SearchScope::File => {
                if let Some(ref path) = active {
                    if let Some(m) = self.search_state.active_file_match().cloned() {
                        if let Some(buffer) = self.buffers.get_mut(path) {
                            let _ = buffer.set_cursor_to_byte(m.byte_range.start);
                        }
                        if let Some(st) = self.editor_states.get_mut(path) {
                            st.request_scroll_to_cursor();
                        }
                    }
                }
            }
            SearchScope::Project => {
                if let Some(m) = self.search_state.active_project_match().cloned() {
                    let file_path = m.path.clone();
                    let byte = m.byte_range.start;
                    if !self.buffers.contains_key(&file_path) {
                        if let Err(e) = self.open_file(file_path.clone()) {
                            self.error_message =
                                Some(format!("Could not open {}: {e}", file_path.display()));
                            return;
                        }
                    } else {
                        self.active = Some(file_path.clone());
                        self.reveal_active_tab = true;
                    }
                    if let Some(buffer) = self.buffers.get_mut(&file_path) {
                        let _ = buffer.set_cursor_to_byte(byte);
                    }
                    if let Some(st) = self.editor_states.get_mut(&file_path) {
                        st.request_scroll_to_cursor();
                    }
                }
            }
        }
    }

    fn show_file_tree(&mut self, context: &egui::Context) {
        if !self.show_tree {
            return;
        }

        let active = self.active.clone();
        let mut render_result = Ok(FileTreeAction::None);
        let statuses = self
            .git
            .as_ref()
            .map(|g| g.status_map.clone())
            .unwrap_or_default();
        let modal_open = self.has_modal();

        egui::SidePanel::left("file_tree")
            .default_width(200.0)
            .show(context, |ui| {
                ui.set_enabled(!modal_open);
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.spacing_mut().button_padding = egui::vec2(2.0, 1.0);
                let full_rect = ui.max_rect();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        render_result = self.tree.render(ui, active.as_deref(), &statuses);
                    });
                crate::keyboard_nav::draw_focus_outline(ui, egui::Id::new("file_tree"), full_rect);
            });

        match render_result {
            Ok(FileTreeAction::Open(path)) => {
                if let Err(error) = self.open_file(path.clone()) {
                    self.error_message =
                        Some(format!("Could not open {}: {error}", path.display()));
                } else {
                    self.error_message = None;
                }
            }
            Ok(FileTreeAction::None) => {}
            Err(error) => self.error_message = Some(format!("Could not update file tree: {error}")),
        }
    }

    fn show_git_panel(&mut self, context: &egui::Context) {
        let modal_open = self.has_modal();
        let network_progress = self.network_progress.clone();
        let Some(git) = self.git.as_mut() else { return };
        let commit_msg = &mut self.git_commit_msg;
        let stash_msg = &mut self.git_stash_msg;

        let mut action = GitPanelAction::None;
        egui::SidePanel::left("git_panel")
            .default_width(240.0)
            .show(context, |ui| {
                ui.set_enabled(!modal_open);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        action = crate::git::render_git_panel(ui, git, commit_msg, stash_msg);
                        if let Some(progress) = &network_progress {
                            crate::git::render_network_progress(ui, progress);
                        }
                    });
            });

        self.handle_git_panel_action(action);

        // Render the branch picker modal on top.
        if self.show_branch_picker {
            let (branches, current) = self
                .git
                .as_ref()
                .map(|git| (git.branches(), git.branch.clone()))
                .unwrap_or_default();
            if let Some(selected) = crate::git::render_branch_picker(
                context,
                &branches,
                &current,
                &mut self.branch_query,
            ) {
                self.show_branch_picker = false;
                self.branch_query.clear();
                self.checkout_branch_and_reload(&selected);
            }
        }

        self.show_git_log_modal(context);
        self.show_tag_manager_modal(context);
        self.show_conflict_resolver_modal(context);
    }

    /// Render the commit-log modal and apply its actions.
    fn show_git_log_modal(&mut self, context: &egui::Context) {
        if !self.show_git_log {
            return;
        }
        match crate::git::render_log_viewer(context, &self.git_log_cache) {
            crate::git::LogAction::None => {}
            crate::git::LogAction::Close => self.show_git_log = false,
            crate::git::LogAction::CherryPick(oid) => {
                if let Some(git) = self.git.as_ref() {
                    if let Err(error) = git.cherry_pick(&oid) {
                        self.error_message =
                            Some(format!("Cherry-pick failed: {}", error.message()));
                    }
                }
                if let Some(git) = self.git.as_mut() {
                    git.dirty = true;
                }
                self.reload_open_buffers_from_disk();
                self.show_git_log = false;
            }
            crate::git::LogAction::TagCommit(_oid) => {
                // Open the tag manager so the user can name the tag.
                self.show_git_log = false;
                self.tag_new_name.clear();
                self.tag_new_message.clear();
                self.show_tag_manager = true;
            }
        }
    }

    /// Render the tag-manager modal and apply its actions.
    fn show_tag_manager_modal(&mut self, context: &egui::Context) {
        if !self.show_tag_manager {
            return;
        }
        let tags = self.git.as_ref().map(|git| git.tags()).unwrap_or_default();
        let outcome = crate::git::render_tag_manager(
            context,
            &tags,
            &mut self.tag_new_name,
            &mut self.tag_new_message,
        );
        match outcome {
            crate::git::TagManagerAction::None => {}
            crate::git::TagManagerAction::Close => self.show_tag_manager = false,
            crate::git::TagManagerAction::Create { name, message } => {
                if let Some(git) = self.git.as_ref() {
                    if let Err(error) = git.create_tag(&name, &message) {
                        self.error_message =
                            Some(format!("Create tag failed: {}", error.message()));
                    }
                }
                self.tag_new_name.clear();
                self.tag_new_message.clear();
            }
            crate::git::TagManagerAction::Delete(name) => {
                if let Some(git) = self.git.as_ref() {
                    if let Err(error) = git.delete_tag(&name) {
                        self.error_message =
                            Some(format!("Delete tag failed: {}", error.message()));
                    }
                }
            }
            crate::git::TagManagerAction::Push(name) => {
                if self.network_receiver.is_none() {
                    if let Some(git) = self.git.as_ref() {
                        let root = git.root.clone();
                        let remote = git.default_remote();
                        let (tx, rx) = crossbeam_channel::unbounded();
                        self.network_receiver = Some(rx);
                        self.network_progress = Some(crate::git::NetworkProgress {
                            op: crate::git::NetworkOp::Push,
                            stage: crate::git::NetworkStage::Connecting,
                        });
                        crate::git::tag::spawn_push_tag(root, remote, name, tx);
                    }
                }
            }
        }
    }

    /// Render the conflict-resolver modal and apply its actions.
    fn show_conflict_resolver_modal(&mut self, context: &egui::Context) {
        if !self.show_conflict_resolver {
            return;
        }
        let outcome = crate::git::render_conflict_resolver(
            context,
            &self.conflict_paths,
            self.conflict_selected,
            &self.conflict_sides,
        );
        if outcome.close {
            self.show_conflict_resolver = false;
            return;
        }
        if let Some(index) = outcome.select_path {
            self.conflict_selected = index;
            self.refresh_conflicts();
        }
        if let Some(resolution) = outcome.resolve {
            if let Some(rel) = self
                .conflict_paths
                .get(self.conflict_selected)
                .and_then(|p| p.to_str())
                .map(String::from)
            {
                if let Some(git) = self.git.as_ref() {
                    if let Err(error) = git.resolve_conflict_with_side(&rel, resolution) {
                        self.error_message =
                            Some(format!("Resolve failed: {}", error.message()));
                    }
                }
                if let Some(git) = self.git.as_mut() {
                    git.dirty = true;
                }
                self.reload_open_buffers_from_disk();
                self.refresh_conflicts();
                if self.conflict_paths.is_empty() {
                    self.show_conflict_resolver = false;
                }
            }
        }
    }

    fn update_outline_panel_tracking(&mut self) {
        let active_changed = self.active != self.outline_panel.last_active_file;
        if active_changed {
            self.outline_panel.last_active_file = self.active.clone();
            self.outline_panel.last_cursor_line = None;
            if let Some(ref path) = self.active {
                if !self.outline_panel.nodes.contains_key(path)
                    && self.outline_panel.pending_request.as_ref() != Some(path)
                {
                    self.request_document_symbols(path.clone());
                }
            }
        }

        if let Some(ref path) = self.active {
            if let Some(buf) = self.buffers.get(path) {
                let cursor_line = buf.cursor().line;
                if self.outline_panel.last_cursor_line != Some(cursor_line) {
                    self.outline_panel.last_cursor_line = Some(cursor_line);
                    let mut best_symbol_line = None;
                    if let Some(nodes) = self.outline_panel.nodes.get(path) {
                        let mut flat_nodes = Vec::new();
                        fn flatten<'a>(
                            nodes: &'a [crate::lsp::types::OutlineNode],
                            out: &mut Vec<&'a crate::lsp::types::OutlineNode>,
                        ) {
                            for node in nodes {
                                out.push(node);
                                flatten(&node.children, out);
                            }
                        }
                        flatten(nodes, &mut flat_nodes);
                        for node in flat_nodes {
                            if cursor_line >= node.line && cursor_line <= node.end_line {
                                best_symbol_line = Some(node.line);
                            }
                        }
                    }
                    if self.outline_panel.current_symbol_line != best_symbol_line {
                        self.outline_panel.current_symbol_line = best_symbol_line;
                        self.outline_panel.needs_scroll_to_symbol = true;
                    }
                }
            }
        } else {
            self.outline_panel.current_symbol_line = None;
            self.outline_panel.last_cursor_line = None;
        }
    }

}

fn get_group_color(groups: &[TabGroup], name: &str) -> egui::Color32 {
    if let Some(g) = groups.iter().find(|g| g.name == name) {
        egui::Color32::from_rgba_unmultiplied(
            g.color_rgba[0],
            g.color_rgba[1],
            g.color_rgba[2],
            g.color_rgba[3],
        )
    } else {
        egui::Color32::from_rgb(0, 120, 215)
    }
}

impl BlueIdeApp {
    fn show_tabs(&mut self, context: &egui::Context) {
        if self.buffers.is_empty() {
            return;
        }

        let active = self.active.clone();
        let mut activate = None;
        let mut close = None;
        let mut revealed_active = false;
        let tab_frame = egui::Frame::side_top_panel(&context.style()).inner_margin(egui::Margin {
            left: 0.0,
            right: 8.0,
            top: 0.0,
            bottom: 0.0,
        });

        egui::TopBottomPanel::top("tabs")
            .exact_height(TAB_STRIP_HEIGHT)
            .show_separator_line(false)
            .frame(tab_frame)
            .show(context, |ui| {
                ui.set_enabled(!self.has_modal());
                egui::ScrollArea::horizontal()
                    .id_source("tab_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            let tabs: Vec<(PathBuf, bool)> = self.buffers.iter()
                                .map(|(p, b)| (p.clone(), b.is_modified()))
                                .collect();
                            for (path, is_modified) in tabs {
                                let name = file_name(&path);
                                let title = if is_modified {
                                    format!("● {name}")
                                } else {
                                    name.clone()
                                };
                                let is_active = active.as_ref() == Some(&path);
                                let is_pinned = self.pinned_tabs.contains(&path);
                                let text_color = if is_active {
                                    ui.visuals().strong_text_color()
                                } else {
                                    ui.visuals().weak_text_color()
                                };
                                let mut tab = egui::Frame::none()
                                    .fill(tab_fill(ui.visuals(), is_active))
                                    .rounding(tab_rounding(is_active))
                                    .inner_margin(egui::Margin {
                                        left: 6.0,
                                        right: 3.0,
                                        top: 0.0,
                                        bottom: 0.0,
                                    })
                                    .begin(ui);
                                tab.content_ui.set_min_width(72.0);
                                tab.content_ui.set_min_height(TAB_STRIP_HEIGHT);
                                
                                let close_clicked = tab
                                    .content_ui
                                    .horizontal_centered(|ui| {
                                        ui.spacing_mut().item_spacing.x = 3.0;

                                        // Render Group Badge if grouped
                                        let group_name = self.tab_to_group.get(&path);
                                        if let Some(gname) = group_name {
                                            let color = get_group_color(&self.tab_groups, gname);
                                            let (rect, _) = ui.allocate_exact_size(egui::vec2(4.0, 12.0), egui::Sense::hover());
                                            ui.painter().rect_filled(rect, egui::Rounding::same(1.0), color);
                                            ui.label(
                                                RichText::new(gname)
                                                    .size(9.0)
                                                    .color(color)
                                                    .strong()
                                            );
                                        }

                                        // File-type icon (Zed-style).
                                        let (icon_rect, _) = ui
                                            .allocate_exact_size(
                                                egui::vec2(14.0, 14.0),
                                                egui::Sense::hover(),
                                            );
                                        crate::file_icons::paint(
                                            ui.painter(),
                                            icon_rect,
                                            &name,
                                            ui.visuals().weak_text_color(),
                                        );

                                        ui.label(
                                            RichText::new(truncate_tab_label(&title))
                                                .size(11.0)
                                                .color(text_color),
                                        );

                                        if is_pinned {
                                            ui.label(RichText::new("📌").size(10.0));
                                            false
                                        } else {
                                            let _close_id = ui.id().with("tab_close");
                                            let close_response = ui.add(
                                                egui::Button::new(
                                                    RichText::new("×")
                                                        .size(11.0)
                                                        .color(ui.visuals().weak_text_color()),
                                                )
                                                .frame(false)
                                                .min_size(egui::vec2(10.0, 10.0)),
                                            );
                                            let close_response = crate::screen_reader::label_element(
                                                ui,
                                                close_response,
                                                &format!("Close {name}"),
                                                &format!("Close tab for {name}"),
                                            );
                                            close_response.clicked()
                                        }
                                    })
                                    .inner;

                                let response = tab
                                    .allocate_space(ui)
                                    .interact(egui::Sense::click())
                                    .on_hover_text(path.display().to_string());
                                
                                response.context_menu(|ui| {
                                    let is_pinned = self.pinned_tabs.contains(&path);
                                    if ui.button(if is_pinned { "Unpin tab" } else { "Pin tab" }).clicked() {
                                        if is_pinned {
                                            self.pinned_tabs.remove(&path);
                                        } else {
                                            self.pinned_tabs.insert(path.clone());
                                        }
                                        ui.close_menu();
                                    }
                                    
                                    ui.menu_button("Tab Group", |ui| {
                                        let current_group = self.tab_to_group.get(&path).cloned();
                                        if current_group.is_some() {
                                            if ui.button("Remove from group").clicked() {
                                                self.tab_to_group.remove(&path);
                                                ui.close_menu();
                                            }
                                        }
                                        for group in &self.tab_groups {
                                            let is_in_group = current_group.as_ref() == Some(&group.name);
                                            let color = get_group_color(&self.tab_groups, &group.name);
                                            let prefix = if is_in_group { "✓ " } else { "" };
                                            ui.horizontal(|ui| {
                                                let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                                                ui.painter().rect_filled(rect, egui::Rounding::same(1.0), color);
                                                if ui.button(format!("{prefix}{}", group.name)).clicked() {
                                                    self.tab_to_group.insert(path.clone(), group.name.clone());
                                                    ui.close_menu();
                                                }
                                            });
                                        }
                                        if ui.button("+ New group...").clicked() {
                                            self.new_tab_group_state.open = true;
                                            self.new_tab_group_state.name = String::new();
                                            self.new_tab_group_state.target_file = Some(path.clone());
                                            self.on_modal_opened();
                                            ui.close_menu();
                                        }
                                    });

                                    ui.separator();

                                    if ui.button("Close").clicked() {
                                        close = Some(path.clone());
                                        ui.close_menu();
                                    }
                                    if ui.button("Close Others").clicked() {
                                        let other_paths: Vec<PathBuf> = self.buffers.keys()
                                            .filter(|p| *p != &path && !self.pinned_tabs.contains(*p))
                                            .cloned()
                                            .collect();
                                        for other_path in other_paths {
                                            self.request_close_file(&other_path);
                                        }
                                        ui.close_menu();
                                    }
                                    if ui.button("Close All").clicked() {
                                        let all_paths: Vec<PathBuf> = self.buffers.keys()
                                            .filter(|p| !self.pinned_tabs.contains(*p))
                                            .cloned()
                                            .collect();
                                        for all_path in all_paths {
                                            self.request_close_file(&all_path);
                                        }
                                        ui.close_menu();
                                    }
                                });

                                response.widget_info(|| {
                                    egui::WidgetInfo::selected(
                                        egui::WidgetType::SelectableLabel,
                                        is_active,
                                        &title,
                                    )
                                });
                                if !is_active && response.hovered() {
                                    tab.frame.fill = ui.visuals().widgets.hovered.weak_bg_fill;
                                }
                                tab.paint(ui);

                                if response.clicked() && !close_clicked {
                                    activate = Some(path.clone());
                                }
                                if response.middle_clicked() || (close_clicked && !is_pinned) {
                                    close = Some(path.clone());
                                }
                                if is_active && self.reveal_active_tab {
                                    response.scroll_to_me(Some(Align::Center));
                                    revealed_active = true;
                                }
                            }
                        });
                    });
                let full_rect = ui.max_rect();
                crate::keyboard_nav::draw_focus_outline(ui, egui::Id::new("tabs"), full_rect);
            });

        if let Some(path) = activate {
            self.pane_tree
                .open_in_pane(self.focus.active_pane, path.clone());
            self.active = Some(path);
        }
        if revealed_active {
            self.reveal_active_tab = false;
        }
        if let Some(path) = close {
            self.request_close_file(&path);
        }
    }

    fn show_workspace_panels(&mut self, context: &egui::Context) -> Option<EditorCommand> {
        // Zen mode: conditionally hide panels
        let show_status = self.zen.show_status_bar();
        let show_tabs = self.zen.show_tab_bar();
        let show_bottom = self.zen.show_bottom_panels();
        let show_side = self.zen.show_file_tree();

        if show_status {
            self.show_status_bar(context);
        }
        // Search panel is registered BEFORE the terminal so that, when both
        // are visible, the terminal occupies the outermost bottom slice and the
        // search panel appears above it.  egui stacks bottom panels inward in
        // registration order: last registered = innermost (nearest the editor).
        if !self.zen.zen_mode {
            self.show_search_panel(context);
        }
        if show_bottom {
            self.show_bottom_panel(context);
        }
        // Panel order is significant in egui: the side panel must claim the left
        // edge before the tab strip so tabs begin exactly where the editor begins.
        if show_side {
            if self.show_git_panel {
                self.show_git_panel(context);
            } else {
                self.show_file_tree(context);
            }
        }
        if show_tabs {
            self.show_tabs(context);
        }
        // Assistant dock claims the outermost right edge before the editor
        // (and its outline panel) are registered.
        self.show_assistant_panel(context);
        self.show_editor(context)
    }

    /// Right-dock AI assistant conversation panel (Zed Assistant Panel style).
    fn show_assistant_panel(&mut self, context: &egui::Context) {
        if !self.assistant.open || self.zen.zen_mode {
            return;
        }
        let palette = self.active_palette.semantic;
        let mut event = None;
        let panel_response = egui::SidePanel::right("assistant_panel")
            .resizable(true)
            .width_range(240.0..=640.0)
            .default_width(self.assistant.width.max(300.0))
            .frame(
                egui::Frame::none()
                    .fill(palette.panel_background)
                    .inner_margin(egui::Margin::same(8.0)),
            )
            .show(context, |ui| {
                let editor_context = self.assistant_editor_context();
                event = self
                    .assistant
                    .show(ui, &palette, &self.settings.assistant.command, &editor_context);
            });
        let panel_width = panel_response.response.rect.width();
        if panel_width > 0.0 {
            self.assistant.width = panel_width;
        }
        match event {
            Some(crate::assistant::AssistantEvent::InsertCode(code)) => {
                let path = self.active.clone();
                if let Some(path) = path {
                    if let Some(buffer) = self.buffers.get_mut(&path) {
                        let _ = buffer.insert_at_cursors(&code);
                    }
                }
            }
            Some(crate::assistant::AssistantEvent::Copy(text)) => {
                context.output_mut(|output| output.copied_text = text);
            }
            None => {}
        }
    }

    /// Snapshot of the active buffer for assistant context chips.
    fn assistant_editor_context(&self) -> crate::assistant::EditorContext {
        let Some(path) = self.active.clone() else {
            return crate::assistant::EditorContext::default();
        };
        let Some(buffer) = self.buffers.get(&path) else {
            return crate::assistant::EditorContext::default();
        };
        let selection = {
            let (start, end) = buffer.primary_cursor().normalize();
            if start == end {
                None
            } else if let (Some(start), Some(end)) = (
                buffer.position_to_char_index(start),
                buffer.position_to_char_index(end),
            ) {
                buffer.char_range_to_string(start..end.saturating_add(1))
            } else {
                None
            }
        };
        let language = crate::language::LanguageId::from_path(&path).display_label();
        let file_text = if buffer.len_chars() <= 200_000 {
            Some(buffer.text())
        } else {
            None
        };
        crate::assistant::EditorContext {
            file_path: Some(path),
            language: Some(language.to_owned()),
            file_text,
            selection,
        }
    }

    fn compute_breadcrumb_segments(
        &self,
        path: &Path,
        cursor_line: usize,
    ) -> Vec<crate::outline::BreadcrumbSegment> {
        let mut segments = Vec::new();

        // 1. Directory name containing the file
        if let Some(parent) = path.parent() {
            let label = parent
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "src".to_string());
            segments.push(crate::outline::BreadcrumbSegment {
                label,
                kind: crate::outline::SegmentKind::File,
                line: None,
                path: Some(parent.to_path_buf()),
            });
        } else {
            segments.push(crate::outline::BreadcrumbSegment {
                label: "src".to_string(),
                kind: crate::outline::SegmentKind::File,
                line: None,
                path: Some(PathBuf::from(".")),
            });
        }

        // 2. Filename
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        segments.push(crate::outline::BreadcrumbSegment {
            label,
            kind: crate::outline::SegmentKind::File,
            line: None,
            path: Some(path.to_path_buf()),
        });

        // 3. Symbol segments from outline nodes
        if let Some(nodes) = self.outline_panel.nodes.get(path) {
            fn walk(
                nodes: &[crate::lsp::types::OutlineNode],
                cursor_line: usize,
                out: &mut Vec<crate::outline::BreadcrumbSegment>,
            ) {
                if let Some(node) = nodes
                    .iter()
                    .find(|n| cursor_line >= n.line && cursor_line <= n.end_line)
                {
                    out.push(crate::outline::BreadcrumbSegment {
                        label: node.name.clone(),
                        kind: crate::outline::SegmentKind::Symbol(node.kind),
                        line: Some(node.line),
                        path: None,
                    });
                    walk(&node.children, cursor_line, out);
                }
            }
            walk(nodes, cursor_line, &mut segments);
        }

        segments
    }

    fn select_breadcrumb_item(
        &mut self,
        pane_id: crate::panes::PaneId,
        current_path: &Path,
        item: &crate::outline::BreadcrumbSegment,
    ) {
        match &item.kind {
            crate::outline::SegmentKind::File => {
                if let Some(ref path) = item.path {
                    if let Err(error) = self.open_file(path.clone()) {
                        self.error_message =
                            Some(format!("Could not open {}: {error}", path.display()));
                    }
                    self.pane_tree.open_in_pane(pane_id, path.clone());
                    self.focus.active_pane = pane_id;
                    self.sync_active_from_focused_pane();
                }
            }
            crate::outline::SegmentKind::Symbol(_) => {
                if let Some(line) = item.line {
                    if let Some(buf) = self.buffers.get_mut(current_path) {
                        let _ =
                            buf.set_cursor(crate::editor::buffer::CursorPosition { line, col: 0 });
                    }
                    if let Some(st) = self.editor_states.get_mut(current_path) {
                        st.request_scroll_to_cursor();
                        st.request_focus();
                    }
                }
            }
        }
    }

    fn update_breadcrumbs(&mut self) {
        let visible_panes = self.pane_tree.all_leaf_ids();
        for pane_id in visible_panes {
            if let Some(path) = self.pane_tree.active_in_pane(pane_id).cloned() {
                if let Some(buf) = self.buffers.get(&path) {
                    let cursor_line = buf.cursor().line;

                    let (file_changed, cursor_changed) = {
                        let state = self.breadcrumbs.entry(pane_id).or_default();
                        (
                            state.last_active_file.as_ref() != Some(&path),
                            state.last_cursor_line != cursor_line,
                        )
                    };

                    if file_changed || cursor_changed {
                        let segments = self.compute_breadcrumb_segments(&path, cursor_line);
                        let state = self.breadcrumbs.entry(pane_id).or_default();
                        state.last_active_file = Some(path.clone());
                        state.last_cursor_line = cursor_line;
                        state.segments = segments;
                        if file_changed {
                            state.open_dropdown = None;
                        }
                    }
                }
            }
        }
    }

    fn show_editor(&mut self, context: &egui::Context) -> Option<EditorCommand> {
        self.diagnostic_tooltip_active = false;
        self.sync_active_from_focused_pane();
        self.update_breadcrumbs();
        let active = self.active.clone();
        let interactive = !self.has_modal();
        let mut command = None;

        self.on_active_tab_changed(&active);

        // Phase 1: refresh file matches and obtain panel output while holding
        // buffer references.  We must NOT call any &mut self method here.
        // Collect needed actions into plain values.
        let panel_out = {
            let active_path = active.clone();
            if let Some(ref path) = active_path {
                // Refresh file-scope cache using the current buffer text.
                if self.search_state.visible && self.search_state.query.scope == SearchScope::File {
                    if let Some(buffer) = self.buffers.get(path) {
                        let text = buffer.to_full_string();
                        let rev = buffer.revision();
                        self.search_state
                            .refresh_file_matches(&text, Some(path), rev);
                    }
                }
            }
            None::<search_panel::PanelOutput> // populated in Phase 2 inside egui
        };
        let _ = panel_out; // suppress warning; real output collected below

        // We need to split the egui closure from the post-processing to avoid
        // holding buffer borrows while calling &mut self methods.
        // Strategy: capture the PanelOutput from inside the closure, then act
        // on it outside.
        let mut collected_panel_out: Option<search_panel::PanelOutput> = None;

        // Consume completion keys before the editor widget runs; apply events
        // after the closure (same deferral pattern as the search panel).
        let collected_completion_keyboard_event =
            self.collect_completion_popup_keyboard_event(context);

        let mut collected_editor_action = None;
        let mut hover_popup = HoverPopupModel::none();
        let mut editor_viewport_rect = None;
        let mut editor_has_focus = false;
        let completion_snapshot_before_edit = active.as_ref().and_then(|path| {
            if self.completion.is_open() && self.completion.is_for_path(path) {
                self.buffers
                    .get(path)
                    .map(|buffer| (buffer.revision(), buffer.cursor()))
            } else {
                None
            }
        });
        let hover_snapshot_before_edit = active.as_ref().and_then(|path| {
            if self.lsp_hover_is_active_for_path(path) {
                self.buffers
                    .get(path)
                    .map(|buffer| (buffer.revision(), buffer.cursor()))
            } else {
                None
            }
        });

        let mut collected_breadcrumb_selection: Option<(
            crate::panes::PaneId,
            std::path::PathBuf,
            crate::outline::BreadcrumbSegment,
        )> = None;
        #[cfg(test)]
        let _ = &mut collected_breadcrumb_selection;
        let mut open_settings_requested = false;

        {
            let BlueIdeApp {
                ref mut pane_tree,
                ref mut focus,
                ref mut pane_actions,
                ref mut buffers,
                ref mut editor_states,
                ref settings,
                #[cfg(test)]
                ref lsp,
                ref lsp_manager,
                ref active_palette,
                ref mut search_state,
                ref mut breadcrumbs,
                ref tree,
                ref mut completion_anchor,
                ref completion,
                ref outline_panel,
                ref git,
                ref show_blame,
                ref blame_cache,
                ref bookmarks,
                ref pane_content,
                ref mut image_viewer_states,
                ref mut markdown_preview_states,
                ref mut diff_viewer_states,
                ref workspace,
                zen: _,
                ..
            } = *self;

            #[cfg(test)]
            let _ = outline_panel;

            let central_frame = {
                let mut f = egui::Frame::central_panel(&context.style());
                f.inner_margin.top = 0.0;
                f
            };
            egui::CentralPanel::default().frame(central_frame).show(context, |ui| {
                ui.set_enabled(interactive);
                let full_rect = ui.max_rect();
                crate::keyboard_nav::draw_focus_outline(ui, egui::Id::new("blue_ide_editor"), full_rect);

                pane_tree.render(ui, focus, pane_actions, &mut |ui, pane_id, active_opt, _tabs, focus_state, actions| {
                    ui.vertical(|ui| {
                        // This stack is application chrome followed by the editor. The
                        // default egui gap reads as a third, empty toolbar row at high DPI.
                        ui.spacing_mut().item_spacing.y = EDITOR_STACK_SPACING;
                        #[cfg(test)]
                        let _ = (&focus_state, &actions, &breadcrumbs); // borrow (not move) to suppress unused warnings
                        #[cfg(not(test))]
                        let _ = &breadcrumbs;
                        // Breadcrumb bar for this pane
                        #[cfg(not(test))]
                        if settings.ui.show_breadcrumbs {
                         if let Some(ref path) = active_opt {
                            let (state_segments, mut state_open_dropdown, mut state_focused_segment, mut state_dropdown_selected_idx, mut state_dropdown_items) = {
                                let state = breadcrumbs.entry(pane_id).or_default();
                                (
                                    state.segments.clone(),
                                    state.open_dropdown,
                                    state.focused_segment,
                                    state.dropdown_selected_idx,
                                    state.dropdown_items.clone(),
                                )
                            };
                            let n = state_segments.len();
                            if n > 0 {
                                let font_id = egui::TextStyle::Body.resolve(ui.style());
                                let item_spacing = ui.spacing().item_spacing.x;
                                let button_padding = ui.spacing().button_padding.x;
                                let available_width = ui.available_width();
                                
                                let get_width = |ui: &egui::Ui, text: &str, is_button: bool| -> f32 {
                                    let w = ui.fonts(|f| f.layout_no_wrap(text.to_owned(), font_id.clone(), egui::Color32::PLACEHOLDER).rect.width());
                                    if is_button {
                                        w + 2.0 * button_padding
                                    } else {
                                        w
                                    }
                                };
                                
                                let mut all_w = 0.0;
                                for i in 0..n {
                                    all_w += get_width(ui, &state_segments[i].label, true);
                                    if i < n - 1 {
                                        all_w += item_spacing + get_width(ui, " › ", false) + item_spacing;
                                    }
                                }
                                
                                let mut render_indices = Vec::new();
                                let mut has_ellipsis = false;
                                let mut ellipsis_after_idx = None;
                                
                                if all_w <= available_width || n <= 2 {
                                    render_indices = (0..n).collect();
                                } else {
                                    let mut best_k = None;
                                    for k in 2..n-1 {
                                        let mut total_w = 0.0;
                                        total_w += get_width(ui, &state_segments[0].label, true);
                                        total_w += item_spacing + get_width(ui, " › ", false) + item_spacing;
                                        total_w += get_width(ui, "…", false);
                                        total_w += item_spacing + get_width(ui, " › ", false) + item_spacing;
                                        for i in k..n {
                                            total_w += get_width(ui, &state_segments[i].label, true);
                                            if i < n - 1 {
                                                total_w += item_spacing + get_width(ui, " › ", false) + item_spacing;
                                            }
                                        }
                                        if total_w <= available_width {
                                            best_k = Some(k);
                                            break;
                                        }
                                    }
                                    let k = best_k.unwrap_or(n - 1);
                                    render_indices.push(0);
                                    has_ellipsis = true;
                                    ellipsis_after_idx = Some(0);
                                    for i in k..n {
                                        render_indices.push(i);
                                    }
                                }
                                
                                // Breadcrumb fixed-height container
                                let (rect, _response) = ui.allocate_at_least(
                                    egui::vec2(ui.available_width(), BREADCRUMB_BAR_HEIGHT),
                                    egui::Sense::hover(),
                                );
                                
                                // Register focus ID
                                let bar_id = ui.make_persistent_id(("breadcrumb_bar", pane_id));
                                let bar_focused = ui.memory(|mem| mem.has_focus(bar_id));
                                if bar_focused && state_focused_segment.is_none() {
                                    state_focused_segment = Some(0);
                                }
                                if !bar_focused {
                                    state_focused_segment = None;
                                }
                                
                                // Helper inline closure to compute dropdown items using destructured fields
                                let compute_items = |idx: usize| -> Vec<crate::outline::BreadcrumbSegment> {
                                    let mut items = Vec::new();
                                    if idx == 0 {
                                        if let Some(dir_path) = state_segments[0].path.as_ref() {
                                            if let Some(ref root) = tree.root {
                                                fn find_dir<'a>(node: &'a crate::filetree::FsNode, target: &Path) -> Option<&'a crate::filetree::FsNode> {
                                                    match node {
                                                        crate::filetree::FsNode::Dir { path, children, .. } => {
                                                            if path == target {
                                                                                            return Some(node);
                                                                                        }
                                                            for child in children {
                                                                if let Some(res) = find_dir(child, target) {
                                                                    return Some(res);
                                                                }
                                                            }
                                                            None
                                                        }
                                                        _ => None,
                                                    }
                                                }
                                                if let Some(node) = find_dir(root, dir_path) {
                                                    if let crate::filetree::FsNode::Dir { children, .. } = node {
                                                        for child in children {
                                                            let (child_path, child_name) = match child {
                                                                crate::filetree::FsNode::File { path, name } => (path.clone(), name.clone()),
                                                                crate::filetree::FsNode::Dir { path, name, .. } => (path.clone(), name.clone()),
                                                            };
                                                            items.push(crate::outline::BreadcrumbSegment {
                                                                label: child_name,
                                                                kind: crate::outline::SegmentKind::File,
                                                                line: None,
                                                                path: Some(child_path),
                                                            });
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    } else if idx == 1 {
                                        let mut open_files: Vec<crate::outline::BreadcrumbSegment> = buffers.keys()
                                            .map(|p| crate::outline::BreadcrumbSegment {
                                                label: p.file_name().unwrap_or_default().to_string_lossy().to_string(),
                                                kind: crate::outline::SegmentKind::File,
                                                line: None,
                                                path: Some(p.clone()),
                                            })
                                            .collect();
                                        open_files.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
                                        items = open_files;
                                    } else {
                                        if let Some(nodes) = outline_panel.nodes.get(path) {
                                            let parent_segments = &state_segments[2..idx];
                                            fn find_children<'a>(
                                                current_nodes: &'a [crate::lsp::types::OutlineNode],
                                                parent_path: &[crate::outline::BreadcrumbSegment],
                                                depth: usize,
                                            ) -> Option<&'a [crate::lsp::types::OutlineNode]> {
                                                if depth == parent_path.len() {
                                                    return Some(current_nodes);
                                                }
                                                let target = &parent_path[depth];
                                                for node in current_nodes {
                                                    if Some(node.line) == target.line && node.name == target.label {
                                                        return find_children(&node.children, parent_path, depth + 1);
                                                    }
                                                }
                                                None
                                            }
                                            let sibling_nodes = find_children(nodes, parent_segments, 0).unwrap_or(nodes);
                                            items = sibling_nodes.iter().map(|node| crate::outline::BreadcrumbSegment {
                                                label: node.name.clone(),
                                                kind: crate::outline::SegmentKind::Symbol(node.kind),
                                                line: Some(node.line),
                                                path: None,
                                            }).collect();
                                        }
                                    }
                                    items
                                };

                                // Keyboard events for bar
                                if bar_focused {
                                    if ui.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
                                        if let Some(curr) = state_focused_segment {
                                            if curr > 0 {
                                                state_focused_segment = Some(curr - 1);
                                            }
                                        }
                                    }
                                    if ui.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                                        if let Some(curr) = state_focused_segment {
                                            if curr + 1 < state_segments.len() {
                                                state_focused_segment = Some(curr + 1);
                                            }
                                        }
                                    }
                                    if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                        if let Some(focused) = state_focused_segment {
                                            if state_open_dropdown.is_none() {
                                                state_open_dropdown = Some(focused);
                                                state_dropdown_items = compute_items(focused);
                                                state_dropdown_selected_idx = Some(0);
                                                collected_breadcrumb_selection = Some((pane_id, path.to_path_buf(), crate::outline::BreadcrumbSegment {
                                                    label: "".to_string(),
                                                    kind: crate::outline::SegmentKind::File,
                                                    line: None,
                                                    path: None, // dummy segment representing clear other dropdowns
                                                }));
                                            }
                                        }
                                    }
                                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                        state_open_dropdown = None;
                                        state_focused_segment = None;
                                        if let Some(st) = editor_states.get_mut(path) {
                                            st.request_focus();
                                        }
                                    }
                                }

                                // Keyboard events inside active dropdown
                                if let Some(_open_idx) = state_open_dropdown {
                                    let items_len = state_dropdown_items.len();
                                    if items_len > 0 {
                                        if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                                            let next_idx = match state_dropdown_selected_idx {
                                                Some(curr) => (curr + 1) % items_len,
                                                None => 0,
                                            };
                                            state_dropdown_selected_idx = Some(next_idx);
                                        }
                                        if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                                            let prev_idx = match state_dropdown_selected_idx {
                                                Some(curr) => (curr + items_len - 1) % items_len,
                                                None => items_len - 1,
                                            };
                                            state_dropdown_selected_idx = Some(prev_idx);
                                        }
                                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                            if let Some(selected_idx) = state_dropdown_selected_idx {
                                                if let Some(item) = state_dropdown_items.get(selected_idx).cloned() {
                                                    collected_breadcrumb_selection = Some((pane_id, path.to_path_buf(), item));
                                                }
                                            }
                                        }
                                    }
                                }

                                ui.allocate_ui_at_rect(rect, |ui| {
                                    ui.painter().rect_filled(
                                        rect,
                                        0.0,
                                        active_palette.semantic.panel_background
                                    );
                                    
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 4.0;
                                        ui.add_space(4.0);
                                        
                                        let mut rendered_count = 0;
                                        for &idx in &render_indices {
                                            let segment = &state_segments[idx];
                                            let label_color = match &segment.kind {
                                                crate::outline::SegmentKind::File => active_palette.semantic.primary_text,
                                                crate::outline::SegmentKind::Symbol(sym_kind) => match sym_kind {
                                                    crate::lsp::types::SymbolKind::Function => egui::Color32::from_rgb(30, 144, 255), // blue
                                                    crate::lsp::types::SymbolKind::Struct
                                                    | crate::lsp::types::SymbolKind::Enum
                                                    | crate::lsp::types::SymbolKind::Trait => egui::Color32::from_rgb(0, 128, 128), // teal
                                                    crate::lsp::types::SymbolKind::Impl => egui::Color32::from_rgb(235, 120, 30), // orange
                                                    crate::lsp::types::SymbolKind::Module => egui::Color32::from_rgb(147, 112, 219), // purple
                                                    _ => active_palette.semantic.primary_text,
                                                }
                                            };
                                            
                                            let is_focused = state_focused_segment == Some(idx);
                                            let btn = egui::Button::new(RichText::new(&segment.label).color(label_color))
                                                .frame(false);
                                                
                                            let response = ui.add(btn);
                                            
                                            if is_focused {
                                                ui.painter().rect_stroke(response.rect, 1.0, ui.visuals().selection.stroke);
                                            } else if response.hovered() {
                                                ui.painter().rect_filled(
                                                    response.rect,
                                                    2.0,
                                                    ui.visuals().widgets.hovered.bg_fill.linear_multiply(0.2)
                                                );
                                            }
                                            
                                            if response.clicked() {
                                                if state_open_dropdown == Some(idx) {
                                                    state_open_dropdown = None;
                                                } else {
                                                    state_open_dropdown = Some(idx);
                                                    state_dropdown_items = compute_items(idx);
                                                    state_dropdown_selected_idx = None;
                                                    collected_breadcrumb_selection = Some((pane_id, path.to_path_buf(), crate::outline::BreadcrumbSegment {
                                                        label: "".to_string(),
                                                        kind: crate::outline::SegmentKind::File,
                                                        line: None,
                                                        path: None,
                                                    }));
                                                }
                                                ui.memory_mut(|mem| mem.request_focus(bar_id));
                                                state_focused_segment = Some(idx);
                                            }
                                            
                                            if state_open_dropdown == Some(idx) {
                                                let space_below = ui.ctx().screen_rect().bottom() - response.rect.bottom();
                                                let render_above = space_below < 300.0 && response.rect.top() > 300.0;
                                                let pos = if render_above {
                                                    response.rect.left_top()
                                                } else {
                                                    response.rect.left_bottom()
                                                };
                                                let pivot = if render_above {
                                                    egui::Align2::LEFT_BOTTOM
                                                } else {
                                                    egui::Align2::LEFT_TOP
                                                };
                                                
                                                let dropdown_id = ui.make_persistent_id(("breadcrumb_dropdown", pane_id, idx));
                                                let mut close_dropdown = false;
                                                let mut select_item = None;
                                                
                                                egui::Area::new(dropdown_id)
                                                    .order(egui::Order::Foreground)
                                                    .fixed_pos(pos)
                                                    .pivot(pivot)
                                                    .show(ui.ctx(), |ui| {
                                                        let frame = egui::Frame::menu(ui.style())
                                                            .inner_margin(egui::Margin::same(4.0))
                                                            .fill(ui.visuals().window_fill())
                                                            .stroke(ui.visuals().window_stroke());
                                                            
                                                        let res = frame.show(ui, |ui| {
                                                            ui.set_width(ui.available_width().clamp(200.0, 400.0));
                                                            
                                                            egui::ScrollArea::vertical()
                                                                .max_height(300.0)
                                                                .show(ui, |ui| {
                                                                    for (item_idx, item) in state_dropdown_items.iter().enumerate() {
                                                                        let is_current = match segment.kind {
                                                                            crate::outline::SegmentKind::File => {
                                                                                item.path.as_ref() == Some(path)
                                                                            }
                                                                            crate::outline::SegmentKind::Symbol(_) => {
                                                                                state_segments.get(idx).map_or(false, |active_seg| {
                                                                                    active_seg.label == item.label && active_seg.line == item.line
                                                                                })
                                                                            }
                                                                        };
                                                                        let is_highlighted = state_dropdown_selected_idx == Some(item_idx);
                                                                        
                                                                        let bg_fill = if is_highlighted {
                                                                            ui.visuals().widgets.hovered.bg_fill.linear_multiply(0.4)
                                                                        } else if is_current {
                                                                            ui.visuals().widgets.active.bg_fill.linear_multiply(0.2)
                                                                        } else {
                                                                            egui::Color32::TRANSPARENT
                                                                        };
                                                                        
                                                                        let mut item_frame = egui::Frame::none()
                                                                            .fill(bg_fill)
                                                                            .inner_margin(egui::Margin::symmetric(6.0, 4.0))
                                                                            .begin(ui);
                                                                            
                                                                        item_frame.content_ui.horizontal(|ui| {
                                                                            ui.spacing_mut().item_spacing.x = 4.0;
                                                                            if let crate::outline::SegmentKind::Symbol(sym_kind) = &item.kind {
                                                                                let icon_color = sym_kind.icon_color(active_palette);
                                                                                ui.colored_label(icon_color, sym_kind.icon_text());
                                                                            }
                                                                            ui.label(&item.label);
                                                                            if let Some(line) = item.line {
                                                                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                                                    ui.add_space(8.0);
                                                                                    ui.colored_label(
                                                                                        active_palette.semantic.muted_text,
                                                                                        format!("Ln {}", line + 1)
                                                                                    );
                                                                                });
                                                                            }
                                                                        });
                                                                        
                                                                        let response = item_frame.allocate_space(ui)
                                                                            .interact(egui::Sense::click());
                                                                            
                                                                        if response.hovered() && !is_highlighted {
                                                                            item_frame.frame.fill = ui.visuals().widgets.hovered.bg_fill.linear_multiply(0.2);
                                                                        }
                                                                        item_frame.paint(ui);
                                                                        
                                                                        if response.clicked() {
                                                                            select_item = Some(item.clone());
                                                                        }
                                                                    }
                                                                });
                                                        });
                                                        
                                                        let dropdown_rect = res.response.rect;
                                                        let clicked_outside = ui.input(|i| i.pointer.any_click()) && {
                                                            if let Some(click_pos) = ui.input(|i| i.pointer.interact_pos()) {
                                                                !dropdown_rect.contains(click_pos) && !response.rect.contains(click_pos)
                                                            } else {
                                                                false
                                                            }
                                                        };
                                                        if clicked_outside || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                                            close_dropdown = true;
                                                        }
                                                    });
                                                    
                                                if let Some(item) = select_item {
                                                    collected_breadcrumb_selection = Some((pane_id, path.to_path_buf(), item));
                                                }
                                                if close_dropdown {
                                                    state_open_dropdown = None;
                                                }
                                            }
                                            
                                            rendered_count += 1;
                                            if rendered_count < render_indices.len() {
                                                ui.label(RichText::new(" › ").color(active_palette.semantic.muted_text));
                                            }
                                            if has_ellipsis && ellipsis_after_idx == Some(idx) {
                                                ui.label(RichText::new("…").color(active_palette.semantic.muted_text));
                                                ui.label(RichText::new(" › ").color(active_palette.semantic.muted_text));
                                            }
                                        }
                                    });
                                });
                            }
                            
                            // Write back breadcrumb state
                            let state = breadcrumbs.entry(pane_id).or_default();
                            state.open_dropdown = state_open_dropdown;
                            state.focused_segment = state_focused_segment;
                            state.dropdown_selected_idx = state_dropdown_selected_idx;
                            state.dropdown_items = state_dropdown_items;
                        }
                        }

                        // 3. Editor widget for this pane — or alternate content renderer
                        if let Some(ref path) = active_opt {
                            // Check if this pane has a non-code content type
                            let current_content = pane_content.get(&pane_id).cloned();
                            let is_alternate = matches!(
                                &current_content,
                                Some(PaneContent::ImageViewer { .. })
                                | Some(PaneContent::MarkdownPreview { .. })
                                | Some(PaneContent::DiffViewer { .. })
                            );

                            if is_alternate {
                                match &current_content {
                                    Some(PaneContent::ImageViewer { .. }) => {
                                        if let Some(img_state) = image_viewer_states.get_mut(&pane_id) {
                                            crate::image_viewer::render_image_viewer(
                                                ui,
                                                img_state,
                                                active_palette.semantic,
                                            );
                                        } else {
                                            // Initialize state
                                            let mut img_state = crate::image_viewer::ImageViewerState::new(path.clone());
                                            img_state.load(ui.ctx());
                                            let pane_size = ui.available_size();
                                            img_state.fit_to_pane(pane_size);
                                            image_viewer_states.insert(pane_id, img_state);
                                            if let Some(img_state) = image_viewer_states.get_mut(&pane_id) {
                                                crate::image_viewer::render_image_viewer(
                                                    ui,
                                                    img_state,
                                                    active_palette.semantic,
                                                );
                                            }
                                        }
                                    }
                                    Some(PaneContent::MarkdownPreview { .. }) => {
                                        let content = buffers
                                            .get(path)
                                            .map(|b| b.text())
                                            .unwrap_or_else(|| {
                                                std::fs::read_to_string(path).unwrap_or_default()
                                            });
                                        if !markdown_preview_states.contains_key(&pane_id) {
                                            markdown_preview_states.insert(
                                                pane_id,
                                                crate::markdown_preview::MarkdownPreviewState::new(path.clone()),
                                            );
                                        }
                                        if let Some(md_state) = markdown_preview_states.get_mut(&pane_id) {
                                            let toggled = crate::markdown_preview::render_markdown(
                                                ui,
                                                md_state,
                                                &content,
                                                active_palette.semantic,
                                            );
                                            if toggled {
                                                // User clicked "Edit source" — switch to CodeEditor
                                                // We can't mutate pane_content here directly since it's borrowed,
                                                // but we push an action to be handled after the closure
                                                // For now just render the toggle button feedback
                                            }
                                        }
                                    }
                                    Some(PaneContent::DiffViewer { .. }) => {
                                        if !diff_viewer_states.contains_key(&pane_id) {
                                            // Diff state should have been set up when opening
                                        }
                                        if let Some(diff_state) = diff_viewer_states.get_mut(&pane_id) {
                                            crate::diff_viewer::render_diff_viewer(
                                                ui,
                                                diff_state,
                                                active_palette.semantic,
                                            );
                                        } else {
                                            ui.centered_and_justified(|ui| {
                                                ui.label("Diff view not available");
                                            });
                                        }
                                    }
                                    _ => {}
                                }
                            } else if let (Some(buffer), Some(state)) =
                                (buffers.get_mut(path), editor_states.get_mut(path))
                            {
                                // Single-file (non-workspace) open falls back to
                                // the file tree root until a Workspace is added.
                                let active_root = workspace
                                    .owner_of(path)
                                    .map(|root| root.path.clone())
                                    .or_else(|| tree.root_path.clone());
                                let lsp_active = active_root
                                    .as_deref()
                                    .is_some_and(|root| is_lsp_path(settings, Some(root), path))
                                    && {
                                        #[cfg(test)]
                                        {
                                            lsp.as_ref().map_or_else(
                                                || {
                                                    crate::language::LanguageId::from_path(path)
                                                        .server_id()
                                                        .is_some_and(|server_id| {
                                                            lsp_manager.is_running(server_id)
                                                        })
                                                },
                                                |c| c.is_running(),
                                            )
                                        }
                                        #[cfg(not(test))]
                                        {
                                            crate::language::LanguageId::from_path(path)
                                                .server_id()
                                                .is_some_and(|server_id| lsp_manager.is_running(server_id))
                                        }
                                    };
                                
                                #[cfg(test)]
                                let pane_diagnostics = lsp.as_ref()
                                    .and_then(|client| client.diagnostics_for(path).map(|d| d.to_vec()))
                                    .unwrap_or_else(|| lsp_manager.diagnostics_for(path).map(|d| d.to_vec()).unwrap_or_default());
                                #[cfg(not(test))]
                                let pane_diagnostics = lsp_manager.diagnostics_for(path)
                                    .map(|d| d.to_vec())
                                    .unwrap_or_default();
                                    
                                let pane_search_highlights: Vec<SearchHighlight> = if search_state.visible
                                    && search_state.query.scope == crate::search::SearchScope::File
                                {
                                    search_state.file_matches
                                        .iter()
                                        .enumerate()
                                        .map(|(i, m)| SearchHighlight {
                                            byte_range: m.byte_range.clone(),
                                            is_active: search_state.active_index == Some(i),
                                        })
                                        .collect()
                                } else {
                                    Vec::new()
                                };

                                let pane_diff_hunks: Vec<crate::git::DiffHunk> = git.as_ref()
                                    .and_then(|g| g.file_diffs.get(path))
                                    .cloned()
                                    .unwrap_or_default();
                                let pane_show_blame = *show_blame.get(path).unwrap_or(&false);
                                let pane_blame_lines: Vec<crate::git::BlameLine> = if pane_show_blame {
                                    blame_cache.get(path).cloned().unwrap_or_default()
                                } else {
                                    Vec::new()
                                };
                                let pane_bookmarks: Vec<usize> = bookmarks
                                    .get(path)
                                    .map(|lines| lines.iter().copied().collect())
                                    .unwrap_or_default();

                                // Ensure minimap state exists for this pane
                                self.minimap_states.entry(pane_id)
                                    .or_insert_with(crate::editor::minimap::MinimapState::new);

                                // ── Large file mode banner ────────────────────────
                                if buffer.large_file_mode && !buffer.large_file_override {
                                    egui::Frame::none()
                                        .fill(egui::Color32::from_rgb(80, 70, 0))
                                        .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                                        .show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    egui::RichText::new("⚡ Large file mode — syntax highlighting and some features disabled")
                                                        .color(egui::Color32::from_rgb(255, 220, 60))
                                                        .size(12.0),
                                                );
                                                if ui.small_button("Enable anyway").clicked() {
                                                    buffer.large_file_override = true;
                                                    buffer.large_file_mode = false;
                                                    buffer.mark_dirty();
                                                    buffer.inlay_hints_dirty = true;
                                                    buffer.code_lens_dirty = true;
                                                    buffer.semantic_tokens_dirty = true;
                                                }
                                                if ui.small_button("Settings").clicked() {
                                                    open_settings_requested = true;
                                                }
                                            });
                                        });
                                }

                                let output = EditorWidget::show(
                                    ui,
                                    state,
                                    buffer,
                                    EditorInteraction::new(
                                        interactive,
                                        lsp_active,
                                        CompletionPopupModel::from_open(completion.is_open()),
                                    ),
                                    EditorAnnotations::new(&pane_diagnostics, &pane_search_highlights)
                                        .with_diff_hunks(&pane_diff_hunks)
                                        .with_blame(&pane_blame_lines, pane_show_blame)
                                        .with_bookmarks(&pane_bookmarks),
                                    EditorPresentation::new(
                                        settings.appearance.editor_font_size,
                                        *active_palette,
                                    )
                                    .with_editor_settings(&settings.editor)
                                    .with_large_file_suppressed(buffer.features_suppressed()),
                                    self.minimap_states.get_mut(&pane_id),
                                    settings.appearance.font_ligatures,
                                    settings.appearance.font_ligatures.then(|| &mut self.ligature_renderer),
                                );
                                
                                if pane_id == focus_state.active_pane {
                                    command = output.command;
                                    collected_editor_action = output.action;
                                    *completion_anchor = output.completion_anchor;
                                    editor_viewport_rect = output.editor_viewport_rect;
                                    hover_popup = output.hover_popup;
                                    editor_has_focus = output.editor_has_focus;
                                }
                                
                                if output.editor_has_focus {
                                    actions.push(PaneAction::FocusPane { pane: pane_id });
                                }
                            } // end `if let (Some(buffer), Some(state))`
                        } else {
                            // pane has no active file
                            ui.centered_and_justified(|ui| {
                                ui.vertical_centered(|ui| {
                                    ui.add_space(ui.available_height() / 3.0);
                                    ui.label(
                                        egui::RichText::new("stack_ide")
                                            .size(32.0)
                                            .color(egui::Color32::from_rgb(80, 80, 90)),
                                    );
                                    ui.add_space(12.0);
                                    ui.label(
                                        egui::RichText::new("Open a file from the sidebar or press Ctrl+P to quick open")
                                            .size(13.0)
                                            .color(egui::Color32::from_rgb(90, 90, 100)),
                                    );
                                    ui.add_space(8.0);
                                    ui.label(
                                        egui::RichText::new("Ctrl+P  Quick Open    Ctrl+O  Open File    Ctrl+N  New File")
                                            .size(11.0)
                                            .color(egui::Color32::from_rgb(70, 70, 80)),
                                    );
                                });
                            });
                        }
                    });
                });

                let file_match_count = search_state.file_matches.len();
                let out = search_panel::show(
                    ui,
                    search_state,
                    file_match_count,
                    active_palette.semantic,
                );
                collected_panel_out = Some(out);
            });
        }

        if open_settings_requested {
            self.show_settings_window = true;
        }

        if let Some((pane_id, path, item)) = collected_breadcrumb_selection {
            self.select_breadcrumb_item(pane_id, &path, &item);
            if let Some(state) = self.breadcrumbs.get_mut(&pane_id) {
                state.open_dropdown = None;
            }
        }

        if let (Some(path), Some((revision_before, cursor_before))) =
            (active.as_ref(), completion_snapshot_before_edit)
        {
            self.refine_or_dismiss_completion(path, revision_before, cursor_before);
        }
        if let (Some(path), Some((revision_before, cursor_before))) =
            (active.as_ref(), hover_snapshot_before_edit)
        {
            self.dismiss_lsp_hover_if_buffer_edited_since(path, revision_before);
            self.dismiss_lsp_hover_if_cursor_moved_since(path, cursor_before);
        }

        // Phase 2: act on panel output outside the closure (no buffer borrows
        // are alive any more).
        if let Some(ref out) = collected_panel_out {
            if out.closed {
                self.search_state.close();
            }
            if out.query_changed {
                self.search_state.recompile();
                self.search_state.invalidate_file_cache();
                if self.search_state.query.scope == SearchScope::Project {
                    self.start_project_search(context.clone());
                }
            }
            if out.next_match || out.prev_match {
                if out.next_match {
                    self.search_state.next_match();
                } else {
                    self.search_state.prev_match();
                }
                // Move cursor to the active match.
                if let Some(ref path) = active {
                    if let Some(m) = self.search_state.active_file_match().cloned() {
                        if let Some(buffer) = self.buffers.get_mut(path) {
                            let _ = buffer.set_cursor_to_byte(m.byte_range.start);
                        }
                        if let Some(st) = self.editor_states.get_mut(path) {
                            st.request_scroll_to_cursor();
                        }
                    }
                }
            }
            if out.replace_one {
                if let Some(ref path) = active {
                    self.do_replace_one(path);
                }
            }
            if out.replace_all {
                if let Some(ref path) = active {
                    let p = path.clone();
                    self.do_replace_all(&p, context.clone());
                }
            }
            if let Some(idx) = out.project_result_clicked {
                self.search_state.active_index = Some(idx);
                if let Some(m) = self.search_state.active_project_match().cloned() {
                    let file_path = m.path.clone();
                    let byte = m.byte_range.start;
                    // Open the file if it isn't already.
                    if !self.buffers.contains_key(&file_path) {
                        if let Err(e) = self.open_file(file_path.clone()) {
                            self.error_message =
                                Some(format!("Could not open {}: {e}", file_path.display()));
                        }
                    } else {
                        self.active = Some(file_path.clone());
                        self.reveal_active_tab = true;
                    }
                    // Move cursor.
                    if let Some(buffer) = self.buffers.get_mut(&file_path) {
                        let _ = buffer.set_cursor_to_byte(byte);
                    }
                    if let Some(st) = self.editor_states.get_mut(&file_path) {
                        st.request_scroll_to_cursor();
                    }
                }
            }
        }

        if let Some(action) = collected_editor_action {
            self.handle_editor_action(action, interactive, editor_has_focus);
        }

        if let Some(event) = collected_completion_keyboard_event {
            self.handle_completion_popup_event(event);
        }

        let collected_completion_pointer_event = if self.completion.is_open() {
            let mut popup_output = crate::editor::completion::CompletionPopupOutput::default();
            if let Some(anchor) = self.completion_anchor.screen_rect {
                popup_output = self
                    .completion
                    .show(context, anchor, self.active_palette.semantic);
            } else if self.completion.popup().loading {
                popup_output = self
                    .completion
                    .show_loading_at_cursor(context, self.active_palette.semantic);
            }
            popup_output
                .event
                .or_else(|| completion_outside_click_event(context, popup_output.popup_rect))
        } else {
            None
        };
        if let Some(event) = collected_completion_pointer_event {
            self.handle_completion_popup_event(event);
        }

        // The editor can still report source text under overlays (completion, search, modals).
        let pointer_over_search_panel = collected_panel_out
            .as_ref()
            .and_then(|out| {
                self.search_state
                    .visible
                    .then_some(out.panel_rect)
                    .filter(|rect| rect.is_positive())
            })
            .is_some_and(|panel_rect| {
                context.input(|input| {
                    input
                        .pointer
                        .hover_pos()
                        .is_some_and(|pos| panel_rect.contains(pos))
                })
            });
        let blocked_by_other_overlay = self.has_modal() || pointer_over_search_panel;
        let gated_hovered_source = apply_lsp_hover_gates(
            hover_popup,
            CompletionPopupModel::from_open(self.completion.is_open()),
            blocked_by_other_overlay,
        );
        self.update_lsp_hover(
            context,
            gated_hovered_source,
            editor_viewport_rect,
            hover_popup,
        );
        self.dismiss_lsp_hover_on_outside_click(context);
        self.diagnostic_tooltip_active = hover_popup.diagnostic_tooltip_active;

        command
    }

    /// Replace the current active match with the replacement text.
    fn do_replace_one(&mut self, path: &Path) {
        let Some(m) = self.search_state.active_file_match().cloned() else {
            return;
        };
        let Some(pattern) = self.search_state.compiled_pattern() else {
            return;
        };
        let Some(buffer) = self.buffers.get_mut(path) else {
            return;
        };
        let text = buffer.to_full_string();
        // Validate the match is still in the buffer at the expected position.
        if text.get(m.byte_range.clone()) != Some(&text[m.byte_range.clone()]) {
            return;
        }
        let replacement = compute_replacement(
            &text,
            m.byte_range.clone(),
            pattern,
            &self.search_state.query.replacement,
            self.search_state.query.use_regex,
        );
        let Some(replacement_text) = replacement else {
            self.error_message = Some("Stale match or invalid replacement".to_owned());
            return;
        };
        if let Err(e) = buffer.replace_byte_range(m.byte_range, &replacement_text) {
            self.error_message = Some(format!("Replace failed: {}", e.message));
            return;
        }
        self.search_state.invalidate_file_cache();
        // Advance to the next match.
        self.search_state.next_match();
    }

    /// Replace all current matches in file scope, or trigger project Replace All.
    fn do_replace_all(&mut self, path: &Path, ctx: egui::Context) {
        if self.search_state.query.scope == SearchScope::Project {
            self.do_replace_all_project(ctx);
            return;
        }
        // File scope.
        let Some(pattern) = self.search_state.compiled_pattern() else {
            return;
        };
        let Some(buffer) = self.buffers.get_mut(path) else {
            return;
        };
        let text = buffer.to_full_string();
        let matches = self.search_state.file_matches.clone();
        let replacement_str = self.search_state.query.replacement.clone();
        let use_regex = self.search_state.query.use_regex;
        // Build the replacement pairs.
        let pairs: Vec<(std::ops::Range<usize>, String)> = matches
            .iter()
            .filter_map(|m| {
                let repl = compute_replacement(
                    &text,
                    m.byte_range.clone(),
                    pattern,
                    &replacement_str,
                    use_regex,
                )?;
                Some((m.byte_range.clone(), repl))
            })
            .collect();
        let count = pairs.len();
        match buffer.apply_byte_replacements(pairs) {
            Ok(replaced) => {
                use crate::search::ReplaceReport;
                self.search_state.last_replace_report = Some(ReplaceReport {
                    replaced,
                    files_affected: if replaced > 0 { 1 } else { 0 },
                    failures: Vec::new(),
                });
                self.search_state.invalidate_file_cache();
            }
            Err(e) => {
                self.error_message = Some(format!("Replace All failed: {}", e.message));
            }
        }
        let _ = count;
    }

    /// Project-scope Replace All: replace in all files.
    fn do_replace_all_project(&mut self, _ctx: egui::Context) {
        let Some(pattern) = self.search_state.compiled_pattern() else {
            return;
        };
        let matches = self.search_state.project_matches.clone();
        let replacement_str = self.search_state.query.replacement.clone();
        let use_regex = self.search_state.query.use_regex;
        // Group matches by file.
        let mut by_file: std::collections::HashMap<PathBuf, Vec<_>> =
            std::collections::HashMap::new();
        for m in &matches {
            by_file.entry(m.path.clone()).or_default().push(m.clone());
        }
        let mut total_replaced = 0usize;
        let mut files_affected = 0usize;
        let mut failures: Vec<(PathBuf, String)> = Vec::new();
        // Snapshot open buffer paths before the loop to satisfy the borrow checker.
        let open_paths: Vec<PathBuf> = self.buffers.keys().cloned().collect();
        for (file_path, file_matches) in &by_file {
            // Check whether the file is open in a buffer.
            let is_open = open_paths.contains(file_path);
            if is_open {
                // Apply to in-memory buffer.
                let Some(buffer) = self.buffers.get_mut(file_path) else {
                    continue;
                };
                let text = buffer.to_full_string();
                let pairs: Vec<_> = file_matches
                    .iter()
                    .filter_map(|m| {
                        // Validate the match still exists.
                        if text.get(m.byte_range.clone()) != Some(&text[m.byte_range.clone()]) {
                            return None;
                        }
                        let repl = compute_replacement(
                            &text,
                            m.byte_range.clone(),
                            pattern,
                            &replacement_str,
                            use_regex,
                        )?;
                        Some((m.byte_range.clone(), repl))
                    })
                    .collect();
                match buffer.apply_byte_replacements(pairs) {
                    Ok(n) => {
                        total_replaced += n;
                        if n > 0 {
                            files_affected += 1;
                        }
                    }
                    Err(e) => failures.push((file_path.clone(), e.message)),
                }
            } else {
                // Read from disk, apply, write atomically.
                let text = match std::fs::read_to_string(file_path) {
                    Ok(t) => t,
                    Err(e) => {
                        failures.push((file_path.clone(), e.to_string()));
                        continue;
                    }
                };
                let pairs: Vec<_> = file_matches
                    .iter()
                    .filter_map(|m| {
                        if text.get(m.byte_range.clone()) != Some(&text[m.byte_range.clone()]) {
                            return None;
                        }
                        let repl = compute_replacement(
                            &text,
                            m.byte_range.clone(),
                            pattern,
                            &replacement_str,
                            use_regex,
                        )?;
                        Some((m.byte_range.clone(), repl))
                    })
                    .collect();
                let count = pairs.len();
                match crate::search::apply_replacements(
                    &text,
                    pairs.iter().map(|(r, s)| (r.clone(), s.clone())).collect(),
                ) {
                    Ok(new_text) => {
                        // Write atomically: temp file + rename.
                        let tmp = file_path.with_extension("__bide_tmp");
                        if let Err(e) = std::fs::write(&tmp, &new_text)
                            .and_then(|_| std::fs::rename(&tmp, file_path))
                        {
                            let _ = std::fs::remove_file(&tmp);
                            failures.push((file_path.clone(), e.to_string()));
                        } else {
                            total_replaced += count;
                            files_affected += 1;
                        }
                    }
                    Err(e) => failures.push((file_path.clone(), e)),
                }
            }
        }
        use crate::search::ReplaceReport;
        self.search_state.last_replace_report = Some(ReplaceReport {
            replaced: total_replaced,
            files_affected,
            failures,
        });
        self.search_state.invalidate_file_cache();
    }

    /// Collect (path, text) snapshots of all open buffers for project search.
    fn open_file_snapshots(&self) -> Vec<(PathBuf, String)> {
        self.buffers
            .iter()
            .map(|(p, b)| (p.clone(), b.to_full_string()))
            .collect()
    }

    /// Kick off a new project search using the current query.
    fn start_project_search(&mut self, ctx: egui::Context) {
        let Some(root) = self.primary_workspace_root() else {
            self.error_message = Some("No folder open for project search".to_owned());
            return;
        };
        let snapshots = self.open_file_snapshots();
        self.search_state.start_project_search(root, snapshots, ctx);
    }

    fn show_confirmation(&mut self, context: &egui::Context) {
        if let Some(path) = self.pending_close.clone() {
            self.show_modal_backdrop(context);
            self.show_close_confirmation(context, path);
        } else if self.pending_exit {
            self.show_modal_backdrop(context);
            self.show_exit_confirmation(context);
        } else if self.search_state.pending_replace_confirm.is_some() {
            self.show_modal_backdrop(context);
            self.show_replace_all_confirmation(context);
        }
    }

    fn show_modal_backdrop(&self, context: &egui::Context) {
        let screen_rect = context.screen_rect();
        egui::Area::new(egui::Id::new("unsaved_changes_backdrop"))
            .order(egui::Order::Middle)
            .fixed_pos(screen_rect.min)
            .show(context, |ui| {
                let (rect, _) = ui.allocate_exact_size(screen_rect.size(), egui::Sense::click());
                ui.painter()
                    .rect_filled(rect, 0.0, Color32::from_black_alpha(120));
            });
    }

    /// Show a destructive-action confirmation dialog before Replace All.
    ///
    /// The dialog is triggered by `SearchState::request_replace_confirm()` and
    /// cleared by either the Yes or Cancel button.  The actual replacement is
    /// dispatched here (not in the panel) to keep the panel output simple.
    fn show_replace_all_confirmation(&mut self, context: &egui::Context) {
        let Some((match_count, file_count)) = self.search_state.pending_replace_confirm else {
            return;
        };

        let mut confirmed = false;
        let mut cancelled = false;

        egui::Window::new("Replace All")
            .id(egui::Id::new("replace_all_confirm_dialog"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .fixed_size(egui::vec2(360.0, 110.0))
            .show(context, |ui| {
                ui.label(format!(
                    "Replace {match_count} occurrence(s) in {file_count} file(s)?"
                ));
                ui.label("This operation cannot be undone.");
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Yes, replace all").clicked() {
                        confirmed = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                });
            });

        if confirmed {
            self.search_state.pending_replace_confirm = None;
            // Dispatch: file-scope or project-scope.
            let active_path = self.active.clone();
            if let Some(ref path) = active_path {
                let p = path.clone();
                self.do_replace_all(&p, context.clone());
            }
        } else if cancelled {
            self.search_state.cancel_replace_confirm();
        }
    }

    fn show_close_confirmation(&mut self, context: &egui::Context, path: PathBuf) {
        let filename = file_name(&path);
        let mut close_anyway = false;
        let mut save_and_close = false;
        let mut cancel = false;
        let request_cancel_focus = self.focus_cancel_on_modal_open;
        self.focus_cancel_on_modal_open = false;

        egui::Window::new("Unsaved changes")
            .id(egui::Id::new("unsaved_changes_dialog"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .fixed_size(egui::vec2(320.0, 100.0))
            .show(context, |ui| {
                ui.label(format!("\"{filename}\" has unsaved changes."));
                ui.label("Close without saving?");
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Close anyway").clicked() {
                        close_anyway = true;
                    }
                    if ui.button("Save and close").clicked() {
                        save_and_close = true;
                    }
                    let cancel_response = ui.button("Cancel");
                    if request_cancel_focus {
                        cancel_response.request_focus();
                    }
                    if cancel_response.clicked() {
                        cancel = true;
                    }
                });
            });

        if context.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) {
            cancel = true;
        }
        if cancel {
            self.pending_close = None;
        } else if close_anyway {
            self.pending_close = None;
            self.close_file(&path);
        } else if save_and_close {
            match self.save_and_close(&path) {
                Ok(()) => {
                    self.error_message = None;
                    self.pending_close = None;
                }
                Err(error) => {
                    self.error_message =
                        Some(format!("Could not save {}: {error}", path.display()));
                }
            }
        }
    }

    fn show_exit_confirmation(&mut self, context: &egui::Context) {
        let count = self
            .buffers
            .values()
            .filter(|buffer| buffer.is_modified())
            .count();
        let mut confirm = false;
        let mut cancel = false;
        let request_cancel_focus = self.focus_cancel_on_modal_open;
        self.focus_cancel_on_modal_open = false;

        egui::Window::new("Unsaved changes")
            .id(egui::Id::new("unsaved_changes_dialog"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .show(context, |ui| {
                ui.label(format!(
                    "{count} file(s) have unsaved changes. Exit anyway?"
                ));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Yes").clicked() {
                        confirm = true;
                    }
                    let cancel_response = ui.button("Cancel");
                    if request_cancel_focus {
                        cancel_response.request_focus();
                    }
                    if cancel_response.clicked() {
                        cancel = true;
                    }
                });
            });

        if context.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) {
            cancel = true;
        }
        if cancel {
            self.pending_exit = false;
        } else if confirm {
            self.pending_exit = false;
            self.allow_close = true;
            context.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    fn save_and_close(&mut self, path: &Path) -> io::Result<()> {
        let buffer = self
            .buffers
            .get_mut(path)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "buffer is missing"))?;
        buffer.save_to_file(path)?;
        self.close_file(path);
        Ok(())
    }

    fn command_specs(&self) -> Vec<CommandSpec> {
        let mut commands = vec![
            CommandSpec::new(CommandId::QuickOpen, "File", "Quick Open", Some("Ctrl+P")),
            CommandSpec::new(CommandId::OpenFolder, "File", "Open Folder…", None),
            CommandSpec::new(
                CommandId::AddFolderToWorkspace,
                "File",
                "Add Folder to Workspace…",
                None,
            ),
            CommandSpec::new(CommandId::OpenFile, "File", "Open File…", Some("Ctrl+O")),
            CommandSpec::new(CommandId::GoToLine, "Go", "Go to Line...", Some("Ctrl+G")),
            CommandSpec::new(
                CommandId::GoToSymbol,
                "Go",
                "Go to Symbol...",
                Some("Ctrl+T"),
            ),
            CommandSpec::new(
                CommandId::SelectTheme,
                "Preferences",
                "Select Theme…",
                Some("Ctrl+Alt+T"),
            ),
            CommandSpec::new(
                CommandId::ToggleVimMode,
                "Editor",
                "Toggle Vim Mode",
                Some("Ctrl+Alt+V"),
            ),
            CommandSpec::new(
                CommandId::ToggleAssistant,
                "AI",
                "Toggle Assistant Panel",
                Some("Ctrl+Alt+A"),
            ),
            CommandSpec::new(
                CommandId::NewTerminal,
                "Run",
                "New Terminal",
                Some("Ctrl+Shift+5"),
            ),
            CommandSpec::new(
                CommandId::SplitEditorRight,
                "Window",
                "Split Editor Right",
                Some("Ctrl+\\"),
            ),
            CommandSpec::new(
                CommandId::SplitEditorDown,
                "Window",
                "Split Editor Down",
                None,
            ),
            CommandSpec::new(
                CommandId::FocusNextGroup,
                "Window",
                "Focus Next Group",
                Some("Ctrl+Alt+Right"),
            ),
            CommandSpec::new(
                CommandId::FocusPreviousGroup,
                "Window",
                "Focus Previous Group",
                Some("Ctrl+Alt+Left"),
            ),
            CommandSpec::new(
                CommandId::ToggleTree,
                "View",
                if self.show_tree {
                    "Hide File Tree"
                } else {
                    "Show File Tree"
                },
                Some("Ctrl+\\"),
            ),
            CommandSpec::new(
                CommandId::ToggleGitPanel,
                "View",
                if self.show_git_panel {
                    "Hide Git Panel"
                } else {
                    "Show Git Panel"
                },
                Some("Ctrl+Shift+G"),
            ),
            CommandSpec::new(
                CommandId::ToggleProblems,
                "View",
                if self.show_bottom_panel && self.bottom_panel_tab == BottomPanelTab::Problems {
                    "Hide Problems"
                } else {
                    "Show Problems"
                },
                Some("Ctrl+Shift+M"),
            ),
            CommandSpec::new(
                CommandId::ToggleTerminal,
                "View",
                if self.show_bottom_panel && self.bottom_panel_tab == BottomPanelTab::Terminal {
                    "Hide Terminal"
                } else {
                    "Show Terminal"
                },
                Some("Ctrl+`"),
            ),
            CommandSpec::new(
                CommandId::ToggleOutline,
                "View",
                if self.outline_panel.show {
                    "Hide Outline"
                } else {
                    "Show Outline"
                },
                Some("Ctrl+Shift+O"),
            ),
            CommandSpec::new(
                CommandId::ToggleMinimap,
                "View",
                {
                    let pane_id = self.focus.active_pane;
                    if self
                        .minimap_states
                        .get(&pane_id)
                        .map_or(true, |m| m.visible)
                    {
                        "Hide Minimap"
                    } else {
                        "Show Minimap"
                    }
                },
                Some("Ctrl+Shift+\\"),
            ),
            CommandSpec::new(
                CommandId::OpenSettings,
                "Preferences",
                "Open Settings",
                None,
            ),
            CommandSpec::new(
                CommandId::ReloadSettings,
                "Preferences",
                "Reload Settings",
                None,
            ),
            CommandSpec::new(
                CommandId::NewProject,
                "File",
                "New Project…",
                None,
            ),
        ];

        // ── Git commands ── shown whenever a repository is open ───────────────
        if self.git.is_some() {
            commands.extend([
                CommandSpec::new(
                    CommandId::GitFetch,
                    "Git",
                    "Fetch",
                    Some("Alt+Shift+F"),
                ),
                CommandSpec::new(
                    CommandId::GitPull,
                    "Git",
                    "Pull",
                    Some("Alt+Shift+L"),
                ),
                CommandSpec::new(
                    CommandId::GitPush,
                    "Git",
                    "Push",
                    Some("Alt+Shift+U"),
                ),
                CommandSpec::new(
                    CommandId::GitShowLog,
                    "Git",
                    "Show Commit History",
                    Some("Alt+Shift+H"),
                ),
                CommandSpec::new(
                    CommandId::GitShowTags,
                    "Git",
                    "Manage Tags",
                    None,
                ),
                CommandSpec::new(
                    CommandId::GitShowConflicts,
                    "Git",
                    "Resolve Conflicts",
                    None,
                ),
                CommandSpec::new(
                    CommandId::GitStashSave,
                    "Git",
                    "Stash Changes",
                    Some("Alt+Shift+S"),
                ),
                CommandSpec::new(
                    CommandId::GitStashPop,
                    "Git",
                    "Pop Stash",
                    Some("Alt+Shift+P"),
                ),
                CommandSpec::new(
                    CommandId::GitToggleBlame,
                    "Git",
                    if self
                        .active
                        .as_ref()
                        .map(|p| *self.show_blame.get(p).unwrap_or(&false))
                        .unwrap_or(false)
                    {
                        "Hide Blame"
                    } else {
                        "Show Blame"
                    },
                    Some("Ctrl+Shift+B"),
                ),
            ]);
        }

        if self.active.is_some() {
            commands.extend([
                CommandSpec::new(CommandId::Save, "File", "Save", Some("Ctrl+S")),
                CommandSpec::new(CommandId::CloseTab, "File", "Close Tab", Some("Ctrl+W")),
                CommandSpec::new(
                    CommandId::FindInFile,
                    "Edit",
                    "Find in File",
                    Some("Ctrl+F"),
                ),
                CommandSpec::new(
                    CommandId::ReplaceInFile,
                    "Edit",
                    "Replace in File",
                    Some("Ctrl+H"),
                ),
                CommandSpec::new(CommandId::SortLinesAscending, "Edit", "Sort Lines Ascending", None),
                CommandSpec::new(CommandId::SortLinesDescending, "Edit", "Sort Lines Descending", None),
                CommandSpec::new(CommandId::TransformUppercase, "Edit", "Transform to UPPERCASE", None),
                CommandSpec::new(CommandId::TransformLowercase, "Edit", "Transform to lowercase", None),
                CommandSpec::new(CommandId::TransformTitleCase, "Edit", "Transform to Title Case", None),
                CommandSpec::new(CommandId::TransformCamelCase, "Edit", "Transform to camelCase", None),
                CommandSpec::new(CommandId::TransformSnakeCase, "Edit", "Transform to snake_case", None),
                CommandSpec::new(CommandId::TransformPascalCase, "Edit", "Transform to PascalCase", None),
                CommandSpec::new(CommandId::TransformKebabCase, "Edit", "Transform to kebab-case", None),
                CommandSpec::new(CommandId::ToggleUndoHistory, "View", "Undo History", Some("Ctrl+Shift+U")),
                CommandSpec::new(CommandId::ToggleCallHierarchy, "LSP", "Show Call Hierarchy", Some("Ctrl+Shift+H")),
                CommandSpec::new(CommandId::ToggleTypeHierarchy, "LSP", "Show Type Hierarchy", Some("Ctrl+Shift+T")),
            ]);
        }
        if self.buffers.len() > 1 {
            commands.extend([
                CommandSpec::new(
                    CommandId::NextTab,
                    "Navigation",
                    "Next Tab",
                    Some("Ctrl+Tab"),
                ),
                CommandSpec::new(
                    CommandId::PreviousTab,
                    "Navigation",
                    "Previous Tab",
                    Some("Ctrl+Shift+Tab"),
                ),
            ]);
        }
        // ── Task commands (when tasks.toml is loaded) ──────────────────────────
        if !self.task_panel.tasks.is_empty() {
            let mut task_names: Vec<String> = self.task_panel.tasks.keys().cloned().collect();
            task_names.sort();
            for name in task_names {
                commands.push(CommandSpec::new(
                    CommandId::RunTask(name.clone()),
                    "Run",
                    format!("Run Task: {name}"),
                    None,
                ));
            }
            if self.task_panel.last_task.is_some() {
                commands.push(CommandSpec::new(
                    CommandId::RerunLastTask,
                    "Run",
                    "Rerun Last Task",
                    Some("Ctrl+Shift+B"),
                ));
                if self.task_panel.running.as_ref().is_some_and(|h| h.is_running()) {
                    commands.push(CommandSpec::new(
                        CommandId::TerminateTask,
                        "Run",
                        "Terminate Running Task",
                        None,
                    ));
                }
            }
        }

        commands
    }

    fn open_command_palette(&mut self) {
        self.on_modal_opened();
        let commands = self.command_specs();
        self.launcher.open_commands(commands);
    }

    fn open_quick_open(&mut self, context: &egui::Context) {
        self.on_modal_opened();
        let roots = if self.workspace.roots().is_empty() {
            self.primary_workspace_root()
                .map_or(vec![], |root| vec![root])
        } else {
            self.workspace
                .roots()
                .iter()
                .map(|root| root.path.clone())
                .collect()
        };
        self.launcher
            .open_files(roots, context);
    }

    fn restore_editor_focus(&mut self) {
        if let Some(state) = self
            .active
            .as_ref()
            .and_then(|path| self.editor_states.get_mut(path))
        {
            state.request_focus();
        }
    }

    fn handle_launcher_event(&mut self, event: LauncherEvent, context: &egui::Context) {
        match event {
            LauncherEvent::Dismissed => self.restore_editor_focus(),
            LauncherEvent::OpenFile(path) => match self.open_file(path.clone()) {
                Ok(()) => {
                    self.error_message = None;
                    self.launcher.dismiss();
                    self.restore_editor_focus();
                }
                Err(error) => {
                    let message = format!("Could not open {}: {error}", path.display());
                    self.error_message = Some(message.clone());
                    self.launcher.set_error(message);
                }
            },
            LauncherEvent::Execute(CommandId::QuickOpen) => self.open_quick_open(context),
            LauncherEvent::Execute(command) => {
                self.launcher.dismiss();
                self.execute_command(command, context);
                if !self.has_modal() {
                    self.restore_editor_focus();
                }
            }
        }
    }

    // ── Plugin helpers ────────────────────────────────────────────────────────

    /// Build a `PluginApiContext` snapshot from the current app state.
    pub fn plugin_context_snapshot(&self) -> PluginApiContext {
        let active_file = self.active.clone();
        let (buffer_content, cursor_line, cursor_col, language) =
            match active_file.as_ref().and_then(|p| self.buffers.get(p)) {
                Some(buf) => {
                    let cur = buf.cursor();
                    let lang = buf.language().display_label().to_lowercase();
                    (buf.text(), cur.line, cur.col, lang)
                }
                None => (String::new(), 0, 0, "unknown".to_string()),
            };
        let workspace_root = active_file
            .as_ref()
            .and_then(|path| self.workspace_root_for_path(path))
            .or_else(|| self.primary_workspace_root());
        PluginApiContext {
            active_file,
            buffer_content,
            cursor_line,
            cursor_col,
            workspace_root,
            language,
        }
    }

    /// Apply a batch of `PluginAction`s that were returned by the plugin system.
    /// Notifications and menu registrations are forwarded to the plugin system itself;
    /// buffer-mutation actions are applied directly to the editor state.
    pub fn apply_plugin_actions(&mut self, actions: Vec<PluginAction>) {
        for action in actions {
            match action {
                PluginAction::SetCursor { line, col } => {
                    if let Some(path) = &self.active.clone() {
                        if let Some(buf) = self.buffers.get_mut(path) {
                            buf.set_cursor(CursorPosition { line, col });
                        }
                    }
                }
                PluginAction::InsertText(text) => {
                    if let Some(path) = &self.active.clone() {
                        if let Some(buf) = self.buffers.get_mut(path) {
                            let _ = buf.insert_at_cursor(&text);
                        }
                    }
                }
                PluginAction::ReplaceText(new_text) => {
                    if let Some(path) = &self.active.clone() {
                        if let Some(buf) = self.buffers.get_mut(path) {
                            let len = buf.len_chars();
                            let _ = buf.replace_char_range(0, len, &new_text);
                        }
                    }
                }
                PluginAction::OpenFile(path) => {
                    if let Err(e) = self.open_file(path.clone()) {
                        self.error_message =
                            Some(format!("Plugin: could not open {}: {e}", path.display()));
                    }
                }
                PluginAction::SaveFile => {
                    self.try_save_active();
                }
                PluginAction::Notify { message, level } => {
                    // Store as a plugin notification (displayed in status bar below).
                    self.plugin_system
                        .notifications
                        .push(crate::plugins::PluginNotification {
                            message,
                            level,
                            plugin_name: "plugin".to_string(),
                            created_at: std::time::Instant::now(),
                        });
                }
                PluginAction::AddMenuItem {
                    label,
                    callback_name,
                } => {
                    // Try to infer the plugin name from the most-recently invoked plugin.
                    // In most cases `split_actions` has already handled this path.
                    self.plugin_system
                        .menu_items
                        .push(crate::plugins::PluginMenuItem {
                            plugin_name: "plugin".to_string(),
                            label,
                            callback_name,
                        });
                }
            }
        }
    }

    /// Drain any queued actions that accumulated from top-level plugin code
    /// (e.g. `blue.show_menu_item(…)` at module scope during load).
    fn drain_plugin_actions(&mut self) {
        let pending = self.plugin_system.drain_pending_actions();
        for (name, actions) in pending {
            let remainder = self.plugin_system.split_actions(actions, &name);
            self.apply_plugin_actions(remainder);
        }
    }

    fn transform_active(&mut self, transform: CaseTransform) {
        if let Some(path) = self.active.clone() {
            if let Some(buffer) = self.buffers.get_mut(&path) {
                let _ = buffer.transform_selections(transform);
            }
        }
    }

    fn show_undo_history(&mut self, context: &egui::Context) {
        if !self.undo_history_panel_visible { return; }
        let Some(path) = self.active.clone() else { return; };
        let Some(buffer) = self.buffers.get_mut(&path) else { return; };
        let mut open = self.undo_history_panel_visible;
        egui::Window::new("Undo History")
            .open(&mut open)
            .default_size([300.0, 400.0])
            .show(context, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let past: Vec<(usize, String)> = buffer.undo_stack.past.iter().enumerate().rev().map(|(index, record)| {
                        let (first, last) = edit_line_range(buffer, record);
                        (index + 1, format!("{} · lines {}–{}", describe_edit(record), first + 1, last + 1))
                    }).collect();
                    for (target, label) in past {
                        if ui.button(format!("● {label}")).clicked() {
                            while buffer.undo_stack.past.len() > target { if !buffer.undo() { break; } }
                        }
                    }
                    ui.colored_label(self.active_palette.semantic.accent, "● Current state");
                    let future: Vec<(usize, String)> = buffer.undo_stack.future.iter().rev().enumerate().map(|(index, record)| {
                        let (first, last) = edit_line_range(buffer, record);
                        (index + 1, format!("{} · lines {}–{}", describe_edit(record), first + 1, last + 1))
                    }).collect();
                    for (steps, label) in future {
                        if ui.button(format!("○ {label}")).clicked() {
                            for _ in 0..steps { if !buffer.redo() { break; } }
                        }
                    }
                });
            });
        self.undo_history_panel_visible = open;
    }
    fn request_focus_for_target(&mut self, context: &egui::Context) {
        let focus_id = match self.focus_target {
            FocusTarget::MenuBar => "menu_bar",
            FocusTarget::SearchBar => "command_center_search",
            FocusTarget::Sidebar => "file_tree",
            FocusTarget::TabBar => "tabs",
            FocusTarget::Editor => "blue_ide_editor",
            FocusTarget::Terminal => "terminal",
        };
        context.memory_mut(|mem| {
            mem.request_focus(egui::Id::new(focus_id));
        });
    }

    fn execute_command(&mut self, command: CommandId, context: &egui::Context) {
        match command {
            CommandId::ShowCommandPalette => self.open_command_palette(),
            CommandId::QuickOpen => self.open_quick_open(context),
            CommandId::OpenFolder => {
                self.open_folder_dialog();
                let roots = self.workspace.roots().iter().map(|r| r.path.clone()).collect();
                self.launcher.refresh_workspace(roots, context.clone());
            }
            CommandId::AddFolderToWorkspace => {
                self.open_folder_dialog_with_mode(true);
                let roots = self.workspace.roots().iter().map(|r| r.path.clone()).collect();
                self.launcher.refresh_workspace(roots, context.clone());
            }
            CommandId::OpenFile => self.open_file_dialog(),
            CommandId::Save if self.active.is_some() => self.try_save_active(),
            CommandId::Save => {}
            CommandId::CloseTab => {
                if let Some(path) = self.active.clone() {
                    self.request_close_file(&path);
                }
            }
            CommandId::NextTab => self.cycle_tab(1),
            CommandId::PreviousTab => self.cycle_tab(-1),
            CommandId::GoToLine => {
                let current_line = self
                    .active
                    .as_ref()
                    .and_then(|path| self.buffers.get(path))
                    .map(|buffer| buffer.cursor().line)
                    .unwrap_or(0);
                self.on_modal_opened();
                self.goto_line.open_for(current_line);
            }
            CommandId::GoToSymbol => {
                self.on_modal_opened();
                self.workspace_symbol.open();
            }
            CommandId::NewTerminal => {
                if !self.trust_allows(crate::workspace::ExecutableCapability::Terminal) {
                    self.error_message = Some(
                        "Terminals require a trusted workspace. Click the trust badge to enable."
                            .to_owned(),
                    );
                    return;
                }
                let cwd = self.primary_workspace_root();
                let env = self.env_editor.enabled_vars();
                self.term_sessions
                    .create_session(cwd, crate::terminal::ShellKind::default_shell(), &env);
                self.term_split.clamp_indices(self.term_sessions.len());
                self.bottom_panel_tab = BottomPanelTab::Terminal;
                self.show_bottom_panel = true;
                self.show_problems = false;
            }
            CommandId::EditEnvVars => {
                if let Some(root) = self.primary_workspace_root() {
                    self.env_editor.open_for(root);
                } else {
                    eprintln!("[env_editor] No project root open");
                }
            }
            CommandId::OpenHistoryBrowser => {
                self.term_history.open();
            }
            CommandId::SelectTheme => {
                self.on_modal_opened();
                self.theme_picker.open = true;
                self.theme_picker.query.clear();
                self.theme_picker.selected = 0;
                self.theme_picker.request_focus = true;
            }
            CommandId::ToggleVimMode => {
                self.settings.editor.vim_mode = !self.settings.editor.vim_mode;
                let mut draft = self.settings.clone();
                draft.editor.vim_mode = self.settings.editor.vim_mode;
                if self.settings_store.save(&draft).is_ok() {
                    self.settings = draft;
                }
                self.error_message = Some(format!(
                    "Vim mode {}",
                    if self.settings.editor.vim_mode {
                        "enabled (Ctrl+Alt+V to disable)"
                    } else {
                        "disabled"
                    }
                ));
            }
            CommandId::ToggleAssistant => {
                self.assistant.open = !self.assistant.open;
            }
            CommandId::SplitEditorRight => {
                self.pane_actions.push(PaneAction::SplitH {
                    pane: self.focus.active_pane,
                });
            }
            CommandId::SplitEditorDown => {
                self.pane_actions.push(PaneAction::SplitV {
                    pane: self.focus.active_pane,
                });
            }
            CommandId::FocusNextGroup => {
                self.focus.cycle_next(&self.pane_tree);
                self.sync_active_from_focused_pane();
            }
            CommandId::FocusPreviousGroup => {
                self.focus.cycle_prev(&self.pane_tree);
                self.sync_active_from_focused_pane();
            }
            CommandId::ToggleTree => self.show_tree = !self.show_tree,
            CommandId::ToggleGitPanel => self.toggle_git_panel(),
            CommandId::ToggleProblems => self.toggle_problems_panel(),
            CommandId::ToggleTerminal => self.toggle_terminal_panel(),
            CommandId::ToggleCallHierarchy => self.toggle_call_hierarchy_panel(),
            CommandId::ToggleTypeHierarchy => self.toggle_type_hierarchy_panel(),
            CommandId::ToggleOutline => self.outline_panel.show = !self.outline_panel.show,
            CommandId::ToggleMinimap => {
                let pane_id = self.focus.active_pane;
                let state = self
                    .minimap_states
                    .entry(pane_id)
                    .or_insert_with(crate::editor::minimap::MinimapState::new);
                state.toggle_visible();
            }
            CommandId::ToggleZenMode => {
                self.zen.zen_mode = !self.zen.zen_mode;
            }
            CommandId::ToggleDistractionFree => {
                self.zen.distraction_free = !self.zen.distraction_free;
            }
            CommandId::OpenDiffWithHead => {
                if let Some(path) = self.active.clone() {
                    self.open_diff_with_head(path);
                }
            }
            CommandId::FindInFile => {
                self.search_state.open_find();
                self.search_state.query.scope = SearchScope::Project;
                self.search_state.recompile();
            }
            CommandId::ReplaceInFile => {
                self.search_state.open_replace();
                self.search_state.query.scope = SearchScope::Project;
                self.search_state.recompile();
            }
            CommandId::SortLinesAscending => { if let Some(path) = self.active.clone() { let _ = self.buffers.get_mut(&path).map(|buffer| buffer.sort_selected_lines(false)); } }
            CommandId::SortLinesDescending => { if let Some(path) = self.active.clone() { let _ = self.buffers.get_mut(&path).map(|buffer| buffer.sort_selected_lines(true)); } }
            CommandId::TransformUppercase => self.transform_active(CaseTransform::Upper),
            CommandId::TransformLowercase => self.transform_active(CaseTransform::Lower),
            CommandId::TransformTitleCase => self.transform_active(CaseTransform::Title),
            CommandId::TransformCamelCase => self.transform_active(CaseTransform::Camel),
            CommandId::TransformSnakeCase => self.transform_active(CaseTransform::Snake),
            CommandId::TransformPascalCase => self.transform_active(CaseTransform::Pascal),
            CommandId::TransformKebabCase => self.transform_active(CaseTransform::Kebab),
            CommandId::ToggleUndoHistory => self.undo_history_panel_visible = !self.undo_history_panel_visible,
            CommandId::OpenSettings => self.open_settings(),
            CommandId::ReloadSettings => self.reload_settings(),
            CommandId::NewProject => {
                self.on_modal_opened();
                self.new_project.open();
            }
            CommandId::RunTask(name) => {
                self.run_task(&name.clone());
            }
            CommandId::RerunLastTask => {
                if !self.trust_allows(crate::workspace::ExecutableCapability::Command) {
                    self.error_message = Some(
                        "Tasks require a trusted workspace. Click the trust badge to enable."
                            .to_owned(),
                    );
                    return;
                }
                if let Some(root) = self.primary_workspace_root() {
                    self.task_panel.rerun_last(&root);
                    self.show_bottom_panel = true;
                    self.bottom_panel_tab = BottomPanelTab::Output;
                }
            }
            CommandId::TerminateTask => {
                self.task_panel.cancel();
            }
            CommandId::ReloadPlugins => {
                if !self.trust_allows(crate::workspace::ExecutableCapability::Plugin) {
                    self.error_message = Some(
                        "Plugins require a trusted workspace. Click the trust badge to enable."
                            .to_owned(),
                    );
                    return;
                }
                self.plugin_system.reload_all();
                self.drain_plugin_actions();
                let count = self.plugin_system.plugin_count();
                self.plugin_system
                    .notifications
                    .push(crate::plugins::PluginNotification {
                        message: format!("Plugins reloaded ({} loaded)", count),
                        level: NotifyLevel::Info,
                        plugin_name: "system".to_string(),
                        created_at: std::time::Instant::now(),
                    });
            }
            CommandId::InvokePluginMenuItem(label) => {
                let snap = self.plugin_context_snapshot();
                let actions = self.plugin_system.invoke_menu_item(&label, &snap);
                self.apply_plugin_actions(actions);
            }
            // ── Git commands ───────────────────────────────────────────────────
            CommandId::GitToggleBlame => self.toggle_blame_for_active(),
            CommandId::GitFetch => self.start_network_op(crate::git::NetworkOp::Fetch),
            CommandId::GitPull => self.start_network_op(crate::git::NetworkOp::Pull),
            CommandId::GitPush => self.start_network_op(crate::git::NetworkOp::Push),
            CommandId::GitShowLog => {
                if let Some(git) = self.git.as_ref() {
                    self.git_log_cache = git.commit_log(200);
                }
                self.show_git_log = true;
            }
            CommandId::GitShowTags => {
                self.tag_new_name.clear();
                self.tag_new_message.clear();
                self.show_tag_manager = true;
            }
            CommandId::GitShowConflicts => {
                self.refresh_conflicts();
                self.show_conflict_resolver = true;
            }
            CommandId::GitStashSave => {
                // Save with an empty message (uses "WIP") and include untracked.
                if let Some(git) = &mut self.git {
                    if let Err(error) = git.stash_save("", true) {
                        self.error_message =
                            Some(format!("Stash save failed: {}", error.message()));
                    } else {
                        git.dirty = true;
                    }
                }
            }
            CommandId::GitStashPop => {
                if let Some(git) = &mut self.git {
                    if let Err(error) = git.stash_pop(0) {
                        self.error_message =
                            Some(format!("Stash pop failed: {}", error.message()));
                    } else {
                        git.dirty = true;
                    }
                }
                self.reload_open_buffers_from_disk();
            }
        }
    }
}

fn describe_edit(record: &EditRecord) -> String {
    let description = match record.operations.as_slice() {
        [RopeOp::Insert { text, .. }] if text.chars().count() == 1 => format!("Typed '{}'", text),
        [RopeOp::Insert { text, .. }] => format!("Inserted {} chars", text.chars().count()),
        [RopeOp::Delete { deleted, .. }] if deleted.chars().count() == 1 => format!("Deleted '{}'", deleted),
        [RopeOp::Delete { deleted, .. }] => format!("Deleted {} chars", deleted.chars().count()),
        operations => format!("Compound edit ({} ops)", operations.len()),
    };
    format!("{}s ago · {description}", record.timestamp.elapsed().as_secs())
}

fn edit_line_range(buffer: &TextBuffer, record: &EditRecord) -> (usize, usize) {
    let mut offsets = record.operations.iter().map(|operation| match operation {
        RopeOp::Insert { char_offset, text } => (*char_offset, *char_offset + text.chars().count()),
        RopeOp::Delete { char_offset, length, .. } => (*char_offset, *char_offset + *length),
    });
    let Some((mut first, mut last)) = offsets.next() else { return (0, 0); };
    for (start, end) in offsets { first = first.min(start); last = last.max(end); }
    let first_line = buffer.char_index_to_position(first.min(buffer.len_chars())).map_or(0, |position| position.line);
    let last_line = buffer.char_index_to_position(last.min(buffer.len_chars())).map_or(first_line, |position| position.line);
    (first_line, last_line)
}

impl eframe::App for BlueIdeApp {
    fn update(&mut self, context: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(active_path) = self.active.clone() {
            self.touch_recent_file(active_path);
        }
        self.show_undo_history(context);
        let reported_system_scheme = match frame.info().system_theme {
            Some(eframe::Theme::Light) => Some(ColorScheme::Light),
            Some(eframe::Theme::Dark) => Some(ColorScheme::Dark),
            None => None,
        };
        if reported_system_scheme != self.system_scheme {
            self.system_scheme = reported_system_scheme;
            let preview_theme = self
                .settings_draft
                .as_ref()
                .map(|draft| draft.appearance.theme)
                .unwrap_or(self.settings.appearance.theme);
            if preview_theme == crate::settings::Theme::System {
                let appearance = self
                    .settings_draft
                    .as_ref()
                    .map(|draft| &draft.appearance)
                    .unwrap_or(&self.settings.appearance);
                self.active_palette =
                    Self::apply_appearance_settings(context, appearance, self.system_scheme);
            }
        }
        self.search_state.poll_project_results();
        self.refresh_git_state();
        self.poll_blame_result();
        self.poll_network_result();
        self.poll_tasks();
        self.poll_auto_save(context);

        // ── Memory RSS polling (at most every 2 seconds) ─────────────────────
        let memory_poll_interval = std::time::Duration::from_secs(2);
        if self.memory_last_poll.elapsed() >= memory_poll_interval {
            if let Some(rss) = crate::perf::memory::get_rss_bytes() {
                self.memory_rss = rss;
            }
            self.memory_last_poll = std::time::Instant::now();
        }

        // Poll file watcher for live-reload
        let changed_files = self.file_watcher.poll();
        for path in changed_files {
            // Reload image viewer states that reference this path
            for (_pane_id, state) in &mut self.image_viewer_states {
                if state.path == path {
                    state.load(context);
                }
            }
            // Invalidate markdown preview states — they'll re-render from buffer next frame
            for (_pane_id, state) in &mut self.markdown_preview_states {
                if state.path == path {
                    state.last_changed = context.input(|i| i.time);
                }
            }
        }

        let close_requested = context.input(|input| input.viewport().close_requested());
        if close_requested
            && !self.allow_close
            && self.buffers.values().any(TextBuffer::is_modified)
        {
            context.send_viewport_cmd(ViewportCommand::CancelClose);
            self.on_modal_opened();
            self.pending_exit = true;
            self.focus_cancel_on_modal_open = true;
        }

        if self.has_modal() {
            self.on_modal_opened();
        }

        // Handle zen mode input + transition animation
        let delta_time = context.input(|i| i.unstable_dt).clamp(0.0, 0.1);
        self.zen.handle_input(context, self.has_modal());
        self.zen.update_transition(delta_time);

        self.handle_shortcuts(context);
        let menu_command = self.show_menu(context);
        let editor_command = self.show_workspace_panels(context);
        self.apply_pane_actions();
        self.poll_lsp();
        self.check_format_on_save_timeout();
        // Drain PTY output for every terminal session each frame so background
        // sessions keep up even when the panel is hidden.
        self.term_sessions.poll_all();

        if let Some(command) = menu_command {
            self.execute_command(command, context);
        }
        match editor_command {
            Some(EditorCommand::Open) => self.open_file_dialog(),
            Some(EditorCommand::Save) => self.try_save_active(),
            Some(EditorCommand::ToggleUndoHistory) => {
                self.undo_history_panel_visible = !self.undo_history_panel_visible
            }
            None => {}
        }

        if let Some(event) = self.launcher.show(context, self.active_palette.semantic) {
            self.handle_launcher_event(event, context);
        }

        self.sync_lsp_changes();
        self.update_outline_panel_tracking();

        // ── Plugin event dispatch ─────────────────────────────────────────────
        if let Some(path) = self.active.clone() {
            let (cur_line, cur_col, needs_sync) = self
                .buffers
                .get(&path)
                .map(|buf| {
                    let cur = buf.cursor();
                    (cur.line, cur.col, buf.needs_lsp_sync())
                })
                .unwrap_or((0, 0, false));

            let snap = self.plugin_context_snapshot();
            let cursor_actions = self
                .plugin_system
                .dispatch_event(PluginEvent::CursorMoved(cur_line, cur_col), &snap);
            self.apply_plugin_actions(cursor_actions);

            if needs_sync {
                let text_actions = self
                    .plugin_system
                    .dispatch_event(PluginEvent::TextChanged(path.clone()), &snap);
                self.apply_plugin_actions(text_actions);
            }
        }

        // Expire plugin notifications older than 4 seconds.
        self.plugin_system
            .expire_notifications(std::time::Duration::from_secs(4));

        // Drain any plugin actions queued outside an explicit event.
        let pending = self.plugin_system.drain_pending_actions();
        for (name, actions) in pending {
            let remainder = self.plugin_system.split_actions(actions, &name);
            self.apply_plugin_actions(remainder);
        }

        self.show_confirmation(context);
        self.show_settings(context);
        self.show_goto_line_modal(context);
        self.show_theme_picker(context);
        self.show_workspace_symbol_picker(context);
        self.show_code_action_picker(context);
        self.show_signature_help_popup(context);
        self.show_recent_workspaces_picker(context);
        self.show_recent_files_picker(context);
        self.show_new_tab_group_modal(context);

        // ── Startup breakdown window ──────────────────────────────────────────
        if let Some(data) = self.startup_data.clone() {
            if let Some(copy_text) = crate::perf::startup::show_startup_breakdown(
                context,
                &mut self.startup_breakdown,
                &data,
                self.active_palette.semantic,
            ) {
                context.copy_text(copy_text);
            }
        }

        // ── New Project wizard ────────────────────────────────────────────────
        let wizard_result = crate::project_template::show_wizard(
            context,
            &mut self.new_project,
            self.active_palette.semantic,
        );
        if let Some(project_path) = wizard_result {
            if self.new_project.open_after {
                self.open_workspace_folder(project_path, false);
                self.new_project.open = false;
            }
        }

        // ── Trust prompt ──────────────────────────────────────────────────────
        let trust_result = crate::trust_ui::show_trust_prompt(
            context,
            &mut self.trust_prompt,
            self.active_palette.semantic,
        );
        if let Some((path, state)) = trust_result {
            if let Some(trust_store) = &mut self.trust_store {
                // Find the matching workspace root and update trust
                let roots = self.workspace.roots().to_vec();
                for root in &roots {
                    if root.path == path || root.canonical_path == path {
                        let _ = trust_store.set(root, state);
                        break;
                    }
                }
            }

            // Unlock (or re-lock) executable capabilities as soon as the user
            // decides. LSP/plugins are started lazily here so an untrusted
            // folder is never auto-spawned at open time.
            if state == crate::workspace::TrustState::Trusted {
                self.lsp_manager.mark_root_trusted(&path);
                self.start_lsp(path.clone());
                let plugin_dir = path.join(".blue").join("plugins");
                self.plugin_system.reload_all();
                self.plugin_system.load_all(&plugin_dir);
                self.drain_plugin_actions();
            } else {
                self.lsp_manager.revoke_all();
            }
        }

        // ── Trust management popup ────────────────────────────────────────────
        if self.trust_management_open {
            let ws_root = self.workspace.roots().first().cloned();
            if let Some(trust_store) = self.trust_store.as_mut() {
                if let Some(root) = &ws_root {
                    crate::trust_ui::show_trust_management(
                        context,
                        root,
                        trust_store,
                        self.active_palette.semantic,
                        &mut self.trust_management_open,
                    );
                }
            }

            // Reconcile executable capabilities after the management popup may
            // have changed the trust state.
            if let (Some(root), Some(ts)) = (ws_root.as_ref(), self.trust_store.as_ref()) {
                if ts.permits(root, crate::workspace::ExecutableCapability::Plugin) {
                    self.lsp_manager.mark_root_trusted(&root.path);
                    self.start_lsp(root.path.clone());
                } else {
                    self.lsp_manager.revoke_all();
                }
            }
        }

        // ── Feature 5: Command History Browser ───────────────────────────────
        if let Some(paste_cmd) = crate::terminal::history::render_history_browser(
            context,
            &mut self.term_history,
            self.active_palette.semantic,
        ) {
            // Paste the selected command into the active terminal session
            if let Some(session) = self.term_sessions.active_mut() {
                session.write_str(&paste_cmd);
            }
        }

        // ── Feature 6: Environment Variable Editor side panel ─────────────────
        if self.env_editor.open {
            let project_name = self
                .tree
                .root_path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Project".to_owned());
            egui::SidePanel::right("env_editor_panel")
                .resizable(true)
                .default_width(420.0)
                .show(context, |ui| {
                    // Close button at top
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("✕").on_hover_text("Close").clicked() {
                                self.env_editor.open = false;
                            }
                        });
                    });
                    crate::terminal::env_editor::render_env_editor(
                        ui,
                        &mut self.env_editor,
                        &project_name,
                    );
                });
        }

        // ── Terminal toast (file not found, etc.) ─────────────────────────────
        if let Some((msg, color, ts)) = &self.term_toast {
            if ts.elapsed().as_secs_f32() < 3.0 {
                let msg_clone = msg.clone();
                let color_clone = *color;
                egui::Window::new("##term_toast")
                    .title_bar(false)
                    .resizable(false)
                    .collapsible(false)
                    .anchor(egui::Align2::RIGHT_BOTTOM, [-20.0, -40.0])
                    .show(context, |ui| {
                        ui.colored_label(color_clone, &msg_clone);
                    });
            } else {
                self.term_toast = None;
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_runtime_settings();
        self.save_session();
        #[cfg(test)]
        if let Some(ref mut client) = self.lsp {
            client.shutdown_and_join();
        }
        self.lsp_manager.shutdown_all();
        for session in &mut self.term_sessions.sessions {
            session.write_str("exit\n");
        }
    }
}

/// Render a single terminal session pane (for both split and single-pane layouts).
fn render_session_pane(
    ui: &mut egui::Ui,
    manager: &mut crate::terminal::session::SessionManager,
    session_idx: usize,
    ligatures_enabled: bool,
    ligature_renderer: &mut crate::text::ligature::LigatureRenderer,
) {
    let session_count = manager.len();
    if session_count == 0 {
        ui.centered_and_justified(|ui| {
            ui.label("No terminal sessions");
        });
        return;
    }
    let idx = session_idx.min(session_count - 1);
    let session = &mut manager.sessions[idx];

    let panel_width = ui.available_width();
    let panel_height = ui.available_height();
    let font_id = egui::FontId::monospace(13.0);
    let char_size = ui.fonts(|f| f.glyph_width(&font_id, 'M'));
    let line_height_val = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;

    let char_width = if char_size > 0.0 { char_size } else { 8.0 };
    let line_height = if line_height_val > 0.0 { line_height_val } else { 15.0 };

    let new_cols = ((panel_width / char_width) as u16).max(40);
    let new_rows = ((panel_height / line_height) as u16).max(2);
    session.pane.resize(new_rows, new_cols);

    let response = crate::terminal::renderer::render_terminal(
        ui,
        &mut session.pane.buffer,
        font_id,
        ligatures_enabled,
        ligatures_enabled.then_some(ligature_renderer),
    );

    if response.has_focus() || ui.memory(|m| m.has_focus(ui.id())) {
        ui.input(|i| {
            for event in &i.events {
                match event {
                    egui::Event::Text(s) => {
                        session.pane.write_str(s);
                    }
                    egui::Event::Key { key, pressed: true, modifiers, .. } => {
                        handle_terminal_key_session(&mut session.pane, *key, *modifiers);
                    }
                    _ => {}
                }
            }
        });
    }
}

/// Collect all terminal output lines from all sessions for search scanning.
fn collect_terminal_lines(
    manager: &crate::terminal::session::SessionManager,
) -> Vec<String> {
    let mut lines = Vec::new();
    for session in &manager.sessions {
        for sb_line in &session.pane.buffer.scroll_back {
            let s: String = sb_line.iter().map(|c| c.ch).collect();
            lines.push(s.trim_end().to_owned());
        }
        for screen_line in &session.pane.buffer.lines {
            let s: String = screen_line.iter().map(|c| c.ch).collect();
            lines.push(s.trim_end().to_owned());
        }
    }
    lines
}

/// Key handler for terminal sessions.
fn handle_terminal_key_session(
    term: &mut crate::terminal::TerminalPane,
    key: egui::Key,
    mods: egui::Modifiers,
) {
    if mods.ctrl {
        let byte = match key {
            egui::Key::C => Some(b"\x03".as_ref()),
            egui::Key::D => Some(b"\x04".as_ref()),
            egui::Key::L => Some(b"\x0c".as_ref()),
            egui::Key::Z => Some(b"\x1a".as_ref()),
            _ => None,
        };
        if let Some(b) = byte {
            term.write(b);
            return;
        }
    }
    let bytes: &[u8] = match key {
        egui::Key::Enter    => b"\r",
        egui::Key::Backspace => b"\x7f",
        egui::Key::Tab      => b"\t",
        egui::Key::Escape   => b"\x1b",
        egui::Key::ArrowUp  => b"\x1b[A",
        egui::Key::ArrowDown => b"\x1b[B",
        egui::Key::ArrowRight => b"\x1b[C",
        egui::Key::ArrowLeft  => b"\x1b[D",
        egui::Key::Home     => b"\x1b[H",
        egui::Key::End      => b"\x1b[F",
        egui::Key::PageUp   => b"\x1b[5~",
        egui::Key::PageDown => b"\x1b[6~",
        egui::Key::Delete   => b"\x1b[3~",
        _ => return,
    };
    term.write(bytes);
}

fn is_lsp_path(settings: &Settings, root: Option<&Path>, path: &Path) -> bool {
    let root_match = root.is_some_and(|r| path.starts_with(r));
    let lang = LanguageId::from_path(path);
    let Some(server_id) = lang.server_id() else {
        return false;
    };
    root_match && settings.lsp.is_enabled(server_id)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

const TAB_STRIP_HEIGHT: f32 = 26.0;
const BREADCRUMB_BAR_HEIGHT: f32 = 26.0;
const EDITOR_STACK_SPACING: f32 = 0.0;

#[cfg(test)]
const fn editor_chrome_height() -> f32 {
    TAB_STRIP_HEIGHT + BREADCRUMB_BAR_HEIGHT + EDITOR_STACK_SPACING
}

fn tab_fill(visuals: &egui::Visuals, is_active: bool) -> Color32 {
    if is_active {
        visuals.extreme_bg_color
    } else {
        Color32::TRANSPARENT
    }
}

fn tab_rounding(is_active: bool) -> egui::Rounding {
    if is_active {
        egui::Rounding {
            nw: 8.0,
            ne: 8.0,
            sw: 0.0,
            se: 0.0,
        }
    } else {
        egui::Rounding::same(8.0)
    }
}

fn truncate_tab_label(label: &str) -> String {
    if label.chars().count() > 16 {
        format!("{}…", label.chars().take(14).collect::<String>())
    } else {
        label.to_owned()
    }
}

fn language_label(path: &Path) -> String {
    let lang = crate::language::LanguageId::from_path(path);
    match lang {
        crate::language::LanguageId::Rust => "RS".to_owned(),
        crate::language::LanguageId::Python => "PY".to_owned(),
        crate::language::LanguageId::JavaScript => "JS".to_owned(),
        crate::language::LanguageId::JavaScriptReact => "JSX".to_owned(),
        crate::language::LanguageId::TypeScript => "TS".to_owned(),
        crate::language::LanguageId::TypeScriptReact => "TSX".to_owned(),
        crate::language::LanguageId::PlainText => path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| match extension.to_ascii_lowercase().as_str() {
                "toml" => "TOML".to_owned(),
                "md" => "MD".to_owned(),
                _ => extension.to_uppercase(),
            })
            .unwrap_or_else(|| "PLAIN".to_owned()),
    }
}

// Hierarchy rendering & correlation helpers (Features 7 & 8)
fn update_call_node(
    node: &mut HierarchyNode,
    parent_uri: &str,
    parent_range: &crate::lsp::types::LspRange,
    children: Vec<HierarchyNode>,
) -> bool {
    if let HierarchyItem::Call(item) = &node.item {
        if item.uri == parent_uri && item.range == *parent_range {
            node.children = Some(children);
            return true;
        }
    }
    if let Some(children_list) = &mut node.children {
        for child in children_list {
            if update_call_node(child, parent_uri, parent_range, children.clone()) {
                return true;
            }
        }
    }
    false
}

fn update_type_node(
    node: &mut HierarchyNode,
    parent_uri: &str,
    parent_range: &crate::lsp::types::LspRange,
    children: Vec<HierarchyNode>,
) -> bool {
    if let HierarchyItem::Type(item) = &node.item {
        if item.uri == parent_uri && item.range == *parent_range {
            node.children = Some(children);
            return true;
        }
    }
    if let Some(children_list) = &mut node.children {
        for child in children_list {
            if update_type_node(child, parent_uri, parent_range, children.clone()) {
                return true;
            }
        }
    }
    false
}

fn show_hierarchy_node_ui(
    ui: &mut egui::Ui,
    node: &mut HierarchyNode,
    panel_kind: HierarchyKind,
    app: &mut BlueIdeApp,
) {
    let name = node.item.name().to_string();
    let uri = node.item.uri().to_string();
    let range = node.item.range();
    
    let path = if let Ok(parsed) = lsp_types::Url::parse(&uri) {
        parsed.to_file_path().unwrap_or_else(|_| PathBuf::from(&uri))
    } else {
        PathBuf::from(&uri)
    };
    let filename = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
    let location_text = format!("{}:{}", filename, range.start.line + 1);

    ui.horizontal(|ui| {
        let arrow = if node.expanded { "▼" } else { "▶" };
        let arrow_btn = ui.selectable_label(false, arrow);
        if arrow_btn.clicked() {
            node.expanded = !node.expanded;
            if node.expanded && node.children.is_none() {
                app.request_hierarchy_children_for_node(panel_kind, &node.item);
            }
        }

        let kind_text = match node.item.kind() {
            1 => "File",
            2 => "Module",
            3 => "Namespace",
            4 => "Package",
            5 => "Class",
            6 => "Method",
            7 => "Property",
            8 => "Field",
            9 => "Constructor",
            10 => "Enum",
            11 => "Interface",
            12 => "Function",
            13 => "Variable",
            14 => "Constant",
            15 => "String",
            16 => "Number",
            17 => "Boolean",
            18 => "Array",
            19 => "Object",
            20 => "Key",
            21 => "Null",
            22 => "EnumMember",
            23 => "Struct",
            24 => "Event",
            25 => "Operator",
            26 => "TypeParameter",
            _ => "Symbol",
        };
        ui.colored_label(app.active_palette.semantic.muted_text, format!("[{kind_text}]"));

        let name_btn = ui.selectable_label(false, &name);
        if name_btn.clicked() {
            app.navigate_to_diagnostic_pos(&path, range.start.line as usize, range.start.character as usize);
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.weak(location_text);
        });
    });

    if node.expanded {
        if let Some(children) = &mut node.children {
            ui.indent(ui.make_persistent_id(&uri), |ui| {
                for child in children {
                    show_hierarchy_node_ui(ui, child, panel_kind, app);
                }
            });
        } else {
            ui.indent(ui.make_persistent_id(&uri), |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.spinner();
                    ui.label("Loading...");
                });
            });
        }
    }
}

fn show_hierarchy_panel_ui(ui: &mut egui::Ui, app: &mut BlueIdeApp) {
    let Some(mut panel) = app.hierarchy_panel.clone() else {
        ui.centered_and_justified(|ui| {
            ui.label("No hierarchy selected");
        });
        return;
    };

    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            match &mut panel.kind {
                HierarchyKind::Call(dir) => {
                    let is_incoming = *dir == HierarchyDirection::Incoming;
                    if ui.selectable_label(is_incoming, "Incoming Calls").clicked() && !is_incoming {
                        *dir = HierarchyDirection::Incoming;
                        panel.root.children = None;
                        app.request_hierarchy_children_for_node(HierarchyKind::Call(HierarchyDirection::Incoming), &panel.root.item);
                    }
                    if ui.selectable_label(!is_incoming, "Outgoing Calls").clicked() && is_incoming {
                        *dir = HierarchyDirection::Outgoing;
                        panel.root.children = None;
                        app.request_hierarchy_children_for_node(HierarchyKind::Call(HierarchyDirection::Outgoing), &panel.root.item);
                    }
                }
                HierarchyKind::Type(dir) => {
                    let is_super = *dir == TypeDirection::Supertypes;
                    if ui.selectable_label(is_super, "Supertypes").clicked() && !is_super {
                        *dir = TypeDirection::Supertypes;
                        panel.root.children = None;
                        app.request_hierarchy_children_for_node(HierarchyKind::Type(TypeDirection::Supertypes), &panel.root.item);
                    }
                    if ui.selectable_label(!is_super, "Subtypes").clicked() && is_super {
                        *dir = TypeDirection::Subtypes;
                        panel.root.children = None;
                        app.request_hierarchy_children_for_node(HierarchyKind::Type(TypeDirection::Subtypes), &panel.root.item);
                    }
                }
            }
        });

        ui.separator();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                show_hierarchy_node_ui(ui, &mut panel.root, panel.kind, app);
            });
    });

    app.hierarchy_panel = Some(panel);
}

impl BlueIdeApp {
    fn toggle_call_hierarchy_panel(&mut self) {
        if self.show_bottom_panel && self.bottom_panel_tab == BottomPanelTab::CallHierarchy {
            self.close_bottom_panel();
        } else {
            self.bottom_panel_tab = BottomPanelTab::CallHierarchy;
            self.show_bottom_panel = true;
            self.request_prepare_call_hierarchy_at_cursor();
        }
    }

    fn toggle_type_hierarchy_panel(&mut self) {
        if self.show_bottom_panel && self.bottom_panel_tab == BottomPanelTab::TypeHierarchy {
            self.close_bottom_panel();
        } else {
            self.bottom_panel_tab = BottomPanelTab::TypeHierarchy;
            self.show_bottom_panel = true;
            self.request_prepare_type_hierarchy_at_cursor();
        }
    }

    fn request_prepare_call_hierarchy_at_cursor(&mut self) {
        let Some(path) = self.active.clone() else { return; };
        let Some(buffer) = self.buffers.get(&path) else { return; };
        let cursor = buffer.cursor();
        let id = self.next_ui_correlation_id();
        self.lsp_pending.insert(id, LspPendingKind::CallHierarchy {
            path: path.clone(),
            parent_uri: String::new(),
            parent_range: None,
        });
        let Some(root) = self.workspace_root_for_path(&path) else { return; };
        self.lsp_manager.request_prepare_call_hierarchy(&path, cursor.line as u32, cursor.col as u32, id, &self.settings, &root);
    }

    fn request_prepare_type_hierarchy_at_cursor(&mut self) {
        let Some(path) = self.active.clone() else { return; };
        let Some(buffer) = self.buffers.get(&path) else { return; };
        let cursor = buffer.cursor();
        let id = self.next_ui_correlation_id();
        self.lsp_pending.insert(id, LspPendingKind::TypeHierarchy {
            path: path.clone(),
            parent_uri: String::new(),
            parent_range: None,
        });
        let Some(root) = self.workspace_root_for_path(&path) else { return; };
        self.lsp_manager.request_prepare_type_hierarchy(&path, cursor.line as u32, cursor.col as u32, id, &self.settings, &root);
    }

    fn request_hierarchy_children_for_node(&mut self, kind: HierarchyKind, item: &HierarchyItem) {
        let path = if let Ok(parsed) = lsp_types::Url::parse(item.uri()) {
            parsed.to_file_path().unwrap_or_else(|_| PathBuf::from(item.uri()))
        } else {
            PathBuf::from(item.uri())
        };
        let id = self.next_ui_correlation_id();
        
        match kind {
            HierarchyKind::Call(dir) => {
                self.lsp_pending.insert(id, LspPendingKind::CallHierarchy {
                    path: path.clone(),
                    parent_uri: item.uri().to_string(),
                    parent_range: Some(item.range()),
                });
                let Some(root) = self.workspace_root_for_path(&path) else { return; };
                if let HierarchyItem::Call(call_item) = item {
                    match dir {
                        HierarchyDirection::Incoming => {
                            self.lsp_manager.request_incoming_calls(&path, call_item.clone(), id, &self.settings, &root);
                        }
                        HierarchyDirection::Outgoing => {
                            self.lsp_manager.request_outgoing_calls(&path, call_item.clone(), id, &self.settings, &root);
                        }
                    }
                }
            }
            HierarchyKind::Type(dir) => {
                self.lsp_pending.insert(id, LspPendingKind::TypeHierarchy {
                    path: path.clone(),
                    parent_uri: item.uri().to_string(),
                    parent_range: Some(item.range()),
                });
                let Some(root) = self.workspace_root_for_path(&path) else { return; };
                if let HierarchyItem::Type(type_item) = item {
                    match dir {
                        TypeDirection::Supertypes => {
                            self.lsp_manager.request_supertypes(&path, type_item.clone(), id, &self.settings, &root);
                        }
                        TypeDirection::Subtypes => {
                            self.lsp_manager.request_subtypes(&path, type_item.clone(), id, &self.settings, &root);
                        }
                    }
                }
            }
        }
    }

    fn receive_call_hierarchy_prepare(&mut self, _path: PathBuf, items: Vec<crate::lsp::types::CallHierarchyItem>) {
        if let Some(first) = items.into_iter().next() {
            let node = HierarchyNode {
                item: HierarchyItem::Call(first),
                children: None,
                expanded: true,
            };
            self.hierarchy_panel = Some(HierarchyPanel {
                root: node.clone(),
                kind: HierarchyKind::Call(HierarchyDirection::Incoming),
                visible: true,
            });
            self.bottom_panel_tab = BottomPanelTab::CallHierarchy;
            self.show_bottom_panel = true;
            self.request_hierarchy_children_for_node(HierarchyKind::Call(HierarchyDirection::Incoming), &node.item);
        }
    }

    fn receive_type_hierarchy_prepare(&mut self, _path: PathBuf, items: Vec<crate::lsp::types::TypeHierarchyItem>) {
        if let Some(first) = items.into_iter().next() {
            let node = HierarchyNode {
                item: HierarchyItem::Type(first),
                children: None,
                expanded: true,
            };
            self.hierarchy_panel = Some(HierarchyPanel {
                root: node.clone(),
                kind: HierarchyKind::Type(TypeDirection::Supertypes),
                visible: true,
            });
            self.bottom_panel_tab = BottomPanelTab::TypeHierarchy;
            self.show_bottom_panel = true;
            self.request_hierarchy_children_for_node(HierarchyKind::Type(TypeDirection::Supertypes), &node.item);
        }
    }

    /// Run a named task. Requires a trusted workspace.
    pub fn run_task(&mut self, task_name: &str) {
        let Some(root) = self.primary_workspace_root() else {
            self.error_message = Some("No workspace open.".to_owned());
            return;
        };

        // Security: require trusted workspace before running code.
        // Fail closed: an absent/unloaded trust store must NOT permit execution.
        if !self
            .trust_store
            .as_ref()
            .and_then(|ts| self.workspace.roots().first().map(|r| ts.permits(r, crate::workspace::ExecutableCapability::Command)))
            .unwrap_or(false)
        {
            self.error_message = Some(
                "Tasks require a trusted workspace. Click the trust badge to enable."
                    .to_owned(),
            );
            return;
        }

        // Clear previous output
        self.task_output.clear();
        let env_vars = self.env_editor.enabled_vars();
        self.task_panel.run_with_env(task_name, root.as_path(), &env_vars);

        // Switch bottom panel to Output
        self.show_bottom_panel = true;
        self.bottom_panel_tab = BottomPanelTab::Output;
    }

    /// Reload the tasks.toml for the current workspace.
    pub fn reload_tasks(&mut self) {
        let Some(root) = self.primary_workspace_root() else {
            return;
        };
        self.task_panel.reload(root.as_path());
    }

    /// Poll the task runner for output — mirror lines into task_output.
    fn poll_tasks(&mut self) {
        self.task_panel.poll();
        if let Some(h) = &self.task_panel.running {
            let existing = self.task_output.len();
            if h.output_lines.len() > existing {
                for line in &h.output_lines[existing..] {
                    self.task_output.push(line.text.clone());
                }
            }
        }
    }

    /// Refresh `.editorconfig` settings for the active file.
    fn refresh_editorconfig(&mut self) {
        if let Some(ref path) = self.active.clone() {
            self.editorconfig = crate::editorconfig::resolve(path);
        } else {
            self.editorconfig = crate::editorconfig::EditorConfigSettings::default();
        }
    }

    fn navigate_to_diagnostic_pos(&mut self, path: &Path, line: usize, col: usize) {
        let path = path.to_path_buf();
        if !self.buffers.contains_key(&path) {
            if let Err(error) = self.open_file(path.clone()) {
                self.error_message = Some(format!("Could not open {}: {error}", path.display()));
                return;
            }
        } else {
            self.active = Some(path.clone());
            self.reveal_active_tab = true;
        }

        if let Some(buffer) = self.buffers.get_mut(&path) {
            buffer.set_cursor(buffer.lsp_position_to_cursor(LspPosition::new(line as u32, col as u32)));
        }

        if let Some(state) = self.editor_states.get_mut(&path) {
            state.request_scroll_to_cursor();
        }

        self.error_message = None;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::all)]
    // App-layer regression tests — see crate root `# Regression tests` (`lib.rs`).

    use std::fs;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::BlueIdeApp;
    use crate::editor::hover::{HoverPopupModel, HoverRequestSession};
    use crate::editor::position::LspPosition;
    use crate::settings::{Settings, SettingsStore, Theme};
    use crate::theme::built_in_theme;

    fn begin_completion_session(
        app: &mut BlueIdeApp,
        path: std::path::PathBuf,
        request_id: u64,
        revision: u64,
        lsp_version: i32,
        cursor: crate::editor::buffer::CursorPosition,
    ) {
        let prefix_char_range = app
            .buffers
            .get(&path)
            .and_then(|buffer| buffer.identifier_prefix_char_range_at(cursor))
            .unwrap_or_else(|| {
                let end = app
                    .buffers
                    .get(&path)
                    .and_then(|buffer| buffer.position_to_char_index(cursor))
                    .unwrap_or(0);
                end..end
            });
        app.completion.begin_session(super::CompletionSession {
            path,
            request_id,
            revision,
            lsp_version,
            cursor,
            prefix_char_range,
        });
    }

    fn test_file(name: &str, contents: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("blue_ide_{unique}_{name}"));
        fs::write(&path, contents).unwrap();
        path
    }

    fn test_dir(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("blue_ide_{unique}_{name}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn trust_store_for(root: &crate::workspace::WorkspaceRoot, trust: crate::workspace::TrustState) -> (
        crate::workspace::TrustStore,
        std::path::PathBuf,
    ) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("blue_ide_{unique}_trust.json"));
        let mut store = crate::workspace::TrustStore::load(&path).unwrap();
        store.set(root, trust).unwrap();
        (store, path)
    }

    /// Build an app with a single trusted workspace root. Returns the app, the
    /// on-disk root dir, the trust-store path, and the workspace root.
    fn trusted_workspace_app(
        name: &str,
    ) -> (
        BlueIdeApp,
        std::path::PathBuf,
        std::path::PathBuf,
        crate::workspace::WorkspaceRoot,
    ) {
        let dir = test_dir(name);
        let mut app = BlueIdeApp::empty();
        let id = app.workspace.add_root(&dir).unwrap();
        let root = app.workspace.root(id).unwrap().clone();
        let (store, store_path) = trust_store_for(&root, crate::workspace::TrustState::Trusted);
        app.trust_store = Some(store);
        (app, dir, store_path, root)
    }

    /// Build an app with a single untrusted workspace root.
    fn untrusted_workspace_app(
        name: &str,
    ) -> (
        BlueIdeApp,
        std::path::PathBuf,
        std::path::PathBuf,
        crate::workspace::WorkspaceRoot,
    ) {
        let dir = test_dir(name);
        let mut app = BlueIdeApp::empty();
        let id = app.workspace.add_root(&dir).unwrap();
        let root = app.workspace.root(id).unwrap().clone();
        let (store, store_path) = trust_store_for(&root, crate::workspace::TrustState::Untrusted);
        app.trust_store = Some(store);
        (app, dir, store_path, root)
    }

    fn settings_app(path: std::path::PathBuf) -> BlueIdeApp {
        let settings = Settings::default();
        let palette = built_in_theme(settings.appearance.theme, None).palette;
        BlueIdeApp::empty_with_settings(settings, SettingsStore::at_path(path), None, None, palette)
    }

    #[test]
    fn opening_settings_clones_the_active_settings() {
        let path = test_file("settings_open.toml", "");
        fs::remove_file(&path).unwrap();
        let mut app = settings_app(path.clone());

        app.open_settings();

        assert!(app.show_settings_window);
        assert_eq!(app.settings_draft.as_ref(), Some(&app.settings));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn preview_changes_appearance_without_promoting_settings_and_cancel_restores_it() {
        let path = test_file("settings_preview.toml", "");
        fs::remove_file(&path).unwrap();
        let mut app = settings_app(path.clone());
        let context = egui::Context::default();
        let original_palette = app.active_palette;
        app.open_settings();
        app.settings_draft.as_mut().unwrap().appearance.theme = Theme::Light;

        app.preview_settings(&context);

        assert_eq!(app.settings.appearance.theme, Theme::Dark);
        assert_ne!(app.active_palette, original_palette);
        app.cancel_settings(&context);
        assert_eq!(app.active_palette, original_palette);
        assert!(!app.show_settings_window);
        assert!(app.settings_draft.is_none());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn apply_persists_and_reopening_uses_latest_settings() {
        let path = test_file("settings_apply.toml", "");
        fs::remove_file(&path).unwrap();
        let mut app = settings_app(path.clone());
        let context = egui::Context::default();
        app.open_settings();
        app.settings_draft.as_mut().unwrap().appearance.theme = Theme::Nord;

        assert!(app.persist_settings_draft(&context, false));

        assert_eq!(app.settings.appearance.theme, Theme::Nord);
        assert!(app.show_settings_window);
        assert_eq!(
            SettingsStore::at_path(path.clone())
                .load()
                .unwrap()
                .appearance
                .theme,
            Theme::Nord
        );
        app.settings_draft.as_mut().unwrap().appearance.theme = Theme::Dracula;
        app.open_settings();
        assert_eq!(
            app.settings_draft.as_ref().unwrap().appearance.theme,
            Theme::Nord
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn failed_persistence_keeps_draft_open_and_active_settings_coherent() {
        let blocking_file = test_file("settings_blocking_parent", "not a directory");
        let path = blocking_file.join("settings.toml");
        let mut app = settings_app(path);
        let context = egui::Context::default();
        app.open_settings();
        app.settings_draft.as_mut().unwrap().appearance.theme = Theme::SolarizedDark;
        app.preview_settings(&context);

        assert!(!app.persist_settings_draft(&context, true));

        assert_eq!(app.settings.appearance.theme, Theme::Dark);
        assert_eq!(
            app.settings_draft.as_ref().unwrap().appearance.theme,
            Theme::SolarizedDark
        );
        assert!(app.show_settings_window);
        assert!(app
            .settings_feedback
            .as_deref()
            .unwrap()
            .starts_with("Could not save settings:"));
        fs::remove_file(blocking_file).unwrap();
    }

    #[test]
    fn opening_an_existing_file_activates_without_reordering_it() {
        let first = test_file("first.rs", "first");
        let second = test_file("second.rs", "second");
        let mut app = BlueIdeApp::empty();

        app.open_file(first.clone()).unwrap();
        app.open_file(second.clone()).unwrap();
        app.open_file(first.clone()).unwrap();

        assert_eq!(app.buffers.keys().collect::<Vec<_>>(), [&first, &second]);
        assert_eq!(app.active.as_ref(), Some(&first));
        let _ = fs::remove_file(first);
        let _ = fs::remove_file(second);
    }

    #[test]
    fn tab_strip_is_absent_without_open_buffers() {
        let mut app = BlueIdeApp::empty();
        let context = egui::Context::default();

        let _ = context.run(egui::RawInput::default(), |context| {
            app.show_tabs(context);
        });

        assert!(
            egui::containers::panel::PanelState::load(&context, egui::Id::new("tabs")).is_none()
        );
    }

    #[test]
    fn editor_chrome_uses_a_compact_vertical_budget() {
        assert_eq!(super::editor_chrome_height(), 52.0);
    }

    #[test]
    fn active_tabs_use_editor_fill_while_inactive_tabs_are_transparent() {
        let visuals = egui::Visuals::dark();

        assert_eq!(super::tab_fill(&visuals, true), visuals.extreme_bg_color);
        assert_eq!(super::tab_fill(&visuals, false), egui::Color32::TRANSPARENT);
    }

    #[test]
    fn active_tabs_have_a_connected_bottom_edge() {
        let active = super::tab_rounding(true);
        let inactive = super::tab_rounding(false);

        assert_eq!((active.nw, active.ne), (8.0, 8.0));
        assert_eq!((active.sw, active.se), (0.0, 0.0));
        assert_eq!((inactive.sw, inactive.se), (8.0, 8.0));
    }

    #[test]
    fn closing_active_tab_selects_left_neighbor_except_at_left_edge() {
        let first = test_file("first.rs", "first");
        let second = test_file("second.rs", "second");
        let third = test_file("third.rs", "third");
        let mut app = BlueIdeApp::empty();
        for path in [&first, &second, &third] {
            app.open_file(path.clone()).unwrap();
        }

        app.active = Some(second.clone());
        app.close_file(&second);
        assert_eq!(app.active.as_ref(), Some(&first));
        app.close_file(&first);
        assert_eq!(app.active.as_ref(), Some(&third));
        app.close_file(&third);
        assert_eq!(app.active, None);

        let _ = fs::remove_file(first);
        let _ = fs::remove_file(second);
        let _ = fs::remove_file(third);
    }

    #[test]
    fn closing_an_inactive_tab_preserves_the_active_tab() {
        let first = test_file("first.rs", "first");
        let second = test_file("second.rs", "second");
        let mut app = BlueIdeApp::empty();
        app.open_file(first.clone()).unwrap();
        app.open_file(second.clone()).unwrap();

        app.close_file(&first);

        assert_eq!(app.active.as_ref(), Some(&second));
        let _ = fs::remove_file(first);
        let _ = fs::remove_file(second);
    }

    #[test]
    fn failed_open_does_not_replace_the_active_buffer() {
        let first = test_file("first.rs", "first");
        let missing = first.with_file_name("blue_ide_missing_file.rs");
        let _ = fs::remove_file(&missing);
        let mut app = BlueIdeApp::empty();
        app.open_file(first.clone()).unwrap();

        assert!(app.open_file(missing).is_err());

        assert_eq!(app.buffers.len(), 1);
        assert_eq!(app.active.as_ref(), Some(&first));
        let _ = fs::remove_file(first);
    }

    #[test]
    fn tab_cycling_wraps_in_both_directions() {
        let first = test_file("first.rs", "first");
        let second = test_file("second.rs", "second");
        let mut app = BlueIdeApp::empty();
        app.open_file(first.clone()).unwrap();
        app.open_file(second.clone()).unwrap();

        app.cycle_tab(1);
        assert_eq!(app.active.as_ref(), Some(&first));
        app.cycle_tab(-1);
        assert_eq!(app.active.as_ref(), Some(&second));

        let _ = fs::remove_file(first);
        let _ = fs::remove_file(second);
    }

    #[test]
    fn tab_cycling_is_safe_without_open_buffers() {
        let mut app = BlueIdeApp::empty();

        app.cycle_tab(1);
        app.cycle_tab(-1);

        assert_eq!(app.active, None);
    }

    #[test]
    fn modified_tab_requires_confirmation_before_removal() {
        let path = test_file("modified.rs", "original");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.buffers
            .get_mut(&path)
            .unwrap()
            .insert_at_cursor("edit")
            .unwrap();

        app.request_close_file(&path);

        assert!(app.buffers.contains_key(&path));
        assert_eq!(app.pending_close.as_ref(), Some(&path));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn tab_labels_truncate_by_character_and_preserve_modified_marker() {
        assert_eq!(
            super::truncate_tab_label("● 文件名非常长的源代码文件.rs"),
            "● 文件名非常长的源代码文件…"
        );
        assert_eq!(super::truncate_tab_label("main.rs"), "main.rs");
    }

    #[test]
    fn language_labels_cover_known_unknown_and_extensionless_files() {
        for (file, expected) in [
            ("main.rs", "RS"),
            ("script.py", "PY"),
            ("app.js", "JS"),
            ("types.ts", "TS"),
            ("Cargo.toml", "TOML"),
            ("README.md", "MD"),
        ] {
            assert_eq!(super::language_label(std::path::Path::new(file)), expected);
        }
        assert_eq!(
            super::language_label(std::path::Path::new("shader.glsl")),
            "GLSL"
        );
        assert_eq!(
            super::language_label(std::path::Path::new("LICENSE")),
            "PLAIN"
        );
    }

    #[test]
    fn save_and_close_keeps_the_tab_open_when_saving_fails() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("blue_ide_save_failure_{unique}"));
        fs::create_dir(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(&path, "original").unwrap();
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.buffers
            .get_mut(&path)
            .unwrap()
            .insert_at_cursor("edited")
            .unwrap();
        fs::remove_file(&path).unwrap();
        fs::remove_dir(&root).unwrap();

        assert!(app.save_and_close(&path).is_err());
        assert!(app.buffers.contains_key(&path));
        assert_eq!(app.active.as_ref(), Some(&path));
        assert!(app.buffers.get(&path).unwrap().is_modified());
    }

    #[test]
    fn save_and_close_writes_changes_before_removing_the_tab() {
        let path = test_file("save_and_close.rs", "original");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.buffers
            .get_mut(&path)
            .unwrap()
            .insert_at_cursor("edited ")
            .unwrap();

        app.save_and_close(&path).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "edited original");
        assert!(!app.buffers.contains_key(&path));
        assert_eq!(app.active, None);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn composed_panels_render_with_an_open_tree_and_buffer() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("blue_ide_ui_{unique}"));
        fs::create_dir(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(&path, "fn main() {}\n").unwrap();

        let mut app = BlueIdeApp::empty();
        app.tree.load(root.clone()).unwrap();
        app.show_tree = true;
        app.open_file(path).unwrap();
        let context = egui::Context::default();

        let _ = context.run(egui::RawInput::default(), |context| {
            let _ = app.show_menu(context);
            app.show_status_bar(context);
            app.show_file_tree(context);
            app.show_tabs(context);
            let _ = app.show_editor(context);
            app.show_confirmation(context);
        });

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn title_bar_is_a_single_compact_row() {
        let mut app = BlueIdeApp::empty();
        let context = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            ..Default::default()
        };

        let _ = context.run(input, |context| {
            let _ = app.show_menu(context);
        });

        let rect = egui::containers::panel::PanelState::load(&context, egui::Id::new("menu_bar"))
            .unwrap()
            .rect;
        assert!(
            (rect.height() - super::TITLE_BAR_HEIGHT).abs() < 0.1,
            "title bar height was {}, expected {}",
            rect.height(),
            super::TITLE_BAR_HEIGHT
        );
        assert_eq!(super::TITLE_BAR_HEIGHT, 32.0);
    }

    #[test]
    fn title_bar_exposes_the_standard_ide_menu_order() {
        assert_eq!(
            super::TOP_MENU_LABELS,
            [
                "File",
                "Edit",
                "Selection",
                "View",
                "Go",
                "Run",
                "Window",
                "Help",
            ]
        );
    }

    #[test]
    fn menu_navigation_commands_open_the_existing_navigation_ui() {
        let mut app = BlueIdeApp::empty();
        let context = egui::Context::default();

        app.execute_command(super::CommandId::GoToLine, &context);
        assert!(app.goto_line.open);

        app.goto_line.open = false;
        app.execute_command(super::CommandId::GoToSymbol, &context);
        assert!(app.workspace_symbol.open);
    }

    #[test]
    fn menu_run_command_creates_and_reveals_a_terminal() {
        let (mut app, _root_dir, _store_path, _root) = trusted_workspace_app("menu_terminal");
        let context = egui::Context::default();
        let terminal_count = app.term_sessions.len();
        app.show_bottom_panel = false;

        app.execute_command(super::CommandId::NewTerminal, &context);

        assert_eq!(app.term_sessions.len(), terminal_count + 1);
        assert!(app.show_bottom_panel);
        assert_eq!(app.bottom_panel_tab, super::BottomPanelTab::Terminal);
    }

    #[test]
    fn run_task_blocks_when_trust_store_is_absent() {
        let dir = test_dir("run_task_no_store");
        let mut app = BlueIdeApp::empty();
        app.workspace.add_root(&dir).unwrap();
        app.trust_store = None;

        app.run_task("build");

        assert!(
            app.error_message.as_deref().unwrap_or("")
                .contains("trusted workspace"),
            "unexpected error: {:?}",
            app.error_message
        );
        assert!(!app.show_bottom_panel);
    }

    #[test]
    fn run_task_is_allowed_after_root_is_trusted() {
        let (mut app, _dir, _store_path, _root) = trusted_workspace_app("run_task_trusted");
        app.show_bottom_panel = false;

        app.run_task("build");

        assert!(app.error_message.is_none());
        // The task itself is absent, but the trusted path must advance to the run UI.
        assert!(app.show_bottom_panel);
    }

    #[test]
    fn new_terminal_requires_a_trusted_workspace() {
        let (mut app, _dir, _store_path, _root) = untrusted_workspace_app("terminal_untrusted");
        let context = egui::Context::default();
        let before = app.term_sessions.len();

        app.execute_command(super::CommandId::NewTerminal, &context);

        assert_eq!(app.term_sessions.len(), before);
        assert!(
            app.error_message.as_deref().unwrap_or("")
                .contains("trusted workspace"),
            "unexpected error: {:?}",
            app.error_message
        );
    }

    #[test]
    fn new_terminal_is_allowed_after_root_is_trusted() {
        let (mut app, _dir, _store_path, _root) = trusted_workspace_app("terminal_trusted");
        let context = egui::Context::default();
        let before = app.term_sessions.len();

        app.execute_command(super::CommandId::NewTerminal, &context);

        assert_eq!(app.term_sessions.len(), before + 1);
    }

    #[test]
    fn menu_window_commands_split_and_focus_editor_groups() {
        let mut app = BlueIdeApp::empty();
        let context = egui::Context::default();
        let first = app.focus.active_pane;

        app.execute_command(super::CommandId::SplitEditorRight, &context);
        assert!(matches!(
            app.pane_actions.as_slice(),
            [crate::panes::PaneAction::SplitH { pane }] if *pane == first
        ));

        app.pane_actions.clear();
        app.execute_command(super::CommandId::SplitEditorDown, &context);
        assert!(matches!(
            app.pane_actions.as_slice(),
            [crate::panes::PaneAction::SplitV { pane }] if *pane == first
        ));

        app.pane_tree.split_h(first);
        app.execute_command(super::CommandId::FocusNextGroup, &context);
        assert_ne!(app.focus.active_pane, first);
        app.execute_command(super::CommandId::FocusPreviousGroup, &context);
        assert_eq!(app.focus.active_pane, first);
    }

    #[test]
    fn menu_commands_are_also_available_from_the_command_palette() {
        let app = BlueIdeApp::empty();
        let commands = app.command_specs();

        for expected in [
            super::CommandId::GoToLine,
            super::CommandId::GoToSymbol,
            super::CommandId::NewTerminal,
            super::CommandId::SplitEditorRight,
            super::CommandId::SplitEditorDown,
            super::CommandId::FocusNextGroup,
            super::CommandId::FocusPreviousGroup,
        ] {
            assert!(commands.iter().any(|command| command.id == expected));
        }
    }

    #[test]
    fn tab_strip_starts_at_the_editor_side_of_the_file_tree() {
        let mut app = BlueIdeApp::empty();
        app.show_tree = true;
        let path = test_file("aligned_tab.rs", "aligned");
        app.open_file(path.clone()).unwrap();
        let context = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };

        let _ = context.run(input, |context| {
            let _ = app.show_workspace_panels(context);
        });

        let tree_rect =
            egui::containers::panel::PanelState::load(&context, egui::Id::new("file_tree"))
                .unwrap()
                .rect;
        let tabs_rect = egui::containers::panel::PanelState::load(&context, egui::Id::new("tabs"))
            .unwrap()
            .rect;
        assert!((tabs_rect.left() - tree_rect.right()).abs() < 0.1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn ui_correlation_ids_are_monotonically_increasing() {
        let mut app = BlueIdeApp::empty();
        let first = app.next_ui_correlation_id();
        let second = app.next_ui_correlation_id();
        let third = app.next_ui_correlation_id();

        assert_eq!(first, 1);
        assert!(first < second);
        assert!(second < third);
    }

    #[test]
    fn hover_null_result_closes_silently_via_poll_lsp() {
        use crate::lsp::types::LspResponse;
        use crate::lsp::LspClient;

        let path = test_file("hover_null.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.error_message = Some("prior status".to_owned());

        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        };
        app.lsp_hover.resting_target = Some(target.clone());
        app.lsp_hover.rest_started = Some(0.0);
        app.lsp_hover.popup_anchor = Some(egui::Rect::from_min_size(
            egui::pos2(80.0, 40.0),
            egui::vec2(24.0, 18.0),
        ));
        set_hover_request_session(&mut app, &path, 43, LspPosition::new(0, 4));
        app.lsp_hover.request_sent_for = Some(target.clone());
        app.lsp_pending.insert(43, super::LspPendingKind::Hover);

        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::HoverResult {
                id: 43,
                content: String::new(),
            });

        app.poll_lsp();

        assert_eq!(app.lsp_pending.get(&43), None);
        assert!(app.lsp_hover.content.is_none());
        assert!(app.lsp_hover.displayed_target.is_none());
        assert!(app.lsp_hover.session.is_none());
        assert_eq!(app.lsp_hover.no_content_target.as_ref(), Some(&target));
        assert!(app.error_message.as_deref() == Some("prior status"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn hover_undisplayable_content_closes_silently_via_poll_lsp() {
        use crate::lsp::types::LspResponse;
        use crate::lsp::LspClient;

        let path = test_file("hover_undisplayable.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.error_message = Some("prior status".to_owned());

        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        };
        app.lsp_hover.resting_target = Some(target.clone());
        app.lsp_hover.rest_started = Some(0.0);
        set_hover_request_session(&mut app, &path, 44, LspPosition::new(0, 4));
        app.lsp_hover.request_sent_for = Some(target.clone());
        app.lsp_pending.insert(44, super::LspPendingKind::Hover);

        let leaked = "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"contents\":\"docs\"}}";
        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::HoverResult {
                id: 44,
                content: leaked.to_owned(),
            });

        app.poll_lsp();

        assert_eq!(app.lsp_pending.get(&44), None);
        assert!(app.lsp_hover.content.is_none());
        assert!(app.lsp_hover.session.is_none());
        assert_eq!(app.lsp_hover.no_content_target.as_ref(), Some(&target));
        assert!(app.error_message.as_deref() == Some("prior status"));

        let _ = fs::remove_file(path);
    }

    fn rust_source_without_comments(source: &str) -> String {
        source
            .lines()
            .filter(|line| {
                let trimmed = line.trim();
                !trimmed.starts_with("//")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn extract_fn_body(source: &str, fn_name: &str) -> Option<String> {
        let signature = format!("fn {fn_name}");
        let start = source.find(&signature)?;
        let brace_start = source[start..].find('{')? + start;
        let bytes = source.as_bytes();
        let mut depth = 0usize;
        let mut started = false;
        for (offset, byte) in bytes[brace_start..].iter().enumerate() {
            match byte {
                b'{' => {
                    depth += 1;
                    started = true;
                }
                b'}' => {
                    if started {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            let end = brace_start + offset;
                            return Some(source[start..=end].to_string());
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn assert_completion_and_hover_render_from_lsp_responses_not_mock_data() {
        let app_rs = include_str!("app.rs").replace("\r\n", "\n");
        let app_production = app_rs
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(&app_rs);
        assert!(
            app_production.contains("LspResponse::CompletionList { id, items }"),
            "completion UI must be fed from poll_lsp CompletionList responses"
        );
        assert!(
            app_production.contains("self.receive_completion(id, items)"),
            "completion must apply typed LSP items via receive_completion"
        );
        assert!(
            app_production.contains("LspResponse::HoverResult { id, content }"),
            "hover UI must be fed from poll_lsp HoverResult responses"
        );
        assert!(
            app_production.contains("self.receive_hover(id, content)"),
            "hover must apply typed LSP content via receive_hover"
        );
        assert!(
            !app_production.contains("vec![LspCompletionItem"),
            "production app must not embed hard-coded completion item lists"
        );

        let receive_hover_body =
            extract_fn_body(app_production, "receive_hover").expect("receive_hover should exist");
        assert_eq!(
            receive_hover_body
                .matches("self.lsp_hover.content = Some")
                .count(),
            1,
            "hover documentation must be stored only from receive_hover LSP content"
        );

        let completion_rs = include_str!("editor/completion.rs");
        let completion_production = completion_rs
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(completion_rs);
        assert!(
            completion_production.contains("self.popup.items = items"),
            "completion popup items must come from try_accept_response"
        );
        assert!(
            completion_production.contains("self.popup.begin_loading()"),
            "completion must show loading until an LSP response arrives"
        );
        assert!(
            !completion_production.contains("sample_item"),
            "production completion must not use test sample_item helpers"
        );
        assert!(
            !rust_source_without_comments(completion_production).contains("popup.items = vec!"),
            "production completion must not hard-code popup item vectors"
        );

        let hover_rs = include_str!("editor/hover.rs");
        let hover_production = hover_rs
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(hover_rs);
        assert!(
            hover_production.contains("pub fn show_hover_documentation"),
            "hover popup must render caller-supplied documentation"
        );
        assert!(
            hover_production
                .contains("fn show_hover_documentation_body(ui: &mut Ui, content: &str)"),
            "hover body must display dynamic LSP content"
        );

        let transport_rs = include_str!("lsp/transport.rs");
        assert!(
            transport_rs.contains("LspResponse::CompletionList"),
            "transport must parse completion results from wire JSON"
        );
        assert!(
            transport_rs.contains("LspResponse::HoverResult"),
            "transport must parse hover results from wire JSON"
        );
    }

    /// Never boundary: completion/hover UI must not use hard-coded mock data (see
    /// **Boundaries → Never** §16).
    #[test]
    fn render_completion_or_hover_using_hard_coded_mock_data() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::{LspCompletionItem, LspResponse};
        use crate::lsp::LspClient;

        assert_completion_and_hover_render_from_lsp_responses_not_mock_data();

        let path = test_file("no_mock_lsp.rs", "fn ma() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();

        let cursor = CursorPosition { line: 0, col: 5 };
        app.buffers.get_mut(&path).unwrap().set_cursor(cursor);
        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        begin_completion_session(&mut app, path.clone(), 7, revision, lsp_version, cursor);
        app.lsp_pending.insert(7, super::LspPendingKind::Completion);

        assert!(app.completion.popup().loading);
        assert!(app.completion.popup().items.is_empty());

        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::CompletionList {
                id: 7,
                items: vec![LspCompletionItem {
                    filter_text: None,
                    label: "from_lsp_completion".to_owned(),
                    kind: None,
                    detail: None,
                    insert_text: None,
                    text_edit: None,
                }],
            });
        app.poll_lsp();

        assert!(!app.completion.popup().loading);
        assert_eq!(app.completion.popup().items.len(), 1);
        assert_eq!(app.completion.popup().items[0].label, "from_lsp_completion");

        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        };
        app.lsp_hover.resting_target = Some(target.clone());
        app.lsp_hover.rest_started = Some(0.0);
        app.lsp_hover.popup_anchor = Some(egui::Rect::from_min_size(
            egui::pos2(80.0, 40.0),
            egui::vec2(24.0, 18.0),
        ));
        set_hover_request_session(&mut app, &path, 8, LspPosition::new(0, 4));
        app.lsp_hover.request_sent_for = Some(target.clone());
        app.lsp_pending.insert(8, super::LspPendingKind::Hover);
        assert!(app.lsp_hover.content.is_none());

        let hover_docs = "Documentation from rust-analyzer only.";
        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::HoverResult {
                id: 8,
                content: hover_docs.to_owned(),
            });
        app.poll_lsp();

        assert_eq!(app.lsp_hover.content.as_deref(), Some(hover_docs));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn poll_lsp_applies_typed_hover_results_without_wire_parsing() {
        use crate::lsp::types::LspResponse;
        use crate::lsp::LspClient;

        let path = test_file("hover_poll.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        };
        app.lsp_hover.resting_target = Some(target.clone());
        app.lsp_hover.rest_started = Some(0.0);
        app.lsp_hover.popup_anchor = Some(egui::Rect::from_min_size(
            egui::pos2(80.0, 40.0),
            egui::vec2(24.0, 18.0),
        ));
        set_hover_request_session(&mut app, &path, 42, LspPosition::new(0, 4));
        app.lsp_hover.request_sent_for = Some(target.clone());
        app.lsp_pending.insert(42, super::LspPendingKind::Hover);

        let parsed = "**Parameters**\n\n- `x`: the input";
        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::HoverResult {
                id: 42,
                content: parsed.to_owned(),
            });

        app.poll_lsp();

        assert_eq!(app.lsp_pending.get(&42), None);
        assert_eq!(app.lsp_hover.content.as_deref(), Some(parsed));
        assert_eq!(app.lsp_hover.displayed_target.as_ref(), Some(&target));
        assert!(app.lsp_hover.session.is_none());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn render_lsp_hover_popup_delegates_layout_to_hover_module() {
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_render_delegate.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        };
        app.lsp_hover.content = Some("fn main — documentation".to_owned());
        app.lsp_hover.displayed_target = Some(target.clone());
        app.lsp_hover.resting_target = Some(target);
        app.lsp_hover.rest_started = Some(0.0);
        app.lsp_hover.popup_anchor =
            Some(Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)));

        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| app.render_lsp_hover_popup(ctx, Some(screen), false),
        );

        assert!(
            app.lsp_hover
                .popup_rect
                .is_some_and(|rect| rect.is_positive()),
            "popup layout should come from editor::hover, not app-local chrome"
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn hover_outbound_request_registers_ui_correlation_in_lsp_pending() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::types::LspRequest;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_correlation.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();

        let pointer_hover = HoveredSourcePosition::for_test(
            CursorPosition { line: 0, col: 4 },
            Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)),
        );
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let base_time = 30.0;
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 0.02),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );

        let request_id = hover_request_id(&app).expect("hover request should be in flight");
        assert_eq!(
            app.lsp_pending.get(&request_id),
            Some(&super::LspPendingKind::Hover)
        );
        assert!(app
            .lsp
            .as_mut()
            .unwrap()
            .drain_pending_requests()
            .iter()
            .any(|request| matches!(
                request,
                LspRequest::Hover {
                    id,
                    line: 0,
                    col: 4,
                    ..
                } if *id == request_id
            )));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn poll_lsp_ignores_uncorrelated_hover_results() {
        use crate::lsp::types::LspResponse;
        use crate::lsp::LspClient;

        let mut app = BlueIdeApp::empty();
        app.lsp = Some(LspClient::new_test_client());
        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::HoverResult {
                id: 999,
                content: "orphan hover".to_owned(),
            });

        app.poll_lsp();

        assert!(app.lsp_hover.content.is_none());
        assert!(!app.lsp_pending.contains_key(&999));
    }

    #[test]
    fn block_waiting_for_rust_analyzer_on_the_ui_thread() {
        use std::time::{Duration, Instant};

        use crate::lsp::types::LspResponse;
        use crate::lsp::LspClient;

        let mut app = BlueIdeApp::empty();
        let client = LspClient::new_test_client_with_running(false);
        let delayed_tx = client
            .test_response_sender()
            .expect("test client should expose a response sender");
        app.lsp = Some(client);

        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            let _ = delayed_tx.send(LspResponse::Initialized {
                token_types: Vec::new(),
            });
        });

        let start = Instant::now();
        app.poll_lsp();
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(200),
            "poll_lsp must not block the UI thread on rust-analyzer ({elapsed:?})"
        );
        assert!(!app.lsp.as_ref().unwrap().is_running());

        std::thread::sleep(Duration::from_millis(600));
        app.poll_lsp();
        assert!(app.lsp.as_ref().unwrap().is_running());
    }

    #[test]
    fn validate_request_context_before_applying_asynchronous_responses() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::{LspCompletionItem, LspResponse};
        use crate::lsp::LspClient;

        fn completion_item(label: &str) -> LspCompletionItem {
            LspCompletionItem {
                label: label.to_owned(),
                ..Default::default()
            }
        }

        let path = test_file("validate_ctx.rs", "fn main() {}\n");
        let other = test_file("validate_ctx_other.rs", "fn other() {}\n");
        let target = test_file("validate_ctx_target.rs", "fn target() {}\n");
        let mut app = BlueIdeApp::empty();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.open_file(other.clone()).unwrap();
        app.open_file(target.clone()).unwrap();
        app.active = Some(path.clone());

        let cursor = CursorPosition { line: 0, col: 4 };
        app.buffers.get_mut(&path).unwrap().set_cursor(cursor);
        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;

        begin_completion_session(&mut app, path.clone(), 1, revision, lsp_version, cursor);
        app.lsp_pending.insert(1, super::LspPendingKind::Completion);
        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::CompletionList {
                id: 1,
                items: vec![completion_item("main")],
            });
        app.poll_lsp();
        assert!(app.completion.is_open());
        assert_eq!(app.completion.popup().items[0].label, "main");

        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::CompletionList {
                id: 2,
                items: vec![completion_item("wrong")],
            });
        app.poll_lsp();
        assert_eq!(app.completion.popup().items[0].label, "main");

        app.buffers
            .get_mut(&path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 0 });
        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::CompletionList {
                id: 1,
                items: vec![completion_item("stale")],
            });
        app.poll_lsp();
        assert_eq!(app.completion.popup().items[0].label, "main");

        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::HoverResult {
                id: 77,
                content: "uncorrelated".to_owned(),
            });
        app.poll_lsp();
        assert!(app.lsp_hover.content.is_none());

        let hover_target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        };
        app.lsp_hover.rest_started = Some(0.0);
        app.lsp_hover.resting_target = Some(hover_target.clone());
        set_hover_request_session(&mut app, &path, 78, LspPosition::new(0, 4));
        app.lsp_pending.insert(78, super::LspPendingKind::Hover);
        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::HoverResult {
                id: 78,
                content: "fn main".to_owned(),
            });
        app.poll_lsp();
        assert_eq!(app.lsp_hover.content.as_deref(), Some("fn main"));

        app.lsp_hover.resting_target = Some(super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 0),
        });
        set_hover_request_session(&mut app, &path, 79, LspPosition::new(0, 4));
        app.lsp_pending.insert(79, super::LspPendingKind::Hover);
        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::HoverResult {
                id: 79,
                content: "moved pointer".to_owned(),
            });
        app.poll_lsp();
        assert_eq!(app.lsp_hover.content.as_deref(), Some("fn main"));

        let goto_id = app.next_ui_correlation_id();
        app.pending_definitions.insert(
            goto_id,
            super::PendingDefinitionRequest {
                source_path: path.clone(),
                source_revision: revision,
                source_position: cursor,
                active_tab: path.clone(),
                is_f12: true,
            },
        );
        app.lsp_pending
            .insert(goto_id, super::LspPendingKind::GotoDefinition);
        app.active = Some(other.clone());
        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::GotoResult {
                id: goto_id,
                path: target.clone(),
                line: 0,
                col: 0,
            });
        app.poll_lsp();
        assert_eq!(app.active.as_ref(), Some(&other));

        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().set_cursor(cursor);
        let goto_id = app.next_ui_correlation_id();
        let revision = app.buffers.get(&path).unwrap().revision();
        app.pending_definitions.insert(
            goto_id,
            super::PendingDefinitionRequest {
                source_path: path.clone(),
                source_revision: revision,
                source_position: cursor,
                active_tab: path.clone(),
                is_f12: true,
            },
        );
        app.lsp_pending
            .insert(goto_id, super::LspPendingKind::GotoDefinition);
        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::GotoResult {
                id: goto_id,
                path: target.clone(),
                line: 0,
                col: 3,
            });
        app.poll_lsp();
        assert_eq!(app.active.as_ref(), Some(&target));

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(other);
        let _ = fs::remove_file(target);
    }

    #[test]
    fn stale_lsp_responses_are_rejected() {
        use crate::editor::buffer::CursorPosition;

        let first = test_file("first.rs", "fn main() {}\n");
        let second = test_file("second.rs", "fn other() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(first.clone()).unwrap();
        app.open_file(second.clone()).unwrap();
        app.active = Some(first.clone());

        // Hover: pointer moved after request.
        let hover_target = super::HoverTarget {
            path: first.clone(),
            position: LspPosition::new(0, 0),
        };
        app.active = Some(first.clone());
        app.lsp_hover.rest_started = Some(0.0);
        set_hover_request_session(&mut app, &first, 10, LspPosition::new(0, 0));
        app.lsp_hover.resting_target = Some(super::HoverTarget {
            path: first.clone(),
            position: LspPosition::new(0, 3),
        });
        app.receive_hover(10, "stale hover".to_owned());
        assert!(app.lsp_hover.content.is_none());

        // Hover: buffer revision changed after request.
        app.lsp_hover.rest_started = Some(0.0);
        set_hover_request_session(&mut app, &first, 12, LspPosition::new(0, 0));
        app.lsp_hover.resting_target = Some(hover_target.clone());
        app.buffers
            .get_mut(&first)
            .unwrap()
            .insert_at_cursor("x")
            .unwrap();
        app.receive_hover(12, "stale hover after edit".to_owned());
        assert!(app.lsp_hover.content.is_none());

        // Hover: active tab changed.
        app.lsp_hover.rest_started = Some(0.0);
        set_hover_request_session(&mut app, &first, 11, LspPosition::new(0, 0));
        app.lsp_hover.resting_target = Some(hover_target.clone());
        app.active = Some(second.clone());
        app.receive_hover(11, "wrong tab".to_owned());
        assert!(app.lsp_hover.content.is_none());

        // Hover: popup dismissed before response.
        app.lsp_hover.rest_started = Some(0.0);
        set_hover_request_session(&mut app, &first, 13, LspPosition::new(0, 0));
        app.lsp_hover.resting_target = Some(hover_target.clone());
        app.lsp_hover.request_sent_for = Some(hover_target.clone());
        app.dismiss_lsp_hover();
        app.receive_hover(13, "dismissed popup".to_owned());
        assert!(app.lsp_hover.content.is_none());

        // Hover: superseded correlation id.
        app.lsp_hover.rest_started = Some(0.0);
        set_hover_request_session(&mut app, &first, 15, LspPosition::new(0, 0));
        app.lsp_hover.resting_target = Some(hover_target.clone());
        app.receive_hover(14, "older request".to_owned());
        assert!(app.lsp_hover.content.is_none());
        assert_eq!(hover_request_id(&app), Some(15));

        // GotoNone: active tab changed.
        let id = app.next_ui_correlation_id();
        app.pending_definitions.insert(
            id,
            super::PendingDefinitionRequest {
                source_path: first.clone(),
                source_revision: app.buffers.get(&first).unwrap().revision(),
                source_position: CursorPosition { line: 0, col: 0 },
                active_tab: first.clone(),
                is_f12: true,
            },
        );
        app.active = Some(second.clone());
        app.receive_goto_none(id);
        assert!(app.error_message.is_none());

        let _ = fs::remove_file(first);
        let _ = fs::remove_file(second);
    }

    fn assert_stale_lsp_results_are_never_displayed_contract() {
        let app_rs = include_str!("app.rs");
        let app_production = app_rs
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(&app_rs);
        assert!(
            app_production.contains("Reject stale responses when"),
            "hover apply path must document stale rejection"
        );
        assert!(
            app_production.contains("fn receive_completion"),
            "completion responses must route through receive_completion"
        );
        assert!(
            app_production.contains(".try_accept_response("),
            "completion must gate display via try_accept_response"
        );
        assert!(
            app_production.contains("fn receive_hover"),
            "hover responses must route through receive_hover"
        );
        assert!(
            app_production.contains("session.is_superseded_response(id)"),
            "hover must reject superseded correlation ids"
        );
        assert!(
            app_production.contains("LspPendingKind::Hover"),
            "poll_lsp must correlate hover responses before display"
        );

        let completion_rs = include_str!("editor/completion.rs");
        assert!(
            completion_rs.contains("reject stale LSP responses"),
            "CompletionSession must document stale guards"
        );
        assert!(
            completion_rs.contains("pub fn is_stale_for"),
            "completion session must expose stale context checks"
        );
        assert!(
            completion_rs.contains("pub fn try_accept_response"),
            "completion popup must gate item display on live context"
        );

        let hover_rs = include_str!("editor/hover.rs");
        assert!(
            hover_rs.contains("reject stale LSP responses"),
            "HoverRequestSession must document stale guards"
        );
        assert!(
            hover_rs.contains("pub fn is_superseded_response"),
            "hover session must reject older correlation ids"
        );
        assert!(
            hover_rs.contains("pub fn is_stale_for"),
            "hover session must expose stale context checks"
        );
    }

    /// Never boundary: stale completion/hover payloads must not reach the UI (see
    /// **Boundaries → Never** §12).
    #[test]
    fn display_stale_lsp_results() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::{LspCompletionItem, LspResponse};
        use crate::lsp::LspClient;

        fn completion_item(label: &str) -> LspCompletionItem {
            LspCompletionItem {
                label: label.to_owned(),
                ..Default::default()
            }
        }

        assert_stale_lsp_results_are_never_displayed_contract();

        let first = test_file("never_stale_first.rs", "fn main() {}\n");
        let second = test_file("never_stale_second.rs", "fn other() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(first.clone()).unwrap();
        app.open_file(second.clone()).unwrap();
        app.active = Some(first.clone());

        let cursor = CursorPosition { line: 0, col: 0 };
        let revision = app.buffers.get(&first).unwrap().revision();
        let lsp_version = app.buffers.get(&first).unwrap().lsp_version;
        begin_completion_session(&mut app, first.clone(), 2, revision, lsp_version, cursor);
        app.receive_completion(1, vec![completion_item("stale completion")]);
        assert!(
            app.completion.popup().loading && app.completion.popup().items.is_empty(),
            "mismatched completion correlation id must not display items"
        );

        begin_completion_session(&mut app, first.clone(), 3, revision, lsp_version, cursor);
        app.buffers
            .get_mut(&first)
            .unwrap()
            .insert_at_cursor("x")
            .unwrap();
        app.receive_completion(3, vec![completion_item("stale after edit")]);
        assert!(
            app.completion.popup().loading && app.completion.popup().items.is_empty(),
            "completion after buffer revision change must not display items"
        );

        let revision = app.buffers.get(&first).unwrap().revision();
        begin_completion_session(&mut app, first.clone(), 4, revision, lsp_version, cursor);
        app.active = Some(second.clone());
        app.receive_completion(4, vec![completion_item("stale tab")]);
        assert!(
            app.completion.popup().loading && app.completion.popup().items.is_empty(),
            "completion for inactive tab must not display items"
        );

        app.active = Some(first.clone());
        app.lsp = Some(LspClient::new_test_client());
        let revision = app.buffers.get(&first).unwrap().revision();
        let lsp_version = app.buffers.get(&first).unwrap().lsp_version;
        let cursor = app.buffers.get(&first).unwrap().cursor();
        begin_completion_session(&mut app, first.clone(), 8, revision, lsp_version, cursor);
        app.lsp_pending.insert(8, super::LspPendingKind::Completion);
        app.buffers
            .get_mut(&first)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 4 });
        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::CompletionList {
                id: 8,
                items: vec![completion_item("stale poll completion")],
            });
        app.poll_lsp();
        assert!(
            app.completion.popup().loading && app.completion.popup().items.is_empty(),
            "poll_lsp must not display completion items after cursor context changes"
        );

        let hover_target = super::HoverTarget {
            path: first.clone(),
            position: LspPosition::new(0, 0),
        };
        app.lsp_hover.rest_started = Some(0.0);
        set_hover_request_session(&mut app, &first, 10, LspPosition::new(0, 0));
        app.lsp_hover.resting_target = Some(super::HoverTarget {
            path: first.clone(),
            position: LspPosition::new(0, 3),
        });
        app.receive_hover(10, "stale hover after pointer move".to_owned());
        assert!(
            app.lsp_hover.content.is_none(),
            "hover after pointer movement must not display documentation"
        );

        app.lsp_hover.rest_started = Some(0.0);
        set_hover_request_session(&mut app, &first, 11, LspPosition::new(0, 0));
        app.lsp_hover.resting_target = Some(hover_target.clone());
        app.active = Some(second.clone());
        app.receive_hover(11, "stale hover after tab switch".to_owned());
        assert!(
            app.lsp_hover.content.is_none(),
            "hover for inactive tab must not display documentation"
        );

        app.active = Some(first.clone());
        app.lsp_hover.rest_started = Some(0.0);
        set_hover_request_session(&mut app, &first, 12, LspPosition::new(0, 0));
        app.lsp_hover.resting_target = Some(hover_target.clone());
        app.lsp_pending.insert(12, super::LspPendingKind::Hover);
        app.dismiss_lsp_hover();
        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::HoverResult {
                id: 12,
                content: "stale hover via poll_lsp".to_owned(),
            });
        app.poll_lsp();
        assert!(
            app.lsp_hover.content.is_none(),
            "poll_lsp must not display hover documentation after dismiss/context loss"
        );

        let _ = fs::remove_file(first);
        let _ = fs::remove_file(second);
    }

    /// Always boundary: stale asynchronous LSP responses must not change buffer text,
    /// caret, revision, or active tab (see **Boundaries → Always** A4, A9, A19).
    #[test]
    fn stale_asynchronous_responses_cannot_affect_the_current_editor_state() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::{LspCompletionItem, LspResponse};
        use crate::lsp::LspClient;

        fn completion_item(label: &str) -> LspCompletionItem {
            LspCompletionItem {
                label: label.to_owned(),
                ..Default::default()
            }
        }

        fn assert_editor_snapshot(
            app: &BlueIdeApp,
            path: &std::path::Path,
            text: &str,
            cursor: CursorPosition,
            revision: u64,
        ) {
            let buffer = app.buffers.get(path).unwrap();
            assert_eq!(buffer.text(), text, "buffer text must stay unchanged");
            assert_eq!(buffer.cursor(), cursor, "caret must stay unchanged");
            assert_eq!(buffer.revision(), revision, "revision must stay unchanged");
        }

        assert_stale_lsp_results_are_never_displayed_contract();

        let first = test_file("stale_editor_first.rs", "fn main() {}\n");
        let second = test_file("stale_editor_second.rs", "fn other() {}\n");
        let mut app = BlueIdeApp::empty();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(first.clone()).unwrap();
        app.open_file(second.clone()).unwrap();
        app.active = Some(first.clone());

        let baseline_text = "fn main() {}\n";
        let baseline_cursor = CursorPosition { line: 0, col: 3 };
        app.buffers
            .get_mut(&first)
            .unwrap()
            .set_cursor(baseline_cursor);
        let baseline_revision = app.buffers.get(&first).unwrap().revision();
        let baseline_lsp_version = app.buffers.get(&first).unwrap().lsp_version;

        begin_completion_session(
            &mut app,
            first.clone(),
            2,
            baseline_revision,
            baseline_lsp_version,
            baseline_cursor,
        );
        app.receive_completion(1, vec![completion_item("stale completion")]);
        assert_editor_snapshot(
            &app,
            &first,
            baseline_text,
            baseline_cursor,
            baseline_revision,
        );
        assert!(
            app.completion.popup().loading && app.completion.popup().items.is_empty(),
            "stale completion correlation id must not populate the popup"
        );

        begin_completion_session(
            &mut app,
            first.clone(),
            3,
            baseline_revision,
            baseline_lsp_version,
            baseline_cursor,
        );
        app.buffers
            .get_mut(&first)
            .unwrap()
            .insert_at_cursor("x")
            .unwrap();
        let edited_cursor = app.buffers.get(&first).unwrap().cursor();
        let edited_revision = app.buffers.get(&first).unwrap().revision();
        app.receive_completion(3, vec![completion_item("stale after edit")]);
        assert_editor_snapshot(
            &app,
            &first,
            "fn xmain() {}\n",
            edited_cursor,
            edited_revision,
        );
        assert!(
            app.completion.popup().loading && app.completion.popup().items.is_empty(),
            "completion after buffer revision change must not display items"
        );

        let edited_lsp_version = app.buffers.get(&first).unwrap().lsp_version;
        begin_completion_session(
            &mut app,
            first.clone(),
            4,
            edited_revision,
            edited_lsp_version,
            edited_cursor,
        );
        app.buffers
            .get_mut(&first)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 0 });
        let moved_cursor = app.buffers.get(&first).unwrap().cursor();
        app.receive_completion(4, vec![completion_item("stale after cursor move")]);
        assert_editor_snapshot(
            &app,
            &first,
            "fn xmain() {}\n",
            moved_cursor,
            edited_revision,
        );
        assert!(
            app.completion.popup().loading && app.completion.popup().items.is_empty(),
            "completion after cursor move must not display items"
        );

        let moved_lsp_version = app.buffers.get(&first).unwrap().lsp_version;
        begin_completion_session(
            &mut app,
            first.clone(),
            5,
            edited_revision,
            moved_lsp_version,
            moved_cursor,
        );
        app.active = Some(second.clone());
        app.receive_completion(5, vec![completion_item("stale tab")]);
        assert_editor_snapshot(
            &app,
            &first,
            "fn xmain() {}\n",
            moved_cursor,
            edited_revision,
        );
        assert_eq!(app.active.as_ref(), Some(&second));
        assert!(
            app.completion.popup().loading && app.completion.popup().items.is_empty(),
            "completion for inactive tab must not display items"
        );

        app.active = Some(first.clone());
        app.buffers
            .get_mut(&first)
            .unwrap()
            .set_cursor(baseline_cursor);
        let poll_revision = app.buffers.get(&first).unwrap().revision();
        let poll_cursor = app.buffers.get(&first).unwrap().cursor();
        let poll_lsp_version = app.buffers.get(&first).unwrap().lsp_version;
        begin_completion_session(
            &mut app,
            first.clone(),
            8,
            poll_revision,
            poll_lsp_version,
            poll_cursor,
        );
        app.lsp_pending.insert(8, super::LspPendingKind::Completion);
        app.buffers
            .get_mut(&first)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 7 });
        let after_poll_cursor = app.buffers.get(&first).unwrap().cursor();
        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::CompletionList {
                id: 8,
                items: vec![completion_item("stale poll completion")],
            });
        app.poll_lsp();
        assert_editor_snapshot(
            &app,
            &first,
            "fn xmain() {}\n",
            after_poll_cursor,
            poll_revision,
        );
        assert!(
            app.completion.popup().loading && app.completion.popup().items.is_empty(),
            "poll_lsp must not display completion items after cursor context changes"
        );

        let hover_target = super::HoverTarget {
            path: first.clone(),
            position: LspPosition::new(0, 3),
        };
        app.lsp_hover.rest_started = Some(0.0);
        set_hover_request_session(&mut app, &first, 10, LspPosition::new(0, 3));
        app.lsp_hover.resting_target = Some(super::HoverTarget {
            path: first.clone(),
            position: LspPosition::new(0, 0),
        });
        app.receive_hover(10, "stale hover after pointer move".to_owned());
        assert_editor_snapshot(
            &app,
            &first,
            "fn xmain() {}\n",
            after_poll_cursor,
            poll_revision,
        );
        assert!(
            app.lsp_hover.content.is_none(),
            "hover after pointer movement must not display documentation"
        );

        app.lsp_hover.rest_started = Some(0.0);
        set_hover_request_session(&mut app, &first, 11, LspPosition::new(0, 3));
        app.lsp_hover.resting_target = Some(hover_target.clone());
        app.active = Some(second.clone());
        app.receive_hover(11, "stale hover after tab switch".to_owned());
        assert_editor_snapshot(
            &app,
            &first,
            "fn xmain() {}\n",
            after_poll_cursor,
            poll_revision,
        );
        assert_eq!(app.active.as_ref(), Some(&second));
        assert!(app.lsp_hover.content.is_none());

        app.active = Some(first.clone());
        app.lsp_hover.rest_started = Some(0.0);
        set_hover_request_session(&mut app, &first, 12, LspPosition::new(0, 3));
        app.lsp_hover.resting_target = Some(hover_target.clone());
        app.lsp_pending.insert(12, super::LspPendingKind::Hover);
        app.dismiss_lsp_hover();
        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::HoverResult {
                id: 12,
                content: "stale hover via poll_lsp".to_owned(),
            });
        app.poll_lsp();
        assert_editor_snapshot(
            &app,
            &first,
            "fn xmain() {}\n",
            after_poll_cursor,
            poll_revision,
        );
        assert!(app.lsp_hover.content.is_none());

        let goto_id = app.next_ui_correlation_id();
        app.pending_definitions.insert(
            goto_id,
            super::PendingDefinitionRequest {
                source_path: first.clone(),
                source_revision: poll_revision,
                source_position: after_poll_cursor,
                active_tab: first.clone(),
                is_f12: true,
            },
        );
        app.active = Some(second.clone());
        app.lsp_pending
            .insert(goto_id, super::LspPendingKind::GotoDefinition);
        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::GotoResult {
                id: goto_id,
                path: first.clone(),
                line: 0,
                col: 0,
            });
        app.poll_lsp();
        assert_eq!(app.active.as_ref(), Some(&second));
        assert_editor_snapshot(
            &app,
            &first,
            "fn xmain() {}\n",
            after_poll_cursor,
            poll_revision,
        );

        app.active = Some(first.clone());
        app.buffers
            .get_mut(&first)
            .unwrap()
            .set_cursor(baseline_cursor);
        let fresh_revision = app.buffers.get(&first).unwrap().revision();
        let fresh_lsp_version = app.buffers.get(&first).unwrap().lsp_version;
        begin_completion_session(
            &mut app,
            first.clone(),
            20,
            fresh_revision,
            fresh_lsp_version,
            baseline_cursor,
        );
        app.receive_completion(20, vec![completion_item("main")]);
        assert_editor_snapshot(
            &app,
            &first,
            "fn xmain() {}\n",
            baseline_cursor,
            fresh_revision,
        );
        assert!(app.completion.is_open());
        assert_eq!(app.completion.popup().items.len(), 1);

        let _ = fs::remove_file(first);
        let _ = fs::remove_file(second);
    }

    #[test]
    fn typing_refines_completion_without_re_requesting() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::LspCompletionItem;

        let path = test_file("type_refine.rs", "fn ma() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        let cursor = CursorPosition { line: 0, col: 5 };
        app.buffers.get_mut(&path).unwrap().set_cursor(cursor);
        begin_completion_session(&mut app, path.clone(), 1, revision, lsp_version, cursor);
        app.receive_completion(
            1,
            vec![LspCompletionItem {
                filter_text: None,
                label: "main".to_owned(),
                kind: None,
                detail: None,
                insert_text: None,
                text_edit: None,
            }],
        );
        assert!(app.completion.is_open());
        assert_eq!(app.lsp_pending.len(), 0);

        let revision_before = app.buffers.get(&path).unwrap().revision();
        let cursor_before = app.buffers.get(&path).unwrap().cursor();
        app.buffers
            .get_mut(&path)
            .unwrap()
            .insert_at_cursor("i")
            .unwrap();
        app.refine_or_dismiss_completion(&path, revision_before, cursor_before);

        assert!(app.completion.is_open());
        assert_eq!(app.buffers.get(&path).unwrap().text(), "fn mai() {}\n");
        assert_eq!(app.completion.popup().query, "mai");
        assert_eq!(app.completion.popup().filtered_indices.len(), 1);

        // typing space should dismiss
        let revision_before2 = app.buffers.get(&path).unwrap().revision();
        let cursor_before2 = app.buffers.get(&path).unwrap().cursor();
        app.buffers
            .get_mut(&path)
            .unwrap()
            .insert_at_cursor(" ")
            .unwrap();
        app.refine_or_dismiss_completion(&path, revision_before2, cursor_before2);
        assert!(!app.completion.is_open());
    }

    #[test]
    fn backspace_refines_completion() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::LspCompletionItem;

        let path = test_file("backspace_refine.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers
            .get_mut(&path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 7 });

        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        let cursor = app.buffers.get(&path).unwrap().cursor();
        begin_completion_session(&mut app, path.clone(), 1, revision, lsp_version, cursor);
        app.receive_completion(
            1,
            vec![LspCompletionItem {
                filter_text: None,
                label: "main".to_owned(),
                kind: None,
                detail: None,
                insert_text: None,
                text_edit: None,
            }],
        );
        assert!(app.completion.is_open());

        let revision_before = app.buffers.get(&path).unwrap().revision();
        let cursor_before = app.buffers.get(&path).unwrap().cursor();
        app.buffers
            .get_mut(&path)
            .unwrap()
            .delete_backward()
            .unwrap();
        app.refine_or_dismiss_completion(&path, revision_before, cursor_before);
        assert!(app.completion.is_open());
        assert_eq!(app.completion.popup().query, "mai");

        // delete another character
        let revision_before2 = app.buffers.get(&path).unwrap().revision();
        let cursor_before2 = app.buffers.get(&path).unwrap().cursor();
        app.buffers
            .get_mut(&path)
            .unwrap()
            .delete_backward()
            .unwrap();
        app.refine_or_dismiss_completion(&path, revision_before2, cursor_before2);
        assert!(app.completion.is_open());
        assert_eq!(app.completion.popup().query, "ma");
    }

    #[test]
    fn cursor_movement_dismisses_completion() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::LspCompletionItem;

        let path = test_file("cursor.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        let cursor = CursorPosition { line: 0, col: 0 };
        begin_completion_session(&mut app, path.clone(), 1, revision, lsp_version, cursor);
        app.receive_completion(
            1,
            vec![LspCompletionItem {
                filter_text: None,
                label: "main".to_owned(),
                kind: None,
                detail: None,
                insert_text: None,
                text_edit: None,
            }],
        );
        assert!(app.completion.is_open());

        let revision_before = app.buffers.get(&path).unwrap().revision();
        app.buffers
            .get_mut(&path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 4 });
        app.refine_or_dismiss_completion(&path, revision_before, cursor);
        assert!(!app.completion.is_open());
    }

    #[test]
    fn arrow_keys_change_completion_selection_without_moving_the_editor_cursor() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::LspCompletionItem;
        use egui::{Event, Key, Modifiers};

        fn completion_item(label: &str) -> LspCompletionItem {
            LspCompletionItem {
                label: label.to_owned(),
                ..Default::default()
            }
        }

        fn arrow_key_event(key: Key) -> Event {
            Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }
        }

        let path = test_file("arrow_completion.rs", "fn main() {\n    let x = 1;\n}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.search_last_active = Some(path.clone());
        let cursor = CursorPosition { line: 1, col: 8 };
        app.buffers.get_mut(&path).unwrap().set_cursor(cursor);

        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        begin_completion_session(&mut app, path.clone(), 1, revision, lsp_version, cursor);
        app.receive_completion(
            1,
            vec![
                completion_item("let"),
                completion_item("loop"),
                completion_item("match"),
            ],
        );
        assert!(app.completion.is_open());
        assert_eq!(app.completion.popup().selected, 0);

        let context = egui::Context::default();
        let editor_id = egui::Id::new(("blue_ide_editor", Some(path.clone())));
        let cursor_before = app.buffers.get(&path).unwrap().cursor();
        let text_before = app.buffers.get(&path).unwrap().text().to_owned();
        let revision_before = app.buffers.get(&path).unwrap().revision();

        for (key, expected_selected) in
            [(Key::ArrowDown, 1), (Key::ArrowUp, 0), (Key::ArrowDown, 1)]
        {
            let input = egui::RawInput {
                focused: true,
                events: vec![arrow_key_event(key)],
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            };

            let _ = context.run(input, |ctx| {
                ctx.memory_mut(|mem| mem.request_focus(editor_id));
                let _ = app.show_editor(ctx);
            });

            assert_eq!(app.completion.popup().selected, expected_selected);
            assert_eq!(app.buffers.get(&path).unwrap().cursor(), cursor_before);
            assert_eq!(app.buffers.get(&path).unwrap().text(), text_before);
            assert_eq!(app.buffers.get(&path).unwrap().revision(), revision_before);
        }
    }

    #[test]
    fn stale_responses_are_ignored() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::LspCompletionItem;

        fn completion_item(label: &str) -> LspCompletionItem {
            LspCompletionItem {
                label: label.to_owned(),
                ..Default::default()
            }
        }

        let first = test_file("stale_first.rs", "fn main() {}\n");
        let second = test_file("stale_second.rs", "fn other() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(first.clone()).unwrap();
        app.open_file(second.clone()).unwrap();
        app.active = Some(first.clone());

        let cursor = CursorPosition { line: 0, col: 0 };
        let item = completion_item("main");
        let revision = app.buffers.get(&first).unwrap().revision();
        let lsp_version = app.buffers.get(&first).unwrap().lsp_version;

        begin_completion_session(&mut app, first.clone(), 2, revision, lsp_version, cursor);
        app.receive_completion(1, vec![item.clone()]);
        assert!(app.completion.popup().loading);
        assert!(app.completion.popup().items.is_empty());

        begin_completion_session(&mut app, first.clone(), 3, revision, lsp_version, cursor);
        app.buffers
            .get_mut(&first)
            .unwrap()
            .insert_at_cursor("x")
            .unwrap();
        app.receive_completion(3, vec![item.clone()]);
        assert!(app.completion.popup().loading);
        assert!(app.completion.popup().items.is_empty());

        let revision = app.buffers.get(&first).unwrap().revision();
        begin_completion_session(&mut app, first.clone(), 5, revision, lsp_version, cursor);
        app.buffers
            .get_mut(&first)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 5 });
        app.receive_completion(5, vec![item.clone()]);
        assert!(app.completion.popup().loading);
        assert!(app.completion.popup().items.is_empty());

        begin_completion_session(&mut app, first.clone(), 4, revision, lsp_version, cursor);
        app.active = Some(second.clone());
        app.receive_completion(4, vec![item.clone()]);
        assert!(app.completion.popup().loading);
        assert!(app.completion.popup().items.is_empty());

        app.active = Some(first.clone());
        app.buffers.get_mut(&first).unwrap().set_cursor(cursor);
        let revision = app.buffers.get(&first).unwrap().revision();
        let lsp_version = app.buffers.get(&first).unwrap().lsp_version;
        let cursor = app.buffers.get(&first).unwrap().cursor();
        begin_completion_session(&mut app, first.clone(), 6, revision, lsp_version, cursor);
        app.receive_completion(6, vec![completion_item("main")]);
        assert!(app.completion.is_open());
        assert_eq!(app.completion.popup().items.len(), 1);

        let _ = fs::remove_file(first);
        let _ = fs::remove_file(second);
    }

    #[test]
    fn tab_file_revision_changes_dismiss_stale_completion_and_hover_state() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::LspCompletionItem;
        use egui::{pos2, Rect, Vec2};

        fn completion_item(label: &str) -> LspCompletionItem {
            LspCompletionItem {
                label: label.to_owned(),
                ..Default::default()
            }
        }

        fn arm_completion_and_hover(
            app: &mut BlueIdeApp,
            path: &std::path::Path,
            completion_id: u64,
        ) {
            let revision = app.buffers.get(path).unwrap().revision();
            let lsp_version = app.buffers.get(path).unwrap().lsp_version;
            let cursor = CursorPosition { line: 0, col: 3 };
            app.buffers.get_mut(path).unwrap().set_cursor(cursor);
            begin_completion_session(
                app,
                path.to_path_buf(),
                completion_id,
                revision,
                lsp_version,
                cursor,
            );
            app.receive_completion(completion_id, vec![completion_item("main")]);
            assert!(app.completion.is_open());

            let target = super::HoverTarget {
                path: path.to_path_buf(),
                position: LspPosition::new(0, 3),
            };
            app.lsp_hover.resting_target = Some(target.clone());
            app.lsp_hover.rest_started = Some(1.0);
            app.lsp_hover.displayed_target = Some(target.clone());
            app.lsp_hover.content = Some("fn main — documentation".to_owned());
            app.lsp_hover.popup_anchor =
                Some(Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)));
            set_hover_request_session(app, path, completion_id + 100, LspPosition::new(0, 3));
        }

        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));

        let first = test_file("tab_dismiss_first.rs", "fn main() {}\n");
        let second = test_file("tab_dismiss_second.rs", "fn other() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(first.clone()).unwrap();
        app.open_file(second.clone()).unwrap();
        app.active = Some(first.clone());
        app.search_last_active = Some(first.clone());
        arm_completion_and_hover(&mut app, &first, 1);

        app.active = Some(second.clone());
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                app.show_editor(ctx);
            },
        );
        assert!(!app.completion.is_open());
        assert_hover_state_cleared(&app);

        app.active = Some(first.clone());
        app.search_last_active = Some(first.clone());
        arm_completion_and_hover(&mut app, &first, 2);
        app.close_file(&first);
        assert!(!app.completion.is_open());
        assert_hover_state_cleared(&app);

        let path = test_file("revision_dismiss.rs", "fn main() {}\n");
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.search_last_active = Some(path.clone());
        arm_completion_and_hover(&mut app, &path, 3);
        let editor_id = egui::Id::new(("blue_ide_editor", Some(path.clone())));
        let revision_before = app.buffers.get(&path).unwrap().revision();
        let _ = context.run(
            egui::RawInput {
                focused: true,
                events: vec![egui::Event::Text("x".into())],
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                ctx.memory_mut(|mem| mem.request_focus(editor_id));
                app.show_editor(ctx);
            },
        );
        assert!(app.completion.is_open());
        assert_hover_state_cleared(&app);
        assert_eq!(
            app.buffers.get(&path).unwrap().revision(),
            revision_before + 1
        );
        assert_eq!(app.buffers.get(&path).unwrap().text(), "fn xmain() {}\n");

        let _revision_before2 = app.buffers.get(&path).unwrap().revision();
        let _ = context.run(
            egui::RawInput {
                focused: true,
                events: vec![egui::Event::Text(" ".into())],
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                ctx.memory_mut(|mem| mem.request_focus(editor_id));
                app.show_editor(ctx);
            },
        );
        assert!(!app.completion.is_open());

        let _ = fs::remove_file(first);
        let _ = fs::remove_file(second);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn empty_results_do_not_open_the_popup() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::LspResponse;
        use crate::lsp::LspClient;

        let path = test_file("empty_completion.rs", "fn ma() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.search_last_active = Some(path.clone());
        let cursor = CursorPosition { line: 0, col: 5 };
        app.buffers.get_mut(&path).unwrap().set_cursor(cursor);

        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        begin_completion_session(&mut app, path.clone(), 1, revision, lsp_version, cursor);
        assert!(
            app.completion.is_open(),
            "loading state is open until results arrive"
        );

        app.receive_completion(1, vec![]);
        assert!(!app.completion.is_open());
        assert!(app.completion.popup().items.is_empty());
        assert!(!app.completion.popup().loading);

        begin_completion_session(&mut app, path.clone(), 2, revision, lsp_version, cursor);
        app.lsp = Some(LspClient::new_test_client());
        app.lsp_pending.insert(2, super::LspPendingKind::Completion);
        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::CompletionList {
                id: 2,
                items: vec![],
            });
        app.poll_lsp();
        assert!(!app.completion.is_open());

        let context = egui::Context::default();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            },
            |ctx| {
                let _ = app.show_editor(ctx);
            },
        );
        assert!(!app.completion.is_open());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn add_tests_for_new_state_transitions_and_text_edits() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::completion::CompletionPopupEvent;
        use crate::lsp::types::{LspCompletionItem, LspTextEdit};

        fn completion_item(label: &str) -> LspCompletionItem {
            LspCompletionItem {
                label: label.to_owned(),
                ..Default::default()
            }
        }

        let path = test_file("state_transition_plain.rs", "fn ma() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        let cursor = CursorPosition { line: 0, col: 5 };
        app.buffers.get_mut(&path).unwrap().set_cursor(cursor);
        let revision0 = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;

        begin_completion_session(&mut app, path.clone(), 1, revision0, lsp_version, cursor);
        assert!(app.completion.is_open());
        assert!(app.completion.popup().loading);
        assert_eq!(app.completion.request_id(), Some(1));

        app.receive_completion(1, vec![completion_item("main")]);
        assert!(app.completion.is_open());
        assert!(!app.completion.popup().loading);
        assert_eq!(app.completion.popup().items.len(), 1);
        assert_eq!(app.buffers.get(&path).unwrap().revision(), revision0);

        app.apply_completion_item(0);
        assert!(!app.completion.is_open());
        assert!(app.completion.request_id().is_none());
        assert_eq!(app.buffers.get(&path).unwrap().text(), "fn main() {}\n");
        assert_eq!(app.buffers.get(&path).unwrap().revision(), revision0 + 1);
        assert!(app.buffers.get(&path).unwrap().needs_lsp_sync());

        let revision1 = app.buffers.get(&path).unwrap().revision();
        let cursor_after = app.buffers.get(&path).unwrap().cursor();
        let lsp_version_after = app.buffers.get(&path).unwrap().lsp_version;
        begin_completion_session(
            &mut app,
            path.clone(),
            2,
            revision1,
            lsp_version_after,
            cursor_after,
        );
        app.receive_completion(2, vec![completion_item("main")]);
        app.handle_completion_popup_event(CompletionPopupEvent::Dismissed);
        assert!(!app.completion.is_open());
        assert_eq!(app.buffers.get(&path).unwrap().revision(), revision1);
        assert_eq!(app.buffers.get(&path).unwrap().text(), "fn main() {}\n");

        let text_edit_path = test_file("state_transition_te.rs", "fn ma() {}\n");
        app.open_file(text_edit_path.clone()).unwrap();
        app.active = Some(text_edit_path.clone());
        let te_cursor = CursorPosition { line: 0, col: 5 };
        app.buffers
            .get_mut(&text_edit_path)
            .unwrap()
            .set_cursor(te_cursor);
        let te_revision = app.buffers.get(&text_edit_path).unwrap().revision();
        let te_version = app.buffers.get(&text_edit_path).unwrap().lsp_version;
        begin_completion_session(
            &mut app,
            text_edit_path.clone(),
            3,
            te_revision,
            te_version,
            te_cursor,
        );
        app.receive_completion(
            3,
            vec![LspCompletionItem {
                filter_text: None,
                label: "main".to_owned(),
                kind: None,
                detail: None,
                insert_text: Some("ignored".to_owned()),
                text_edit: Some(LspTextEdit {
                    line_start: 0,
                    col_start: 3,
                    line_end: 0,
                    col_end: 5,
                    new_text: "main".to_owned(),
                }),
            }],
        );
        app.apply_completion_item(0);
        assert!(!app.completion.is_open());
        assert_eq!(
            app.buffers.get(&text_edit_path).unwrap().text(),
            "fn main() {}\n"
        );
        assert_eq!(
            app.buffers.get(&text_edit_path).unwrap().revision(),
            te_revision + 1
        );

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(text_edit_path);
    }

    #[test]
    fn keep_all_popup_interactions_keyboard_accessible() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::LspCompletionItem;
        use crate::search::SearchState;
        use crate::search_panel;
        use egui::{Event, Key, Modifiers};

        fn key_event(key: Key) -> Event {
            Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }
        }

        fn completion_item(label: &str) -> LspCompletionItem {
            LspCompletionItem {
                label: label.to_owned(),
                ..Default::default()
            }
        }

        fn run_editor_key(app: &mut BlueIdeApp, path: &Path, key: Key) {
            let ctx = egui::Context::default();
            let editor_id = egui::Id::new(("blue_ide_editor", Some(path.to_path_buf())));
            let input = egui::RawInput {
                focused: true,
                events: vec![key_event(key)],
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                ctx.memory_mut(|mem| mem.request_focus(editor_id));
                let _ = app.show_editor(ctx);
            });
        }

        let completion_path = test_file("kbd_completion.rs", "fn main() {\n    let x = 1;\n}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(completion_path.clone()).unwrap();
        app.active = Some(completion_path.clone());
        app.search_last_active = Some(completion_path.clone());
        let cursor = CursorPosition { line: 1, col: 8 };
        app.buffers
            .get_mut(&completion_path)
            .unwrap()
            .set_cursor(cursor);
        let revision = app.buffers.get(&completion_path).unwrap().revision();
        let lsp_version = app.buffers.get(&completion_path).unwrap().lsp_version;
        begin_completion_session(
            &mut app,
            completion_path.clone(),
            1,
            revision,
            lsp_version,
            cursor,
        );
        app.receive_completion(
            1,
            vec![
                completion_item("let"),
                completion_item("loop"),
                completion_item("match"),
            ],
        );
        assert!(app.completion.is_open());

        run_editor_key(&mut app, &completion_path, Key::ArrowDown);
        assert_eq!(app.completion.popup().selected, 1);

        run_editor_key(&mut app, &completion_path, Key::PageDown);
        assert_eq!(app.completion.popup().selected, 2);

        run_editor_key(&mut app, &completion_path, Key::Tab);
        assert!(!app.completion.is_open());

        let revision_after_tab = app.buffers.get(&completion_path).unwrap().revision();
        let cursor_after_tab = app.buffers.get(&completion_path).unwrap().cursor();
        begin_completion_session(
            &mut app,
            completion_path.clone(),
            2,
            revision_after_tab,
            lsp_version,
            cursor_after_tab,
        );
        app.receive_completion(2, vec![completion_item("let")]);
        run_editor_key(&mut app, &completion_path, Key::Escape);
        assert!(!app.completion.is_open());

        let settings_path = test_file("kbd_settings.toml", "");
        let _ = fs::remove_file(&settings_path);
        let mut app = settings_app(settings_path.clone());
        app.open_settings();
        assert!(app.show_settings_window);

        let ctx = egui::Context::default();
        let _ = ctx.run(
            egui::RawInput {
                events: vec![key_event(Key::Escape)],
                ..Default::default()
            },
            |ctx| app.show_settings(ctx),
        );
        assert!(
            !app.show_settings_window,
            "Settings modal should cancel on Escape"
        );

        let path = test_file("kbd_unsaved.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.buffers
            .get_mut(&path)
            .unwrap()
            .insert_at_cursor("x")
            .unwrap();
        app.pending_close = Some(path.clone());
        let _ = ctx.run(
            egui::RawInput {
                events: vec![key_event(Key::Escape)],
                ..Default::default()
            },
            |ctx| app.show_confirmation(ctx),
        );
        assert!(
            app.pending_close.is_none(),
            "Unsaved-close modal should cancel on Escape"
        );

        app.pending_exit = true;
        let _ = ctx.run(
            egui::RawInput {
                events: vec![key_event(Key::Escape)],
                ..Default::default()
            },
            |ctx| app.show_confirmation(ctx),
        );
        assert!(
            !app.pending_exit,
            "Exit-confirm modal should cancel on Escape"
        );

        let mut search = SearchState::new();
        search.visible = true;
        let palette = built_in_theme(Theme::Dark, None).palette.semantic;
        let mut panel_closed = false;
        let _ = ctx.run(
            egui::RawInput {
                focused: true,
                events: vec![key_event(Key::Escape)],
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let out = search_panel::show(ui, &mut search, 0, palette);
                    panel_closed = out.closed;
                });
            },
        );
        assert!(panel_closed, "Find/replace panel should close on Escape");

        let _ = fs::remove_file(settings_path);
        let _ = fs::remove_file(completion_path);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn escape_dismisses_without_editing() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::LspCompletionItem;
        use egui::{Event, Key, Modifiers};

        fn completion_item(label: &str) -> LspCompletionItem {
            LspCompletionItem {
                label: label.to_owned(),
                ..Default::default()
            }
        }

        let path = test_file("escape_dismiss.rs", "fn main() {\n    let x = 1;\n}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.search_last_active = Some(path.clone());
        let cursor = CursorPosition { line: 1, col: 8 };
        app.buffers.get_mut(&path).unwrap().set_cursor(cursor);

        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        begin_completion_session(&mut app, path.clone(), 1, revision, lsp_version, cursor);
        app.receive_completion(1, vec![completion_item("let"), completion_item("loop")]);
        assert!(app.completion.is_open());

        let text_before = app.buffers.get(&path).unwrap().text().to_owned();
        let revision_before = app.buffers.get(&path).unwrap().revision();
        let cursor_before = app.buffers.get(&path).unwrap().cursor();

        let context = egui::Context::default();
        let editor_id = egui::Id::new(("blue_ide_editor", Some(path.clone())));
        let input = egui::RawInput {
            focused: true,
            events: vec![Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };

        let mut key_consumed = false;
        let _ = context.run(input, |ctx| {
            ctx.memory_mut(|mem| mem.request_focus(editor_id));
            let _ = app.show_editor(ctx);
            key_consumed = !ctx.input(|input| {
                input.events.iter().any(|event| {
                    matches!(
                        event,
                        Event::Key {
                            key: Key::Escape,
                            pressed: true,
                            modifiers: Modifiers::NONE,
                            ..
                        }
                    )
                })
            });
        });

        assert!(
            key_consumed,
            "Escape should be consumed before editor input"
        );
        assert!(!app.completion.is_open());
        assert_eq!(app.buffers.get(&path).unwrap().text(), text_before);
        assert_eq!(app.buffers.get(&path).unwrap().revision(), revision_before);
        assert_eq!(app.buffers.get(&path).unwrap().cursor(), cursor_before);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn enter_and_tab_accept() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::LspCompletionItem;
        use egui::{Event, Key, Modifiers};

        fn completion_item(label: &str) -> LspCompletionItem {
            LspCompletionItem {
                label: label.to_owned(),
                ..Default::default()
            }
        }

        fn accept_key_event(key: Key) -> Event {
            Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }
        }

        let path = test_file("enter_tab_accept.rs", "fn ma() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.search_last_active = Some(path.clone());

        let context = egui::Context::default();
        let editor_id = egui::Id::new(("blue_ide_editor", Some(path.clone())));

        for (request_id, key) in [(1, Key::Enter), (2, Key::Tab)] {
            app.buffers
                .get_mut(&path)
                .unwrap()
                .set_cursor(CursorPosition { line: 0, col: 5 });
            let revision = app.buffers.get(&path).unwrap().revision();
            let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
            let cursor = app.buffers.get(&path).unwrap().cursor();
            begin_completion_session(
                &mut app,
                path.clone(),
                request_id,
                revision,
                lsp_version,
                cursor,
            );
            app.receive_completion(request_id, vec![completion_item("main")]);
            assert!(app.completion.is_open());

            let revision_before = app.buffers.get(&path).unwrap().revision();
            let input = egui::RawInput {
                focused: true,
                events: vec![accept_key_event(key)],
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            };

            let mut key_consumed = false;
            let _ = context.run(input, |ctx| {
                ctx.memory_mut(|mem| mem.request_focus(editor_id));
                let _ = app.show_editor(ctx);
                key_consumed = !ctx.input(|input| {
                    input.events.iter().any(|event| {
                        matches!(
                            event,
                            Event::Key {
                                key: pressed_key,
                                pressed: true,
                                modifiers,
                                ..
                            } if *pressed_key == key && *modifiers == Modifiers::NONE
                        )
                    })
                });
            });

            assert!(
                key_consumed,
                "{key:?} should be consumed before editor input"
            );
            assert_eq!(app.buffers.get(&path).unwrap().text(), "fn main() {}\n");
            assert_eq!(
                app.buffers.get(&path).unwrap().revision(),
                revision_before + 1
            );
            assert!(!app.completion.is_open());

            if key == Key::Enter {
                app.buffers
                    .get_mut(&path)
                    .unwrap()
                    .replace_char_range(3, 7, "ma")
                    .unwrap();
            }
        }

        let _ = fs::remove_file(path);
    }

    fn assert_popup_keyboard_poll_skips_when_completion_is_closed() {
        let app_rs = include_str!("app.rs");
        let app_production = app_rs
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(&app_rs);
        assert!(
            app_production.contains("if !self.completion.is_open()"),
            "app must skip completion keyboard poll when the popup is closed"
        );
        assert!(
            app_production.contains("fn collect_completion_popup_keyboard_event"),
            "completion keyboard routing must be centralized"
        );

        let completion_rs = include_str!("editor/completion.rs");
        assert!(
            completion_rs.contains("if !self.is_open() {\n            return (false, None);"),
            "CompletionState::poll_keyboard_event must not consume keys when closed"
        );

        let widget_rs = include_str!("editor/widget.rs");
        let widget_production = widget_rs
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(widget_rs);
        assert!(
            widget_production.contains("fn handle_keyboard_input"),
            "editor widget must handle normal keyboard input"
        );
        assert!(
            widget_production.contains("Key::ArrowDown"),
            "arrow keys must reach the editor input path"
        );
        assert!(
            widget_production.contains("Event::Text(text)"),
            "typing must reach the editor input path"
        );
    }

    /// Never boundary: normal editor keystrokes must not be swallowed when no popup is
    /// open (see **Boundaries → Never** §14).
    #[test]
    fn swallow_normal_editor_keystrokes_when_no_popup_is_open() {
        use crate::editor::buffer::CursorPosition;
        use egui::{Event, Key, Modifiers};

        fn key_event(key: Key) -> Event {
            Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }
        }

        assert_popup_keyboard_poll_skips_when_completion_is_closed();

        let path = test_file("no_popup_keys.rs", "fn main() {\n    let x = 1;\n}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.search_last_active = Some(path.clone());
        assert!(!app.completion.is_open());

        let cursor = CursorPosition { line: 0, col: 3 };
        app.buffers.get_mut(&path).unwrap().set_cursor(cursor);

        let context = egui::Context::default();
        let editor_id = egui::Id::new(("blue_ide_editor", Some(path.clone())));
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

        let revision_before = app.buffers.get(&path).unwrap().revision();
        let _ = context.run(
            egui::RawInput {
                focused: true,
                events: vec![Event::Text("!".into())],
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                ctx.memory_mut(|mem| mem.request_focus(editor_id));
                let _ = app.show_editor(ctx);
            },
        );
        assert!(
            app.buffers.get(&path).unwrap().revision() > revision_before,
            "typing must edit the buffer when completion is closed"
        );
        assert_eq!(
            app.buffers.get(&path).unwrap().text(),
            "fn !main() {\n    let x = 1;\n}\n"
        );

        app.buffers.get_mut(&path).unwrap().set_cursor(cursor);
        app.editor_states.get_mut(&path).unwrap().request_focus();
        let _ = context.run(
            egui::RawInput {
                focused: true,
                events: vec![key_event(Key::ArrowLeft)],
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                ctx.memory_mut(|mem| mem.request_focus(editor_id));
                let _ = app.show_editor(ctx);
            },
        );
        assert_eq!(
            app.buffers.get(&path).unwrap().cursor(),
            CursorPosition { line: 0, col: 2 },
            "arrow keys must move the editor cursor when completion is closed"
        );

        let mut key_still_available = false;
        let _ = context.run(
            egui::RawInput {
                focused: true,
                events: vec![key_event(Key::ArrowDown)],
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                ctx.memory_mut(|mem| mem.request_focus(editor_id));
                let event = app.collect_completion_popup_keyboard_event(ctx);
                assert!(
                    event.is_none(),
                    "completion keyboard poll must not emit events when closed"
                );
                key_still_available = ctx.input(|input| {
                    input.events.iter().any(|event| {
                        matches!(
                            event,
                            Event::Key {
                                key: Key::ArrowDown,
                                pressed: true,
                                modifiers: Modifiers::NONE,
                                ..
                            }
                        )
                    })
                });
            },
        );
        assert!(
            key_still_available,
            "completion must not consume arrow keys when the popup is closed"
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn completion_navigation_keys_are_consumed_before_editor_input() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::LspCompletionItem;
        use egui::{Event, Key, Modifiers};

        fn completion_item(label: &str) -> LspCompletionItem {
            LspCompletionItem {
                label: label.to_owned(),
                ..Default::default()
            }
        }

        fn key_event(key: Key) -> Event {
            Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }
        }

        fn run_navigation_key(
            app: &mut BlueIdeApp,
            context: &egui::Context,
            path: &std::path::Path,
            editor_id: egui::Id,
            key: Key,
        ) {
            let cursor_before = app.buffers.get(path).unwrap().cursor();
            let revision_before = app.buffers.get(path).unwrap().revision();
            let text_before = app.buffers.get(path).unwrap().text().to_owned();

            let input = egui::RawInput {
                focused: true,
                events: vec![key_event(key)],
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            };

            let mut key_consumed = false;
            let _ = context.run(input, |ctx| {
                ctx.memory_mut(|mem| mem.request_focus(editor_id));
                let _ = app.show_editor(ctx);
                key_consumed = !ctx.input(|input| {
                    input.events.iter().any(|event| {
                        matches!(
                            event,
                            Event::Key {
                                key: pressed_key,
                                pressed: true,
                                modifiers,
                                ..
                            } if *pressed_key == key && *modifiers == Modifiers::NONE
                        )
                    })
                });
            });

            assert!(key_consumed);
            assert_eq!(app.buffers.get(path).unwrap().cursor(), cursor_before);
            assert_eq!(app.buffers.get(path).unwrap().revision(), revision_before);
            assert_eq!(app.buffers.get(path).unwrap().text(), text_before);
        }

        let path = test_file("nav_keys.rs", "fn main() {\n    let x = 1;\n}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.search_last_active = Some(path.clone());
        let cursor = CursorPosition { line: 1, col: 8 };
        app.buffers.get_mut(&path).unwrap().set_cursor(cursor);

        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        begin_completion_session(&mut app, path.clone(), 1, revision, lsp_version, cursor);
        app.receive_completion(
            1,
            vec![
                completion_item("let"),
                completion_item("loop"),
                completion_item("match"),
            ],
        );
        assert!(app.completion.is_open());
        assert_eq!(app.completion.popup().selected, 0);

        let context = egui::Context::default();
        let editor_id = egui::Id::new(("blue_ide_editor", Some(path.clone())));

        run_navigation_key(&mut app, &context, &path, editor_id, Key::ArrowDown);
        assert_eq!(app.completion.popup().selected, 1);

        run_navigation_key(&mut app, &context, &path, editor_id, Key::ArrowUp);
        assert_eq!(app.completion.popup().selected, 0);

        run_navigation_key(&mut app, &context, &path, editor_id, Key::PageDown);
        assert_eq!(app.completion.popup().selected, 2);

        run_navigation_key(&mut app, &context, &path, editor_id, Key::PageUp);
        assert_eq!(app.completion.popup().selected, 0);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn file_closure_and_modal_opening_dismiss_completion() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::LspCompletionItem;

        let path = test_file("close_modal.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        begin_completion_session(
            &mut app,
            path.clone(),
            1,
            revision,
            lsp_version,
            CursorPosition { line: 0, col: 0 },
        );
        app.receive_completion(
            1,
            vec![LspCompletionItem {
                label: "main".to_owned(),
                ..Default::default()
            }],
        );
        assert!(app.completion.is_open());

        app.close_file(&path);
        assert!(!app.completion.is_open());

        begin_completion_session(
            &mut app,
            path.clone(),
            2,
            revision,
            lsp_version,
            CursorPosition { line: 0, col: 0 },
        );
        app.receive_completion(
            2,
            vec![LspCompletionItem {
                label: "main".to_owned(),
                ..Default::default()
            }],
        );
        assert!(app.completion.is_open());

        app.open_settings();
        assert!(!app.completion.is_open());
        assert!(app.has_modal());
    }

    #[test]
    fn completion_inserts_snippet_like_text_as_plain_text() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::{LspCompletionItem, LspTextEdit};

        let snippet_body = "println!(\"{}\", ${1:})";
        let path = test_file("snippet_plain.rs", "");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        let cursor = CursorPosition { line: 0, col: 0 };
        begin_completion_session(&mut app, path.clone(), 1, revision, lsp_version, cursor);
        app.receive_completion(
            1,
            vec![LspCompletionItem {
                label: "println!".to_owned(),
                kind: Some("Snippet".to_owned()),
                insert_text: Some(snippet_body.to_owned()),
                ..Default::default()
            }],
        );
        app.apply_completion_item(0);
        assert_eq!(app.buffers.get(&path).unwrap().text(), snippet_body);
        assert_eq!(
            app.buffers.get(&path).unwrap().cursor(),
            CursorPosition {
                line: 0,
                col: snippet_body.chars().count(),
            }
        );

        {
            let buffer = app.buffers.get_mut(&path).unwrap();
            let len = buffer.len_chars();
            buffer.replace_char_range(0, len, "x").unwrap();
            buffer.set_cursor(CursorPosition { line: 0, col: 1 });
        }
        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        let cursor = app.buffers.get(&path).unwrap().cursor();
        begin_completion_session(&mut app, path.clone(), 2, revision, lsp_version, cursor);
        app.receive_completion(
            2,
            vec![LspCompletionItem {
                label: "for loop".to_owned(),
                kind: Some("Snippet".to_owned()),
                text_edit: Some(LspTextEdit {
                    line_start: 0,
                    col_start: 0,
                    line_end: 0,
                    col_end: 1,
                    new_text: "for ${1:i} in ${2:iter} {}".to_owned(),
                }),
                ..Default::default()
            }],
        );
        app.apply_completion_item(0);
        assert_eq!(
            app.buffers.get(&path).unwrap().text(),
            "for ${1:i} in ${2:iter} {}"
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn completion_uses_insert_text_when_non_empty_otherwise_label() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::LspCompletionItem;

        let path = test_file("insert_text.rs", "fn ma() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers
            .get_mut(&path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 5 });

        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        let cursor = app.buffers.get(&path).unwrap().cursor();
        begin_completion_session(&mut app, path.clone(), 1, revision, lsp_version, cursor);
        app.receive_completion(
            1,
            vec![LspCompletionItem {
                label: "main".to_owned(),
                insert_text: Some("inserted".to_owned()),
                ..Default::default()
            }],
        );
        app.apply_completion_item(0);
        assert_eq!(app.buffers.get(&path).unwrap().text(), "fn inserted() {}\n");

        app.buffers
            .get_mut(&path)
            .unwrap()
            .replace_char_range(3, 11, "ma")
            .unwrap();
        app.buffers
            .get_mut(&path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 5 });
        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        let cursor = app.buffers.get(&path).unwrap().cursor();
        begin_completion_session(&mut app, path.clone(), 2, revision, lsp_version, cursor);
        app.receive_completion(
            2,
            vec![LspCompletionItem {
                label: "main".to_owned(),
                ..Default::default()
            }],
        );
        app.apply_completion_item(0);
        assert_eq!(app.buffers.get(&path).unwrap().text(), "fn main() {}\n");

        app.buffers
            .get_mut(&path)
            .unwrap()
            .replace_char_range(3, 7, "ma")
            .unwrap();
        app.buffers
            .get_mut(&path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 5 });
        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        let cursor = app.buffers.get(&path).unwrap().cursor();
        begin_completion_session(&mut app, path.clone(), 3, revision, lsp_version, cursor);
        app.receive_completion(
            3,
            vec![LspCompletionItem {
                label: "main".to_owned(),
                insert_text: Some(String::new()),
                ..Default::default()
            }],
        );
        app.apply_completion_item(0);
        assert_eq!(app.buffers.get(&path).unwrap().text(), "fn main() {}\n");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn completion_text_edit_beats_insert_text_at_acceptance_time() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::{LspCompletionItem, LspTextEdit};

        let path = test_file("accept_precedence.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers
            .get_mut(&path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 11 });

        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        let cursor = app.buffers.get(&path).unwrap().cursor();
        begin_completion_session(&mut app, path.clone(), 1, revision, lsp_version, cursor);
        app.receive_completion(
            1,
            vec![LspCompletionItem {
                filter_text: None,
                label: "main".to_owned(),
                insert_text: Some("from_insert_text".to_owned()),
                text_edit: Some(LspTextEdit {
                    line_start: 0,
                    col_start: 3,
                    line_end: 0,
                    col_end: 7,
                    new_text: "test".to_owned(),
                }),
                ..Default::default()
            }],
        );
        app.apply_completion_item(0);
        assert_eq!(app.buffers.get(&path).unwrap().text(), "fn test() {}\n");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn completion_replaces_identifier_prefix_without_text_edit() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::LspCompletionItem;

        let path = test_file("prefix.rs", "fn ma() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers
            .get_mut(&path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 5 });

        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        let cursor = app.buffers.get(&path).unwrap().cursor();
        let prefix = app
            .buffers
            .get(&path)
            .unwrap()
            .identifier_prefix_char_range_at(cursor)
            .expect("prefix");
        assert_eq!(prefix, 3..5);
        begin_completion_session(&mut app, path.clone(), 1, revision, lsp_version, cursor);
        app.receive_completion(
            1,
            vec![LspCompletionItem {
                filter_text: None,
                label: "main".to_owned(),
                ..Default::default()
            }],
        );
        app.apply_completion_item(0);

        assert_eq!(app.buffers.get(&path).unwrap().text(), "fn main() {}\n");
        assert_eq!(
            app.buffers.get(&path).unwrap().cursor(),
            CursorPosition { line: 0, col: 7 }
        );
        assert_eq!(app.buffers.get(&path).unwrap().revision(), revision + 1);
        assert!(app.buffers.get(&path).unwrap().is_modified());
        assert!(app.buffers.get(&path).unwrap().needs_lsp_sync());
        assert!(!app.completion.is_open());
    }

    #[test]
    fn completion_apply_allows_normal_lsp_did_change_sync() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::LspCompletionItem;
        use crate::lsp::LspClient;

        let root = std::env::temp_dir().join(format!(
            "blue_ide_lsp_sync_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(&path, "fn ma() {}\n").unwrap();

        let mut app = BlueIdeApp::empty();
        app.tree.load(root.clone()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();
        app.buffers
            .get_mut(&path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 5 });

        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        let cursor = app.buffers.get(&path).unwrap().cursor();
        begin_completion_session(&mut app, path.clone(), 1, revision, lsp_version, cursor);
        app.receive_completion(
            1,
            vec![LspCompletionItem {
                filter_text: None,
                label: "main".to_owned(),
                ..Default::default()
            }],
        );
        app.apply_completion_item(0);

        assert!(app.buffers.get(&path).unwrap().needs_lsp_sync());
        app.sync_lsp_changes();
        assert!(!app.buffers.get(&path).unwrap().needs_lsp_sync());

        let requests = app.lsp.as_mut().unwrap().drain_pending_requests();
        let has_did_open = requests.iter().any(|r| matches!(r, crate::lsp::types::LspRequest::DidOpen { path: opened, .. } if *opened == path));
        let has_doc_symbol = requests.iter().any(|r| matches!(r, crate::lsp::types::LspRequest::DocumentSymbol { path: symbol_path, .. } if *symbol_path == path));
        let has_did_change = requests.iter().any(|r| matches!(
            r,
            crate::lsp::types::LspRequest::DidChange {
                path: changed,
                text,
                version: 1,
            } if *changed == path && text == "fn main() {}\n"
        ));
        assert!(has_did_open, "Should send DidOpen request");
        assert!(has_doc_symbol, "Should send DocumentSymbol request");
        assert!(has_did_change, "Should send DidChange request");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn completion_does_not_delete_punctuation_or_whitespace() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::LspCompletionItem;

        fn accept_label(
            app: &mut BlueIdeApp,
            path: &std::path::Path,
            cursor: CursorPosition,
            request_id: u64,
            label: &str,
        ) {
            let revision = app.buffers.get(path).unwrap().revision();
            let lsp_version = app.buffers.get(path).unwrap().lsp_version;
            begin_completion_session(
                app,
                path.to_path_buf(),
                request_id,
                revision,
                lsp_version,
                cursor,
            );
            app.receive_completion(
                request_id,
                vec![LspCompletionItem {
                    filter_text: None,
                    label: label.to_owned(),
                    ..Default::default()
                }],
            );
            app.apply_completion_item(0);
        }

        let path = test_file("preserve_punct.rs", "self.ba\nfoo::ba\nmain(\nlet ma\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        app.buffers
            .get_mut(&path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 7 });
        accept_label(
            &mut app,
            &path,
            CursorPosition { line: 0, col: 7 },
            1,
            "bar",
        );

        app.buffers
            .get_mut(&path)
            .unwrap()
            .set_cursor(CursorPosition { line: 1, col: 7 });
        accept_label(
            &mut app,
            &path,
            CursorPosition { line: 1, col: 7 },
            2,
            "bar",
        );

        app.buffers
            .get_mut(&path)
            .unwrap()
            .set_cursor(CursorPosition { line: 2, col: 5 });
        accept_label(
            &mut app,
            &path,
            CursorPosition { line: 2, col: 5 },
            3,
            "arg",
        );

        app.buffers
            .get_mut(&path)
            .unwrap()
            .set_cursor(CursorPosition { line: 3, col: 6 });
        accept_label(
            &mut app,
            &path,
            CursorPosition { line: 3, col: 6 },
            4,
            "main",
        );

        assert_eq!(
            app.buffers.get(&path).unwrap().text(),
            "self.bar\nfoo::bar\nmain(arg\nlet main\n"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn completion_inserts_at_cursor_when_identifier_prefix_is_empty() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::LspCompletionItem;

        let path = test_file("no_prefix.rs", "fn () {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers
            .get_mut(&path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 3 });

        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        let cursor = app.buffers.get(&path).unwrap().cursor();
        let prefix = app
            .buffers
            .get(&path)
            .unwrap()
            .identifier_prefix_char_range_at(cursor)
            .expect("prefix");
        assert_eq!(prefix, 3..3);
        begin_completion_session(&mut app, path.clone(), 1, revision, lsp_version, cursor);
        app.receive_completion(
            1,
            vec![LspCompletionItem {
                filter_text: None,
                label: "main".to_owned(),
                ..Default::default()
            }],
        );
        app.apply_completion_item(0);

        assert_eq!(app.buffers.get(&path).unwrap().text(), "fn main() {}\n");
        assert!(!app.completion.is_open());
    }

    #[test]
    fn clicking_completion_row_applies_clicked_item() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::completion::{
            CompletionPopupAnchor, CompletionPopupEvent, CompletionPopupOutput,
        };
        use crate::lsp::types::LspCompletionItem;
        use egui::{pos2, PointerButton, Rect, Vec2};

        fn completion_item(label: &str) -> LspCompletionItem {
            LspCompletionItem {
                filter_text: None,
                label: label.to_owned(),
                ..Default::default()
            }
        }

        let path = test_file("click_accept.rs", "fn ma() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.search_last_active = Some(path.clone());
        app.buffers
            .get_mut(&path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 5 });

        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        let cursor = app.buffers.get(&path).unwrap().cursor();
        begin_completion_session(&mut app, path.clone(), 1, revision, lsp_version, cursor);
        app.receive_completion(1, vec![completion_item("main"), completion_item("match")]);
        assert!(app.completion.is_open());
        assert_eq!(app.completion.popup().selected, 0);

        let context = egui::Context::default();
        let anchor = Rect::from_min_max(pos2(100.0, 200.0), pos2(108.0, 220.0));
        app.completion_anchor = CompletionPopupAnchor::from_screen_rect(Some(anchor));
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let palette = app.active_palette.semantic;

        let mut layout_output = CompletionPopupOutput::default();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                layout_output = app.completion.show(ctx, anchor, palette);
            },
        );
        assert!(layout_output.row_hit_rects.len() > 1);

        let click_pos = layout_output.row_hit_rects[1].center();
        let modifiers = egui::Modifiers::NONE;
        let mut click_output = CompletionPopupOutput::default();
        for input in [
            egui::RawInput {
                screen_rect: Some(screen),
                events: vec![egui::Event::PointerMoved(click_pos)],
                modifiers,
                ..Default::default()
            },
            egui::RawInput {
                screen_rect: Some(screen),
                events: vec![egui::Event::PointerButton {
                    pos: click_pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers,
                }],
                modifiers,
                ..Default::default()
            },
            egui::RawInput {
                screen_rect: Some(screen),
                events: vec![egui::Event::PointerButton {
                    pos: click_pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers,
                }],
                modifiers,
                ..Default::default()
            },
        ] {
            let _ = context.run(input, |ctx| {
                click_output = app.completion.show(ctx, anchor, palette);
            });
        }

        assert_eq!(
            click_output.event,
            Some(CompletionPopupEvent::Accepted { index: 1 })
        );
        assert_eq!(app.completion.popup().selected, 1);
        if let Some(event) = click_output.event {
            app.handle_completion_popup_event(event);
        }

        assert_eq!(app.buffers.get(&path).unwrap().text(), "fn match() {}\n");
        assert!(!app.completion.is_open());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn clicking_outside_completion_popup_dismisses_it() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::completion::{CompletionPopupAnchor, CompletionPopupOutput};
        use crate::lsp::types::LspCompletionItem;
        use egui::{pos2, PointerButton, Rect, Vec2};

        let path = test_file("outside_click.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.search_last_active = Some(path.clone());

        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        let cursor = CursorPosition { line: 0, col: 8 };
        app.buffers.get_mut(&path).unwrap().set_cursor(cursor);
        begin_completion_session(&mut app, path.clone(), 1, revision, lsp_version, cursor);
        app.receive_completion(
            1,
            vec![LspCompletionItem {
                filter_text: None,
                label: "main".to_owned(),
                ..Default::default()
            }],
        );
        assert!(app.completion.is_open());

        let context = egui::Context::default();
        let anchor = Rect::from_min_max(pos2(100.0, 200.0), pos2(108.0, 220.0));
        app.completion_anchor = CompletionPopupAnchor::from_screen_rect(Some(anchor));
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let text_before = app.buffers.get(&path).unwrap().text().to_owned();
        let revision_before = app.buffers.get(&path).unwrap().revision();

        let mut layout_output = CompletionPopupOutput::default();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                layout_output = app
                    .completion
                    .show(ctx, anchor, app.active_palette.semantic);
            },
        );
        assert!(layout_output.popup_rect.is_positive());

        let outside_pos = pos2(layout_output.popup_rect.left() - 20.0, 10.0);
        assert!(!layout_output.popup_rect.contains(outside_pos));

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                events: vec![
                    egui::Event::PointerMoved(outside_pos),
                    egui::Event::PointerButton {
                        pos: outside_pos,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            },
            |ctx| {
                let _ = app.show_editor(ctx);
            },
        );

        assert!(!app.completion.is_open());
        assert_eq!(app.buffers.get(&path).unwrap().text(), text_before);
        assert_eq!(app.buffers.get(&path).unwrap().revision(), revision_before);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn clicking_outside_hover_popup_dismisses_it() {
        use crate::editor::hover::show_hover_documentation;
        use crate::lsp::LspClient;
        use egui::{pos2, Event, PointerButton, Rect, Vec2};

        let path = test_file("hover_outside_click.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        };
        app.lsp_hover.resting_target = Some(target);
        app.lsp_hover.displayed_target = app.lsp_hover.resting_target.clone();
        app.lsp_hover.popup_anchor =
            Some(Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)));
        app.lsp_hover.content = Some("fn main — documentation".to_owned());

        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let mut layout_output = crate::editor::hover::HoverPopupOutput::default();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                layout_output = show_hover_documentation(
                    ctx,
                    app.lsp_hover.content.as_deref().unwrap(),
                    app.lsp_hover.popup_anchor.unwrap(),
                    Some(screen),
                );
            },
        );
        app.lsp_hover.popup_rect = Some(layout_output.popup_rect);
        assert!(layout_output.popup_rect.is_positive());

        let outside_pos = pos2(layout_output.popup_rect.left() - 20.0, 10.0);
        assert!(!layout_output.popup_rect.contains(outside_pos));
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                events: vec![
                    Event::PointerMoved(outside_pos),
                    Event::PointerButton {
                        pos: outside_pos,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            },
            |ctx| app.dismiss_lsp_hover_on_outside_click(ctx),
        );

        assert_hover_state_cleared(&app);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn clicking_source_text_dismisses_lsp_hover() {
        use crate::editor::hover::show_hover_documentation;
        use crate::lsp::LspClient;
        use egui::{pos2, Event, PointerButton, Rect, Vec2};

        let path = test_file("hover_source_click.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        };
        app.lsp_hover.resting_target = Some(target);
        app.lsp_hover.displayed_target = app.lsp_hover.resting_target.clone();
        app.lsp_hover.popup_anchor =
            Some(Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)));
        app.lsp_hover.content = Some("fn main — documentation".to_owned());

        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let mut layout_output = crate::editor::hover::HoverPopupOutput::default();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                layout_output = show_hover_documentation(
                    ctx,
                    app.lsp_hover.content.as_deref().unwrap(),
                    app.lsp_hover.popup_anchor.unwrap(),
                    Some(screen),
                );
            },
        );
        app.lsp_hover.popup_rect = Some(layout_output.popup_rect);

        let source_pos = app.lsp_hover.popup_anchor.unwrap().center();
        assert!(!layout_output.popup_rect.contains(source_pos));
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                events: vec![
                    Event::PointerMoved(source_pos),
                    Event::PointerButton {
                        pos: source_pos,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            },
            |ctx| app.dismiss_lsp_hover_on_outside_click(ctx),
        );

        assert_hover_state_cleared(&app);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn clicking_hover_popup_does_not_dismiss_it() {
        use crate::editor::hover::show_hover_documentation;
        use crate::lsp::LspClient;
        use egui::{pos2, Event, PointerButton, Rect, Vec2};

        let path = test_file("hover_popup_click.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        };
        app.lsp_hover.resting_target = Some(target.clone());
        app.lsp_hover.displayed_target = Some(target.clone());
        app.lsp_hover.popup_anchor =
            Some(Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)));
        app.lsp_hover.content = Some("fn main — documentation".to_owned());

        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let mut layout_output = crate::editor::hover::HoverPopupOutput::default();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                layout_output = show_hover_documentation(
                    ctx,
                    app.lsp_hover.content.as_deref().unwrap(),
                    app.lsp_hover.popup_anchor.unwrap(),
                    Some(screen),
                );
            },
        );
        app.lsp_hover.popup_rect = Some(layout_output.popup_rect);

        let over_popup = layout_output.popup_rect.center();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                events: vec![
                    Event::PointerMoved(over_popup),
                    Event::PointerButton {
                        pos: over_popup,
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            },
            |ctx| app.dismiss_lsp_hover_on_outside_click(ctx),
        );

        assert_eq!(
            app.lsp_hover.content.as_deref(),
            Some("fn main — documentation")
        );
        assert_eq!(app.lsp_hover.resting_target.as_ref(), Some(&target));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn hover_request_is_debounced_until_pointer_rests() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::types::LspRequest;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_debounce.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();

        let pointer_hover = HoveredSourcePosition {
            position: CursorPosition { line: 0, col: 4 },
            token_rect: egui::Rect::from_min_size(pos2(80.0, 40.0), egui::vec2(24.0, 18.0)),
        };
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let base_time = 10.0;

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );
        assert!(app.lsp_hover.resting_target.is_some());
        assert!(hover_request_id(&app).is_none());
        assert!(app
            .lsp
            .as_mut()
            .unwrap()
            .drain_pending_requests()
            .iter()
            .all(|request| !matches!(request, LspRequest::Hover { .. })));

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS - 0.02),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );
        assert!(hover_request_id(&app).is_none());

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 0.02),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );
        assert!(hover_request_id(&app).is_some());
        assert!(app
            .lsp
            .as_mut()
            .unwrap()
            .drain_pending_requests()
            .iter()
            .any(|request| matches!(request, LspRequest::Hover { .. })));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn pointer_movement_resets_hover_debounce() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_reset.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();

        let first_hover = HoveredSourcePosition {
            position: CursorPosition { line: 0, col: 2 },
            token_rect: egui::Rect::from_min_size(pos2(70.0, 40.0), egui::vec2(24.0, 18.0)),
        };
        let second_hover = HoveredSourcePosition {
            position: CursorPosition { line: 0, col: 6 },
            token_rect: egui::Rect::from_min_size(pos2(110.0, 40.0), egui::vec2(24.0, 18.0)),
        };
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let base_time = 20.0;

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(first_hover), None, HoverPopupModel::none()),
        );
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 0.05),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(second_hover), None, HoverPopupModel::none()),
        );
        assert!(
            hover_request_id(&app).is_none(),
            "moving to a new logical position must reset the debounce timer"
        );

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 0.05 + HOVER_REST_DELAY_SECS + 0.05),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(second_hover), None, HoverPopupModel::none()),
        );
        assert!(hover_request_id(&app).is_some());
        assert_eq!(
            app.lsp_hover
                .session
                .as_ref()
                .map(|session| session.position.utf16_col),
            Some(6)
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn hover_not_requested_when_lsp_not_running() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::types::LspRequest;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_no_lsp.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        let mut client = LspClient::new_test_client();
        client.set_running_for_test(false);
        app.lsp = Some(client);
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        let pointer_hover = HoveredSourcePosition {
            position: CursorPosition { line: 0, col: 4 },
            token_rect: egui::Rect::from_min_size(pos2(80.0, 40.0), egui::vec2(24.0, 18.0)),
        };
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(50.0 + HOVER_REST_DELAY_SECS + 0.1),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );

        assert_hover_state_cleared(&app);
        assert!(app
            .lsp
            .as_mut()
            .unwrap()
            .drain_pending_requests()
            .iter()
            .all(|request| !matches!(request, LspRequest::Hover { .. })));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn hover_not_requested_for_ineligible_file() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::types::LspRequest;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("notes.txt", "not rust\n");
        let mut app = BlueIdeApp::empty();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        let pointer_hover = HoveredSourcePosition {
            position: CursorPosition { line: 0, col: 3 },
            token_rect: egui::Rect::from_min_size(pos2(80.0, 40.0), egui::vec2(24.0, 18.0)),
        };
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(60.0 + HOVER_REST_DELAY_SECS + 0.1),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );

        assert_hover_state_cleared(&app);
        assert!(app
            .lsp
            .as_mut()
            .unwrap()
            .drain_pending_requests()
            .iter()
            .all(|request| !matches!(request, LspRequest::Hover { .. })));

        let _ = fs::remove_file(path);
    }

    fn hover_request_id(app: &BlueIdeApp) -> Option<u64> {
        app.lsp_hover
            .session
            .as_ref()
            .map(|session| session.request_id)
    }

    fn set_hover_request_session(
        app: &mut BlueIdeApp,
        path: &Path,
        request_id: u64,
        position: LspPosition,
    ) {
        let (revision, lsp_version) = app
            .buffers
            .get(path)
            .map(|buffer| (buffer.revision(), buffer.lsp_version))
            .unwrap_or((0, 0));
        app.lsp_hover.session = Some(HoverRequestSession {
            request_id,
            path: path.to_path_buf(),
            position,
            revision,
            lsp_version,
            position_entered_at: app.lsp_hover.rest_started.unwrap_or(0.0),
            popup_anchor: app.lsp_hover.popup_anchor.unwrap_or(egui::Rect::NOTHING),
        });
    }

    fn assert_hover_state_cleared(app: &BlueIdeApp) {
        assert!(app.lsp_hover.content.is_none());
        assert!(hover_request_id(app).is_none());
        assert!(app.lsp_hover.resting_target.is_none());
        assert!(app.lsp_hover.rest_started.is_none());
        assert!(app.lsp_hover.session.is_none());
        assert!(app.lsp_hover.displayed_target.is_none());
        assert!(app.lsp_hover.content_snapshot.is_none());
        assert!(app.lsp_hover.no_content_target.is_none());
        assert!(app.lsp_hover.request_sent_for.is_none());
        assert!(app.lsp_hover.popup_anchor.is_none());
        assert!(app.lsp_hover.popup_rect.is_none());
    }

    #[test]
    fn hover_rejects_stale_response_when_buffer_revision_changed() {
        let path = test_file("hover_stale_buffer.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 3),
        };
        app.lsp_hover.resting_target = Some(target.clone());
        app.lsp_hover.rest_started = Some(6.0);
        app.lsp_hover.request_sent_for = Some(target);
        set_hover_request_session(&mut app, &path, 12, LspPosition::new(0, 3));

        let revision_at_request = app.buffers.get(&path).unwrap().revision();
        assert_eq!(
            app.lsp_hover
                .session
                .as_ref()
                .map(|session| session.revision),
            Some(revision_at_request)
        );

        app.buffers
            .get_mut(&path)
            .unwrap()
            .insert_at_cursor("x")
            .unwrap();
        assert_ne!(
            app.buffers.get(&path).unwrap().revision(),
            revision_at_request
        );
        app.receive_hover(12, "stale docs after edit".to_owned());
        assert!(app.lsp_hover.content.is_none());
        assert!(app.lsp_hover.displayed_target.is_none());
        assert!(app.lsp_hover.content_snapshot.is_none());
        assert!(hover_request_id(&app).is_none());
        assert!(app.lsp_hover.request_sent_for.is_none());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn empty_hover_response_is_rejected_when_buffer_revision_changed() {
        let path = test_file("hover_empty_stale_buffer.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 0),
        };
        app.lsp_hover.resting_target = Some(target);
        app.lsp_hover.rest_started = Some(7.0);
        set_hover_request_session(&mut app, &path, 15, LspPosition::new(0, 0));

        app.buffers
            .get_mut(&path)
            .unwrap()
            .insert_at_cursor("x")
            .unwrap();
        app.receive_hover(15, String::new());
        assert!(app.lsp_hover.no_content_target.is_none());
        assert!(app.lsp_hover.request_sent_for.is_none());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn hover_request_captures_buffer_revision_at_send_time() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_revision_capture.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();

        let pointer_hover = HoveredSourcePosition {
            position: CursorPosition { line: 0, col: 4 },
            token_rect: egui::Rect::from_min_size(pos2(80.0, 40.0), egui::vec2(24.0, 18.0)),
        };
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let base_time = 12.0;
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 0.02),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );

        let revision_at_request = app.buffers.get(&path).unwrap().revision();
        assert_eq!(
            app.lsp_hover
                .session
                .as_ref()
                .map(|session| session.revision),
            Some(revision_at_request)
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn hover_tracks_time_pointer_entered_position() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_entry_time.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();

        let first_hover = HoveredSourcePosition {
            position: CursorPosition { line: 0, col: 2 },
            token_rect: egui::Rect::from_min_size(pos2(70.0, 40.0), egui::vec2(24.0, 18.0)),
        };
        let second_hover = HoveredSourcePosition {
            position: CursorPosition { line: 0, col: 6 },
            token_rect: egui::Rect::from_min_size(pos2(110.0, 40.0), egui::vec2(24.0, 18.0)),
        };
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let entry_time = 16.0;

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(entry_time),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(first_hover), None, HoverPopupModel::none()),
        );
        assert_eq!(app.lsp_hover.rest_started, Some(entry_time));

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(entry_time + 0.1),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(second_hover), None, HoverPopupModel::none()),
        );
        let moved_time = entry_time + 0.1;
        assert_eq!(app.lsp_hover.rest_started, Some(moved_time));

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(moved_time + HOVER_REST_DELAY_SECS + 0.02),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(second_hover), None, HoverPopupModel::none()),
        );
        assert_eq!(
            app.lsp_hover
                .session
                .as_ref()
                .map(|session| session.position_entered_at),
            Some(moved_time)
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn hover_rejects_response_when_position_entry_time_changed() {
        let path = test_file("hover_entry_stale.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        set_hover_request_session(&mut app, &path, 14, LspPosition::new(0, 0));
        app.lsp_hover.rest_started = Some(20.0);
        app.lsp_hover.session.as_mut().unwrap().position_entered_at = 19.0;
        app.lsp_hover.resting_target = Some(super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 0),
        });

        app.receive_hover(14, "entered-at mismatch".to_owned());
        assert!(app.lsp_hover.content.is_none());
        assert!(hover_request_id(&app).is_none());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn hover_rejects_stale_response_when_newer_request_exists() {
        let path = test_file("hover_superseded_id.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        app.lsp_hover.resting_target = Some(super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 0),
        });
        app.lsp_hover.rest_started = Some(0.0);
        set_hover_request_session(&mut app, &path, 8, LspPosition::new(0, 0));

        app.receive_hover(7, "older request".to_owned());
        assert_eq!(
            hover_request_id(&app),
            Some(8),
            "an unmatched response must not clear the in-flight session"
        );
        assert!(app.lsp_hover.content.is_none());

        app.receive_hover(8, "current request".to_owned());
        assert!(hover_request_id(&app).is_none());
        assert_eq!(app.lsp_hover.content.as_deref(), Some("current request"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn newer_hover_request_supersedes_in_flight_request() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_newer_request.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();
        let _ = app.lsp.as_mut().unwrap().drain_pending_requests();

        let first_hover = HoveredSourcePosition {
            position: CursorPosition { line: 0, col: 2 },
            token_rect: egui::Rect::from_min_size(pos2(70.0, 40.0), egui::vec2(24.0, 18.0)),
        };
        let second_hover = HoveredSourcePosition {
            position: CursorPosition { line: 0, col: 6 },
            token_rect: egui::Rect::from_min_size(pos2(110.0, 40.0), egui::vec2(24.0, 18.0)),
        };
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let base_time = 32.0;

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(first_hover), None, HoverPopupModel::none()),
        );
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 0.05),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(first_hover), None, HoverPopupModel::none()),
        );
        let older_request_id =
            hover_request_id(&app).expect("first hover request should be in flight");
        let _ = app.lsp.as_mut().unwrap().drain_pending_requests();

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 0.1),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(second_hover), None, HoverPopupModel::none()),
        );
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 0.1 + HOVER_REST_DELAY_SECS + 0.05),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(second_hover), None, HoverPopupModel::none()),
        );
        let newer_request_id =
            hover_request_id(&app).expect("second hover request should be in flight");
        assert_ne!(older_request_id, newer_request_id);

        app.receive_hover(older_request_id, "stale first position".to_owned());
        assert!(app.lsp_hover.content.is_none());
        assert_eq!(hover_request_id(&app), Some(newer_request_id));

        app.receive_hover(newer_request_id, "current second position".to_owned());
        assert_eq!(
            app.lsp_hover.content.as_deref(),
            Some("current second position")
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn hover_rejects_stale_response_when_active_tab_changed() {
        let first = test_file("hover_tab_first.rs", "fn main() {}\n");
        let second = test_file("hover_tab_second.rs", "fn other() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(first.clone()).unwrap();
        app.open_file(second.clone()).unwrap();

        app.lsp_hover.resting_target = Some(super::HoverTarget {
            path: first.clone(),
            position: LspPosition::new(0, 0),
        });
        app.lsp_hover.rest_started = Some(8.0);
        app.lsp_hover.request_sent_for = Some(super::HoverTarget {
            path: first.clone(),
            position: LspPosition::new(0, 0),
        });
        set_hover_request_session(&mut app, &first, 9, LspPosition::new(0, 0));
        app.active = Some(second.clone());

        app.receive_hover(9, "docs for the wrong tab".to_owned());
        assert!(app.lsp_hover.content.is_none());
        assert!(app.lsp_hover.displayed_target.is_none());
        assert!(hover_request_id(&app).is_none());
        assert!(app.lsp_hover.request_sent_for.is_none());

        let _ = fs::remove_file(first);
        let _ = fs::remove_file(second);
    }

    #[test]
    fn hover_rejects_stale_response_when_popup_was_dismissed() {
        let path = test_file("hover_dismiss_stale.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 0),
        };
        app.lsp_hover.resting_target = Some(target.clone());
        app.lsp_hover.rest_started = Some(9.0);
        app.lsp_hover.request_sent_for = Some(target);
        app.lsp_hover.popup_anchor = Some(egui::Rect::from_min_size(
            egui::pos2(80.0, 40.0),
            egui::vec2(24.0, 18.0),
        ));
        set_hover_request_session(&mut app, &path, 18, LspPosition::new(0, 0));

        app.dismiss_lsp_hover();

        app.receive_hover(18, "docs after dismiss".to_owned());
        assert!(app.lsp_hover.content.is_none());
        assert!(app.lsp_hover.displayed_target.is_none());
        assert!(hover_request_id(&app).is_none());
        assert!(app.lsp_hover.request_sent_for.is_none());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn hover_is_cleared_when_popup_is_dismissed() {
        let path = test_file("hover_dismiss_clear.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        app.lsp_hover.resting_target = Some(super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 0),
        });
        app.lsp_hover.rest_started = Some(14.0);
        app.lsp_hover.popup_anchor = Some(egui::Rect::from_min_size(
            egui::pos2(80.0, 40.0),
            egui::vec2(24.0, 18.0),
        ));
        app.lsp_hover.content = Some("fn main — docs".to_owned());
        app.lsp_hover.displayed_target = Some(super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 0),
        });
        set_hover_request_session(&mut app, &path, 19, LspPosition::new(0, 0));

        app.dismiss_lsp_hover();
        assert_hover_state_cleared(&app);

        app.receive_hover(19, "stale after dismiss".to_owned());
        assert!(app.lsp_hover.content.is_none());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn hover_rejects_stale_response_when_pointer_moved() {
        let path = test_file("hover_position_stale.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        app.lsp_hover.resting_target = Some(super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 5),
        });
        app.lsp_hover.rest_started = Some(9.0);
        app.lsp_hover.request_sent_for = Some(super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 0),
        });
        set_hover_request_session(&mut app, &path, 13, LspPosition::new(0, 0));

        app.receive_hover(13, "stale hovered position".to_owned());
        assert!(app.lsp_hover.content.is_none());
        assert!(app.lsp_hover.displayed_target.is_none());
        assert!(hover_request_id(&app).is_none());
        assert!(app.lsp_hover.request_sent_for.is_none());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn raw_json_hover_response_is_rejected_like_empty_content() {
        let path = test_file("hover_raw_json.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        app.lsp_hover.resting_target = Some(super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        });
        app.lsp_hover.rest_started = Some(10.0);
        set_hover_request_session(&mut app, &path, 20, LspPosition::new(0, 4));
        let expected_target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        };

        app.receive_hover(20, "{\"kind\":\"markdown\",\"value\":\"docs\"}".to_owned());
        assert!(app.lsp_hover.content.is_none());
        assert_eq!(
            app.lsp_hover.no_content_target.as_ref(),
            Some(&expected_target)
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn empty_hover_response_is_rejected_when_pointer_moved() {
        let path = test_file("hover_empty_stale_position.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        app.lsp_hover.resting_target = Some(super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        });
        app.lsp_hover.rest_started = Some(10.0);
        set_hover_request_session(&mut app, &path, 16, LspPosition::new(0, 0));

        app.receive_hover(16, String::new());
        assert!(app.lsp_hover.no_content_target.is_none());
        assert!(app.lsp_hover.request_sent_for.is_none());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn hover_request_captures_hovered_source_position() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_position_capture.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();

        let pointer_hover = HoveredSourcePosition {
            position: CursorPosition { line: 0, col: 4 },
            token_rect: egui::Rect::from_min_size(pos2(80.0, 40.0), egui::vec2(24.0, 18.0)),
        };
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let base_time = 14.0;
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 0.02),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );

        let resting = app.lsp_hover.resting_target.as_ref().expect("hover target");
        let session = app.lsp_hover.session.as_ref().expect("hover session");
        assert_eq!(session.position, resting.position);
        assert_eq!(resting.position, LspPosition::new(0, 4));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn hover_request_captures_popup_anchor_position() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_anchor_capture.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();

        let anchor = Rect::from_min_size(pos2(132.0, 88.0), Vec2::new(28.0, 18.0));
        let pointer_hover =
            HoveredSourcePosition::for_test(CursorPosition { line: 0, col: 4 }, anchor);
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let base_time = 22.0;
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );
        assert_eq!(app.lsp_hover.popup_anchor, Some(anchor));

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 0.02),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );
        assert_eq!(
            app.lsp_hover
                .session
                .as_ref()
                .map(|session| session.popup_anchor),
            Some(anchor)
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn hover_loading_popup_uses_request_anchor_when_pointer_moves() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_anchor_loading.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();
        let _ = app.lsp.as_mut().unwrap().drain_pending_requests();

        let request_anchor = Rect::from_min_size(pos2(90.0, 50.0), Vec2::new(28.0, 18.0));
        let moved_anchor = Rect::from_min_size(pos2(140.0, 70.0), Vec2::new(28.0, 18.0));
        let pointer_hover =
            HoveredSourcePosition::for_test(CursorPosition { line: 0, col: 4 }, request_anchor);
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let base_time = 24.0;
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 0.02),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );
        let request_id = hover_request_id(&app).expect("hover request should be in flight");
        assert_eq!(
            app.lsp_hover
                .session
                .as_ref()
                .map(|session| session.popup_anchor),
            Some(request_anchor)
        );
        let _ = app.lsp.as_mut().unwrap().drain_pending_requests();

        let moved_hover =
            HoveredSourcePosition::for_test(CursorPosition { line: 0, col: 4 }, moved_anchor);
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 0.05),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(moved_hover), None, HoverPopupModel::none()),
        );
        assert_eq!(app.lsp_hover.popup_anchor, Some(moved_anchor));
        assert_eq!(
            app.lsp_hover
                .session
                .as_ref()
                .map(|session| session.popup_anchor),
            Some(request_anchor),
            "the in-flight request must keep the anchor captured at send time"
        );
        assert_eq!(hover_request_id(&app), Some(request_id));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn typing_dismisses_lsp_hover() {
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_type_dismiss.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();

        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        };
        app.lsp_hover.resting_target = Some(target.clone());
        app.lsp_hover.rest_started = Some(49.0);
        let request_id = app.next_ui_correlation_id();
        set_hover_request_session(&mut app, &path, request_id, LspPosition::new(0, 4));
        app.lsp_hover.request_sent_for = Some(target);
        app.lsp_hover.popup_anchor =
            Some(Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)));
        app.receive_hover(request_id, "fn main — documentation".to_owned());
        assert!(app.lsp_hover.content.is_some());

        let revision_before = app.buffers.get(&path).unwrap().revision();
        app.buffers
            .get_mut(&path)
            .unwrap()
            .insert_at_cursor("x")
            .unwrap();
        app.dismiss_lsp_hover_if_buffer_edited_since(&path, revision_before);

        assert_hover_state_cleared(&app);
        assert_eq!(app.buffers.get(&path).unwrap().text(), "xfn main() {}\n");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn cursor_movement_dismisses_lsp_hover() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_cursor_dismiss.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();

        let cursor = CursorPosition { line: 0, col: 0 };
        app.buffers.get_mut(&path).unwrap().set_cursor(cursor);

        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        };
        app.lsp_hover.resting_target = Some(target.clone());
        app.lsp_hover.rest_started = Some(49.0);
        let request_id = app.next_ui_correlation_id();
        set_hover_request_session(&mut app, &path, request_id, LspPosition::new(0, 4));
        app.lsp_hover.request_sent_for = Some(target);
        app.lsp_hover.popup_anchor =
            Some(Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)));
        app.receive_hover(request_id, "fn main — documentation".to_owned());
        assert!(app.lsp_hover.content.is_some());

        app.buffers
            .get_mut(&path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 4 });
        app.dismiss_lsp_hover_if_cursor_moved_since(&path, cursor);

        assert_hover_state_cleared(&app);
        assert_eq!(
            app.buffers.get(&path).unwrap().cursor(),
            CursorPosition { line: 0, col: 4 }
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn typing_resets_hover_debounce_without_leaving_source_text() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_buffer_invalidate.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();

        let pointer_hover = HoveredSourcePosition {
            position: CursorPosition { line: 0, col: 4 },
            token_rect: egui::Rect::from_min_size(pos2(80.0, 40.0), egui::vec2(24.0, 18.0)),
        };
        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        };
        app.lsp_hover.resting_target = Some(target.clone());
        app.lsp_hover.rest_started = Some(49.0);
        let request_id = app.next_ui_correlation_id();
        set_hover_request_session(&mut app, &path, request_id, LspPosition::new(0, 4));
        app.lsp_hover.request_sent_for = Some(target.clone());
        app.lsp_hover.popup_anchor =
            Some(Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)));
        app.receive_hover(request_id, "fn main — documentation".to_owned());
        assert!(app.lsp_hover.content.is_some());

        let revision_before = app.buffers.get(&path).unwrap().revision();
        app.buffers
            .get_mut(&path)
            .unwrap()
            .insert_at_cursor("x")
            .unwrap();
        app.dismiss_lsp_hover_if_buffer_edited_since(&path, revision_before);

        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let after_type_time = 49.0 + HOVER_REST_DELAY_SECS + 0.05;
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(after_type_time),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );

        assert!(app.lsp_hover.content.is_none());
        assert!(app.lsp_hover.displayed_target.is_none());
        assert_eq!(app.lsp_hover.resting_target.as_ref(), Some(&target));
        assert!(
            hover_request_id(&app).is_none(),
            "typing must close the popup and restart the hover debounce"
        );

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(after_type_time + HOVER_REST_DELAY_SECS + 0.05),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );
        assert!(
            hover_request_id(&app).is_some(),
            "hover should request again only after the pointer rests post-edit"
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn moving_outside_source_text_dismisses_lsp_hover() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_leave.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();

        let pointer_hover = HoveredSourcePosition {
            position: CursorPosition { line: 0, col: 4 },
            token_rect: egui::Rect::from_min_size(pos2(80.0, 40.0), egui::vec2(24.0, 18.0)),
        };
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let base_time = 40.0;

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 0.05),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );
        assert!(hover_request_id(&app).is_some());

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 0.1),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, None, None, HoverPopupModel::none()),
        );
        assert_hover_state_cleared(&app);

        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        };
        app.lsp_hover.resting_target = Some(target.clone());
        app.lsp_hover.rest_started = Some(base_time);
        let request_id = app.next_ui_correlation_id();
        set_hover_request_session(&mut app, &path, request_id, LspPosition::new(0, 4));
        app.lsp_hover.popup_anchor =
            Some(Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)));
        app.receive_hover(request_id, "fn main — documentation".to_owned());
        assert!(app.lsp_hover.content.is_some());

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + 5.0),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, None, None, HoverPopupModel::none()),
        );
        assert_hover_state_cleared(&app);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn pointer_over_hover_popup_keeps_documentation_visible() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::{show_hover_documentation, HOVER_REST_DELAY_SECS};
        use crate::lsp::LspClient;
        use egui::{pos2, Event, Rect, Vec2};

        let path = test_file("hover_popup_pointer.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();

        let pointer_hover = HoveredSourcePosition {
            position: CursorPosition { line: 0, col: 4 },
            token_rect: Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)),
        };
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let base_time = 40.0;

        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        };
        app.lsp_hover.resting_target = Some(target.clone());
        app.lsp_hover.rest_started = Some(base_time);
        app.lsp_hover.popup_anchor = Some(pointer_hover.token_rect);
        let request_id = app.next_ui_correlation_id();
        set_hover_request_session(&mut app, &path, request_id, LspPosition::new(0, 4));
        app.receive_hover(request_id, "fn main — documentation".to_owned());
        assert!(app.lsp_hover.content.is_some());

        let mut layout_output = crate::editor::hover::HoverPopupOutput::default();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 0.1),
                ..Default::default()
            },
            |ctx| {
                layout_output = show_hover_documentation(
                    ctx,
                    app.lsp_hover.content.as_deref().unwrap(),
                    pointer_hover.token_rect,
                    Some(screen),
                );
                app.store_lsp_hover_popup_rect(layout_output);
            },
        );
        assert!(layout_output.popup_rect.is_positive());
        app.lsp_hover.popup_rect = Some(layout_output.popup_rect);

        let over_popup = layout_output.popup_rect.center();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 0.2),
                events: vec![Event::PointerMoved(over_popup)],
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, None, Some(screen), HoverPopupModel::none()),
        );
        assert_eq!(
            app.lsp_hover.content.as_deref(),
            Some("fn main — documentation")
        );
        assert!(app.lsp_hover.popup_rect.is_some());

        let outside_popup = pos2(layout_output.popup_rect.left() - 20.0, 10.0);
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 0.3),
                events: vec![Event::PointerMoved(outside_popup)],
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, None, Some(screen), HoverPopupModel::none()),
        );
        assert_hover_state_cleared(&app);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn hover_debounce_produces_at_most_one_request_for_a_stationary_source_position() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::types::LspRequest;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_once.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();
        let _ = app.lsp.as_mut().unwrap().drain_pending_requests();

        let pointer_hover = HoveredSourcePosition {
            position: CursorPosition { line: 0, col: 4 },
            token_rect: egui::Rect::from_min_size(pos2(80.0, 40.0), egui::vec2(24.0, 18.0)),
        };
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let base_time = 30.0;
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );

        let request_id = (1..7).find_map(|frame| {
            let _ = context.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    time: Some(base_time + HOVER_REST_DELAY_SECS + frame as f64 * 0.016),
                    ..Default::default()
                },
                |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
            );
            hover_request_id(&app)
        });

        let request_id = request_id.expect("hover request should be issued once debounce elapses");
        let hover_requests = app
            .lsp
            .as_mut()
            .unwrap()
            .drain_pending_requests()
            .into_iter()
            .filter(|request| matches!(request, LspRequest::Hover { .. }))
            .count();
        assert_eq!(
            hover_requests, 1,
            "the same resting target must not enqueue a hover request every frame"
        );

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 1.0),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );
        assert_eq!(hover_request_id(&app), Some(request_id));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn hover_tracks_request_already_sent_for_target() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::types::LspRequest;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_sent_for.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();
        let _ = app.lsp.as_mut().unwrap().drain_pending_requests();

        let pointer_hover = HoveredSourcePosition {
            position: CursorPosition { line: 0, col: 4 },
            token_rect: egui::Rect::from_min_size(pos2(80.0, 40.0), egui::vec2(24.0, 18.0)),
        };
        let expected_target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        };
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let base_time = 50.0;
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );

        let request_id = (1..7)
            .find_map(|frame| {
                let _ = context.run(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        time: Some(base_time + HOVER_REST_DELAY_SECS + frame as f64 * 0.016),
                        ..Default::default()
                    },
                    |ctx| {
                        app.update_lsp_hover(
                            ctx,
                            Some(pointer_hover),
                            None,
                            HoverPopupModel::none(),
                        )
                    },
                );
                hover_request_id(&app)
            })
            .expect("hover request should be issued once debounce elapses");
        assert_eq!(
            app.lsp_hover.request_sent_for,
            Some(expected_target.clone())
        );
        let _ = app.lsp.as_mut().unwrap().drain_pending_requests();

        app.receive_hover(request_id, "fn main — documentation".to_owned());
        assert!(hover_request_id(&app).is_none());
        assert_eq!(app.lsp_hover.request_sent_for, Some(expected_target));
        assert!(app.lsp_hover.content.is_some());

        for frame in 1..=5 {
            let _ = context.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    time: Some(base_time + HOVER_REST_DELAY_SECS + 1.0 + frame as f64 * 0.016),
                    ..Default::default()
                },
                |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
            );
        }

        let hover_requests = app
            .lsp
            .as_mut()
            .unwrap()
            .drain_pending_requests()
            .into_iter()
            .filter(|request| matches!(request, LspRequest::Hover { .. }))
            .count();
        assert_eq!(
            hover_requests, 0,
            "accepted hover content must not trigger another request for the same target"
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn hover_clears_request_sent_for_when_response_is_rejected() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::types::LspRequest;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_sent_retry.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();

        let pointer_hover = HoveredSourcePosition {
            position: CursorPosition { line: 0, col: 4 },
            token_rect: egui::Rect::from_min_size(pos2(80.0, 40.0), egui::vec2(24.0, 18.0)),
        };
        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        };
        app.lsp_hover.rest_started = Some(60.0);
        let request_id = app.next_ui_correlation_id();
        set_hover_request_session(&mut app, &path, request_id, LspPosition::new(0, 4));
        app.lsp_hover.request_sent_for = Some(target);
        app.lsp_hover.resting_target = Some(super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 5),
        });

        app.receive_hover(request_id, "stale hovered position".to_owned());
        assert!(app.lsp_hover.request_sent_for.is_none());

        app.lsp_hover.resting_target = Some(super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        });
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(60.0 + HOVER_REST_DELAY_SECS + 0.05),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );
        assert!(
            hover_request_id(&app).is_some(),
            "rejected hover responses must allow a fresh request for the current target"
        );

        let hover_requests = app
            .lsp
            .as_mut()
            .unwrap()
            .drain_pending_requests()
            .into_iter()
            .filter(|request| matches!(request, LspRequest::Hover { .. }))
            .count();
        assert_eq!(hover_requests, 1);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn empty_hover_response_does_not_show_popup() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::types::LspRequest;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_empty.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();
        let _ = app.lsp.as_mut().unwrap().drain_pending_requests();

        let pointer_hover = HoveredSourcePosition {
            position: CursorPosition { line: 0, col: 4 },
            token_rect: egui::Rect::from_min_size(pos2(80.0, 40.0), egui::vec2(24.0, 18.0)),
        };
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let base_time = 40.0;
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );

        let request_id = (1..7)
            .find_map(|frame| {
                let _ = context.run(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        time: Some(base_time + HOVER_REST_DELAY_SECS + frame as f64 * 0.016),
                        ..Default::default()
                    },
                    |ctx| {
                        app.update_lsp_hover(
                            ctx,
                            Some(pointer_hover),
                            None,
                            HoverPopupModel::none(),
                        )
                    },
                );
                hover_request_id(&app)
            })
            .expect("hover request should be issued once debounce elapses");
        let _ = app.lsp.as_mut().unwrap().drain_pending_requests();

        app.receive_hover(request_id, String::new());
        assert!(hover_request_id(&app).is_none());
        assert!(app.lsp_hover.content.is_none());
        assert_eq!(
            app.lsp_hover.no_content_target,
            app.lsp_hover.resting_target
        );

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 1.0),
                ..Default::default()
            },
            |ctx| app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none()),
        );
        assert!(hover_request_id(&app).is_none());
        assert!(app.lsp_hover.content.is_none());

        let hover_requests = app
            .lsp
            .as_mut()
            .unwrap()
            .drain_pending_requests()
            .into_iter()
            .filter(|request| matches!(request, LspRequest::Hover { .. }))
            .count();
        assert_eq!(
            hover_requests, 0,
            "empty hover responses must not re-request or leave a loading popup active"
        );

        let _ = fs::remove_file(path);
    }

    /// Always boundary: Ctrl+Space must enqueue a real `textDocument/completion` request and
    /// open a navigable popup anchored at the caret (see **Boundaries → Always** A13).
    #[test]
    fn ctrl_space_sends_a_real_completion_request_and_opens_a_functional_caret_anchored_dropdown() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::completion::CompletionPopupOutput;
        use crate::lsp::types::{LspCompletionItem, LspRequest, LspResponse};
        use crate::lsp::LspClient;
        use egui::{Event, Key, Modifiers, Rect, Vec2};

        fn completion_item(label: &str) -> LspCompletionItem {
            LspCompletionItem {
                filter_text: None,
                label: label.to_owned(),
                ..Default::default()
            }
        }

        fn ctrl_space_event() -> Event {
            Event::Key {
                key: Key::Space,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::COMMAND,
            }
        }

        let path = test_file("ctrl_space_dropdown.rs", "fn ma() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.search_last_active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();

        let cursor = CursorPosition { line: 0, col: 5 };
        app.buffers.get_mut(&path).unwrap().set_cursor(cursor);
        let text_before = app.buffers.get(&path).unwrap().text().to_owned();
        let revision_before = app.buffers.get(&path).unwrap().revision();

        let context = egui::Context::default();
        let editor_id = egui::Id::new(("blue_ide_editor", Some(path.clone())));
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));

        let _ = context.run(
            egui::RawInput {
                focused: true,
                events: vec![ctrl_space_event()],
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                ctx.memory_mut(|mem| mem.request_focus(editor_id));
                let _ = app.show_editor(ctx);
            },
        );

        assert_eq!(app.buffers.get(&path).unwrap().text(), text_before);
        assert_eq!(app.buffers.get(&path).unwrap().revision(), revision_before);
        assert!(
            app.completion.is_open(),
            "Ctrl+Space must begin a completion session"
        );
        assert!(
            app.completion.popup().loading,
            "completion popup should show loading until LSP results arrive"
        );

        let request_id = app
            .completion
            .request_id()
            .expect("completion session must record the outbound request id");
        assert!(
            app.lsp_pending
                .get(&request_id)
                .is_some_and(|kind| matches!(kind, super::LspPendingKind::Completion)),
            "Ctrl+Space must register a correlated completion request"
        );

        let requests = app.lsp.as_mut().unwrap().drain_pending_requests();
        let completion_request = requests.iter().find_map(|request| {
            if let LspRequest::Completion {
                path: requested_path,
                line,
                col,
                id,
            } = request
            {
                Some((requested_path, *line, *col, *id))
            } else {
                None
            }
        });
        assert_eq!(
            completion_request,
            Some((&path, 0, 5, request_id)),
            "Ctrl+Space must enqueue a real LspRequest::Completion at the caret UTF-16 column"
        );

        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::CompletionList {
                id: request_id,
                items: vec![completion_item("main"), completion_item("match")],
            });
        app.poll_lsp();

        assert!(app.completion.is_open());
        assert!(!app.completion.popup().loading);
        assert_eq!(app.completion.popup().items.len(), 2);
        assert_eq!(app.completion.popup().selected, 0);

        let _ = context.run(
            egui::RawInput {
                focused: true,
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                ctx.memory_mut(|mem| mem.request_focus(editor_id));
                let _ = app.show_editor(ctx);
            },
        );

        let anchor = app
            .completion_anchor
            .screen_rect
            .expect("editor must hand off a caret screen anchor for completion positioning");
        assert!(
            anchor.height() > 0.0,
            "caret anchor must carry vertical extent for popup positioning"
        );

        let cursor_before_nav = app.buffers.get(&path).unwrap().cursor();
        let _ = context.run(
            egui::RawInput {
                focused: true,
                events: vec![Event::Key {
                    key: Key::ArrowDown,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }],
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                ctx.memory_mut(|mem| mem.request_focus(editor_id));
                let _ = app.show_editor(ctx);
            },
        );
        assert_eq!(
            app.completion.popup().selected,
            1,
            "dropdown must remain keyboard-navigable after LSP results arrive"
        );
        assert_eq!(
            app.buffers.get(&path).unwrap().cursor(),
            cursor_before_nav,
            "completion navigation must not move the editor caret"
        );

        let mut layout_output = CompletionPopupOutput::default();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                layout_output = app
                    .completion
                    .show(ctx, anchor, app.active_palette.semantic);
            },
        );
        assert!(
            layout_output.row_hit_rects.len() >= 2,
            "caret-anchored dropdown must lay out clickable completion rows"
        );
        assert_ne!(
            layout_output.popup_rect,
            Rect::NOTHING,
            "caret-anchored dropdown must produce a screen-space popup rectangle"
        );

        let _ = fs::remove_file(path);
    }

    /// Always boundary: the completion dropdown must support keyboard navigation, keyboard
    /// acceptance, row clicks, and dismiss paths (see **Boundaries → Always** A14).
    #[test]
    fn completion_can_be_navigated_accepted_clicked_and_dismissed() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::completion::{CompletionPopupAnchor, CompletionPopupOutput};
        use crate::lsp::types::LspCompletionItem;
        use egui::{pos2, Event, Key, Modifiers, PointerButton, Rect, Vec2};

        fn completion_item(label: &str) -> LspCompletionItem {
            LspCompletionItem {
                filter_text: None,
                label: label.to_owned(),
                ..Default::default()
            }
        }

        fn key_event(key: Key) -> Event {
            Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }
        }

        fn reset_partial_identifier(app: &mut BlueIdeApp, path: &std::path::Path) {
            let len_chars = app.buffers.get(path).unwrap().len_chars();
            app.buffers
                .get_mut(path)
                .unwrap()
                .replace_char_range(0, len_chars, "fn ma() {}\n")
                .unwrap();
            app.buffers
                .get_mut(path)
                .unwrap()
                .set_cursor(CursorPosition { line: 0, col: 5 });
        }

        fn arm_completion_popup(
            app: &mut BlueIdeApp,
            path: &std::path::Path,
            request_id: u64,
        ) -> (u64, CursorPosition) {
            let revision = app.buffers.get(path).unwrap().revision();
            let lsp_version = app.buffers.get(path).unwrap().lsp_version;
            let cursor = app.buffers.get(path).unwrap().cursor();
            begin_completion_session(
                app,
                path.to_path_buf(),
                request_id,
                revision,
                lsp_version,
                cursor,
            );
            app.receive_completion(
                request_id,
                vec![completion_item("main"), completion_item("match")],
            );
            assert!(app.completion.is_open());
            (revision, cursor)
        }

        fn run_show_editor(
            app: &mut BlueIdeApp,
            context: &egui::Context,
            path: &std::path::Path,
            events: Vec<Event>,
        ) {
            let editor_id = egui::Id::new(("blue_ide_editor", Some(path.to_path_buf())));
            let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
            let _ = context.run(
                egui::RawInput {
                    focused: true,
                    events,
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ctx| {
                    ctx.memory_mut(|mem| mem.request_focus(editor_id));
                    let _ = app.show_editor(ctx);
                },
            );
        }

        let path = test_file("completion_interactions.rs", "fn ma() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.search_last_active = Some(path.clone());
        app.buffers
            .get_mut(&path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 5 });

        let context = egui::Context::default();
        let anchor = Rect::from_min_max(pos2(100.0, 200.0), pos2(108.0, 220.0));
        app.completion_anchor = CompletionPopupAnchor::from_screen_rect(Some(anchor));
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));

        let (revision_before, cursor_before) = arm_completion_popup(&mut app, &path, 1);
        let text_before = app.buffers.get(&path).unwrap().text().to_owned();
        run_show_editor(&mut app, &context, &path, vec![key_event(Key::ArrowDown)]);
        assert_eq!(app.completion.popup().selected, 1);
        assert_eq!(app.buffers.get(&path).unwrap().cursor(), cursor_before);
        assert_eq!(app.buffers.get(&path).unwrap().text(), text_before);
        assert_eq!(app.buffers.get(&path).unwrap().revision(), revision_before);

        run_show_editor(&mut app, &context, &path, vec![key_event(Key::Enter)]);
        assert_eq!(app.buffers.get(&path).unwrap().text(), "fn match() {}\n");
        assert!(!app.completion.is_open());

        reset_partial_identifier(&mut app, &path);
        arm_completion_popup(&mut app, &path, 2);
        run_show_editor(&mut app, &context, &path, vec![key_event(Key::Tab)]);
        assert_eq!(app.buffers.get(&path).unwrap().text(), "fn main() {}\n");
        assert!(!app.completion.is_open());

        reset_partial_identifier(&mut app, &path);
        arm_completion_popup(&mut app, &path, 3);
        let palette = app.active_palette.semantic;
        let mut layout_output = CompletionPopupOutput::default();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                layout_output = app.completion.show(ctx, anchor, palette);
            },
        );
        let click_pos = layout_output.row_hit_rects[1].center();
        let mut click_output = CompletionPopupOutput::default();
        for input in [
            egui::RawInput {
                screen_rect: Some(screen),
                events: vec![egui::Event::PointerMoved(click_pos)],
                modifiers: Modifiers::NONE,
                ..Default::default()
            },
            egui::RawInput {
                screen_rect: Some(screen),
                events: vec![egui::Event::PointerButton {
                    pos: click_pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                }],
                modifiers: Modifiers::NONE,
                ..Default::default()
            },
            egui::RawInput {
                screen_rect: Some(screen),
                events: vec![egui::Event::PointerButton {
                    pos: click_pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Modifiers::NONE,
                }],
                modifiers: Modifiers::NONE,
                ..Default::default()
            },
        ] {
            let _ = context.run(input, |ctx| {
                click_output = app.completion.show(ctx, anchor, palette);
            });
        }
        if let Some(event) = click_output.event {
            app.handle_completion_popup_event(event);
        }
        assert_eq!(app.buffers.get(&path).unwrap().text(), "fn match() {}\n");
        assert!(!app.completion.is_open());

        reset_partial_identifier(&mut app, &path);
        let (revision_before, cursor_before) = arm_completion_popup(&mut app, &path, 4);
        let text_before = app.buffers.get(&path).unwrap().text().to_owned();
        run_show_editor(&mut app, &context, &path, vec![key_event(Key::Escape)]);
        assert!(!app.completion.is_open());
        assert_eq!(app.buffers.get(&path).unwrap().text(), text_before);
        assert_eq!(app.buffers.get(&path).unwrap().revision(), revision_before);
        assert_eq!(app.buffers.get(&path).unwrap().cursor(), cursor_before);

        reset_partial_identifier(&mut app, &path);
        let (revision_before, cursor_before) = arm_completion_popup(&mut app, &path, 5);
        let text_before = app.buffers.get(&path).unwrap().text().to_owned();
        let mut layout_output = CompletionPopupOutput::default();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                layout_output = app
                    .completion
                    .show(ctx, anchor, app.active_palette.semantic);
            },
        );
        let outside_pos = pos2(layout_output.popup_rect.left() - 20.0, 10.0);
        assert!(!layout_output.popup_rect.contains(outside_pos));
        run_show_editor(
            &mut app,
            &context,
            &path,
            vec![
                egui::Event::PointerMoved(outside_pos),
                egui::Event::PointerButton {
                    pos: outside_pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ],
        );
        assert!(!app.completion.is_open());
        assert_eq!(app.buffers.get(&path).unwrap().text(), text_before);
        assert_eq!(app.buffers.get(&path).unwrap().revision(), revision_before);
        assert_eq!(app.buffers.get(&path).unwrap().cursor(), cursor_before);

        let _ = fs::remove_file(path);
    }

    /// Always boundary: accepted completions must replace only the identifier prefix
    /// snapshotted in the completion session (see **Boundaries → Always** A15).
    #[test]
    fn accepted_completion_edits_the_correct_identifier_prefix() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::LspCompletionItem;
        use egui::{Event, Key, Modifiers, Rect, Vec2};
        use std::ops::Range;

        fn completion_item(label: &str) -> LspCompletionItem {
            LspCompletionItem {
                label: label.to_owned(),
                ..Default::default()
            }
        }

        let app_rs = include_str!("app.rs").replace("\r\n", "\n");
        let app_production = app_rs
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(&app_rs);
        assert!(
            app_production.contains("identifier_prefix_char_range_at(cursor)"),
            "completion request must snapshot the identifier prefix at the caret"
        );
        assert!(
            app_production.contains("self.completion.prefix_char_range()"),
            "accepted completions must apply the frozen session prefix range"
        );
        assert!(
            include_str!("editor/completion.rs").contains("prefix_char_range: Range<usize>"),
            "CompletionSession must store the prefix char range at request time"
        );

        fn accept_via_enter(app: &mut BlueIdeApp, context: &egui::Context, path: &std::path::Path) {
            let editor_id = egui::Id::new(("blue_ide_editor", Some(path.to_path_buf())));
            let _ = context.run(
                egui::RawInput {
                    focused: true,
                    events: vec![Event::Key {
                        key: Key::Enter,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Modifiers::NONE,
                    }],
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(800.0, 600.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    ctx.memory_mut(|mem| mem.request_focus(editor_id));
                    let _ = app.show_editor(ctx);
                },
            );
        }

        fn assert_accept_replaces_prefix(
            app: &mut BlueIdeApp,
            context: &egui::Context,
            path: &std::path::Path,
            initial_text: &str,
            cursor: CursorPosition,
            request_id: u64,
            label: &str,
            expected_prefix: Range<usize>,
            expected_text: &str,
            expected_cursor: CursorPosition,
            accept_via_show_editor: bool,
        ) {
            let len_chars = app.buffers.get(path).unwrap().len_chars();
            app.buffers
                .get_mut(path)
                .unwrap()
                .replace_char_range(0, len_chars, initial_text)
                .unwrap();
            app.buffers.get_mut(path).unwrap().set_cursor(cursor);

            let prefix = app
                .buffers
                .get(path)
                .unwrap()
                .identifier_prefix_char_range_at(cursor)
                .expect("caret must resolve an identifier prefix range");
            assert_eq!(
                prefix, expected_prefix,
                "identifier prefix range must match the partial token at the caret"
            );

            let revision = app.buffers.get(path).unwrap().revision();
            let lsp_version = app.buffers.get(path).unwrap().lsp_version;
            begin_completion_session(
                app,
                path.to_path_buf(),
                request_id,
                revision,
                lsp_version,
                cursor,
            );
            assert_eq!(
                app.completion.prefix_char_range(),
                Some(expected_prefix.clone()),
                "completion session must freeze the prefix range from request time"
            );
            app.receive_completion(request_id, vec![completion_item(label)]);
            assert!(app.completion.is_open());

            if accept_via_show_editor {
                accept_via_enter(app, context, path);
            } else {
                app.apply_completion_item(0);
            }

            assert_eq!(app.buffers.get(path).unwrap().text(), expected_text);
            assert_eq!(app.buffers.get(path).unwrap().cursor(), expected_cursor);
            assert!(!app.completion.is_open());
        }

        let path = test_file("prefix_accept.rs", "fn pri() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.search_last_active = Some(path.clone());
        let context = egui::Context::default();

        assert_accept_replaces_prefix(
            &mut app,
            &context,
            &path,
            "fn pri() {}\n",
            CursorPosition { line: 0, col: 6 },
            1,
            "println",
            3..6,
            "fn println() {}\n",
            CursorPosition { line: 0, col: 10 },
            true,
        );

        assert_accept_replaces_prefix(
            &mut app,
            &context,
            &path,
            "self.ba\n",
            CursorPosition { line: 0, col: 7 },
            2,
            "bar",
            5..7,
            "self.bar\n",
            CursorPosition { line: 0, col: 8 },
            true,
        );

        assert_accept_replaces_prefix(
            &mut app,
            &context,
            &path,
            "foo::ba\n",
            CursorPosition { line: 0, col: 7 },
            3,
            "bar",
            5..7,
            "foo::bar\n",
            CursorPosition { line: 0, col: 8 },
            true,
        );

        assert_accept_replaces_prefix(
            &mut app,
            &context,
            &path,
            "let 🙂pri\n",
            CursorPosition { line: 0, col: 8 },
            4,
            "println",
            5..8,
            "let 🙂println\n",
            CursorPosition { line: 0, col: 12 },
            false,
        );

        let _ = fs::remove_file(path);
    }

    /// Always boundary: pointer hover must debounce, enqueue a real `textDocument/hover`
    /// request, and display typed documentation from `poll_lsp()` (see **Boundaries →
    /// Always** A16).
    #[test]
    fn pointer_hover_sends_a_debounced_real_lsp_hover_request_and_displays_documentation() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::types::{LspRequest, LspResponse};
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let app_rs = include_str!("app.rs").replace("\r\n", "\n");
        let app_production = app_rs
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(&app_rs);
        assert!(
            app_production.contains("fn update_lsp_hover"),
            "pointer hover lifecycle must flow through update_lsp_hover"
        );
        assert!(
            app_production.contains("rested_for >= HOVER_REST_DELAY_SECS"),
            "hover requests must wait for the rest debounce before sending"
        );
        assert!(
            app_production.contains("lsp.request_hover"),
            "hover must enqueue a real LspClient::request_hover call"
        );
        assert!(
            include_str!("editor/hover.rs").contains("pub const HOVER_REST_DELAY_SECS"),
            "hover debounce delay must remain a named constant"
        );

        let path = test_file("pointer_hover_e2e.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.search_last_active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();
        let _ = app.lsp.as_mut().unwrap().drain_pending_requests();

        let pointer_hover = HoveredSourcePosition::for_test(
            CursorPosition { line: 0, col: 4 },
            Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)),
        );
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let base_time = 40.0;

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time),
                ..Default::default()
            },
            |ctx| {
                app.update_lsp_hover(
                    ctx,
                    Some(pointer_hover),
                    Some(screen),
                    HoverPopupModel::none(),
                )
            },
        );
        assert!(app.lsp_hover.resting_target.is_some());
        assert!(hover_request_id(&app).is_none());
        assert!(app
            .lsp
            .as_mut()
            .unwrap()
            .drain_pending_requests()
            .iter()
            .all(|request| !matches!(request, LspRequest::Hover { .. })));

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS - 0.02),
                ..Default::default()
            },
            |ctx| {
                app.update_lsp_hover(
                    ctx,
                    Some(pointer_hover),
                    Some(screen),
                    HoverPopupModel::none(),
                )
            },
        );
        assert!(hover_request_id(&app).is_none());

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 0.02),
                ..Default::default()
            },
            |ctx| {
                app.update_lsp_hover(
                    ctx,
                    Some(pointer_hover),
                    Some(screen),
                    HoverPopupModel::none(),
                )
            },
        );
        let request_id = hover_request_id(&app).expect("debounced hover must send a request");
        assert_eq!(
            app.lsp_pending.get(&request_id),
            Some(&super::LspPendingKind::Hover)
        );
        let hover_request = app
            .lsp
            .as_mut()
            .unwrap()
            .drain_pending_requests()
            .into_iter()
            .find_map(|request| {
                if let LspRequest::Hover {
                    path: requested_path,
                    line,
                    col,
                    id,
                } = request
                {
                    Some((requested_path, line, col, id))
                } else {
                    None
                }
            });
        assert_eq!(
            hover_request,
            Some((path.clone(), 0, 4, request_id)),
            "hover must enqueue a real LspRequest::Hover at the UTF-16 caret column"
        );
        assert!(
            app.lsp_hover.session.is_some(),
            "in-flight hover should show loading until documentation arrives"
        );
        assert!(app.lsp_hover.content.is_none());

        let docs = "fn main — documentation from rust-analyzer";
        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::HoverResult {
                id: request_id,
                content: docs.to_owned(),
            });
        app.poll_lsp();

        assert_eq!(app.lsp_hover.content.as_deref(), Some(docs));
        assert_eq!(
            app.lsp_hover.displayed_target.as_ref(),
            app.lsp_hover.resting_target.as_ref()
        );
        assert!(app.lsp_hover.session.is_none());

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(base_time + HOVER_REST_DELAY_SECS + 0.5),
                ..Default::default()
            },
            |ctx| {
                app.update_lsp_hover(
                    ctx,
                    Some(pointer_hover),
                    Some(screen),
                    HoverPopupModel::none(),
                )
            },
        );
        assert!(
            app.lsp_hover
                .popup_rect
                .is_some_and(|rect| rect.is_positive()),
            "typed hover documentation must render in a screen-space popup"
        );

        let _ = fs::remove_file(path);
    }

    /// Always boundary: diagnostic tooltips and LSP hover must coexist per the fixed
    /// precedence order (see **Boundaries → Always** A6, A17).
    #[test]
    fn diagnostic_tooltips_and_lsp_hover_coexist_according_to_the_specified_precedence() {
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::types::{DiagnosticSeverity, LspDiagnostic, LspRequest, LspResponse};
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        assert_diagnostic_tooltips_are_not_replaced_by_lsp_hover_contract();
        assert!(
            include_str!("editor/widget.rs").contains("pub fn resolve_pointer_hover_precedence"),
            "widget must resolve diagnostic vs source-text hover precedence"
        );
        assert!(
            include_str!("editor/hover.rs").contains("pub fn lsp_hover_allowed"),
            "LSP hover rendering must be gated while a diagnostic tooltip is active"
        );
        let app_rs = include_str!("app.rs");
        let app_production = app_rs
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(&app_rs);
        assert!(
            app_production.contains("if self.diagnostic_tooltip_active"),
            "receive_hover must ignore LSP docs while diagnostic tooltip is active"
        );
        assert!(
            app_production.contains("lsp_hover_allowed(diagnostic_tooltip_active)"),
            "render_lsp_hover_popup must respect diagnostic tooltip gate"
        );

        let path = test_file("coexist_precedence.rs", "fn main() {}\nlet ok = 1;\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.search_last_active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();
        app.lsp.as_mut().unwrap().insert_diagnostics_for_test(
            path.clone(),
            vec![LspDiagnostic {
                line_start: 0,
                col_start: 3,
                line_end: 0,
                col_end: 7,
                severity: DiagnosticSeverity::Error,
                message: "expected `;`".to_owned(),
                code: Some("E0425".to_owned()),
            }],
        );
        let _ = app.lsp.as_mut().unwrap().drain_pending_requests();

        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let row_height = 20.0;
        let diagnostic_line_y = 10.0 + row_height * 0.5;
        let clean_line_y = 10.0 + row_height + row_height * 0.5;
        let diagnostic_pointer = pos2(92.0, diagnostic_line_y);
        let source_pointer = pos2(80.0, clean_line_y);
        let tooltip_id = egui::Id::new(("diagnostic_tooltip", Some(&path), 0usize));
        let hover_input = |time: f64, pointer: egui::Pos2| egui::RawInput {
            screen_rect: Some(screen),
            events: vec![egui::Event::PointerMoved(pointer)],
            time: Some(time),
            ..Default::default()
        };

        let diagnostic_time = 20.0;
        for _ in 0..2 {
            let _ = context.run(
                hover_input(
                    diagnostic_time + HOVER_REST_DELAY_SECS + 0.1,
                    diagnostic_pointer,
                ),
                |ctx| {
                    let _ = app.show_editor(ctx);
                },
            );
        }

        assert!(
            app.diagnostic_tooltip_active,
            "diagnostic squiggle hover must activate the editor-owned tooltip"
        );
        assert_hover_state_cleared(&app);
        assert!(app
            .lsp
            .as_mut()
            .unwrap()
            .drain_pending_requests()
            .iter()
            .all(|request| !matches!(request, LspRequest::Hover { .. })));
        assert!(
            egui::containers::popup::was_tooltip_open_last_frame(&context, tooltip_id),
            "diagnostic tooltip must remain visible over the squiggle"
        );
        assert!(
            app.lsp_hover.popup_rect.is_none(),
            "LSP documentation must not render while diagnostic tooltip takes precedence"
        );

        set_hover_request_session(&mut app, &path, 77, LspPosition::new(0, 4));
        app.diagnostic_tooltip_active = true;
        app.receive_hover(77, "late lsp docs".to_owned());
        assert!(
            app.lsp_hover.content.is_none(),
            "receive_hover must ignore LSP docs while diagnostic tooltip is active"
        );

        let source_time = 60.0;
        let _ = context.run(hover_input(source_time, source_pointer), |ctx| {
            let _ = app.show_editor(ctx);
        });
        let _ = context.run(
            hover_input(source_time + HOVER_REST_DELAY_SECS + 0.1, source_pointer),
            |ctx| {
                let _ = app.show_editor(ctx);
            },
        );

        assert!(
            !app.diagnostic_tooltip_active,
            "moving to clean source text must not keep the diagnostic tooltip active"
        );
        assert!(
            app.lsp_hover.resting_target.is_some(),
            "LSP hover must arm over source text when no diagnostic tooltip is active"
        );
        let request_id = hover_request_id(&app).expect("LSP hover must send after debounce");
        let hover_request = app
            .lsp
            .as_mut()
            .unwrap()
            .drain_pending_requests()
            .into_iter()
            .find_map(|request| {
                if let LspRequest::Hover {
                    path: requested_path,
                    line,
                    col,
                    id,
                } = request
                {
                    Some((requested_path, line, col, id))
                } else {
                    None
                }
            });
        assert!(
            hover_request.is_some(),
            "clean source text must enqueue a real hover request"
        );
        let (_, line, col, id) = hover_request.unwrap();
        assert_eq!(id, request_id);
        assert_eq!(line, 1, "hover must target the clean source line");
        assert!(
            col > 0,
            "hover must send a UTF-16 column for the symbol under the pointer"
        );

        let docs = "fn — documentation from rust-analyzer";
        app.lsp
            .as_ref()
            .unwrap()
            .push_test_response(LspResponse::HoverResult {
                id: request_id,
                content: docs.to_owned(),
            });
        app.poll_lsp();
        assert_eq!(app.lsp_hover.content.as_deref(), Some(docs));

        let _ = context.run(
            hover_input(source_time + HOVER_REST_DELAY_SECS + 0.5, source_pointer),
            |ctx| {
                let _ = app.show_editor(ctx);
            },
        );
        assert!(
            app.lsp_hover
                .popup_rect
                .is_some_and(|rect| rect.is_positive()),
            "LSP documentation must render once diagnostic tooltip is no longer active"
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn unicode_earlier_on_the_same_line_does_not_offset_the_hover_request() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::types::LspRequest;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("hover_utf16.rs", "a🙂z\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();

        let hover_position = CursorPosition { line: 0, col: 2 };
        let pointer_hover = HoveredSourcePosition {
            position: hover_position,
            token_rect: egui::Rect::from_min_size(pos2(80.0, 40.0), egui::vec2(24.0, 18.0)),
        };
        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));

        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(1.0),
                ..Default::default()
            },
            |ctx| {
                app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none());
            },
        );
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(1.0 + HOVER_REST_DELAY_SECS + 0.05),
                ..Default::default()
            },
            |ctx| {
                app.update_lsp_hover(ctx, Some(pointer_hover), None, HoverPopupModel::none());
            },
        );

        let resting = app.lsp_hover.resting_target.as_ref().expect("hover target");
        assert_eq!(
            resting.position,
            LspPosition::new(0, 3),
            "char column 2 after emoji must encode as UTF-16 column 3"
        );
        assert!(hover_request_id(&app).is_some());

        let requests = app.lsp.as_mut().unwrap().drain_pending_requests();
        let hover_request = requests.iter().find_map(|request| {
            if let LspRequest::Hover {
                path: requested_path,
                line,
                col,
                ..
            } = request
            {
                Some((requested_path, *line, *col))
            } else {
                None
            }
        });
        assert_eq!(
            hover_request,
            Some((&path, 0, 3)),
            "hover request must send UTF-16 column, not the raw character index"
        );
        let session = app.lsp_hover.session.as_ref().expect("hover session");
        assert_eq!(session.position, resting.position);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn unicode_earlier_on_the_same_line_does_not_offset_the_completion_request() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::LspRequest;
        use crate::lsp::LspClient;

        let path = test_file("completion_utf16.rs", "a🙂pri\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();

        let cursor = CursorPosition { line: 0, col: 5 };
        app.buffers.get_mut(&path).unwrap().set_cursor(cursor);

        let lsp_position = app.buffers.get(&path).unwrap().cursor_lsp_position();
        assert_eq!(
            lsp_position,
            LspPosition::new(0, 6),
            "char column 5 after emoji must encode as UTF-16 column 6"
        );
        assert_ne!(
            lsp_position.utf16_col, cursor.col as u32,
            "raw character index must not be sent on the wire"
        );

        app.request_completion_at_cursor();

        assert!(app.completion.is_open());

        let requests = app.lsp.as_mut().unwrap().drain_pending_requests();
        let completion_request = requests.iter().find_map(|request| {
            if let LspRequest::Completion {
                path: requested_path,
                line,
                col,
                ..
            } = request
            {
                Some((requested_path, *line, *col))
            } else {
                None
            }
        });
        assert_eq!(
            completion_request,
            Some((&path, 0, 6)),
            "completion request must send UTF-16 column, not the raw character index"
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn lsp_hover_appears_only_when_no_diagnostic_tooltip_active() {
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("lsp_without_diagnostic.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.search_last_active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();

        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let row_height = 20.0;
        let line_y = 10.0 + row_height * 0.5;
        let pointer = pos2(80.0, line_y);
        let base_time = 12.0;
        let hover_input = |time: f64| egui::RawInput {
            screen_rect: Some(screen),
            events: vec![egui::Event::PointerMoved(pointer)],
            time: Some(time),
            ..Default::default()
        };
        let _ = context.run(hover_input(base_time), |ctx| {
            let _ = app.show_editor(ctx);
        });
        let _ = context.run(
            hover_input(base_time + HOVER_REST_DELAY_SECS + 0.1),
            |ctx| {
                let _ = app.show_editor(ctx);
            },
        );

        assert!(
            app.lsp_hover.resting_target.is_some(),
            "LSP hover should arm over source text when no diagnostic tooltip is active"
        );
        assert!(
            app.lsp_hover.request_sent_for.is_some() || hover_request_id(&app).is_some(),
            "LSP hover should request documentation only when no diagnostic tooltip is active"
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn active_diagnostic_tooltip_blocks_lsp_hover_popup() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("active_diagnostic_blocks_lsp.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.lsp_hover.resting_target = Some(super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 0),
        });
        app.lsp_hover.rest_started = Some(5.0);
        app.lsp_hover.displayed_target = app.lsp_hover.resting_target.clone();
        app.lsp_hover.popup_anchor =
            Some(Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)));
        app.lsp_hover.content = Some("fn main — documentation".to_owned());

        let pointer_hover = HoveredSourcePosition::for_test(
            CursorPosition { line: 0, col: 3 },
            Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)),
        );
        let context = egui::Context::default();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(800.0, 600.0),
                )),
                time: Some(6.0),
                ..Default::default()
            },
            |ctx| {
                app.update_lsp_hover(
                    ctx,
                    Some(pointer_hover),
                    None,
                    HoverPopupModel {
                        hovered_source: None,
                        diagnostic_tooltip_active: true,
                    },
                )
            },
        );

        assert_hover_state_cleared(&app);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn receive_hover_ignored_while_diagnostic_tooltip_at_same_pointer() {
        let path = test_file("recv_hover_diag.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        set_hover_request_session(&mut app, &path, 42, LspPosition::new(0, 4));
        app.diagnostic_tooltip_active = true;

        app.receive_hover(42, "fn main — documentation".to_owned());

        assert!(app.lsp_hover.content.is_none());
        assert!(app.lsp_hover.displayed_target.is_none());
        assert!(app.lsp_hover.session.is_none());
        assert!(app.lsp_hover.request_sent_for.is_none());

        let _ = fs::remove_file(path);
    }

    fn assert_diagnostic_tooltips_are_not_replaced_by_lsp_hover_contract() {
        let widget_rs = include_str!("editor/widget.rs");
        assert!(
            widget_rs.contains("diagnostic squiggle tooltips via `show_tooltip_at_pointer`"),
            "diagnostic tooltips must stay editor-owned"
        );
        assert!(
            widget_rs.contains("pub fn diagnostic_wins_over_lsp_hover"),
            "diagnostic precedence helper must exist"
        );
        assert!(
            widget_rs.contains("egui::show_tooltip_at_pointer"),
            "diagnostic squiggles must render via show_tooltip_at_pointer"
        );
        assert!(
            widget_rs
                .contains("fn widget_renders_diagnostic_tooltips_not_lsp_documentation_popups"),
            "widget must keep diagnostic vs LSP popup separation tested"
        );

        let hover_rs = include_str!("editor/hover.rs");
        assert!(
            hover_rs.contains("pub fn lsp_hover_allowed"),
            "LSP hover must be gated when diagnostic tooltip is active"
        );
        assert!(
            hover_rs.contains("Does not perform editor hit-testing, diagnostic"),
            "hover module must document diagnostic tooltip ownership in widget.rs"
        );
        assert!(
            hover_rs.contains("pub fn show_hover_documentation"),
            "LSP documentation must use a separate popup path"
        );

        let app_rs = include_str!("app.rs");
        let app_production = app_rs
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(&app_rs);
        assert!(
            app_production.contains("if self.diagnostic_tooltip_active"),
            "receive_hover must not apply LSP docs while diagnostic tooltip is active"
        );
        assert!(
            app_production.contains("lsp_hover_allowed(diagnostic_tooltip_active)"),
            "render_lsp_hover_popup must respect diagnostic tooltip gate"
        );
    }

    /// Never boundary: diagnostic squiggle tooltips must not be replaced by LSP hover (see
    /// **Boundaries → Never** §15).
    #[test]
    fn replace_diagnostic_tooltips_with_lsp_hover() {
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::types::{DiagnosticSeverity, LspDiagnostic, LspRequest};
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        assert_diagnostic_tooltips_are_not_replaced_by_lsp_hover_contract();

        let path = test_file("never_replace_diag.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();
        app.search_last_active = Some(path.clone());
        app.lsp.as_mut().unwrap().insert_diagnostics_for_test(
            path.clone(),
            vec![LspDiagnostic {
                line_start: 0,
                col_start: 3,
                line_end: 0,
                col_end: 7,
                severity: DiagnosticSeverity::Error,
                message: "expected `;`".to_owned(),
                code: Some("E0425".to_owned()),
            }],
        );

        set_hover_request_session(&mut app, &path, 42, LspPosition::new(0, 4));
        app.lsp_hover.content = Some("fn main — documentation".to_owned());
        app.lsp_hover.displayed_target = Some(super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        });
        app.lsp_hover.resting_target = app.lsp_hover.displayed_target.clone();
        app.lsp_hover.rest_started = Some(30.0);
        app.lsp_hover.popup_anchor =
            Some(Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)));

        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let row_height = 20.0;
        let line_y = 10.0 + row_height * 0.5;
        let pointer = pos2(92.0, line_y);
        let tooltip_id = egui::Id::new(("diagnostic_tooltip", Some(&path), 0usize));
        let input = egui::RawInput {
            screen_rect: Some(screen),
            events: vec![egui::Event::PointerMoved(pointer)],
            time: Some(30.0 + HOVER_REST_DELAY_SECS + 0.1),
            ..Default::default()
        };
        for _ in 0..2 {
            let _ = context.run(input.clone(), |ctx| {
                let _ = app.show_editor(ctx);
            });
        }

        assert!(app.diagnostic_tooltip_active);
        assert_hover_state_cleared(&app);
        assert!(app
            .lsp
            .as_mut()
            .unwrap()
            .drain_pending_requests()
            .iter()
            .all(|request| !matches!(request, LspRequest::Hover { .. })));
        assert!(
            egui::containers::popup::was_tooltip_open_last_frame(&context, tooltip_id),
            "diagnostic squiggle tooltip must remain visible"
        );
        assert!(
            app.lsp_hover.popup_rect.is_none(),
            "LSP documentation must not replace the diagnostic tooltip"
        );

        app.lsp_hover.rest_started = Some(40.0);
        set_hover_request_session(&mut app, &path, 43, LspPosition::new(0, 4));
        app.lsp_hover.resting_target = Some(super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        });
        app.diagnostic_tooltip_active = true;
        app.receive_hover(43, "late lsp docs".to_owned());
        assert!(
            app.lsp_hover.content.is_none(),
            "LSP hover responses must not populate while diagnostic tooltip is active"
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn diagnostic_hover_suppresses_lsp_hover() {
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::types::{DiagnosticSeverity, LspDiagnostic, LspRequest};
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("same_pointer_diag.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();
        app.search_last_active = Some(path.clone());
        app.lsp.as_mut().unwrap().insert_diagnostics_for_test(
            path.clone(),
            vec![LspDiagnostic {
                line_start: 0,
                col_start: 3,
                line_end: 0,
                col_end: 7,
                severity: DiagnosticSeverity::Error,
                message: "expected `;`".to_owned(),
                code: Some("E0425".to_owned()),
            }],
        );
        set_hover_request_session(&mut app, &path, 99, LspPosition::new(0, 4));
        app.lsp_hover.content = Some("fn main — documentation".to_owned());
        app.lsp_hover.displayed_target = Some(super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        });
        app.lsp_hover.resting_target = app.lsp_hover.displayed_target.clone();
        app.lsp_hover.rest_started = Some(20.0);
        app.lsp_hover.popup_anchor =
            Some(Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)));

        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let row_height = 20.0;
        let line_y = 10.0 + row_height * 0.5;
        let pointer = pos2(92.0, line_y);
        let tooltip_id = egui::Id::new(("diagnostic_tooltip", Some(&path), 0usize));
        let input = egui::RawInput {
            screen_rect: Some(screen),
            events: vec![egui::Event::PointerMoved(pointer)],
            time: Some(20.0 + HOVER_REST_DELAY_SECS + 0.1),
            ..Default::default()
        };
        for _ in 0..2 {
            let _ = context.run(input.clone(), |ctx| {
                let _ = app.show_editor(ctx);
            });
        }

        assert!(app.diagnostic_tooltip_active);
        assert_hover_state_cleared(&app);
        assert!(app
            .lsp
            .as_mut()
            .unwrap()
            .drain_pending_requests()
            .iter()
            .all(|request| !matches!(request, LspRequest::Hover { .. })));
        assert!(
            egui::containers::popup::was_tooltip_open_last_frame(&context, tooltip_id),
            "diagnostic tooltip must remain visible at the pointer"
        );
        assert!(
            app.lsp_hover.popup_rect.is_none(),
            "LSP hover popup must not render at the same pointer as an active diagnostic tooltip"
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn opening_modal_dismisses_lsp_hover() {
        use egui::{pos2, Rect, Vec2};

        let path = test_file("modal_dismiss_hover.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 0),
        };
        app.lsp_hover.resting_target = Some(target);
        app.lsp_hover.rest_started = Some(11.0);
        app.lsp_hover.displayed_target = app.lsp_hover.resting_target.clone();
        app.lsp_hover.popup_anchor =
            Some(Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)));
        app.lsp_hover.content = Some("fn main — documentation".to_owned());
        set_hover_request_session(&mut app, &path, 17, LspPosition::new(0, 0));

        app.open_settings();

        assert!(app.has_modal());
        assert_hover_state_cleared(&app);
        app.receive_hover(17, "stale modal docs".to_owned());
        assert!(app.lsp_hover.content.is_none());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn close_confirmation_modal_dismisses_lsp_hover() {
        use egui::{pos2, Rect, Vec2};

        let path = test_file("modal_close_hover.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers
            .get_mut(&path)
            .unwrap()
            .insert_at_cursor("x")
            .unwrap();

        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 0),
        };
        app.lsp_hover.resting_target = Some(target);
        app.lsp_hover.content = Some("fn main — documentation".to_owned());
        app.lsp_hover.popup_anchor =
            Some(Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)));

        app.request_close_file(&path);

        assert!(app.has_modal());
        assert_hover_state_cleared(&app);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn modal_overlays_exclude_lsp_hover_detection() {
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("modal_hover.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.open_settings();
        assert!(app.has_modal());

        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let pointer = pos2(120.0, 220.0);
        let input = egui::RawInput {
            screen_rect: Some(screen),
            events: vec![egui::Event::PointerMoved(pointer)],
            time: Some(HOVER_REST_DELAY_SECS + 1.0),
            ..Default::default()
        };
        let _ = context.run(input, |ctx| {
            let _ = app.show_editor(ctx);
        });

        assert!(app.lsp_hover.resting_target.is_none());
        assert!(hover_request_id(&app).is_none());
        assert!(app.lsp_hover.content.is_none());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn search_panel_excludes_lsp_hover_detection() {
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::LspClient;
        use crate::search_panel;
        use egui::{Rect, Vec2};

        let path = test_file("search_hover.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.search_last_active = Some(path.clone());
        app.search_state.open_find();

        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let mut panel_rect = Rect::NOTHING;
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                let _ = app.show_editor(ctx);
                // show_editor does not return panel_rect; lay out once to locate it.
                egui::CentralPanel::default().show(ctx, |ui| {
                    let out = search_panel::show(
                        ui,
                        &mut app.search_state,
                        0,
                        app.active_palette.semantic,
                    );
                    panel_rect = out.panel_rect;
                });
            },
        );
        assert!(panel_rect.is_positive());

        let over_panel = panel_rect.center();
        let input = egui::RawInput {
            screen_rect: Some(screen),
            events: vec![egui::Event::PointerMoved(over_panel)],
            time: Some(HOVER_REST_DELAY_SECS + 1.0),
            ..Default::default()
        };
        let _ = context.run(input, |ctx| {
            let _ = app.show_editor(ctx);
        });

        assert!(app.lsp_hover.resting_target.is_none());
        assert!(hover_request_id(&app).is_none());
        assert!(app.lsp_hover.content.is_none());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn opening_completion_popup_dismisses_lsp_hover() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("completion_opens_hover.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();
        app.buffers
            .get_mut(&path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 5 });

        let target = super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        };
        app.lsp_hover.resting_target = Some(target);
        app.lsp_hover.rest_started = Some(10.0);
        app.lsp_hover.displayed_target = app.lsp_hover.resting_target.clone();
        app.lsp_hover.popup_anchor =
            Some(Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)));
        app.lsp_hover.content = Some("fn main — documentation".to_owned());

        app.request_completion_at_cursor();

        assert!(app.completion.is_open());
        assert_hover_state_cleared(&app);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn completion_popup_suppresses_lsp_hover() {
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::types::{DiagnosticSeverity, LspCompletionItem, LspDiagnostic};
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("completion_precedence.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());
        app.search_last_active = Some(path.clone());
        app.buffers.get_mut(&path).unwrap().mark_lsp_synced();
        app.lsp.as_mut().unwrap().insert_diagnostics_for_test(
            path.clone(),
            vec![LspDiagnostic {
                line_start: 0,
                col_start: 3,
                col_end: 7,
                line_end: 0,
                severity: DiagnosticSeverity::Error,
                message: "expected `;`".to_owned(),
                code: Some("E0425".to_owned()),
            }],
        );

        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        let cursor = crate::editor::buffer::CursorPosition { line: 0, col: 2 };
        app.buffers.get_mut(&path).unwrap().set_cursor(cursor);
        begin_completion_session(&mut app, path.clone(), 1, revision, lsp_version, cursor);
        app.receive_completion(
            1,
            vec![LspCompletionItem {
                filter_text: None,
                label: "main".to_owned(),
                ..Default::default()
            }],
        );
        assert!(app.completion.is_open());

        app.lsp_hover.resting_target = Some(super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 4),
        });
        app.lsp_hover.rest_started = Some(10.0);
        app.lsp_hover.displayed_target = app.lsp_hover.resting_target.clone();
        app.lsp_hover.popup_anchor =
            Some(Rect::from_min_size(pos2(80.0, 40.0), Vec2::new(24.0, 18.0)));
        app.lsp_hover.content = Some("fn main — documentation".to_owned());

        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let row_height = 20.0;
        let line_y = 10.0 + row_height * 0.5;
        let pointer = pos2(92.0, line_y);
        let tooltip_id = egui::Id::new(("diagnostic_tooltip", Some(&path), 0usize));
        let input = egui::RawInput {
            screen_rect: Some(screen),
            events: vec![egui::Event::PointerMoved(pointer)],
            time: Some(10.0 + HOVER_REST_DELAY_SECS + 0.1),
            ..Default::default()
        };
        for _ in 0..2 {
            let _ = context.run(input.clone(), |ctx| {
                let _ = app.show_editor(ctx);
            });
        }

        assert_hover_state_cleared(&app);
        assert!(
            hover_request_id(&app).is_none(),
            "completion must suppress LSP hover requests over source text"
        );
        assert!(
            !egui::containers::popup::was_tooltip_open_last_frame(&context, tooltip_id),
            "completion must suppress diagnostic squiggle tooltips"
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn completion_dropdown_excludes_lsp_hover_detection() {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HoveredSourcePosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::types::LspCompletionItem;
        use crate::lsp::LspClient;
        use egui::{pos2, Rect, Vec2};

        let path = test_file("completion_hover.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.tree.load(path.parent().unwrap().to_path_buf()).unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.open_file(path.clone()).unwrap();
        app.active = Some(path.clone());

        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        let cursor = CursorPosition { line: 0, col: 2 };
        app.buffers.get_mut(&path).unwrap().set_cursor(cursor);
        begin_completion_session(&mut app, path.clone(), 1, revision, lsp_version, cursor);
        app.receive_completion(
            1,
            vec![LspCompletionItem {
                filter_text: None,
                label: "main".to_owned(),
                ..Default::default()
            }],
        );
        assert!(app.completion.is_open());

        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let pointer = pos2(120.0, 220.0);
        let input = egui::RawInput {
            screen_rect: Some(screen),
            events: vec![egui::Event::PointerMoved(pointer)],
            time: Some(HOVER_REST_DELAY_SECS + 1.0),
            ..Default::default()
        };
        let _ = context.run(input, |ctx| {
            app.update_lsp_hover(
                ctx,
                Some(HoveredSourcePosition::for_test(
                    CursorPosition { line: 0, col: 2 },
                    Rect::from_min_size(pointer, Vec2::new(20.0, 18.0)),
                )),
                None,
                HoverPopupModel::none(),
            );
        });

        assert!(app.lsp_hover.resting_target.is_none());
        assert!(hover_request_id(&app).is_none());
        assert!(app.lsp_hover.content.is_none());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn lsp_completion_and_hover_errors_are_silent() {
        use crate::editor::buffer::CursorPosition;

        let path = test_file("silent_err.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();
        app.error_message = Some("prior status".to_owned());

        let revision = app.buffers.get(&path).unwrap().revision();
        let lsp_version = app.buffers.get(&path).unwrap().lsp_version;
        begin_completion_session(
            &mut app,
            path.clone(),
            1,
            revision,
            lsp_version,
            CursorPosition { line: 0, col: 0 },
        );
        app.handle_lsp_request_error(
            1,
            Some(super::LspPendingKind::Completion),
            "failed".to_owned(),
        );
        assert!(app.error_message.as_deref() == Some("prior status"));
        assert!(!app.completion.is_open());

        set_hover_request_session(&mut app, &path, 2, LspPosition::new(0, 0));
        app.handle_lsp_request_error(2, Some(super::LspPendingKind::Hover), "failed".to_owned());
        assert!(app.error_message.as_deref() == Some("prior status"));
        assert!(hover_request_id(&app).is_none());

        app.lsp_hover.resting_target = Some(super::HoverTarget {
            path: path.clone(),
            position: LspPosition::new(0, 0),
        });
        app.lsp_hover.rest_started = Some(0.0);
        set_hover_request_session(&mut app, &path, 3, LspPosition::new(0, 0));
        app.receive_hover(3, String::new());
        assert!(app.lsp_hover.content.is_none());
        assert!(hover_request_id(&app).is_none());
        assert_eq!(
            app.lsp_hover.no_content_target,
            app.lsp_hover.resting_target
        );

        begin_completion_session(
            &mut app,
            path.clone(),
            4,
            revision,
            lsp_version,
            CursorPosition { line: 0, col: 0 },
        );
        app.receive_completion(4, Vec::new());
        assert!(!app.completion.is_open());
        assert!(app.error_message.as_deref() == Some("prior status"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_goto_definition_navigation() {
        use crate::editor::buffer::CursorPosition;

        let source_path = test_file("source.rs", "fn foo() {}\nfn main() {\n    foo();\n}\n");
        let target_path = test_file("target.rs", "pub fn bar() {}\n");

        // Setup app
        let mut app = BlueIdeApp::empty();
        app.open_file(source_path.clone()).unwrap();

        // Start LSP request GotoDefinition simulation
        let id = app.next_ui_correlation_id();
        app.pending_definitions.insert(
            id,
            super::PendingDefinitionRequest {
                source_path: source_path.clone(),
                source_revision: app.buffers.get(&source_path).unwrap().revision(),
                source_position: CursorPosition { line: 2, col: 4 },
                active_tab: source_path.clone(),
                is_f12: true,
            },
        );
        app.lsp_pending
            .insert(id, super::LspPendingKind::GotoDefinition);

        // Set cursor to match the mock F12 trigger position
        app.buffers
            .get_mut(&source_path)
            .unwrap()
            .set_cursor(CursorPosition { line: 2, col: 4 });

        // 1. Same-file navigation moves the cursor correctly
        app.receive_goto_definition(id, source_path.clone(), 0, 3);

        assert_eq!(app.active.as_ref(), Some(&source_path));
        let source_buf = app.buffers.get(&source_path).unwrap();
        assert_eq!(source_buf.cursor(), CursorPosition { line: 0, col: 3 });

        // 2. Cross-file navigation opens and activates the target
        let id = app.next_ui_correlation_id();
        app.pending_definitions.insert(
            id,
            super::PendingDefinitionRequest {
                source_path: source_path.clone(),
                source_revision: app.buffers.get(&source_path).unwrap().revision(),
                source_position: CursorPosition { line: 0, col: 3 },
                active_tab: source_path.clone(),
                is_f12: true,
            },
        );
        app.lsp_pending
            .insert(id, super::LspPendingKind::GotoDefinition);

        // Set cursor of source to match the mock position
        app.buffers
            .get_mut(&source_path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 3 });

        app.receive_goto_definition(id, target_path.clone(), 0, 7);

        assert_eq!(app.active.as_ref(), Some(&target_path));
        let target_buf = app.buffers.get(&target_path).unwrap();
        assert_eq!(target_buf.cursor(), CursorPosition { line: 0, col: 7 });

        // 3. Navigation reuses an already-open target buffer, preserving unsaved changes and not reloading.
        app.buffers
            .get_mut(&target_path)
            .unwrap()
            .insert_at_cursor("xyz")
            .unwrap();
        assert!(app.buffers.get(&target_path).unwrap().is_modified());

        // Now simulate another jump to target
        let id = app.next_ui_correlation_id();
        app.pending_definitions.insert(
            id,
            super::PendingDefinitionRequest {
                source_path: source_path.clone(),
                source_revision: app.buffers.get(&source_path).unwrap().revision(),
                source_position: CursorPosition { line: 0, col: 3 },
                active_tab: source_path.clone(),
                is_f12: true,
            },
        );
        app.lsp_pending
            .insert(id, super::LspPendingKind::GotoDefinition);

        // Set cursor of source to match mock position
        app.buffers
            .get_mut(&source_path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 3 });

        // Switch back to source first
        app.active = Some(source_path.clone());

        app.receive_goto_definition(id, target_path.clone(), 0, 10);

        assert_eq!(app.active.as_ref(), Some(&target_path));
        let target_buf = app.buffers.get(&target_path).unwrap();
        assert_eq!(target_buf.cursor(), CursorPosition { line: 0, col: 10 });
        assert!(
            target_buf.is_modified(),
            "Unsaved changes must be preserved"
        );

        // 4. Unicode target columns produce the correct caret position.
        let unicode_path = test_file("unicode.rs", "fn a🙂z() {}\n");
        app.open_file(unicode_path.clone()).unwrap();

        let id = app.next_ui_correlation_id();
        app.pending_definitions.insert(
            id,
            super::PendingDefinitionRequest {
                source_path: unicode_path.clone(),
                source_revision: app.buffers.get(&unicode_path).unwrap().revision(),
                source_position: CursorPosition { line: 0, col: 0 },
                active_tab: unicode_path.clone(),
                is_f12: true,
            },
        );
        app.lsp_pending
            .insert(id, super::LspPendingKind::GotoDefinition);

        // Set cursor of unicode buffer to match mock position
        app.buffers
            .get_mut(&unicode_path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 0 });

        app.receive_goto_definition(id, unicode_path.clone(), 0, 6);
        let unicode_buf = app.buffers.get(&unicode_path).unwrap();
        assert_eq!(unicode_buf.cursor(), CursorPosition { line: 0, col: 5 });

        // 5. Scroll/Focus is requested.
        let state = app.editor_states.get(&unicode_path).unwrap();
        assert!(state.is_scroll_requested());
        assert!(state.is_focus_requested());

        // 6. Missing target files preserve the source tab
        let missing_path = std::env::temp_dir().join("does_not_exist_xyz.rs");
        let id = app.next_ui_correlation_id();
        app.pending_definitions.insert(
            id,
            super::PendingDefinitionRequest {
                source_path: source_path.clone(),
                source_revision: app.buffers.get(&source_path).unwrap().revision(),
                source_position: CursorPosition { line: 0, col: 3 },
                active_tab: source_path.clone(),
                is_f12: true,
            },
        );
        app.lsp_pending
            .insert(id, super::LspPendingKind::GotoDefinition);
        app.active = Some(source_path.clone());

        // Set cursor of source
        app.buffers
            .get_mut(&source_path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 3 });

        app.receive_goto_definition(id, missing_path, 0, 0);

        assert_eq!(app.active.as_ref(), Some(&source_path));
        assert!(app.error_message.is_some());

        // 7. A stale response after a buffer edit is ignored.
        let id = app.next_ui_correlation_id();
        app.pending_definitions.insert(
            id,
            super::PendingDefinitionRequest {
                source_path: source_path.clone(),
                source_revision: app.buffers.get(&source_path).unwrap().revision(),
                source_position: CursorPosition { line: 0, col: 3 },
                active_tab: source_path.clone(),
                is_f12: true,
            },
        );
        app.lsp_pending
            .insert(id, super::LspPendingKind::GotoDefinition);
        app.active = Some(source_path.clone());

        // Set cursor of source
        app.buffers
            .get_mut(&source_path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 3 });

        app.buffers
            .get_mut(&source_path)
            .unwrap()
            .insert_at_cursor("edit")
            .unwrap();

        app.receive_goto_definition(id, target_path.clone(), 0, 0);
        assert_eq!(app.active.as_ref(), Some(&source_path));

        // 8. A stale response after a tab switch is ignored.
        let id = app.next_ui_correlation_id();
        app.pending_definitions.insert(
            id,
            super::PendingDefinitionRequest {
                source_path: source_path.clone(),
                source_revision: app.buffers.get(&source_path).unwrap().revision(),
                source_position: CursorPosition { line: 0, col: 3 },
                active_tab: source_path.clone(),
                is_f12: true,
            },
        );
        app.lsp_pending
            .insert(id, super::LspPendingKind::GotoDefinition);
        app.active = Some(target_path.clone());

        // Set cursor of source
        app.buffers
            .get_mut(&source_path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 3 });

        app.receive_goto_definition(id, target_path.clone(), 0, 0);
        assert_eq!(app.active.as_ref(), Some(&target_path));

        // 9. A superseded request cannot navigate.
        let id1 = app.next_ui_correlation_id();
        app.pending_definitions.insert(
            id1,
            super::PendingDefinitionRequest {
                source_path: source_path.clone(),
                source_revision: app.buffers.get(&source_path).unwrap().revision(),
                source_position: CursorPosition { line: 0, col: 3 },
                active_tab: source_path.clone(),
                is_f12: true,
            },
        );
        app.lsp_pending
            .insert(id1, super::LspPendingKind::GotoDefinition);

        let id2 = app.next_ui_correlation_id();
        app.lsp_pending
            .retain(|_, kind| *kind != super::LspPendingKind::GotoDefinition);
        app.pending_definitions
            .retain(|_, pending| pending.active_tab != source_path);

        app.pending_definitions.insert(
            id2,
            super::PendingDefinitionRequest {
                source_path: source_path.clone(),
                source_revision: app.buffers.get(&source_path).unwrap().revision(),
                source_position: CursorPosition { line: 0, col: 4 },
                active_tab: source_path.clone(),
                is_f12: true,
            },
        );
        app.lsp_pending
            .insert(id2, super::LspPendingKind::GotoDefinition);

        app.active = Some(source_path.clone());

        // Set cursor of source matching id1
        app.buffers
            .get_mut(&source_path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 3 });

        app.receive_goto_definition(id1, target_path.clone(), 0, 0);
        assert_eq!(app.active.as_ref(), Some(&source_path));

        // 10. Empty results (GotoNone) do not change the active file.
        let id = app.next_ui_correlation_id();
        app.pending_definitions.insert(
            id,
            super::PendingDefinitionRequest {
                source_path: source_path.clone(),
                source_revision: app.buffers.get(&source_path).unwrap().revision(),
                source_position: CursorPosition { line: 0, col: 3 },
                active_tab: source_path.clone(),
                is_f12: true,
            },
        );
        app.lsp_pending
            .insert(id, super::LspPendingKind::GotoDefinition);
        app.active = Some(source_path.clone());

        // Set cursor of source
        app.buffers
            .get_mut(&source_path)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 3 });

        app.receive_goto_none(id);
        assert_eq!(app.active.as_ref(), Some(&source_path));

        // Cleanup temp files
        let _ = fs::remove_file(source_path);
        let _ = fs::remove_file(target_path);
        let _ = fs::remove_file(unicode_path);
    }

    #[test]
    fn test_problems_panel_navigation_updates_active_file_and_cursor() {
        use crate::editor::buffer::CursorPosition;
        use crate::lsp::types::DiagnosticSeverity;
        use crate::problems_panel::DiagnosticRow;

        let path = test_file("diag.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();

        let row = DiagnosticRow {
            path: path.clone(),
            line: 0,
            col: 3,
            severity: DiagnosticSeverity::Error,
            message: "test error".to_owned(),
            code: None,
        };

        app.navigate_to_diagnostic(&row);

        // Verify active file and cursor position
        assert_eq!(app.active.as_ref(), Some(&path));
        let buf = app.buffers.get(&path).unwrap();
        assert_eq!(buf.cursor(), CursorPosition { line: 0, col: 3 });

        // Verify scrolling is requested
        let state = app.editor_states.get(&path).unwrap();
        assert!(state.is_scroll_requested());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_problems_panel_navigation_missing_file_reports_error() {
        use crate::lsp::types::DiagnosticSeverity;
        use crate::problems_panel::DiagnosticRow;

        let path = test_file("diag.rs", "fn main() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(path.clone()).unwrap();

        let missing_path = std::env::temp_dir().join("does_not_exist_diag.rs");
        let row = DiagnosticRow {
            path: missing_path.clone(),
            line: 0,
            col: 0,
            severity: DiagnosticSeverity::Error,
            message: "test error".to_owned(),
            code: None,
        };

        app.navigate_to_diagnostic(&row);

        // Active tab must still be the original one
        assert_eq!(app.active.as_ref(), Some(&path));
        assert!(app.error_message.is_some());
        assert!(app
            .error_message
            .as_ref()
            .unwrap()
            .contains("does_not_exist_diag.rs"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_problems_panel_toggle_preserves_diagnostics() {
        use crate::lsp::types::{DiagnosticSeverity, LspDiagnostic};
        use crate::lsp::LspClient;

        let mut app = BlueIdeApp::empty();
        let mut client = LspClient::new_test_client();

        let path = test_file("diag.rs", "fn main() {}\n");
        let diag = LspDiagnostic {
            line_start: 0,
            col_start: 0,
            line_end: 0,
            col_end: 0,
            severity: DiagnosticSeverity::Error,
            message: "error".to_owned(),
            code: None,
        };
        client.insert_diagnostics_for_test(path.clone(), vec![diag]);
        app.lsp = Some(client);

        // Initially hidden (VS Code-style bottom panel)
        assert!(!app.show_problems);
        assert!(!app.show_bottom_panel);

        // Close panel
        app.close_bottom_panel();
        let diags_closed = app.lsp.as_ref().unwrap().diagnostics().clone();
        assert_eq!(diags_closed.len(), 1);
        assert_eq!(diags_closed.get(&path).unwrap()[0].message, "error");

        // Open panel
        app.toggle_problems_panel();
        assert!(app.show_problems);
        assert!(app.show_bottom_panel);
        let diags_open = app.lsp.as_ref().unwrap().diagnostics().clone();
        assert_eq!(diags_open.len(), 1);
        assert_eq!(diags_open.get(&path).unwrap()[0].message, "error");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn normal_typing_cursor_movement_scrolling_search_highlighting_diagnostic_underlines_diagnostic_tooltips_file_tabs_and_modal_behavior_continue_to_work(
    ) {
        use crate::editor::buffer::CursorPosition;
        use crate::editor::hover::HOVER_REST_DELAY_SECS;
        use crate::lsp::types::{DiagnosticSeverity, LspDiagnostic};
        use crate::lsp::LspClient;
        use crate::search::SearchScope;
        use egui::{pos2, Event, Key, Modifiers, Rect, Vec2};

        let primary = test_file("baseline_primary.rs", "fn main() {}\n");
        let secondary = test_file("baseline_secondary.rs", "fn other() {}\n");
        let mut app = BlueIdeApp::empty();
        app.open_file(primary.clone()).unwrap();
        app.open_file(secondary.clone()).unwrap();
        app.pane_tree
            .open_in_pane(app.focus.active_pane, primary.clone());
        app.active = Some(primary.clone());
        app.search_last_active = Some(primary.clone());

        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let editor_id = egui::Id::new(("blue_ide_editor", Some(primary.clone())));
        app.buffers
            .get_mut(&primary)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 3 });

        // Typing inserts text into the active buffer.
        let revision_before = app.buffers.get(&primary).unwrap().revision();
        let _ = context.run(
            egui::RawInput {
                focused: true,
                events: vec![Event::Text("!".into())],
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                ctx.memory_mut(|mem| mem.request_focus(editor_id));
                let _ = app.show_editor(ctx);
            },
        );
        assert_eq!(
            app.buffers.get(&primary).unwrap().revision(),
            revision_before + 1
        );
        assert_eq!(app.buffers.get(&primary).unwrap().text(), "fn !main() {}\n");

        // Cursor movement via arrow keys (focus + key in one frame).
        app.buffers
            .get_mut(&primary)
            .unwrap()
            .set_cursor(CursorPosition { line: 0, col: 5 });
        app.editor_states.get_mut(&primary).unwrap().request_focus();
        let _ = context.run(
            egui::RawInput {
                focused: true,
                events: vec![Event::Key {
                    key: Key::ArrowLeft,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }],
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                ctx.memory_mut(|mem| mem.request_focus(editor_id));
                let _ = app.show_editor(ctx);
            },
        );
        assert_eq!(
            app.buffers.get(&primary).unwrap().cursor(),
            CursorPosition { line: 0, col: 4 },
            "arrow keys should move the editor cursor"
        );

        // Scrolling: scroll-to-cursor requests are honored by the editor widget.
        app.editor_states
            .get_mut(&primary)
            .unwrap()
            .request_scroll_to_cursor();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                let _ = app.show_editor(ctx);
            },
        );
        assert!(
            !app.editor_states
                .get(&primary)
                .unwrap()
                .is_scroll_requested(),
            "scroll-to-cursor should be consumed after rendering"
        );

        // Search highlighting: find panel produces file-scope matches for the buffer.
        app.search_state.open_find();
        app.search_state.query.scope = SearchScope::File;
        app.search_state.query.query = "main".to_owned();
        app.search_state.recompile();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                let _ = app.show_editor(ctx);
            },
        );
        assert!(
            !app.search_state.file_matches.is_empty(),
            "search highlighting requires non-empty file-scope matches"
        );

        // Diagnostic underlines and tooltips stay wired through the LSP client.
        app.tree
            .load(primary.parent().unwrap().to_path_buf())
            .unwrap();
        app.lsp = Some(LspClient::new_test_client());
        app.buffers.get_mut(&primary).unwrap().mark_lsp_synced();
        app.lsp.as_mut().unwrap().insert_diagnostics_for_test(
            primary.clone(),
            vec![LspDiagnostic {
                line_start: 0,
                col_start: 3,
                line_end: 0,
                col_end: 7,
                severity: DiagnosticSeverity::Error,
                message: "expected `;`".to_owned(),
                code: Some("E0425".to_owned()),
            }],
        );
        let diagnostics = app
            .lsp
            .as_ref()
            .unwrap()
            .diagnostics_for(&primary)
            .expect("diagnostics should render squiggles");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].message, "expected `;`");

        let row_height = 20.0;
        let line_y = 10.0 + row_height * 0.5;
        let pointer = pos2(80.0, line_y);
        let tooltip_id = egui::Id::new(("diagnostic_tooltip", Some(&primary), 0usize));
        let hover_input = egui::RawInput {
            screen_rect: Some(screen),
            events: vec![Event::PointerMoved(pointer)],
            time: Some(HOVER_REST_DELAY_SECS + 1.0),
            ..Default::default()
        };
        for i in 0..2 {
            let _ = context.run(hover_input.clone(), |ctx| {
                let _ = app.show_editor(ctx);
            });
            println!(
                "Frame {}: diagnostic_tooltip_active = {}, lsp_hover active = {:?}",
                i, app.diagnostic_tooltip_active, app.lsp_hover
            );
        }

        // If tooltip is not active, it might be due to mock layout returning character width 1.0.
        // Try hovering at x = 57.0 (which corresponds to index 3-7 under mock layout text_left=52.3).
        if !app.diagnostic_tooltip_active {
            let mock_pointer = pos2(57.0, line_y);
            let mock_hover_input = egui::RawInput {
                screen_rect: Some(screen),
                events: vec![Event::PointerMoved(mock_pointer)],
                time: Some(HOVER_REST_DELAY_SECS + 1.0),
                ..Default::default()
            };
            for i in 0..2 {
                let _ = context.run(mock_hover_input.clone(), |ctx| {
                    let _ = app.show_editor(ctx);
                });
                println!(
                    "Mock Frame {}: diagnostic_tooltip_active = {}, lsp_hover active = {:?}",
                    i, app.diagnostic_tooltip_active, app.lsp_hover
                );
            }
        }
        assert!(app.diagnostic_tooltip_active);
        assert!(
            egui::containers::popup::was_tooltip_open_last_frame(&context, tooltip_id),
            "diagnostic tooltip should open over the squiggle"
        );

        // File tabs: cycling and rendering stay functional with multiple buffers open.
        assert_eq!(app.active.as_ref(), Some(&primary));
        app.cycle_tab(1);
        assert_eq!(app.active.as_ref(), Some(&secondary));
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                app.show_tabs(ctx);
            },
        );
        app.cycle_tab(-1);
        assert_eq!(app.active.as_ref(), Some(&primary));

        // Modal behavior: settings modal blocks buffer edits.
        app.open_settings();
        assert!(app.has_modal());
        let revision_with_modal = app.buffers.get(&primary).unwrap().revision();
        let _ = context.run(
            egui::RawInput {
                focused: true,
                events: vec![Event::Text("x".into())],
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                ctx.memory_mut(|mem| mem.request_focus(editor_id));
                let _ = app.show_editor(ctx);
            },
        );
        assert_eq!(
            app.buffers.get(&primary).unwrap().revision(),
            revision_with_modal,
            "typing must not mutate the buffer while a modal is open"
        );

        let _ = fs::remove_file(primary);
        let _ = fs::remove_file(secondary);
    }

    #[test]
    fn command_palette_and_quick_open_shortcuts_open_keyboard_modal_launchers() {
        use egui::{Event, Key, Modifiers};

        fn key_event(key: Key, modifiers: Modifiers) -> Event {
            Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }
        }

        let mut app = BlueIdeApp::empty();
        let context = egui::Context::default();
        let command_shift = Modifiers {
            command: true,
            shift: true,
            ..Modifiers::NONE
        };
        let _ = context.run(
            egui::RawInput {
                events: vec![key_event(Key::P, command_shift)],
                ..Default::default()
            },
            |ctx| app.handle_shortcuts(ctx),
        );
        assert_eq!(
            app.launcher.mode(),
            Some(crate::launcher::LauncherMode::Commands)
        );
        assert!(app.has_modal());

        app.launcher.dismiss();
        let _ = context.run(
            egui::RawInput {
                events: vec![key_event(Key::P, Modifiers::COMMAND)],
                ..Default::default()
            },
            |ctx| app.handle_shortcuts(ctx),
        );
        assert_eq!(
            app.launcher.mode(),
            Some(crate::launcher::LauncherMode::Files)
        );
    }

    #[test]
    fn quick_open_selection_uses_the_normal_file_open_path() {
        let path = test_file("quick_open_selection.rs", "fn selected() {}\n");
        let mut app = BlueIdeApp::empty();
        let context = egui::Context::default();
        app.open_quick_open(&context);

        app.handle_launcher_event(
            crate::launcher::LauncherEvent::OpenFile(path.clone()),
            &context,
        );

        assert_eq!(app.active.as_ref(), Some(&path));
        assert!(app.buffers.contains_key(&path));
        assert!(!app.launcher.is_open());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_pinned_tabs_prevent_close() {
        let path1 = test_file("test_pinned_1.rs", "content 1");
        let path2 = test_file("test_pinned_2.rs", "content 2");
        let mut app = BlueIdeApp::empty();

        let _ = app.open_file(path1.clone());
        let _ = app.open_file(path2.clone());

        assert!(app.buffers.contains_key(&path1));
        assert!(app.buffers.contains_key(&path2));

        // Pin path1
        app.pinned_tabs.insert(path1.clone());

        // Try closing path1 (pinned)
        app.request_close_file(&path1);
        assert!(app.buffers.contains_key(&path1), "Pinned tab must not be closed");

        // Try closing path2 (unpinned)
        app.request_close_file(&path2);
        assert!(!app.buffers.contains_key(&path2), "Unpinned tab should be closed");

        // Unpin path1 and close
        app.pinned_tabs.remove(&path1);
        app.request_close_file(&path1);
        assert!(!app.buffers.contains_key(&path1), "Unpinned tab should be closed");

        let _ = fs::remove_file(path1);
        let _ = fs::remove_file(path2);
    }

    #[test]
    fn test_tab_groups() {
        let path = std::path::PathBuf::from("src/main.rs");
        let mut app = BlueIdeApp::empty();

        app.tab_groups.push(super::TabGroup {
            name: "Models".to_string(),
            color_rgba: [255, 0, 0, 255],
        });

        app.tab_to_group.insert(path.clone(), "Models".to_string());

        assert_eq!(app.tab_to_group.get(&path).map(|s| s.as_str()), Some("Models"));
    }

    #[test]
    fn test_recent_files_tracking() {
        let path1 = std::path::PathBuf::from("file1.rs");
        let path2 = std::path::PathBuf::from("file2.rs");
        let mut app = BlueIdeApp::empty();

        app.touch_recent_file(path1.clone());
        app.touch_recent_file(path2.clone());

        assert_eq!(app.recent_files[0], path2);
        assert_eq!(app.recent_files[1], path1);

        // Touch path1 again to move it to the front
        app.touch_recent_file(path1.clone());
        assert_eq!(app.recent_files[0], path1);
        assert_eq!(app.recent_files[1], path2);
    }

    #[test]
    fn test_bookmarks_navigation() {
        let path = test_file("test_bookmarks.rs", "line0\nline1\nline2\nline3\n");
        let mut app = BlueIdeApp::empty();

        let _ = app.open_file(path.clone());
        app.active = Some(path.clone());

        // Toggle bookmark on line 1 and line 3
        app.handle_editor_action(super::EditorAction::ToggleBookmark { line: 1 }, true, true);
        app.handle_editor_action(super::EditorAction::ToggleBookmark { line: 3 }, true, true);

        {
            let bookmarks = app.bookmarks.get(&path).unwrap();
            assert!(bookmarks.contains(&1));
            assert!(bookmarks.contains(&3));
        }

        // Set cursor to line 0 and navigate next
        app.buffers.get_mut(&path).unwrap().set_cursor(crate::editor::buffer::CursorPosition { line: 0, col: 0 });
        app.handle_editor_action(super::EditorAction::NextBookmark, true, true);
        assert_eq!(app.buffers.get(&path).unwrap().cursor().line, 1);

        // Navigate next again
        app.handle_editor_action(super::EditorAction::NextBookmark, true, true);
        assert_eq!(app.buffers.get(&path).unwrap().cursor().line, 3);

        // Navigate prev
        app.handle_editor_action(super::EditorAction::PrevBookmark, true, true);
        assert_eq!(app.buffers.get(&path).unwrap().cursor().line, 1);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_session_save_restore() {
        let path = test_file("test_session.rs", "fn main() {}");
        // Use a unique session file to avoid racing with other parallel tests.
        let session_file = std::env::temp_dir().join(format!(
            "blue_ide_session_test_{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_file(&session_file);

        let mut app = BlueIdeApp::empty();
        app.settings_store = crate::settings::SettingsStore::at_path(session_file.clone());

        // Open a file, pin it, add to group, bookmark it
        let _ = app.open_file(path.clone());
        app.active = Some(path.clone());
        app.pinned_tabs.insert(path.clone());
        app.tab_groups.push(super::TabGroup {
            name: "Logic".to_string(),
            color_rgba: [0, 255, 0, 255],
        });
        app.tab_to_group.insert(path.clone(), "Logic".to_string());
        app.bookmarks.entry(path.clone()).or_default().insert(5);

        // Save session
        app.save_session();

        // Load session on another instance
        let mut app2 = BlueIdeApp::empty();
        app2.settings_store = crate::settings::SettingsStore::at_path(session_file.clone());
        app2.load_session();

        // Verify restored state
        assert!(app2.pinned_tabs.contains(&path));
        assert!(app2.tab_groups.iter().any(|g| g.name == "Logic"));
        assert_eq!(app2.tab_to_group.get(&path).map(|s| s.as_str()), Some("Logic"));
        assert!(app2.bookmarks.get(&path).unwrap().contains(&5));

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(session_file);
    }

    #[test]
    fn workspace_root_resolution_prefers_the_deepest_matching_root() {
        let outer = std::env::temp_dir().join("blue_ide_multi_root_outer");
        let inner = outer.join("nested");
        fs::create_dir_all(&inner).unwrap();

        let mut app = BlueIdeApp::empty();
        app.workspace.add_root(&outer).unwrap();
        app.workspace.add_root(&inner).unwrap();

        assert_eq!(
            app.workspace_root_for_path(&inner.join("src/main.rs"))
                .as_deref(),
            Some(inner.as_path())
        );
        assert_eq!(
            app.workspace_root_for_path(&outer.join("README.md"))
                .as_deref(),
            Some(outer.as_path())
        );

        let _ = fs::remove_dir_all(&outer);
    }
}
