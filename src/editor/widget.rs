//! Custom editor widget: coordinate math, hit-testing, diagnostic tooltips, and
//! editor-local pointer-hover precedence.
//!
//! Editor interaction responsibilities:
//! - **Hit-testing** — gutter vs source glyphs vs diagnostic squiggles; reports
//!   `HoveredSourcePosition` (logical position + screen-space `token_rect`).
//! - **Anchor rectangles** — `CompletionPopupAnchor` (caret/completion anchor) and
//!   `HoveredSourcePosition::token_rect` (source-text LSP hover anchor),
//!   both converted to screen space after scroll offset.
//! - **Input capture** — click-to-caret, keyboard editing, and focused shortcuts;
//!   pointer-hover hit-testing does not move the caret or mutate buffer text.
//! - **Popup rendering** — diagnostic squiggle tooltips via `show_tooltip_at_pointer`
//!   (message + `[code]`). LSP documentation popups live in `editor/hover.rs`.
//! - **Precedence** — completion dropdown → diagnostic tooltip → deferred LSP
//!   hover candidate → none (`resolve_pointer_hover_precedence_with_completion`).
//! - **App handoff** — `HoverPopupModel` (`hovered_source`, diagnostic flag) is
//!   consumed by `app.rs` for LSP hover debounce and popup anchors.
//! - **Frame input** — `EditorInteraction` (`CompletionPopupModel`), `EditorAnnotations`,
//!   and `EditorPresentation` group per-frame arguments for `EditorWidget::show`.
//! - **Requested actions** — `EditorAction::RequestCompletion` and
//!   `GoToDefinition` replace ad-hoc output flags; `app.rs` applies gates.
//! - **Accepted/dismissed events** — `CompletionPopupEvent` and `HoverPopupEvent`
//!   mirror requested actions for popup lifecycle (click, keyboard, outside click).
//! - **Ownership** — `EditorWidget` is stateless per frame. The app owns `TextBuffer`,
//!   `EditorState`, `CompletionState`, hover sessions, and `LspClient`. Requested
//!   actions and popup events flow upward only; the app applies gates and performs
//!   all LSP I/O. No circular app ↔ widget ↔ LSP ownership, and the widget never
//!   owns or calls `LspClient`.
//! - **Borrowing** — `app.rs` collects `EditorAction` and popup events during the
//!   egui closure, then applies buffer edits and LSP requests only after buffer /
//!   editor-state borrows end (same pattern as the search panel).
//! - **Position encoding** — widget hit-testing reports `CursorPosition` (Rust
//!   character index). Outbound LSP requests encode via [`encode_char_column`]; inbound
//!   diagnostic ranges decode via [`lsp_utf16_range_char_span_on_line`] in
//!   `editor/position.rs`.
//!
//! Does not debounce hover, send LSP requests, or parse JSON-RPC (`app.rs`,
//! `lsp/transport.rs`).
//!
//! # Editor/UI state tests
//!
//! Part of crate-level **Regression tests** (`lib.rs`).
//!
//! Focused tests for per-frame editor interaction, popup handoff, and keyboard/pointer
//! input. Where practical, widget tests drive `EditorWidget::show` through
//! `egui::Context::run(RawInput, …)` with `focused`, `screen_rect`, and synthetic
//! `Event::Key` / pointer streams (`show_test_editor` helpers below). Pure precedence
//! helpers (`resolve_pointer_hover_precedence`, …) stay unit-tested without egui.
//! App-layer lifecycle (completion accept, hover debounce, LSP gates) and popup UI in
//! `completion.rs` / `hover.rs` use the same `RawInput` pattern when keyboard or
//! outside-click behavior matters.
//!
//! | Area | Example tests | Run |
//! |------|---------------|-----|
//! | Frame input types | `editor_interaction_model_groups_per_frame_gates` | `cargo test --lib editor::widget` |
//! | F12 / Ctrl+click go-to-definition | `test_editor_interaction` | `cargo test --lib test_editor_interaction` |
//! | Ctrl+Space completion request | `ctrl_space_produces_one_completion_request_action_and_does_not_insert_a_space` | `cargo test --lib ctrl_space_produces` |
//! | Ctrl+Space completion e2e (app) | `ctrl_space_sends_a_real_completion_request_and_opens_a_functional_caret_anchored_dropdown` | `cargo test --lib ctrl_space_sends_a_real_completion` |
//! | Completion navigate/accept/click/dismiss | `completion_can_be_navigated_accepted_clicked_and_dismissed` | `cargo test --lib completion_can_be_navigated` |
//! | Manual completion trigger (no type-ahead) | `ask_before_adding_automatic_completion_on_every_typed_character` | `cargo test --test integration_test ask_before_adding_automatic_completion_on_every_typed_character` |
//! | Editor widget architecture fingerprint | `ask_before_broadly_redesigning_the_editor_widget` | `cargo test --test integration_test ask_before_broadly_redesigning_the_editor_widget` |
//! | Pointer hover hit-test | `pointer_text_hover_*` | `cargo test --lib pointer_text_hover` |
//! | Diagnostic hover suppresses LSP hover | `diagnostic_hover_suppresses_lsp_hover` | `cargo test --lib diagnostic_hover_suppresses` |
//! | Diagnostic vs LSP hover coexistence (app) | `diagnostic_tooltips_and_lsp_hover_coexist_according_to_the_specified_precedence` | `cargo test --lib diagnostic_tooltips_and_lsp_hover` |
//! | Diagnostic vs LSP hover precedence (widget) | `resolve_pointer_hover_precedence_*`, `diagnostic_underline_wins_*` | `cargo test --lib resolve_pointer_hover_precedence` |
//! | Hover popup handoff | `editor_hover_popup_groups_*` | `cargo test --lib editor_hover_popup` |
//! | Completion anchor | `completion_anchor_reports_*` | `cargo test --lib completion_anchor` |
//! | Diagnostic squiggle tooltips | `diagnostic_squiggle_*`, `diagnostic_tooltip_*` | `cargo test --lib diagnostic_squiggle` |
//! | Completion popup suppresses LSP hover | `completion_popup_suppresses_lsp_hover` | `cargo test --lib completion_popup_suppresses` |
//! | Completion blocks hover (widget) | `completion_open_suppresses_*` | `cargo test --lib completion_open_suppresses` |
//! | Widget renders diagnostic tooltips only | `widget_renders_diagnostic_tooltips_*` | `cargo test --lib widget_renders_diagnostic` |
//! | All popup keyboard paths | `keep_all_popup_interactions_keyboard_accessible` | `cargo test --lib keep_all_popup_interactions_keyboard_accessible` |
//! | Enter/Tab accept completion | `enter_and_tab_accept` | `cargo test --lib enter_and_tab_accept` |
//! | Escape dismisses without editing | `escape_dismisses_without_editing` | `cargo test --lib escape_dismisses_without_editing` |
//! | Empty completion results | `empty_results_do_not_open_the_popup` | `cargo test --lib empty_results_do_not_open` |
//! | Stale completion responses ignored | `stale_responses_are_ignored` | `cargo test --lib stale_responses_are_ignored` |
//! | Stale async responses (app) | `stale_asynchronous_responses_cannot_affect_the_current_editor_state` | `cargo test --lib stale_asynchronous_responses_cannot` |
//! | Tab/file/revision dismiss popups | `tab_file_revision_changes_dismiss_stale_completion_and_hover_state` | `cargo test --lib tab_file_revision_changes` |
//! | Completion popup keyboard/click | `clicking_completion_row_*`, `outside_click_*` | `cargo test --lib editor::completion` |
//! | Hover popup layout/gates | `apply_lsp_hover_gates_*`, `hover_popup_*` | `cargo test --lib editor::hover` |
//! | Completion arrow keys (selection, cursor) | `arrow_keys_change_completion_selection_without_moving_the_editor_cursor` | `cargo test --lib arrow_keys_change_completion` |
//! | App completion navigation (consume keys) | `completion_navigation_keys_are_consumed_*` | `cargo test --lib completion_navigation_keys` |
//! | Hover debounce (one request per position) | `hover_debounce_produces_at_most_one_request_for_a_stationary_source_position` | `cargo test --lib hover_debounce_produces` |
//! | Pointer movement resets hover debounce | `pointer_movement_resets_hover_debounce` | `cargo test --lib pointer_movement_resets_hover` |
//! | App hover debounce / stale guards | `hover_request_is_debounced_*`, `hover_rejects_stale_*`, `hover_tracks_request_*` | `cargo test --lib hover_request` |
//! | Baseline editor regression | `normal_typing_cursor_movement_scrolling_search_highlighting_diagnostic_underlines_diagnostic_tooltips_file_tabs_and_modal_behavior_continue_to_work` | `cargo test --lib normal_typing_cursor_movement` |
//! | Never: swallow editor keys (no popup) | `swallow_normal_editor_keystrokes_when_no_popup_is_open` | `cargo test --lib swallow_normal_editor_keystrokes_when_no_popup_is_open` |
//! | Never: replace diagnostic tooltips with LSP hover | `replace_diagnostic_tooltips_with_lsp_hover` | `cargo test --lib replace_diagnostic_tooltips_with_lsp_hover` |

use std::ops::Range;
use std::path::PathBuf;
use std::time::Duration;

use egui::{
    pos2, vec2, Align, Align2, Color32, Event, FontId, Key, Modifiers, Rect, Response, ScrollArea,
    Sense, Stroke, Ui, WidgetInfo, WidgetType,
};

use super::buffer::{CursorPosition, TextBuffer};
use super::completion::{CompletionPopupAnchor, CompletionPopupModel};
pub use super::hover::{HoverPopupModel, HoveredSourcePosition};
use super::position::{decode_utf16_column, lsp_utf16_range_char_span_on_line};
use crate::lsp::types::{DiagnosticSeverity, LspDiagnostic};
use crate::text::rtl::{contains_rtl, create_layout_job_for_line, split_at_comment};
use crate::theme::{SemanticPalette, ThemePalette};

/// A single search highlight to paint over the editor text.
///
/// Byte offsets must be relative to the start of the file, not the line.
#[derive(Debug, Clone)]
pub struct SearchHighlight {
    /// File-relative byte range of the match.
    pub byte_range: std::ops::Range<usize>,
    /// Whether this is the currently active (navigated-to) match.
    pub is_active: bool,
}

/// Per-frame interaction policy from the app shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorInteraction {
    /// When false, the editor surrenders focus and ignores keyboard/mouse edits.
    pub enabled: bool,
    /// LSP-backed gestures (F12, Ctrl+click go-to-definition).
    pub lsp_active: bool,
    /// Completion popup state; when open, suppresses pointer-hover handoff.
    pub completion_popup: CompletionPopupModel,
}

impl EditorInteraction {
    pub const fn new(
        enabled: bool,
        lsp_active: bool,
        completion_popup: CompletionPopupModel,
    ) -> Self {
        Self {
            enabled,
            lsp_active,
            completion_popup,
        }
    }

    /// Editor accepts input; LSP gestures follow `lsp_active`.
    pub const fn interactive(lsp_active: bool) -> Self {
        Self::new(true, lsp_active, CompletionPopupModel::closed())
    }

    pub const fn with_completion_popup(mut self, popup: CompletionPopupModel) -> Self {
        self.completion_popup = popup;
        self
    }

    #[cfg(test)]
    pub const fn for_test() -> Self {
        Self::interactive(true)
    }
}

/// Read-only overlay data painted by the editor widget.
#[derive(Debug, Clone, Copy)]
pub struct EditorAnnotations<'a> {
    pub diagnostics: &'a [LspDiagnostic],
    pub search_highlights: &'a [SearchHighlight],
    pub diff_hunks: &'a [crate::git::DiffHunk],
    pub blame_lines: &'a [crate::git::BlameLine],
    pub show_blame: bool,
    pub bookmarks: &'a [usize],
}

impl<'a> Default for EditorAnnotations<'a> {
    fn default() -> Self {
        Self {
            diagnostics: &[],
            search_highlights: &[],
            diff_hunks: &[],
            blame_lines: &[],
            show_blame: false,
            bookmarks: &[],
        }
    }
}

impl<'a> EditorAnnotations<'a> {
    pub const fn new(
        diagnostics: &'a [LspDiagnostic],
        search_highlights: &'a [SearchHighlight],
    ) -> Self {
        Self {
            diagnostics,
            search_highlights,
            diff_hunks: &[],
            blame_lines: &[],
            show_blame: false,
            bookmarks: &[],
        }
    }

    pub const fn empty() -> Self {
        Self {
            diagnostics: &[],
            search_highlights: &[],
            diff_hunks: &[],
            blame_lines: &[],
            show_blame: false,
            bookmarks: &[],
        }
    }

    pub const fn with_diff_hunks(mut self, hunks: &'a [crate::git::DiffHunk]) -> Self {
        self.diff_hunks = hunks;
        self
    }

    pub const fn with_blame(
        mut self,
        blame_lines: &'a [crate::git::BlameLine],
        show_blame: bool,
    ) -> Self {
        self.blame_lines = blame_lines;
        self.show_blame = show_blame;
        self
    }

    pub const fn with_bookmarks(mut self, bookmarks: &'a [usize]) -> Self {
        self.bookmarks = bookmarks;
        self
    }
}

/// Visual configuration for one editor frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultiCursorModifier {
    Alt,
    Ctrl,
    Command,
}

#[derive(Debug, Clone, Copy)]
pub struct EditorPresentation {
    pub font_size: f32,
    pub palette: ThemePalette,
    pub tab_width: usize,
    pub insert_spaces: bool,
    pub show_indent_guides: bool,
    pub sticky_scroll: bool,
    pub sticky_scroll_max_lines: usize,
    pub bracket_colorization: bool,
    pub bracket_matching: bool,
    pub indent_guide_color: Color32,
    pub indent_guide_active_color: Color32,
    pub multi_cursor_modifier: MultiCursorModifier,
    /// Vim/modal editing enabled (`editor.vim_mode`).
    pub vim_enabled: bool,
    /// Render diagnostic messages inline at end of line (Zed-style).
    pub inline_diagnostics: bool,
}

impl EditorPresentation {
    pub const fn new(font_size: f32, palette: ThemePalette) -> Self {
        Self {
            font_size,
            palette,
            tab_width: 4,
            insert_spaces: true,
            show_indent_guides: true,
            sticky_scroll: true,
            sticky_scroll_max_lines: 5,
            bracket_colorization: true,
            bracket_matching: true,
            indent_guide_color: Color32::from_rgb(42, 42, 42),
            indent_guide_active_color: Color32::from_rgb(64, 64, 64),
            multi_cursor_modifier: MultiCursorModifier::Alt,
            vim_enabled: false,
        }
    }

    pub fn with_editor_settings(mut self, settings: &crate::settings::EditorSettings) -> Self {
        self.tab_width = settings.tab_width as usize;
        self.insert_spaces = settings.insert_spaces;
        self.show_indent_guides = settings.show_indent_guides;
        self.sticky_scroll = settings.sticky_scroll;
        self.sticky_scroll_max_lines = settings.sticky_scroll_max_lines.max(1);
        self.bracket_colorization = settings.bracket_colorization;
        self.bracket_matching = settings.bracket_matching;
        self.indent_guide_color =
            parse_hex_color(&settings.indent_guide_color).unwrap_or(self.indent_guide_color);
        self.indent_guide_active_color = parse_hex_color(&settings.indent_guide_active_color)
            .unwrap_or(self.indent_guide_active_color);
        self.multi_cursor_modifier =
            match settings.multi_cursor_modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => MultiCursorModifier::Ctrl,
                "command" | "cmd" | "meta" => MultiCursorModifier::Command,
                _ => MultiCursorModifier::Alt,
            };
        self.vim_enabled = settings.vim_mode;
        self.inline_diagnostics = settings.inline_diagnostics;
        self
    }

    /// When large file mode is active, disable expensive rendering features.
    pub fn with_large_file_suppressed(mut self, suppressed: bool) -> Self {
        if suppressed {
            self.bracket_colorization = false;
            self.bracket_matching = false;
            self.show_indent_guides = false;
            self.sticky_scroll = false;
        }
        self
    }

    #[cfg(test)]
    pub fn test(font_size: f32) -> Self {
        Self::new(
            font_size,
            crate::theme::built_in_theme(crate::settings::Theme::Dark, None).palette,
        )
    }
}

fn parse_hex_color(value: &str) -> Option<Color32> {
    let value = value.strip_prefix('#').unwrap_or(value);
    if value.len() != 6 {
        return None;
    }
    Some(Color32::from_rgb(
        u8::from_str_radix(&value[0..2], 16).ok()?,
        u8::from_str_radix(&value[2..4], 16).ok()?,
        u8::from_str_radix(&value[4..6], 16).ok()?,
    ))
}

fn multi_cursor_modifier_pressed(modifiers: Modifiers, configured: MultiCursorModifier) -> bool {
    match configured {
        MultiCursorModifier::Alt => modifiers.alt,
        MultiCursorModifier::Ctrl => modifiers.ctrl,
        MultiCursorModifier::Command => modifiers.command,
    }
}

const ROW_PADDING: f32 = 4.0;
const FOLD_GUTTER_WIDTH: f32 = 12.0;
const GUTTER_PADDING: f32 = 8.0;
const TEXT_PADDING: f32 = 8.0;
const OVERSCAN_ROWS: usize = 2;
const BLAME_GUTTER_WIDTH: f32 = 120.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorCommand {
    Open,
    Save,
    ToggleUndoHistory,
}

/// Editor pointer-hover outcome after applying the explicit precedence rule:
/// completion dropdown → none; diagnostic squiggle → immediate tooltip;
/// source text → deferred LSP hover; elsewhere → none.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PointerHoverPrecedence {
    Diagnostic(usize),
    SourceText(HoveredSourcePosition),
    None,
}

/// Completion dropdown has highest pointer-hover priority.
pub fn completion_blocks_pointer_hover(popup: CompletionPopupModel) -> bool {
    popup.blocks_pointer_hover()
}

/// Diagnostic underline wins over deferred LSP hover when the pointer is over the squiggle.
pub fn diagnostic_wins_over_lsp_hover(hovered_diagnostic: Option<usize>) -> bool {
    hovered_diagnostic.is_some()
}

/// True when the editor is showing a diagnostic tooltip at the pointer this frame.
pub fn diagnostic_tooltip_blocks_lsp_hover_at_pointer(diagnostic_tooltip_active: bool) -> bool {
    diagnostic_tooltip_active
}

/// Resolve which editor-owned pointer hover wins for this frame.
///
/// Diagnostic underlines take precedence over source-text LSP hover. When the pointer is
/// elsewhere, neither interaction is armed.
pub fn resolve_pointer_hover_precedence(
    hovered_diagnostic: Option<usize>,
    candidate_source_text: Option<HoveredSourcePosition>,
) -> PointerHoverPrecedence {
    match (
        diagnostic_wins_over_lsp_hover(hovered_diagnostic),
        hovered_diagnostic,
        candidate_source_text,
    ) {
        (true, Some(index), _) => PointerHoverPrecedence::Diagnostic(index),
        (_, _, Some(hover)) => PointerHoverPrecedence::SourceText(hover),
        _ => PointerHoverPrecedence::None,
    }
}

/// Apply the completion gate before editor-local diagnostic/source precedence.
pub fn resolve_pointer_hover_precedence_with_completion(
    completion_popup: CompletionPopupModel,
    hovered_diagnostic: Option<usize>,
    candidate_source_text: Option<HoveredSourcePosition>,
) -> PointerHoverPrecedence {
    if completion_blocks_pointer_hover(completion_popup) {
        PointerHoverPrecedence::None
    } else {
        resolve_pointer_hover_precedence(hovered_diagnostic, candidate_source_text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionTrigger {
    F12,
    CtrlClick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorAction {
    GoToDefinition {
        position: CursorPosition,
        source: DefinitionTrigger,
    },
    /// Ctrl+Space (Cmd+Space on macOS) while focused; key is consumed so no
    /// literal space is inserted. `app.rs` applies eligibility gates before
    /// sending `textDocument/completion`.
    RequestCompletion,
    RequestSignatureHelp,
    ToggleBookmark {
        line: usize,
    },
    NextBookmark,
    PrevBookmark,
    /// A vim `:` command that needs the app shell (`:w`, `:q`, `:wq`, `:noh`).
    VimEx(crate::vim::ExCommand),
    /// A vim `/` pattern was accepted; mirror it into the search panel.
    VimSearch(String),
}

/// Per-frame editor widget results consumed by the app shell.
pub struct EditorOutput {
    pub response: Response,
    pub command: Option<EditorCommand>,
    pub action: Option<EditorAction>,
    /// Screen-space caret anchor for completion popup positioning.
    pub completion_anchor: CompletionPopupAnchor,
    /// Screen-space bounds of the visible editor viewport (CentralPanel content).
    pub editor_viewport_rect: Option<Rect>,
    /// LSP hover popup handoff (hovered source + diagnostic flag).
    pub hover_popup: HoverPopupModel,
    /// True when the code editor widget has keyboard focus.
    pub editor_has_focus: bool,
}

#[derive(Debug)]
pub struct EditorState {
    preferred_col: Option<usize>,
    scroll_to_cursor: bool,
    request_initial_focus: bool,
    max_text_width: f32,
    last_path: Option<PathBuf>,
    fold_chord_armed: bool,
    pub desired_scroll_y: Option<f32>,
    column_anchor: Option<CursorPosition>,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            preferred_col: None,
            scroll_to_cursor: false,
            request_initial_focus: true,
            max_text_width: 0.0,
            last_path: None,
            fold_chord_armed: false,
            desired_scroll_y: None,
            column_anchor: None,
        }
    }
}

impl EditorState {
    /// Request the editor to scroll so the cursor is visible next frame.
    /// Used by the search panel after moving the cursor to a match.
    pub fn request_scroll_to_cursor(&mut self) {
        self.scroll_to_cursor = true;
    }

    /// Request keyboard focus next frame.
    pub fn request_focus(&mut self) {
        self.request_initial_focus = true;
    }

    #[cfg(test)]
    pub fn is_scroll_requested(&self) -> bool {
        self.scroll_to_cursor
    }

    #[cfg(test)]
    pub fn is_focus_requested(&self) -> bool {
        self.request_initial_focus
    }
}

pub struct EditorWidget;

impl EditorWidget {
    pub fn show(
        ui: &mut Ui,
        state: &mut EditorState,
        buffer: &mut TextBuffer,
        interaction: EditorInteraction,
        annotations: EditorAnnotations<'_>,
        presentation: EditorPresentation,
        minimap_state: Option<&mut crate::editor::minimap::MinimapState>,
        ligatures_enabled: bool,
        _ligature_renderer: Option<&mut crate::text::ligature::LigatureRenderer>,
    ) -> EditorOutput {
        let EditorInteraction {
            enabled: interactive,
            lsp_active,
            completion_popup,
        } = interaction;
        let EditorAnnotations {
            diagnostics,
            search_highlights,
            diff_hunks,
            blame_lines,
            show_blame,
            bookmarks,
        } = annotations;
        let EditorPresentation {
            font_size,
            palette,
            tab_width,
            insert_spaces,
            show_indent_guides,
            sticky_scroll,
            sticky_scroll_max_lines,
            bracket_colorization,
            bracket_matching,
            indent_guide_color,
            indent_guide_active_color,
            multi_cursor_modifier,
            vim_enabled,
            inline_diagnostics,
        } = presentation;
        buffer.set_bracket_features(bracket_colorization, bracket_matching);
        if state.last_path.as_deref() != buffer.path() {
            state.last_path = buffer.path().map(PathBuf::from);
            state.max_text_width = 0.0;
            state.preferred_col = None;
        }
        let path_id = buffer.path().map(PathBuf::from);

        let font_id = if ligatures_enabled {
            FontId::new(font_size, egui::FontFamily::Name("ligature_code".into()))
        } else {
            FontId::monospace(font_size)
        };
        let row_height = ui.fonts(|fonts| fonts.row_height(&font_id)) + ROW_PADDING;
        let layout_job = buffer.get_layout_with_palette(font_id.clone(), palette.syntax);
        let line_count = buffer.len_lines();
        let visible_lines: Vec<usize> = (0..line_count)
            .filter(|line| buffer.is_line_visible(*line))
            .collect();
        let visible_line_count = visible_lines.len().max(1);
        let number_sample = "9".repeat(line_count.max(1).to_string().len());
        let number_width = ui
            .painter()
            .layout_no_wrap(
                number_sample,
                font_id.clone(),
                ui.visuals().weak_text_color(),
            )
            .size()
            .x;
        let gutter_width = FOLD_GUTTER_WIDTH + number_width + GUTTER_PADDING * 2.0;

        let mut command = None;
        if interactive {
            command = ui.ctx().input_mut(|input| {
                if input.consume_key(Modifiers::COMMAND, Key::S) {
                    Some(EditorCommand::Save)
                } else if input.consume_key(Modifiers::COMMAND, Key::O) {
                    Some(EditorCommand::Open)
                } else if input.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::U) {
                    Some(EditorCommand::ToggleUndoHistory)
                } else {
                    None
                }
            });
        }

        // ── Minimap width reservation ────────────────────────────────────────
        // Determine whether the minimap should be shown this frame.
        // Auto-hide when the pane is too narrow, regardless of user toggle.
        let total_available_width = ui.available_width();
        let minimap_panel_width = crate::editor::minimap::MINIMAP_WIDTH as f32;
        let minimap_active = minimap_state.is_some()
            && minimap_state.as_ref().map_or(false, |m| m.visible)
            && total_available_width >= crate::editor::minimap::MINIMAP_AUTO_HIDE_WIDTH;

        let available_width = if minimap_active {
            (total_available_width - minimap_panel_width).max(100.0)
        } else {
            total_available_width
        };

        let text_color = palette.syntax.default;
        let weak_text_color = palette.semantic.muted_text;
        let current_line_color = palette.semantic.current_line;
        let editor_background = palette.semantic.editor_background;
        let gutter_background = palette.semantic.panel_background;
        let separator_color = palette.semantic.border;
        let cursor_color = ui.visuals().selection.stroke.color;
        let hover_pos = ui.input(|input| input.pointer.hover_pos());
        let mut hovered_diagnostic = None;

        let ctrl_pressed = ui.input(|input| {
            input.modifiers.ctrl || (cfg!(target_os = "macos") && input.modifiers.command)
        });
        let mut editor_action: Option<EditorAction> = None;
        let mut cursor_screen_rect = None;
        let mut candidate_hovered_source = None;
        if sticky_scroll && !buffer.sticky_lines.is_empty() {
            egui::Frame::none()
                .fill(palette.semantic.panel_background)
                .show(ui, |ui| {
                    for line in buffer.sticky_lines.clone() {
                        let text = buffer.line_text(line).unwrap_or_default();
                        let start = buffer
                            .position_to_byte_index(CursorPosition { line, col: 0 })
                            .unwrap_or(0);
                        let end = start + text.len();
                        let mut line_job = egui::text::LayoutJob::default();
                        for section in &layout_job.sections {
                            if section.byte_range.end > start && section.byte_range.start < end {
                                let from = section.byte_range.start.max(start) - start;
                                let to = section.byte_range.end.min(end) - start;
                                if from < to {
                                    line_job.append(&text[from..to], 0.0, section.format.clone());
                                }
                            }
                        }
                        ui.label(line_job);
                    }
                    ui.separator();
                });
        }
        // Constrain the scroll area to the editor width (leaves room for minimap on the right).
        let editor_available_height = ui.available_height();
        let scroll_output = ui
            .allocate_ui(vec2(available_width, editor_available_height), |ui| {
                ScrollArea::both()
                    .id_source(("blue_ide_editor_scroll", path_id.as_ref()))
                    .auto_shrink([false, false])
                    .show_viewport(ui, |ui, viewport| {
                        if (viewport.top() - buffer.last_scroll_y).abs() > 1.0 {
                            buffer.last_scroll_y = viewport.top();
                            let mut first_row = 0;
                            let top_y = viewport.top().max(0.0);
                            for (row_index, &line) in visible_lines.iter().enumerate() {
                                let line_y = line_to_y(line, buffer, row_height, lsp_active);
                                let height = row_height
                                    + if lsp_active && buffer.has_code_lens(line) {
                                        18.0
                                    } else {
                                        0.0
                                    };
                                if line_y + height >= top_y {
                                    first_row = row_index;
                                    break;
                                }
                            }
                            let first = visible_lines.get(first_row).copied().unwrap_or(0);
                            if sticky_scroll {
                                buffer.update_sticky_lines(first, sticky_scroll_max_lines);
                            } else {
                                buffer.sticky_lines.clear();
                            }
                        }
                        let content_left = ui.max_rect().left();
                        let content_top = ui.max_rect().top();
                        let total_height = line_to_y(line_count, buffer, row_height, lsp_active);
                        let content_height = total_height.max(row_height);
                        let blame_gutter_width = if show_blame { BLAME_GUTTER_WIDTH } else { 0.0 };
                        let content_width = (gutter_width
                            + blame_gutter_width
                            + TEXT_PADDING * 2.0
                            + state.max_text_width)
                            .max(available_width);
                        ui.set_min_size(vec2(content_width, content_height));

                        let content_rect = Rect::from_min_size(
                            pos2(content_left, content_top),
                            vec2(content_width, content_height),
                        );
                        let sense = if interactive {
                            Sense::click_and_drag()
                        } else {
                            Sense::hover()
                        };
                        let mut response = ui.interact(
                            content_rect,
                            ui.make_persistent_id(("blue_ide_editor", path_id.as_ref())),
                            sense,
                        );
                        response.widget_info(|| {
                            WidgetInfo::labeled(WidgetType::TextEdit, "Code editor")
                        });

                        if state.request_initial_focus && interactive {
                            response.request_focus();
                            state.request_initial_focus = false;
                        }
                        if !interactive && response.has_focus() {
                            response.surrender_focus();
                        }

                        let text_left =
                            content_left + gutter_width + blame_gutter_width + TEXT_PADDING;
                        if interactive && response.clicked() {
                            if let Some(pointer) = response.interact_pointer_pos() {
                                let clicked_row = y_to_visible_row(
                                    pointer.y,
                                    content_top,
                                    buffer,
                                    row_height,
                                    &visible_lines,
                                    lsp_active,
                                );
                                let clicked_line = visible_lines
                                    .get(clicked_row)
                                    .copied()
                                    .unwrap_or_else(|| line_count.saturating_sub(1));
                                let visible_left = content_left + viewport.left();

                                let is_in_gutter =
                                    pointer.x < visible_left + gutter_width + blame_gutter_width;
                                let is_in_fold_gutter =
                                    pointer.x < visible_left + FOLD_GUTTER_WIDTH;
                                let is_within_y = pointer.y >= content_top
                                    && pointer.y < content_top + content_height;

                                let consumed_fold_click = if is_in_fold_gutter && is_within_y {
                                    if buffer.toggle_fold(clicked_line) {
                                        state.scroll_to_cursor = true;
                                        response.mark_changed();
                                    }
                                    response.request_focus();
                                    true
                                } else {
                                    false
                                };

                                if !consumed_fold_click {
                                    if is_in_gutter && is_within_y {
                                        editor_action = Some(EditorAction::ToggleBookmark {
                                            line: clicked_line,
                                        });
                                        response.mark_changed();
                                        response.request_focus();
                                    } else {
                                        let line =
                                            buffer.line_text(clicked_line).unwrap_or_default();
                                        let galley = ui.painter().layout_no_wrap(
                                            line.clone(),
                                            font_id.clone(),
                                            text_color,
                                        );
                                        let is_over_text = pointer.x >= text_left
                                            && pointer.x <= text_left + galley.size().x;

                                        let is_ctrl_click = ctrl_pressed
                                            && !is_in_gutter
                                            && is_within_y
                                            && is_over_text
                                            && response.clicked_by(egui::PointerButton::Primary);

                                        let col = if is_in_gutter {
                                            0
                                        } else {
                                            galley
                                                .cursor_from_pos(vec2(
                                                    pointer.x - text_left,
                                                    row_height * 0.5,
                                                ))
                                                .ccursor
                                                .index
                                        };

                                        let modifiers = ui.input(|input| input.modifiers);
                                        let alt_pressed = modifiers.alt;
                                        let shift_pressed = modifiers.shift;
                                        let multi_cursor_pressed = multi_cursor_modifier_pressed(
                                            modifiers,
                                            multi_cursor_modifier,
                                        );
                                        if alt_pressed && shift_pressed {
                                            let anchor = buffer.primary_cursor().head;
                                            buffer.set_column_selection(
                                                anchor,
                                                CursorPosition {
                                                    line: clicked_line,
                                                    col,
                                                },
                                            );
                                            state.scroll_to_cursor = true;
                                            response.request_focus();
                                        } else if multi_cursor_pressed {
                                            if let Some(index) =
                                                buffer.cursors.iter().position(|cursor| {
                                                    cursor.head.line == clicked_line
                                                        && cursor.head.col == col
                                                })
                                            {
                                                buffer.remove_cursor(index);
                                            } else {
                                                buffer.add_cursor(clicked_line, col);
                                            }
                                            state.scroll_to_cursor = true;
                                            response.request_focus();
                                        } else if ctrl_pressed {
                                            if is_ctrl_click && lsp_active {
                                                editor_action =
                                                    Some(EditorAction::GoToDefinition {
                                                        position: CursorPosition {
                                                            line: clicked_line,
                                                            col,
                                                        },
                                                        source: DefinitionTrigger::CtrlClick,
                                                    });
                                            }
                                            response.request_focus();
                                        } else {
                                            buffer.set_cursor(CursorPosition {
                                                line: clicked_line,
                                                col,
                                            });
                                            state.preferred_col = None;
                                            state.scroll_to_cursor = true;
                                            response.request_focus();
                                        }
                                    }
                                }
                            }
                        }

                        let column_modifiers =
                            ui.input(|input| input.modifiers.alt && input.modifiers.shift);
                        if interactive
                            && column_modifiers
                            && (response.drag_started() || response.dragged())
                        {
                            if let Some(pointer) = response.interact_pointer_pos() {
                                let visible_row = y_to_visible_row(
                                    pointer.y,
                                    content_top,
                                    buffer,
                                    row_height,
                                    &visible_lines,
                                    lsp_active,
                                );
                                let line = visible_lines.get(visible_row).copied().unwrap_or(0);
                                let text = buffer.line_text(line).unwrap_or_default();
                                let galley =
                                    ui.painter()
                                        .layout_no_wrap(text, font_id.clone(), text_color);
                                let col = galley
                                    .cursor_from_pos(vec2(
                                        (pointer.x - text_left).max(0.0),
                                        row_height * 0.5,
                                    ))
                                    .ccursor
                                    .index;
                                let head = CursorPosition { line, col };
                                let anchor = *state.column_anchor.get_or_insert(head);
                                buffer.set_column_selection(anchor, head);
                                response.mark_changed();
                            }
                        }
                        if response.drag_stopped() {
                            buffer.finish_column_selection();
                            state.column_anchor = None;
                        }

                        if interactive && response.has_focus() {
                            let f12_pressed = ui
                                .ctx()
                                .input_mut(|input| input.consume_key(Modifiers::NONE, Key::F12));
                            if f12_pressed && lsp_active {
                                editor_action = Some(EditorAction::GoToDefinition {
                                    position: buffer.cursor(),
                                    source: DefinitionTrigger::F12,
                                });
                            }
                        }

                        let revision_before_input = buffer.revision();
                        if interactive && response.has_focus() {
                            let completion_requested = ui.ctx().input_mut(|input| {
                                input.consume_key(Modifiers::COMMAND, Key::Space)
                            });
                            if completion_requested && editor_action.is_none() {
                                editor_action = Some(EditorAction::RequestCompletion);
                            }
                            if let Some(action) = handle_keyboard_input(
                                ui,
                                state,
                                buffer,
                                completion_popup.open,
                                tab_width,
                                insert_spaces,
                                lsp_active,
                                vim_enabled && !buffer.large_file_mode,
                            ) {
                                if editor_action.is_none() {
                                    editor_action = Some(action);
                                }
                            }
                        }
                        if buffer.revision() != revision_before_input {
                            response.mark_changed();
                        }

                        let visible_screen_rect = Rect::from_min_size(
                            pos2(content_left + viewport.left(), content_top + viewport.top()),
                            viewport.size(),
                        );
                        let text_clip = Rect::from_min_max(
                            pos2(
                                visible_screen_rect.left() + gutter_width + blame_gutter_width,
                                visible_screen_rect.top(),
                            ),
                            visible_screen_rect.max,
                        );
                        let painter = ui.painter().with_clip_rect(text_clip);
                        painter.rect_filled(content_rect, 0.0, editor_background);

                        let rows = visible_line_range_with_lens(
                            viewport,
                            row_height,
                            &visible_lines,
                            buffer,
                            OVERSCAN_ROWS,
                            lsp_active,
                        );
                        let mut cursor_rect = None;
                        let mut caret_rects: Vec<(Rect, bool)> = Vec::new();
                        for visible_row in rows {
                            let Some(&line_index) = visible_lines.get(visible_row) else {
                                continue;
                            };
                            let y =
                                content_top + line_to_y(line_index, buffer, row_height, lsp_active);
                            let shift = if lsp_active && buffer.has_code_lens(line_index) {
                                18.0
                            } else {
                                0.0
                            };

                            // Render code lenses if active and available
                            if lsp_active && buffer.has_code_lens(line_index) {
                                if let Some(lens) =
                                    buffer.code_lenses.iter().find(|l| l.line == line_index)
                                {
                                    let lens_text = lens
                                        .entries
                                        .iter()
                                        .map(|e| &e.title)
                                        .cloned()
                                        .collect::<Vec<_>>()
                                        .join(" | ");
                                    if !lens_text.is_empty() {
                                        let lens_font = FontId::monospace(font_size * 0.85);
                                        let lens_color = weak_text_color;
                                        let lens_galley = ui.fonts(|f| {
                                            f.layout_no_wrap(lens_text, lens_font, lens_color)
                                        });
                                        painter.galley(
                                            pos2(
                                                text_left,
                                                y + (18.0 - lens_galley.size().y) * 0.5,
                                            ),
                                            lens_galley,
                                            Color32::TRANSPARENT,
                                        );
                                    }
                                }
                            }

                            if line_index == buffer.cursor().line {
                                painter.rect_filled(
                                    Rect::from_min_size(
                                        pos2(content_left, y + shift),
                                        vec2(content_width, row_height),
                                    ),
                                    0.0,
                                    current_line_color,
                                );
                            }

                            let line = buffer.line_text(line_index).unwrap_or_default();
                            // Get full layout and extract this line's syntax highlighting
                            // Build a line-specific layout from the full document layout
                            let mut line_job = egui::text::LayoutJob::default();
                            let line_start_byte = buffer
                                .position_to_byte_index(CursorPosition {
                                    line: line_index,
                                    col: 0,
                                })
                                .unwrap_or(0);
                            let line_end_byte = line_start_byte + line.len();

                            // Check if this line has RTL characters in a comment
                            let has_rtl_in_comment = split_at_comment(&line)
                                .map(|(_, comment)| contains_rtl(comment))
                                .unwrap_or(false);

                            if has_rtl_in_comment {
                                // Use RTL-aware layout job for lines with RTL in comments
                                line_job = create_layout_job_for_line(
                                    &line,
                                    font_id.clone(),
                                    text_color,
                                                             );
                            } else {
                                // Normal syntax highlighting
                                for section in &layout_job.sections {
                                    let sec_start = section.byte_range.start;
                                    let sec_end = section.byte_range.end;

                                    // Check if section overlaps with this line
                                    if sec_end > line_start_byte && sec_start < line_end_byte {
                                        let start_in_line = sec_start.saturating_sub(line_start_byte);
                                        let end_in_line = (sec_end - line_start_byte).min(line.len());
                                        if start_in_line < end_in_line {
                                            line_job.append(
                                                &line[start_in_line..end_in_line],
                                                0.0,
                                                section.format.clone(),
                                            );
                                        }
                                    }
                                }

                                // Fallback: if no sections, render plain
                                if line_job.sections.is_empty() {
                                    line_job.append(
                                        &line,
                                        0.0,
                                        egui::TextFormat {
                                            font_id: font_id.clone(),
                                            color: text_color,
                                            ..Default::default()
                                        },
                                    );
                                }
                            }
                            if let Some(fold) = buffer.fold_starting_at(line_index) {
                                if buffer.fold_state.collapsed.contains(&fold.start_line) {
                                    line_job.append(
                                        &format!(
                                            "  {{ \u{2026} {} lines }}",
                                            fold.end_line - fold.start_line
                                        ),
                                        0.0,
                                        egui::TextFormat {
                                            font_id: font_id.clone(),
                                            color: weak_text_color,
                                            italics: true,
                                            ..Default::default()
                                        },
                                    );
                                }
                            }

                            let galley = ui.fonts(|f| f.layout_job(line_job));
                            if galley.size().x > state.max_text_width {
                                state.max_text_width = galley.size().x;
                                ui.ctx().request_repaint();
                            }
                            let text_y = y + shift + (row_height - galley.size().y) * 0.5;
                            let indent = line
                                .chars()
                                .take_while(|ch| *ch == ' ' || *ch == '\t')
                                .fold(0usize, |count, ch| {
                                    count + if ch == '\t' { tab_width.max(1) } else { 1 }
                                });
                            let char_width = ui.fonts(|fonts| fonts.glyph_width(&font_id, ' '));
                            let indent_levels = if show_indent_guides {
                                indent / tab_width.max(1)
                            } else {
                                0
                            };
                            for level in 1..=indent_levels {
                                let active = line_index == buffer.primary_cursor().head.line
                                    && level == indent_levels;
                                let color = if active {
                                    indent_guide_active_color
                                } else {
                                    indent_guide_color
                                };
                                let x =
                                    text_left + level as f32 * tab_width.max(1) as f32 * char_width;
                                painter.line_segment(
                                    [pos2(x, y + shift), pos2(x, y + shift + row_height)],
                                    Stroke::new(1.0, color),
                                );
                            }
                            if let Some((open_line, open_col, close_line, close_col)) =
                                buffer.bracket_match
                            {
                                for (match_line, match_col) in
                                    [(open_line, open_col), (close_line, close_col)]
                                {
                                    if match_line == line_index {
                                        let left = galley
                                            .pos_from_ccursor(egui::text::CCursor {
                                                index: match_col,
                                                prefer_next_row: false,
                                            })
                                            .left();
                                        let right = galley
                                            .pos_from_ccursor(egui::text::CCursor {
                                                index: match_col + 1,
                                                prefer_next_row: false,
                                            })
                                            .left();
                                        painter.rect_filled(
                                            Rect::from_min_max(
                                                pos2(text_left + left, text_y),
                                                pos2(text_left + right, text_y + galley.size().y),
                                            ),
                                            0.0,
                                            Color32::from_rgba_unmultiplied(
                                                cursor_color.r(),
                                                cursor_color.g(),
                                                cursor_color.b(),
                                                60,
                                            ),
                                        );
                                    }
                                }
                            }
                            for cursor in &buffer.cursors {
                                let (start, end) = cursor.normalize();
                                if line_index >= start.line
                                    && line_index <= end.line
                                    && start != end
                                {
                                    let selection_start = if line_index == start.line {
                                        start.col
                                    } else {
                                        0
                                    };
                                    let selection_end = if line_index == end.line {
                                        end.col
                                    } else {
                                        line.chars().count()
                                    };
                                    let left = galley
                                        .pos_from_ccursor(egui::text::CCursor {
                                            index: selection_start,
                                            prefer_next_row: false,
                                        })
                                        .left();
                                    let right = galley
                                        .pos_from_ccursor(egui::text::CCursor {
                                            index: selection_end,
                                            prefer_next_row: false,
                                        })
                                        .left();
                                    painter.rect_filled(
                                        Rect::from_min_max(
                                            pos2(text_left + left, text_y),
                                            pos2(
                                                (text_left + right).max(text_left + left + 2.0),
                                                text_y + galley.size().y,
                                            ),
                                        ),
                                        0.0,
                                        palette.semantic.selection,
                                    );
                                }
                            }
                            if let Some(column) = &buffer.column_selection {
                                let first_line = column.anchor_line.min(column.head_line);
                                let last_line = column.anchor_line.max(column.head_line);
                                if line_index >= first_line && line_index <= last_line {
                                    let first_col = column.anchor_col.min(column.head_col) as f32;
                                    let last_col = column.anchor_col.max(column.head_col) as f32;
                                    let width = ui.fonts(|fonts| fonts.glyph_width(&font_id, ' '));
                                    painter.rect_filled(
                                        Rect::from_min_max(
                                            pos2(text_left + first_col * width, y + shift),
                                            pos2(
                                                text_left + last_col * width,
                                                y + shift + row_height,
                                            ),
                                        ),
                                        0.0,
                                        Color32::from_rgba_unmultiplied(
                                            cursor_color.r(),
                                            cursor_color.g(),
                                            cursor_color.b(),
                                            45,
                                        ),
                                    );
                                }
                            }
                            if let Some((open_line, open_col, close_line, close_col)) =
                                buffer.bracket_match
                            {
                                for (match_line, match_col) in
                                    [(open_line, open_col), (close_line, close_col)]
                                {
                                    if match_line == line_index {
                                        let left = galley
                                            .pos_from_ccursor(egui::text::CCursor {
                                                index: match_col,
                                                prefer_next_row: false,
                                            })
                                            .left();
                                        let right = galley
                                            .pos_from_ccursor(egui::text::CCursor {
                                                index: match_col + 1,
                                                prefer_next_row: false,
                                            })
                                            .left();
                                        painter.line_segment(
                                            [
                                                pos2(text_left + left, text_y + galley.size().y),
                                                pos2(text_left + right, text_y + galley.size().y),
                                            ],
                                            Stroke::new(2.0, cursor_color),
                                        );
                                    }
                                }
                            }
                            let mut rendered = false;
                            if has_rtl_in_comment {
                                if let Some((code_part, comment_part)) = split_at_comment(&line) {
                                    if !code_part.trim().is_empty() {
                                        let job_code = crate::rtl_text::create_layout_job_for_code(
                                            code_part,
                                            font_id.clone(),
                                            text_color,
                                        );
                                        let job_comment = crate::rtl_text::create_layout_job_for_comment(
                                            comment_part,
                                            font_id.clone(),
                                            weak_text_color,
                                        );
                                        let galley_code = ui.fonts(|f| f.layout_job(job_code));
                                        let galley_comment = ui.fonts(|f| f.layout_job(job_comment));
                                        painter.galley(
                                            pos2(text_left, text_y),
                                            galley_code.clone(),
                                            Color32::TRANSPARENT,
                                        );
                                        painter.galley(
                                            pos2(text_left + galley_code.size().x, text_y),
                                            galley_comment,
                                            Color32::TRANSPARENT,
                                        );
                                        rendered = true;
                                    }
                                }
                            }
                            if !rendered {
                                painter.galley(
                                    pos2(text_left, text_y),
                                    galley.clone(),
                                    Color32::TRANSPARENT,
                                );
                            }

                            for hint in buffer
                                .inlay_hints
                                .iter()
                                .filter(|hint| hint.position.line as usize == line_index)
                            {
                                let col = decode_utf16_column(&line, hint.position.utf16_col);
                                let x = galley
                                    .pos_from_ccursor(egui::text::CCursor {
                                        index: col,
                                        prefer_next_row: false,
                                    })
                                    .left();
                                let mut label = hint.display_text();
                                if hint.padding_left {
                                    label.insert(0, ' ');
                                }
                                if hint.padding_right {
                                    label.push(' ');
                                }
                                let hint_galley = ui.painter().layout_no_wrap(
                                    label,
                                    FontId::monospace(font_size * 0.85),
                                    weak_text_color,
                                );
                                painter.rect_filled(
                                    Rect::from_min_size(
                                        pos2(text_left + x, text_y),
                                        hint_galley.size(),
                                    ),
                                    2.0,
                                    palette.semantic.panel_background,
                                );
                                painter.galley(
                                    pos2(text_left + x, text_y),
                                    hint_galley,
                                    Color32::TRANSPARENT,
                                );
                            }

                            // ---- Search highlights ----
                            //
                            // Paint highlight rectangles BEFORE the diagnostic underlines
                            // so that underlines remain visible on top.
                            //
                            // `line_start_byte` and `line_end_byte` are already computed above.
                            for sh in search_highlights {
                                // Clip the highlight to this line.
                                if sh.byte_range.end <= line_start_byte
                                    || sh.byte_range.start >= line_end_byte
                                {
                                    continue;
                                }
                                let hl_start = sh.byte_range.start.saturating_sub(line_start_byte);
                                let hl_end = sh
                                    .byte_range
                                    .end
                                    .saturating_sub(line_start_byte)
                                    .min(line.len());
                                if hl_start >= hl_end && hl_end > 0 {
                                    continue;
                                }
                                // Convert byte offsets to char indices within the line string.
                                let char_start = if line.is_char_boundary(hl_start) {
                                    line[..hl_start].chars().count()
                                } else {
                                    line[..hl_start.min(line.len())].char_indices().count()
                                };
                                let char_end =
                                    if hl_end <= line.len() && line.is_char_boundary(hl_end) {
                                        line[..hl_end].chars().count()
                                    } else {
                                        line[..hl_end.min(line.len())].chars().count()
                                    };
                                // Ask the galley for pixel positions.
                                let x_start_rect = galley.pos_from_ccursor(egui::text::CCursor {
                                    index: char_start,
                                    prefer_next_row: false,
                                });
                                let x_end_rect = galley.pos_from_ccursor(egui::text::CCursor {
                                    index: char_end.max(char_start + 1),
                                    prefer_next_row: false,
                                });
                                let px_start = text_left + x_start_rect.left();
                                let px_end = (text_left + x_end_rect.left()).max(px_start + 3.0);
                                let highlight_rect = Rect::from_min_max(
                                    pos2(px_start, text_y),
                                    pos2(px_end, text_y + galley.size().y),
                                );
                                let color = if sh.is_active {
                                    palette.semantic.active_search_match
                                } else {
                                    palette.semantic.search_match
                                };
                                painter.rect_filled(highlight_rect, 2.0, color);
                            }

                            // Draw subtle hover underline if active
                            let mut line_underline_range = None;
                            if interactive && lsp_active && ctrl_pressed {
                                if let Some(pointer) = hover_pos {
                                    let is_in_gutter = pointer.x
                                        < visible_screen_rect.left()
                                            + gutter_width
                                            + blame_gutter_width;
                                    let is_within_y = pointer.y >= y + shift
                                        && pointer.y < y + shift + row_height;
                                    if !is_in_gutter && is_within_y {
                                        let is_over_text = pointer.x >= text_left
                                            && pointer.x <= text_left + galley.size().x;
                                        if is_over_text {
                                            let col = galley
                                                .cursor_from_pos(vec2(
                                                    pointer.x - text_left,
                                                    row_height * 0.5,
                                                ))
                                                .ccursor
                                                .index;
                                            let word_range = find_word_range(&line, col);
                                            if !word_range.is_empty() {
                                                line_underline_range = Some(word_range);
                                                ui.ctx().set_cursor_icon(
                                                    egui::CursorIcon::PointingHand,
                                                );
                                            }
                                        }
                                    }
                                }
                            }

                            if let Some(word_range) = line_underline_range {
                                let x_start_rect = galley.pos_from_ccursor(egui::text::CCursor {
                                    index: word_range.start,
                                    prefer_next_row: false,
                                });
                                let x_end_rect = galley.pos_from_ccursor(egui::text::CCursor {
                                    index: word_range.end,
                                    prefer_next_row: false,
                                });
                                let px_start = text_left + x_start_rect.left();
                                let px_end = text_left + x_end_rect.left();
                                let underline_y = y + shift + row_height - 2.0;
                                let stroke_color = ui.visuals().selection.stroke.color;
                                painter.line_segment(
                                    [pos2(px_start, underline_y), pos2(px_end, underline_y)],
                                    Stroke::new(1.0, stroke_color),
                                );
                            }

                            for (diagnostic_index, diagnostic) in diagnostics.iter().enumerate() {
                                let Some((start, end)) = lsp_utf16_range_char_span_on_line(
                                    line_index,
                                    &line,
                                    diagnostic.line_start,
                                    diagnostic.col_start,
                                    diagnostic.line_end,
                                    diagnostic.col_end,
                                ) else {
                                    continue;
                                };
                                let start_rect = galley.pos_from_ccursor(egui::text::CCursor {
                                    index: start,
                                    prefer_next_row: false,
                                });
                                let end_rect = galley.pos_from_ccursor(egui::text::CCursor {
                                    index: end,
                                    prefer_next_row: false,
                                });
                                let x_start = text_left + start_rect.left();
                                let line_right = text_left + galley.size().x;
                                let mut x_end = (text_left + end_rect.left()).min(line_right);
                                if x_end <= x_start {
                                    x_end = x_start + 4.0;
                                }
                                let underline_y = y + shift + row_height - 2.0;
                                draw_wavy_underline(
                                    &painter,
                                    x_start,
                                    x_end,
                                    underline_y,
                                    diagnostic_color(diagnostic.severity, palette.semantic),
                                );

                                let hit_rect = Rect::from_min_max(
                                    pos2(x_start, y + shift),
                                    pos2(x_end.max(x_start + 4.0), y + shift + row_height),
                                );
                                if !completion_popup.open
                                    && hover_pos.is_some_and(|position| hit_rect.contains(position))
                                {
                                    hovered_diagnostic = Some(diagnostic_index);
                                }
                            }

                            // Zed-style inline diagnostic message at end of line.
                            if inline_diagnostics {
                                let on_line = diagnostics.iter().find(|diagnostic| {
                                    lsp_utf16_range_char_span_on_line(
                                        line_index,
                                        &line,
                                        diagnostic.line_start,
                                        diagnostic.col_start,
                                        diagnostic.line_end,
                                        diagnostic.col_end,
                                    )
                                    .is_some()
                                });
                                if let Some(first) = on_line {
                                    let message = first.message.lines().next().unwrap_or("").trim();
                                    if !message.is_empty() {
                                        const MAX_INLINE_MESSAGE_CHARS: usize = 120;
                                        let text: String = if message.chars().count()
                                            > MAX_INLINE_MESSAGE_CHARS
                                        {
                                            message
                                                .chars()
                                                .take(MAX_INLINE_MESSAGE_CHARS)
                                                .collect()
                                        } else {
                                            message.to_owned()
                                        };
                                        let line_right = text_left + galley.size().x;
                                        painter.text(
                                            pos2(
                                                line_right + 24.0,
                                                y + shift + row_height * 0.5,
                                            ),
                                            Align2::LEFT_CENTER,
                                            format!("● {text}"),
                                            FontId::monospace(font_size * 0.85),
                                            diagnostic_color(first.severity, palette.semantic),
                                        );
                                    }
                                }
                            }

                            for (index, cursor) in buffer.cursors.iter().enumerate() {
                                if cursor.head.line == line_index {
                                    let local = galley.pos_from_ccursor(egui::text::CCursor {
                                        index: cursor.head.col,
                                        prefer_next_row: false,
                                    });
                                    let rect = local.translate(vec2(text_left, text_y));
                                    caret_rects.push((rect, index == buffer.primary));
                                    if index == buffer.primary {
                                        cursor_rect = Some(rect);
                                    }
                                }
                            }
                        }

                        // Several key-repeat events can move the cursor farther than the
                        // overscan window in one frame. Layout only that off-screen line
                        // so scroll_to_rect still has an exact glyph-aware target.
                        if cursor_rect.is_none() {
                            let cursor = buffer.cursor();
                            let line = buffer.line_text(cursor.line).unwrap_or_default();
                            let galley = painter.layout_no_wrap(line, font_id.clone(), text_color);
                            let text_y = content_top
                                + visible_lines
                                    .iter()
                                    .position(|line| *line == cursor.line)
                                    .unwrap_or(0) as f32
                                    * row_height
                                + (row_height - galley.size().y) * 0.5;
                            let local_cursor = galley.pos_from_ccursor(egui::text::CCursor {
                                index: cursor.col,
                                prefer_next_row: false,
                            });
                            cursor_rect = Some(local_cursor.translate(vec2(text_left, text_y)));
                        }

                        if response.has_focus() {
                            ui.ctx().request_repaint_after(Duration::from_millis(500));
                            let show_cursor =
                                ((ui.input(|input| input.time) * 2.0) as u64).is_multiple_of(2);
                            if show_cursor {
                                // Vim normal/visual modes use a block cursor
                                // (Zed's vim mode look); insert keeps the bar.
                                let vim_block = vim_enabled
                                    && matches!(
                                        buffer.vim.mode,
                                        crate::vim::VimMode::Normal
                                            | crate::vim::VimMode::Visual
                                            | crate::vim::VimMode::VisualLine
                                    );
                                for (rect, primary) in &caret_rects {
                                    let color = if *primary {
                                        cursor_color
                                    } else {
                                        Color32::from_rgba_unmultiplied(
                                            cursor_color.r(),
                                            cursor_color.g(),
                                            cursor_color.b(),
                                            180,
                                        )
                                    };
                                    if vim_block {
                                        let width = if rect.width() > 0.5 {
                                            rect.width().max(4.0)
                                        } else {
                                            // End-of-line: half-width block.
                                            row_height * 0.45
                                        };
                                        let block = Rect::from_min_size(
                                            rect.min,
                                            vec2(width, row_height),
                                        );
                                        painter.rect_filled(block, 1.0, color);
                                    } else {
                                        painter.line_segment(
                                            [rect.left_top(), rect.left_bottom()],
                                            Stroke::new(if *primary { 1.5 } else { 1.0 }, color),
                                        );
                                    }
                                }
                            }
                        }

                        if state.scroll_to_cursor {
                            if let Some(desired_y) = state.desired_scroll_y.take() {
                                // Scroll to the desired Y position from minimap click
                                let reveal = Rect::from_min_max(
                                    pos2(content_left, desired_y),
                                    pos2(
                                        content_left + content_width,
                                        desired_y + viewport.height(),
                                    ),
                                );
                                ui.scroll_to_rect(reveal, Some(Align::Center));
                            } else if let Some(rect) = cursor_rect {
                                // Normal scroll to cursor behavior
                                let reveal = Rect::from_min_max(
                                    pos2(rect.left() - TEXT_PADDING, rect.top() - row_height * 2.0),
                                    pos2(
                                        rect.right() + TEXT_PADDING,
                                        rect.bottom() + row_height * 2.0,
                                    ),
                                );
                                ui.scroll_to_rect(reveal, Some(Align::Center));
                            }
                            state.scroll_to_cursor = false;
                        }

                        if interactive && !diagnostic_wins_over_lsp_hover(hovered_diagnostic) {
                            if let Some(pointer) = hover_pos {
                                let is_in_gutter = pointer.x
                                    < visible_screen_rect.left()
                                        + gutter_width
                                        + blame_gutter_width;
                                if !is_in_gutter
                                    && text_clip.contains(pointer)
                                    && pointer.x >= text_left
                                {
                                    let line = (((pointer.y - content_top) / row_height).floor()
                                        as isize)
                                        .clamp(0, visible_line_count.saturating_sub(1) as isize)
                                        as usize;
                                    let line = visible_lines
                                        .get(line)
                                        .copied()
                                        .unwrap_or_else(|| line_count.saturating_sub(1));
                                    let line_text = buffer.line_text(line).unwrap_or_default();
                                    let galley = ui.painter().layout_no_wrap(
                                        line_text.clone(),
                                        font_id.clone(),
                                        text_color,
                                    );
                                    let text_right = text_left + galley.size().x;
                                    let is_over_source = pointer.x <= text_right;
                                    if is_over_source {
                                        let col = galley
                                            .cursor_from_pos(vec2(
                                                pointer.x - text_left,
                                                row_height * 0.5,
                                            ))
                                            .ccursor
                                            .index;
                                        let (start_col, end_col) =
                                            hovered_token_col_range(&line_text, col);
                                        let start_rect =
                                            galley.pos_from_ccursor(egui::text::CCursor {
                                                index: start_col,
                                                prefer_next_row: false,
                                            });
                                        let end_rect =
                                            galley.pos_from_ccursor(egui::text::CCursor {
                                                index: end_col,
                                                prefer_next_row: false,
                                            });
                                        let visible_row = visible_lines
                                            .iter()
                                            .position(|real_line| *real_line == line)
                                            .unwrap_or(0);
                                        let y = content_top + visible_row as f32 * row_height;
                                        let x_start = text_left + start_rect.left();
                                        let mut x_end = text_left + end_rect.left();
                                        if x_end <= x_start {
                                            x_end = x_start + 4.0;
                                        }
                                        let token_rect = Rect::from_min_max(
                                            pos2(x_start, y),
                                            pos2(x_end, y + row_height),
                                        );
                                        candidate_hovered_source = Some(HoveredSourcePosition {
                                            position: CursorPosition { line, col },
                                            token_rect,
                                        });
                                    }
                                }
                            }
                        }

                        // Paint git overlays inside the scroll viewport so they scroll with
                        // the content and are clipped to the visible area.
                        let gutter_rect = Rect::from_min_max(
                            pos2(content_left, content_top),
                            pos2(
                                content_left + gutter_width + blame_gutter_width,
                                content_top + content_height,
                            ),
                        );
                        let gutter_painter = ui.painter().with_clip_rect(gutter_rect);
                        if show_blame {
                            let blame_rect = Rect::from_min_max(
                                pos2(content_left + gutter_width, content_top),
                                pos2(
                                    content_left + gutter_width + blame_gutter_width,
                                    content_top + content_height,
                                ),
                            );
                            let visible_rows = (viewport.height() / row_height).ceil() as usize
                                + OVERSCAN_ROWS * 2;
                            crate::git::render_blame_gutter(
                                &gutter_painter,
                                blame_rect,
                                blame_lines,
                                viewport.top().max(0.0),
                                row_height,
                                visible_rows,
                            );
                        } else {
                            let gutter_x = content_left + gutter_width;
                            crate::git::render_diff_gutters(
                                &gutter_painter,
                                diff_hunks,
                                gutter_x,
                                pos2(text_left, content_top),
                                row_height,
                            );
                        }

                        cursor_screen_rect = cursor_rect;
                        response
                    })
            })
            .inner; // close allocate_ui — extract ScrollOutput from InnerResponse

        let scroll_offset = scroll_output.state.offset;
        let editor_origin = scroll_output.inner.rect.min;

        // ── Minimap rendering ─────────────────────────────────────────────────
        if minimap_active {
            if let Some(minimap) = minimap_state {
                let minimap_panel_w = crate::editor::minimap::MINIMAP_WIDTH as f32;
                let panel_top = scroll_output.inner_rect.top();
                let panel_height = scroll_output.inner_rect.height();

                // Position the minimap to the right of the editor scroll area.
                let minimap_rect = Rect::from_min_size(
                    pos2(scroll_output.inner_rect.right(), panel_top),
                    vec2(minimap_panel_w, panel_height),
                );

                // Rebuild texture if buffer content changed.
                // pane_id is not available here so we use the scroll-area id hash as
                // a stable discriminant (same effect as a real pane id for texture naming).
                let pane_discriminant = ui
                    .make_persistent_id(("minimap_pane", path_id.as_ref()))
                    .value();
                minimap.rebuild_if_needed(ui.ctx(), buffer, palette, pane_discriminant);

                let visible_line_count_for_minimap = visible_lines.len().max(1);
                // Clone the painter (cheap — arc + clip-rect) so we can pass both
                // &Painter and &mut Ui into minimap.render() without double-borrow.
                let painter = ui.painter().clone();
                let new_scroll = minimap.render(
                    &painter,
                    minimap_rect,
                    scroll_offset.y,
                    panel_height,
                    visible_line_count_for_minimap,
                    row_height,
                    diagnostics,
                    diff_hunks,
                    buffer.cursor().line,
                    ui,
                    palette,
                    path_id.as_ref(),
                );
                if let Some(target_y) = new_scroll {
                    state.desired_scroll_y = Some(target_y);
                    state.scroll_to_cursor = true;
                }
            }
        }

        if let Some(rect) = cursor_screen_rect {
            cursor_screen_rect = Some(Rect::from_min_size(
                editor_origin + rect.min.to_vec2() - scroll_offset,
                rect.size(),
            ));
        }

        // ── Vim command line (`:` / `/`) overlay ────────────────────────────
        if vim_enabled
            && matches!(
                buffer.vim.mode,
                crate::vim::VimMode::Command | crate::vim::VimMode::Search
            )
        {
            let inner = scroll_output.inner_rect;
            let cmdline_rect = Rect::from_min_size(
                pos2(inner.left() + 8.0, inner.bottom() - 28.0),
                vec2(inner.width().min(480.0).max(120.0), 24.0),
            );
            let painter = ui.painter();
            painter.rect_filled(cmdline_rect, 3.0, palette.semantic.elevated_background);
            painter.rect_stroke(cmdline_rect, 3.0, Stroke::new(1.0, palette.semantic.border));
            painter.text(
                cmdline_rect.left_top() + vec2(8.0, 12.0),
                egui::Align2::LEFT_CENTER,
                buffer.vim.cmdline(),
                FontId::monospace(presentation_font_size_snapshot(&font_id)),
                palette.semantic.primary_text,
            );
        }
        if let Some(hover) = candidate_hovered_source.as_mut() {
            hover.token_rect = Rect::from_min_size(
                editor_origin + hover.token_rect.min.to_vec2() - scroll_offset,
                hover.token_rect.size(),
            );
        }

        let pointer_hover_precedence = resolve_pointer_hover_precedence_with_completion(
            completion_popup,
            hovered_diagnostic,
            candidate_hovered_source,
        );
        let diagnostic_tooltip_active = diagnostic_tooltip_blocks_lsp_hover_at_pointer(matches!(
            pointer_hover_precedence,
            PointerHoverPrecedence::Diagnostic(_)
        ));
        let hovered_source = match pointer_hover_precedence {
            PointerHoverPrecedence::SourceText(hover) => Some(hover),
            _ => None,
        };

        // Diagnostic squiggle tooltips are rendered here; LSP documentation popups
        // are rendered by `editor/hover.rs` from anchors supplied by the app layer.
        if let PointerHoverPrecedence::Diagnostic(index) = pointer_hover_precedence {
            if let Some(diagnostic) = diagnostics.get(index) {
                egui::show_tooltip_at_pointer(
                    ui.ctx(),
                    egui::Id::new(("diagnostic_tooltip", path_id.as_ref(), index)),
                    |ui| {
                        ui.label(&diagnostic.message);
                        if let Some(code) = &diagnostic.code {
                            ui.weak(format!("[{code}]"));
                        }
                    },
                );
            }
        }

        paint_gutter(
            ui,
            buffer,
            scroll_output.inner_rect,
            scroll_output.state.offset.y,
            gutter_width,
            row_height,
            &visible_lines,
            font_id,
            gutter_background,
            current_line_color,
            weak_text_color,
            separator_color,
            bookmarks,
            palette.semantic.accent,
        );

        let editor_viewport_rect = scroll_output.inner.rect;
        let editor_has_focus = scroll_output.inner.has_focus();
        EditorOutput {
            response: scroll_output.inner,
            command,
            action: editor_action,
            completion_anchor: CompletionPopupAnchor::from_screen_rect(cursor_screen_rect),
            editor_viewport_rect: Some(editor_viewport_rect),
            hover_popup: HoverPopupModel {
                hovered_source,
                diagnostic_tooltip_active,
            },
            editor_has_focus,
        }
    }
}

fn is_hovered_token_char(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

fn hovered_token_col_range(line: &str, col: usize) -> (usize, usize) {
    let len = line.chars().count();
    if len == 0 {
        return (0, 0);
    }
    let col = col.min(len.saturating_sub(1));
    let mut start = col;
    let mut end = col + 1;
    if line.chars().nth(col).is_some_and(is_hovered_token_char) {
        while start > 0 {
            let ch = line.chars().nth(start - 1).unwrap_or(' ');
            if !is_hovered_token_char(ch) {
                break;
            }
            start -= 1;
        }
        while end < len {
            let ch = line.chars().nth(end).unwrap_or(' ');
            if !is_hovered_token_char(ch) {
                break;
            }
            end += 1;
        }
    }
    (start, end.min(len))
}

fn diagnostic_color(severity: DiagnosticSeverity, palette: SemanticPalette) -> Color32 {
    match severity {
        DiagnosticSeverity::Error => palette.error,
        DiagnosticSeverity::Warning => palette.warning,
        DiagnosticSeverity::Information => palette.information,
        DiagnosticSeverity::Hint => palette.muted_text,
    }
}

fn draw_wavy_underline(painter: &egui::Painter, x_start: f32, x_end: f32, y: f32, color: Color32) {
    let mut x = x_start;
    let mut up = true;
    while x < x_end {
        let next_x = (x + 4.0).min(x_end);
        let next_y = if up { y - 2.0 } else { y + 2.0 };
        painter.line_segment([pos2(x, y), pos2(next_x, next_y)], Stroke::new(1.0, color));
        x = next_x;
        up = !up;
    }
}

fn find_word_range(line: &str, col: usize) -> Range<usize> {
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return 0..0;
    }
    let col = col.min(chars.len());
    let is_word_char = |character: char| character.is_alphanumeric() || character == '_';

    let mut start = col;
    while start > 0 && is_word_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && is_word_char(chars[end]) {
        end += 1;
    }
    start..end
}

fn handle_keyboard_input(
    ui: &Ui,
    state: &mut EditorState,
    buffer: &mut TextBuffer,
    completion_popup_open: bool,
    tab_width: usize,
    insert_spaces: bool,
    lsp_active: bool,
    vim_enabled: bool,
) -> Option<EditorAction> {
    let mut action = None;
    let vim_options = crate::vim::VimOptions {
        tab_width,
        insert_spaces,
    };
    // Feeds one input into the per-buffer vim state machine and stores it back.
    fn feed_vim(
        buffer: &mut TextBuffer,
        input: crate::vim::VimInput,
        options: crate::vim::VimOptions,
    ) -> crate::vim::VimResult {
        let mut vim = std::mem::take(&mut buffer.vim);
        let result = vim.process(buffer, input, options);
        buffer.vim = vim;
        result
    }
    let events = ui.input(|input| input.events.clone());
    for event in events {
        match event {
            Event::Text(text) if !text.is_empty() => {
                if ui.input(|input| input.modifiers.command || input.modifiers.alt) {
                    continue;
                }
                if vim_enabled {
                    let mut consumed_any = false;
                    for ch in text.chars() {
                        let result = feed_vim(buffer, crate::vim::VimInput::Char(ch), vim_options);
                        if result.consumed {
                            consumed_any = true;
                            if action.is_none() {
                                if let Some(ex) = result.ex {
                                    action = Some(EditorAction::VimEx(ex));
                                } else if let Some(pattern) = result.search {
                                    action = Some(EditorAction::VimSearch(pattern));
                                }
                            }
                        }
                    }
                    if consumed_any {
                        state.scroll_to_cursor = true;
                        continue;
                    }
                }
                let _ = buffer.insert_at_cursors(&text);
                state.preferred_col = None;
                state.scroll_to_cursor = true;
                if lsp_active && (text == "(" || text == ",") {
                    action = Some(EditorAction::RequestSignatureHelp);
                }
            }
            Event::Paste(text) if !text.is_empty() => {
                let _ = buffer.insert_at_cursors(&text);
                state.preferred_col = None;
                state.scroll_to_cursor = true;
            }
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                // ── Vim interception (before every other binding) ────────────
                if vim_enabled && !completion_popup_open {
                    // Ctrl+R reaches vim only in normal/visual (redo).
                    let ctrl_chord_redo = modifiers.ctrl
                        && !modifiers.shift
                        && !modifiers.alt
                        && key == Key::R
                        && buffer.vim.mode != crate::vim::VimMode::Insert;
                    let named = if ctrl_chord_redo {
                        Some(crate::vim::NamedKey::CtrlR)
                    } else if modifiers.ctrl || modifiers.command || modifiers.alt {
                        None
                    } else {
                        match key {
                            Key::Escape => Some(crate::vim::NamedKey::Escape),
                            Key::Enter => Some(crate::vim::NamedKey::Enter),
                            Key::Backspace => Some(crate::vim::NamedKey::Backspace),
                            Key::Delete => Some(crate::vim::NamedKey::Delete),
                            Key::ArrowLeft => Some(crate::vim::NamedKey::Left),
                            Key::ArrowRight => Some(crate::vim::NamedKey::Right),
                            Key::ArrowUp => Some(crate::vim::NamedKey::Up),
                            Key::ArrowDown => Some(crate::vim::NamedKey::Down),
                            Key::Home => Some(crate::vim::NamedKey::Home),
                            Key::End => Some(crate::vim::NamedKey::End),
                            Key::PageUp => Some(crate::vim::NamedKey::PageUp),
                            Key::PageDown => Some(crate::vim::NamedKey::PageDown),
                            _ => None,
                        }
                    };
                    if let Some(named) = named {
                        let result = feed_vim(buffer, crate::vim::VimInput::Key(named), vim_options);
                        if result.consumed {
                            state.scroll_to_cursor = true;
                            if action.is_none() {
                                if let Some(ex) = result.ex {
                                    action = Some(EditorAction::VimEx(ex));
                                } else if let Some(pattern) = result.search {
                                    action = Some(EditorAction::VimSearch(pattern));
                                }
                            }
                            continue;
                        }
                        // Insert mode: fall through to the default handler.
                    }
                }
                if completion_popup_open
                    && matches!(
                        key,
                        Key::ArrowUp
                            | Key::ArrowDown
                            | Key::PageUp
                            | Key::PageDown
                            | Key::Enter
                            | Key::Tab
                            | Key::Escape
                    )
                {
                    continue;
                }
                if key == Key::F9 {
                    if modifiers.shift {
                        // Shift+F9 — navigate to previous bookmark
                        action = Some(EditorAction::PrevBookmark);
                    } else {
                        // F9 — navigate to next bookmark
                        action = Some(EditorAction::NextBookmark);
                    }
                    continue;
                }
                // Ctrl+F2 — toggle bookmark on the current line (keyboard shortcut)
                if key == Key::F2 && modifiers.ctrl && !modifiers.shift && !modifiers.alt {
                    action = Some(EditorAction::ToggleBookmark {
                        line: buffer.cursor().line,
                    });
                    continue;
                }
                let command = modifiers.command || (cfg!(target_os = "macos") && modifiers.mac_cmd);
                if command && key == Key::Z {
                    if modifiers.shift {
                        buffer.redo();
                    } else {
                        buffer.undo();
                    }
                    state.scroll_to_cursor = true;
                    continue;
                }
                if command && key == Key::Y {
                    buffer.redo();
                    state.scroll_to_cursor = true;
                    continue;
                }
                if command && modifiers.alt && key == Key::ArrowUp {
                    buffer.add_cursor_vertical(false);
                    state.scroll_to_cursor = true;
                    continue;
                }
                if command && modifiers.alt && key == Key::ArrowDown {
                    buffer.add_cursor_vertical(true);
                    state.scroll_to_cursor = true;
                    continue;
                }
                if command && key == Key::J {
                    let _ = buffer.join_lines();
                    state.scroll_to_cursor = true;
                    continue;
                }
                if command && key == Key::D && !modifiers.shift {
                    buffer.select_next_occurrence();
                    state.scroll_to_cursor = true;
                    continue;
                }
                if command && modifiers.shift && key == Key::D {
                    let _ = buffer.duplicate_selection_or_line();
                    state.scroll_to_cursor = true;
                    continue;
                }
                if command && modifiers.shift && key == Key::L {
                    buffer.select_all_occurrences();
                    state.scroll_to_cursor = true;
                    continue;
                }
                if key == Key::Escape && buffer.cursors.len() > 1 {
                    buffer.collapse_to_primary();
                    continue;
                }
                if key == Key::Home {
                    buffer.smart_home(modifiers.shift);
                    state.scroll_to_cursor = true;
                    continue;
                }
                if modifiers.alt && modifiers.shift && buffer.column_selection.is_some() {
                    let column = buffer.column_selection.clone().unwrap();
                    let mut head = CursorPosition {
                        line: column.head_line,
                        col: column.head_col,
                    };
                    match key {
                        Key::ArrowUp => head.line = head.line.saturating_sub(1),
                        Key::ArrowDown => {
                            head.line = (head.line + 1).min(buffer.len_lines().saturating_sub(1))
                        }
                        Key::ArrowLeft => head.col = head.col.saturating_sub(1),
                        Key::ArrowRight => head.col = head.col.saturating_add(1),
                        _ => {}
                    }
                    if matches!(
                        key,
                        Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight
                    ) {
                        buffer.set_column_selection(
                            CursorPosition {
                                line: column.anchor_line,
                                col: column.anchor_col,
                            },
                            head,
                        );
                        state.scroll_to_cursor = true;
                        continue;
                    }
                }
                if modifiers.alt && modifiers.shift && matches!(key, Key::ArrowUp | Key::ArrowDown)
                {
                    let anchor = buffer.primary_cursor().head;
                    let mut head = anchor;
                    if key == Key::ArrowUp {
                        head.line = head.line.saturating_sub(1);
                    } else {
                        head.line = (head.line + 1).min(buffer.len_lines().saturating_sub(1));
                    }
                    buffer.set_column_selection(anchor, head);
                    state.scroll_to_cursor = true;
                    continue;
                }
                if modifiers.alt && modifiers.shift && key == Key::ArrowRight {
                    buffer.expand_ast_selection();
                    state.scroll_to_cursor = true;
                    continue;
                }
                if modifiers.alt && modifiers.shift && key == Key::ArrowLeft {
                    buffer.shrink_ast_selection();
                    state.scroll_to_cursor = true;
                    continue;
                }
                if modifiers.alt && !modifiers.shift && key == Key::ArrowUp {
                    let _ = buffer.move_selected_lines(false);
                    state.scroll_to_cursor = true;
                    continue;
                }
                if modifiers.alt && !modifiers.shift && key == Key::ArrowDown {
                    let _ = buffer.move_selected_lines(true);
                    state.scroll_to_cursor = true;
                    continue;
                }
                if modifiers.alt {
                    continue;
                }
                if command && modifiers.shift && key == Key::OpenBracket {
                    let line = buffer.cursor().line;
                    if buffer.collapse_fold_at(line) {
                        state.scroll_to_cursor = true;
                    }
                    state.fold_chord_armed = false;
                    continue;
                }
                if command && modifiers.shift && key == Key::CloseBracket {
                    let line = buffer.cursor().line;
                    if buffer.expand_fold_at(line) {
                        state.scroll_to_cursor = true;
                    }
                    state.fold_chord_armed = false;
                    continue;
                }
                if command && key == Key::K {
                    state.fold_chord_armed = true;
                    continue;
                }
                if state.fold_chord_armed && command && key == Key::Num0 {
                    buffer.collapse_all_folds();
                    state.scroll_to_cursor = true;
                    state.fold_chord_armed = false;
                    continue;
                }
                if state.fold_chord_armed && command && key == Key::J {
                    buffer.expand_all_folds();
                    state.scroll_to_cursor = true;
                    state.fold_chord_armed = false;
                    continue;
                }
                state.fold_chord_armed = false;
                if command {
                    continue;
                }
                let handled = match key {
                    Key::Backspace => {
                        let _ = buffer.delete_at_all_cursors(true);
                        state.preferred_col = None;
                        true
                    }
                    Key::Delete => {
                        let _ = buffer.delete_at_all_cursors(false);
                        state.preferred_col = None;
                        true
                    }
                    Key::Enter => {
                        let newline = if buffer.text().contains("\r\n") {
                            "\r\n"
                        } else {
                            "\n"
                        };
                        let _ = buffer.insert_at_cursors(newline);
                        state.preferred_col = None;
                        true
                    }
                    Key::Tab => {
                        let text = if insert_spaces {
                            " ".repeat(tab_width.max(1))
                        } else {
                            "\t".to_owned()
                        };
                        let _ = buffer.insert_at_cursors(&text);
                        state.preferred_col = None;
                        true
                    }
                    Key::ArrowLeft => {
                        buffer.move_cursors_horizontal(false, modifiers.shift);
                        state.preferred_col = None;
                        true
                    }
                    Key::ArrowRight => {
                        buffer.move_cursors_horizontal(true, modifiers.shift);
                        state.preferred_col = None;
                        true
                    }
                    Key::ArrowUp => {
                        buffer.move_cursors_vertical(false, modifiers.shift);
                        true
                    }
                    Key::ArrowDown => {
                        buffer.move_cursors_vertical(true, modifiers.shift);
                        true
                    }
                    _ => false,
                };
                if handled {
                    buffer.update_bracket_match();
                    state.scroll_to_cursor = true;
                }
            }
            _ => {}
        }
    }
    action
}

#[allow(clippy::too_many_arguments)]
fn paint_gutter(
    ui: &Ui,
    buffer: &TextBuffer,
    viewport: Rect,
    vertical_offset: f32,
    gutter_width: f32,
    row_height: f32,
    visible_lines: &[usize],
    font_id: FontId,
    background: Color32,
    current_line: Color32,
    text_color: Color32,
    separator: Color32,
    bookmarks: &[usize],
    bookmark_color: Color32,
) {
    let gutter_rect = Rect::from_min_max(
        viewport.min,
        pos2(
            (viewport.left() + gutter_width).min(viewport.right()),
            viewport.bottom(),
        ),
    );
    let painter = ui.painter().with_clip_rect(gutter_rect);
    painter.rect_filled(gutter_rect, 0.0, background);

    let relative_viewport = Rect::from_min_size(
        pos2(0.0, vertical_offset),
        vec2(viewport.width(), viewport.height()),
    );
    for line_index in visible_line_range(
        relative_viewport,
        row_height,
        visible_lines.len(),
        OVERSCAN_ROWS,
    ) {
        let Some(&real_line) = visible_lines.get(line_index) else {
            continue;
        };
        let y = viewport.top() + line_index as f32 * row_height - vertical_offset;
        if real_line == buffer.cursor().line {
            painter.rect_filled(
                Rect::from_min_size(pos2(viewport.left(), y), vec2(gutter_width, row_height)),
                0.0,
                current_line,
            );
        }
        if bookmarks.contains(&real_line) {
            let bx = viewport.left() + 2.0;
            let by = y + (row_height - 12.0) * 0.5;
            let width = 8.0;
            let height = 12.0;
            let points = vec![
                pos2(bx, by),
                pos2(bx + width, by),
                pos2(bx + width, by + height),
                pos2(bx + width * 0.5, by + height - 3.0),
                pos2(bx, by + height),
            ];
            painter.add(egui::Shape::convex_polygon(
                points,
                bookmark_color,
                egui::Stroke::NONE,
            ));
        }
        if let Some(fold) = buffer.fold_starting_at(real_line) {
            let icon = if buffer.fold_state.collapsed.contains(&fold.start_line) {
                "\u{25b6}"
            } else {
                "\u{25bc}"
            };
            let galley = painter.layout_no_wrap(icon.to_owned(), font_id.clone(), text_color);
            painter.galley(
                pos2(
                    viewport.left() + (FOLD_GUTTER_WIDTH - galley.size().x) * 0.5,
                    y + (row_height - galley.size().y) * 0.5,
                ),
                galley,
                text_color,
            );
        }
        let number = (real_line + 1).to_string();
        let galley = painter.layout_no_wrap(number, font_id.clone(), text_color);
        painter.galley(
            pos2(
                viewport.left() + gutter_width - GUTTER_PADDING - galley.size().x,
                y + (row_height - galley.size().y) * 0.5,
            ),
            galley,
            text_color,
        );
    }

    painter.line_segment(
        [gutter_rect.right_top(), gutter_rect.right_bottom()],
        Stroke::new(1.0, separator),
    );
}

fn visible_line_range(
    viewport: Rect,
    row_height: f32,
    line_count: usize,
    overscan: usize,
) -> Range<usize> {
    if line_count == 0 || row_height <= 0.0 {
        return 0..0;
    }

    let first = (viewport.top().max(0.0) / row_height).floor() as usize;
    // Include the first row just beyond the viewport so a one-line arrow move
    // can compute and reveal its caret in the same frame.
    let last = (viewport.bottom().max(0.0) / row_height).ceil() as usize + 1;
    first.saturating_sub(overscan)..last.saturating_add(overscan).min(line_count)
}

fn line_to_y(line: usize, buffer: &TextBuffer, row_height: f32, code_lenses: bool) -> f32 {
    (0..line.min(buffer.len_lines()))
        .filter(|candidate| buffer.is_line_visible(*candidate))
        .map(|candidate| {
            row_height
                + if code_lenses && buffer.has_code_lens(candidate) {
                    18.0
                } else {
                    0.0
                }
        })
        .sum()
}

fn y_to_visible_row(
    pointer_y: f32,
    content_top: f32,
    buffer: &TextBuffer,
    row_height: f32,
    visible_lines: &[usize],
    code_lenses: bool,
) -> usize {
    let local_y = (pointer_y - content_top).max(0.0);
    visible_lines
        .iter()
        .position(|line| {
            let top = line_to_y(*line, buffer, row_height, code_lenses);
            let height = row_height
                + if code_lenses && buffer.has_code_lens(*line) {
                    18.0
                } else {
                    0.0
                };
            local_y < top + height
        })
        .unwrap_or_else(|| visible_lines.len().saturating_sub(1))
}

fn visible_line_range_with_lens(
    viewport: Rect,
    row_height: f32,
    visible_lines: &[usize],
    buffer: &TextBuffer,
    overscan: usize,
    code_lenses: bool,
) -> Range<usize> {
    if visible_lines.is_empty() {
        return 0..0;
    }
    let first = visible_lines
        .iter()
        .position(|line| {
            let top = line_to_y(*line, buffer, row_height, code_lenses);
            let height = row_height
                + if code_lenses && buffer.has_code_lens(*line) {
                    18.0
                } else {
                    0.0
                };
            top + height >= viewport.top()
        })
        .unwrap_or(0)
        .saturating_sub(overscan);
    let last = visible_lines
        .iter()
        .rposition(|line| line_to_y(*line, buffer, row_height, code_lenses) <= viewport.bottom())
        .unwrap_or(visible_lines.len() - 1)
        .saturating_add(overscan + 1)
        .min(visible_lines.len());
    first..last.max(first)
}

#[cfg(test)]
mod tests {
    // Editor/UI state tests — drive EditorWidget through egui::RawInput where practical.
    // See module docs `# Editor/UI state tests` for the full table and cargo filters.

    use egui::{pos2, Rect, Ui};

    use crate::lsp::types::{DiagnosticSeverity, LspDiagnostic};

    use crate::editor::buffer::TextBuffer;

    use crate::editor::completion::CompletionPopupModel;

    use super::{
        visible_line_range, EditorAnnotations, EditorInteraction, EditorPresentation, EditorState,
        EditorWidget,
    };
    use crate::editor::position::lsp_utf16_range_char_span_on_line;

    fn show_test_editor(
        ui: &mut Ui,
        state: &mut EditorState,
        buffer: &mut TextBuffer,
        interaction: EditorInteraction,
    ) -> super::EditorOutput {
        EditorWidget::show(
            ui,
            state,
            buffer,
            interaction,
            EditorAnnotations::empty(),
            EditorPresentation::test(14.0),
            None,
            false,
            None,
        )
    }

    fn show_test_editor_with_diagnostics(
        ui: &mut Ui,
        state: &mut EditorState,
        buffer: &mut TextBuffer,
        diagnostics: &[LspDiagnostic],
        interaction: EditorInteraction,
    ) -> super::EditorOutput {
        EditorWidget::show(
            ui,
            state,
            buffer,
            interaction,
            EditorAnnotations::new(diagnostics, &[]),
            EditorPresentation::test(14.0),
            None,
            false,
            None,
        )
    }

    #[test]
    fn editor_interaction_model_groups_per_frame_gates() {
        use super::completion_blocks_pointer_hover;

        let enabled = EditorInteraction::interactive(true);
        assert!(enabled.enabled);
        assert!(enabled.lsp_active);
        assert!(!enabled.completion_popup.open);

        let blocked = enabled.with_completion_popup(CompletionPopupModel::open());
        assert!(completion_blocks_pointer_hover(blocked.completion_popup));

        let view_only = EditorInteraction::new(false, false, CompletionPopupModel::closed());
        assert!(!view_only.enabled);
    }

    #[test]
    fn visible_rows_are_clamped_and_overscanned() {
        let viewport = Rect::from_min_max(pos2(0.0, 100.0), pos2(800.0, 200.0));
        assert_eq!(visible_line_range(viewport, 20.0, 100, 2), 3..13);
    }

    #[test]
    fn visible_rows_handle_empty_and_end_of_document() {
        let viewport = Rect::from_min_max(pos2(0.0, 180.0), pos2(800.0, 260.0));
        assert_eq!(visible_line_range(viewport, 20.0, 10, 2), 7..10);
        assert_eq!(visible_line_range(viewport, 20.0, 0, 2), 0..0);
    }

    #[test]
    fn diagnostic_columns_are_utf16_aware_and_clamped() {
        let diagnostic = LspDiagnostic {
            line_start: 0,
            col_start: 1,
            line_end: 0,
            col_end: 99,
            severity: DiagnosticSeverity::Error,
            message: "error".to_owned(),
            code: None,
        };

        assert_eq!(
            lsp_utf16_range_char_span_on_line(
                0,
                "a🙂z",
                diagnostic.line_start,
                diagnostic.col_start,
                diagnostic.line_end,
                diagnostic.col_end,
            ),
            Some((1, 3))
        );
        assert_eq!(
            lsp_utf16_range_char_span_on_line(
                1,
                "other",
                diagnostic.line_start,
                diagnostic.col_start,
                diagnostic.line_end,
                diagnostic.col_end,
            ),
            None
        );
    }

    #[test]
    fn multiline_diagnostics_cover_intermediate_lines() {
        let diagnostic = LspDiagnostic {
            line_start: 1,
            col_start: 2,
            line_end: 3,
            col_end: 1,
            severity: DiagnosticSeverity::Warning,
            message: "warning".to_owned(),
            code: None,
        };

        assert_eq!(
            lsp_utf16_range_char_span_on_line(
                1,
                "abcd",
                diagnostic.line_start,
                diagnostic.col_start,
                diagnostic.line_end,
                diagnostic.col_end,
            ),
            Some((2, 4))
        );
        assert_eq!(
            lsp_utf16_range_char_span_on_line(
                2,
                "middle",
                diagnostic.line_start,
                diagnostic.col_start,
                diagnostic.line_end,
                diagnostic.col_end,
            ),
            Some((0, 6))
        );
        assert_eq!(
            lsp_utf16_range_char_span_on_line(
                3,
                "end",
                diagnostic.line_start,
                diagnostic.col_start,
                diagnostic.line_end,
                diagnostic.col_end,
            ),
            Some((0, 1))
        );
    }

    #[test]
    fn ctrl_space_produces_one_completion_request_action_and_does_not_insert_a_space() {
        use super::{EditorAction, EditorState};
        use crate::editor::buffer::{CursorPosition, TextBuffer};
        use egui::{Context, Key, Modifiers};

        let mut buffer = TextBuffer::from_text("fn main() {\n    let x = 42;\n}");
        buffer.set_cursor(CursorPosition { line: 1, col: 8 });
        let text_before = buffer.text().to_owned();
        let revision_before = buffer.revision();
        let ctx = Context::default();

        let mut state = EditorState::default();
        let input = egui::RawInput {
            focused: true,
            events: vec![egui::Event::Key {
                key: Key::Space,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::COMMAND,
            }],
            ..Default::default()
        };

        let mut output = None;
        let _ = ctx.run(input, |ctx| {
            let path_id = buffer.path().map(std::path::PathBuf::from);
            let editor_id = egui::Id::new(("blue_ide_editor", &path_id));
            ctx.memory_mut(|mem| mem.request_focus(editor_id));
            egui::CentralPanel::default().show(ctx, |ui| {
                output = Some(show_test_editor(
                    ui,
                    &mut state,
                    &mut buffer,
                    EditorInteraction::interactive(true),
                ));
            });
        });
        let output = output.unwrap();

        assert_eq!(output.action, Some(EditorAction::RequestCompletion));
        assert_eq!(buffer.text(), text_before);
        assert_eq!(buffer.revision(), revision_before);
    }

    #[test]
    fn test_editor_interaction() {
        use super::{DefinitionTrigger, EditorAction, EditorState};
        use crate::editor::buffer::{CursorPosition, TextBuffer};
        use egui::{Context, Key, Modifiers, PointerButton};

        let mut buffer = TextBuffer::from_text("fn main() {\n    let x = 42;\n}");
        let ctx = Context::default();

        // 1. Test F12 emits one go-to-definition action when focused.
        {
            let mut state = EditorState::default();
            let input = egui::RawInput {
                focused: true,
                events: vec![egui::Event::Key {
                    key: Key::F12,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }],
                ..Default::default()
            };

            let mut output = None;
            let _ = ctx.run(input, |ctx| {
                let path_id = buffer.path().map(std::path::PathBuf::from);
                let editor_id = egui::Id::new(("blue_ide_editor", &path_id));
                ctx.memory_mut(|mem| mem.request_focus(editor_id));
                egui::CentralPanel::default().show(ctx, |ui| {
                    output = Some(show_test_editor(
                        ui,
                        &mut state,
                        &mut buffer,
                        EditorInteraction::interactive(true),
                    ));
                });
            });
            let output = output.unwrap();

            assert!(matches!(
                output.action,
                Some(EditorAction::GoToDefinition {
                    position,
                    source: DefinitionTrigger::F12,
                }) if position == CursorPosition { line: 0, col: 0 }
            ));
        }

        // 2. F12 does nothing without editor focus.
        {
            let mut state = EditorState::default();
            let input = egui::RawInput {
                focused: false,
                events: vec![egui::Event::Key {
                    key: Key::F12,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }],
                ..Default::default()
            };

            let mut output = None;
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    output = Some(show_test_editor(
                        ui,
                        &mut state,
                        &mut buffer,
                        EditorInteraction::interactive(true),
                    ));
                });
            });
            let output = output.unwrap();

            assert!(output.action.is_none());
        }

        // 3. F12 does nothing while a modal disables editor interaction.
        {
            let mut state = EditorState::default();
            let input = egui::RawInput {
                focused: true,
                events: vec![egui::Event::Key {
                    key: Key::F12,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }],
                ..Default::default()
            };

            let mut output = None;
            let _ = ctx.run(input, |ctx| {
                let path_id = buffer.path().map(std::path::PathBuf::from);
                let editor_id = egui::Id::new(("blue_ide_editor", &path_id));
                ctx.memory_mut(|mem| mem.request_focus(editor_id));
                egui::CentralPanel::default().show(ctx, |ui| {
                    output = Some(show_test_editor(
                        ui,
                        &mut state,
                        &mut buffer,
                        EditorInteraction::new(false, true, CompletionPopupModel::closed()),
                    ));
                });
            });
            let output = output.unwrap();

            assert!(output.action.is_none());
        }

        // Helper to simulate mouse clicks over multiple frames
        let simulate_click = |ctx: &Context,
                              state: &mut EditorState,
                              buffer: &mut TextBuffer,
                              click_pos: egui::Pos2,
                              ctrl: bool| {
            let mods = egui::Modifiers {
                ctrl,
                ..Default::default()
            };

            // Frame 1: hover
            let input1 = egui::RawInput {
                events: vec![egui::Event::PointerMoved(click_pos)],
                modifiers: mods,
                ..Default::default()
            };
            let _ = ctx.run(input1, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ =
                        show_test_editor(ui, state, buffer, EditorInteraction::interactive(true));
                });
            });

            // Frame 2: press
            let input2 = egui::RawInput {
                events: vec![egui::Event::PointerButton {
                    pos: click_pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: mods,
                }],
                modifiers: mods,
                ..Default::default()
            };
            let _ = ctx.run(input2, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ =
                        show_test_editor(ui, state, buffer, EditorInteraction::interactive(true));
                });
            });

            // Frame 3: release
            let input3 = egui::RawInput {
                events: vec![egui::Event::PointerButton {
                    pos: click_pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: mods,
                }],
                modifiers: mods,
                ..Default::default()
            };
            let mut output = None;
            let _ = ctx.run(input3, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    output = Some(show_test_editor(
                        ui,
                        state,
                        buffer,
                        EditorInteraction::interactive(true),
                    ));
                });
            });
            output.unwrap()
        };

        // 4. Plain click moves the cursor.
        {
            let mut state = EditorState::default();
            let row_height = 20.0;
            let click_pos = egui::pos2(80.0, 10.0 + row_height + row_height * 0.5);
            let _ = simulate_click(&ctx, &mut state, &mut buffer, click_pos, false);

            assert_eq!(buffer.cursor().line, 1);
        }

        // 5. Ctrl+click does not move the cursor but emits a go-to-definition action.
        {
            let mut state = EditorState::default();
            buffer.set_cursor(CursorPosition { line: 0, col: 0 });
            let row_height = 20.0;
            let click_pos = egui::pos2(80.0, 10.0 + row_height + row_height * 0.5);
            let output = simulate_click(&ctx, &mut state, &mut buffer, click_pos, true);

            assert_eq!(buffer.cursor(), CursorPosition { line: 0, col: 0 });
            assert!(matches!(
                output.action,
                Some(EditorAction::GoToDefinition {
                    position,
                    source: DefinitionTrigger::CtrlClick,
                }) if position.line == 1
            ));
        }
    }

    #[test]
    fn pointer_text_hover_reports_glyph_position_without_moving_cursor() {
        use super::EditorState;
        use crate::editor::buffer::{CursorPosition, TextBuffer};
        use egui::Context;

        let mut buffer = TextBuffer::from_text("fn main() {}\n");
        buffer.set_cursor(CursorPosition { line: 1, col: 0 });
        let cursor_before = buffer.cursor();
        let ctx = Context::default();
        let row_height = 20.0;
        let line_y = 10.0 + row_height * 0.5;

        let mut state = EditorState::default();
        let input = egui::RawInput {
            events: vec![egui::Event::PointerMoved(egui::pos2(80.0, line_y))],
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let mut output = None;
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                output = Some(show_test_editor(
                    ui,
                    &mut state,
                    &mut buffer,
                    EditorInteraction::interactive(false),
                ));
            });
        });

        let hover = output
            .unwrap()
            .hover_popup
            .hovered_source
            .expect("hover position");
        assert_eq!(hover.cursor_position().line, 0);
        assert!(hover.cursor_position().col > 0);
        assert!(hover.token_rect.width() > 0.0);
        assert!(hover.token_rect.height() > 0.0);
        assert_eq!(buffer.cursor(), cursor_before);
    }

    #[test]
    fn pointer_text_hover_reports_token_rect_for_identifier() {
        use super::EditorState;
        use crate::editor::buffer::TextBuffer;
        use egui::Context;

        let mut buffer = TextBuffer::from_text("fn main() {}\n");
        let ctx = Context::default();
        let row_height = 20.0;
        let line_y = 10.0 + row_height * 0.5;

        let mut state = EditorState::default();
        let input = egui::RawInput {
            events: vec![egui::Event::PointerMoved(egui::pos2(92.0, line_y))],
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let mut output = None;
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                output = Some(show_test_editor(
                    ui,
                    &mut state,
                    &mut buffer,
                    EditorInteraction::interactive(false),
                ));
            });
        });

        let hover = output
            .unwrap()
            .hover_popup
            .hovered_source
            .expect("hover position");
        assert_eq!(hover.cursor_position().line, 0);
        assert!(hover.cursor_position().col >= 4);
        assert!(hover.token_rect.width() >= 20.0);
    }

    #[test]
    fn pointer_text_hover_excludes_gutter() {
        use super::EditorState;
        use crate::editor::buffer::TextBuffer;
        use egui::Context;

        let mut buffer = TextBuffer::from_text("fn main() {}\n");
        let ctx = Context::default();
        let row_height = 20.0;
        let line_y = 10.0 + row_height * 0.5;

        let mut hover_at = |pos: egui::Pos2| {
            let mut state = EditorState::default();
            let input = egui::RawInput {
                events: vec![egui::Event::PointerMoved(pos)],
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            };
            let mut output = None;
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    output = Some(show_test_editor(
                        ui,
                        &mut state,
                        &mut buffer,
                        EditorInteraction::interactive(false),
                    ));
                });
            });
            output.unwrap().hover_popup.hovered_source
        };

        assert!(
            hover_at(egui::pos2(8.0, line_y)).is_none(),
            "line-number gutter must not trigger LSP hover"
        );
        assert!(
            hover_at(egui::pos2(80.0, line_y)).is_some(),
            "rendered source text should trigger LSP hover detection"
        );
    }

    #[test]
    fn pointer_text_hover_excludes_empty_background_beyond_line_end() {
        use super::EditorState;
        use crate::editor::buffer::TextBuffer;
        use egui::Context;

        let mut buffer = TextBuffer::from_text("fn main() {}\n");
        let ctx = Context::default();
        let row_height = 20.0;
        let line_y = 10.0 + row_height * 0.5;

        let mut hover_at = |pos: egui::Pos2| {
            let mut state = EditorState::default();
            let input = egui::RawInput {
                events: vec![egui::Event::PointerMoved(pos)],
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    e
/// Snapshot the monospace font size from a `FontId`.
fn presentation_font_size_snapshot(font_id: &FontId) -> f32 {
    font_id.size
}
32 {
    font_id.size
}
