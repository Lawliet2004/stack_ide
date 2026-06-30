//! LSP completion popup: rendering, request-session staleness, and lifecycle events.
//!
//! `CompletionState` is owned by `app.rs`. This module emits `CompletionPopupEvent`
//! (accepted/dismissed) and renders the dropdown; it does not own or call `LspClient`.
//! The app sends `textDocument/completion`, receives typed [`LspCompletionItem`] values
//! from `lsp/transport.rs` (see completion parsing considerations there), applies
//! accepted items as literal buffer text, and dismisses sessions. Snippet placeholders
//! and `additionalTextEdits` are not supported.
//!
//! ## Acceptance-time `insertText` fallback
//!
//! On the plain apply path (no `text_edit`), `completion_acceptance_insert_text` chooses
//! non-empty `insert_text`, else `label`. Empty wire `insertText` (`""`) is treated as
//! absent at acceptance even though transport retains it verbatim at parse time.
//! When `text_edit` is present, `TextBuffer::apply_completion_insertion` applies the edit
//! and ignores the resolved plain-path text.
//!
//! Popup keyboard and outside-click tests use `egui::RawInput`; see **Editor/UI state tests**
//! in `editor/widget.rs` (`cargo test --lib editor::completion`).
//!
//! | Ctrl+Space caret-anchored dropdown | `ctrl_space_sends_a_real_completion_request_and_opens_a_functional_caret_anchored_dropdown` | `cargo test --lib ctrl_space_sends_a_real_completion` |
//! | Navigate / accept / click / dismiss | `completion_can_be_navigated_accepted_clicked_and_dismissed` | `cargo test --lib completion_can_be_navigated` |
//! | Accepted prefix edit | `accepted_completion_edits_the_correct_identifier_prefix` | `cargo test --lib accepted_completion_edits_the_correct_identifier_prefix` |
//! | Caret popup positioning | `popup_prefers_below_caret_when_space_allows` | `cargo test --lib popup_prefers_below_caret` |
//! | State transitions + text edits | `add_tests_for_new_state_transitions_and_text_edits` | `cargo test --lib add_tests_for_new_state_transitions_and_text_edits` |
//! | Stale async responses (app) | `stale_asynchronous_responses_cannot_affect_the_current_editor_state` | `cargo test --lib stale_asynchronous_responses_cannot` |
//! | Never: display stale LSP results | `display_stale_lsp_results` | `cargo test --lib display_stale_lsp_results` |
//! | Never: swallow editor keys (no popup) | `swallow_normal_editor_keystrokes_when_no_popup_is_open` | `cargo test --lib swallow_normal_editor_keystrokes_when_no_popup_is_open` |
//! | Never: hard-coded completion/hover mock data | `render_completion_or_hover_using_hard_coded_mock_data` | `cargo test --lib render_completion_or_hover_using_hard_coded_mock_data` |
//! | Snippet literal-only policy | `ask_before_adding_snippet_placeholder_support` | `cargo test --test integration_test ask_before_adding_snippet_placeholder_support` |

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use egui::{
    pos2, vec2, Color32, Context, Id, Key, Label, Modifiers, Order, Pos2, Rect, Response, RichText,
    ScrollArea, Sense, Ui, Vec2,
};

use crate::editor::buffer::{
    is_identifier_prefix_char_pub as is_identifier_prefix_char, CursorPosition, TextBuffer,
};
use crate::lsp::types::LspCompletionItem;
use crate::theme::SemanticPalette;

/// Plain-path insertion text for an accepted completion item.
///
/// Non-empty `insert_text` wins; missing or empty `insert_text` falls back to `label`.
/// `detail` is never used. Callers pass the result to
/// [`TextBuffer::apply_completion_insertion`](crate::editor::buffer::TextBuffer::apply_completion_insertion)
/// only when `text_edit` is absent — `text_edit` always takes precedence at acceptance.
pub(crate) fn completion_acceptance_insert_text(item: &LspCompletionItem) -> &str {
    item.insert_text
        .as_deref()
        .filter(|text| !text.is_empty())
        .unwrap_or(&item.label)
}

/// Per-frame completion popup context from the app shell to the editor widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CompletionPopupModel {
    /// Completion dropdown is visible; suppresses pointer-hover handoff.
    pub open: bool,
}

impl CompletionPopupModel {
    pub const fn closed() -> Self {
        Self { open: false }
    }

    pub const fn open() -> Self {
        Self { open: true }
    }

    pub const fn from_open(open: bool) -> Self {
        Self { open }
    }

    pub const fn blocks_pointer_hover(self) -> bool {
        self.open
    }
}

/// Screen-space caret anchor handed off for completion popup positioning.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CompletionPopupAnchor {
    pub screen_rect: Option<Rect>,
}

impl CompletionPopupAnchor {
    pub const fn none() -> Self {
        Self { screen_rect: None }
    }

    pub fn from_screen_rect(screen_rect: Option<Rect>) -> Self {
        Self { screen_rect }
    }
}

const MAX_VISIBLE_ITEMS: usize = 12;
const ROW_HEIGHT: f32 = 22.0;
const MAX_POPUP_HEIGHT: f32 = ROW_HEIGHT * MAX_VISIBLE_ITEMS as f32 + 8.0;
const POPUP_VERTICAL_GAP: f32 = 4.0;
/// Header, detail, hint, and frame space beyond the scroll list maximum.
const POPUP_CHROME_HEIGHT: f32 = 96.0;
const POPUP_MIN_WIDTH: f32 = 280.0;
const POPUP_MAX_WIDTH: f32 = 480.0;

/// User-driven completion popup lifecycle event emitted from render or input polling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionPopupEvent {
    Accepted { index: usize },
    Dismissed,
}

/// Result of rendering the completion popup for one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionPopupOutput {
    pub event: Option<CompletionPopupEvent>,
    pub popup_rect: Rect,
    #[cfg(test)]
    pub row_hit_rects: Vec<Rect>,
}

impl Default for CompletionPopupOutput {
    fn default() -> Self {
        Self {
            event: None,
            popup_rect: Rect::NOTHING,
            #[cfg(test)]
            row_hit_rects: Vec::new(),
        }
    }
}

/// Outside-click dismiss when the pointer press lands outside the rendered popup bounds.
pub fn completion_outside_click_event(
    ctx: &Context,
    popup_rect: Rect,
) -> Option<CompletionPopupEvent> {
    if !popup_rect.is_positive() {
        return None;
    }
    let outside_click = ctx.input(|input| {
        input.pointer.any_pressed()
            && input
                .pointer
                .interact_pos()
                .is_none_or(|pos| !popup_rect.contains(pos))
    });
    outside_click.then_some(CompletionPopupEvent::Dismissed)
}

/// In-flight completion request context used to reject stale LSP responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionSession {
    pub path: PathBuf,
    pub request_id: u64,
    pub revision: u64,
    pub lsp_version: i32,
    pub cursor: CursorPosition,
    /// Identifier prefix to replace on the plain insertion path (no primary `text_edit`).
    pub prefix_char_range: Range<usize>,
}

impl CompletionSession {
    pub fn get_refinement_query(&self, buffer: &TextBuffer) -> Option<String> {
        if self.path != buffer.path().unwrap_or(Path::new("")) {
            return None;
        }
        let current_cursor = buffer.cursor();
        if current_cursor.line != self.cursor.line {
            return None;
        }
        let prefix_start_char = self.prefix_char_range.start;
        let cursor_char = buffer.position_to_char_index(current_cursor)?;
        if cursor_char < prefix_start_char {
            return None;
        }
        let query_str = buffer.char_range_to_string(prefix_start_char..cursor_char)?;
        for ch in query_str.chars() {
            if !is_identifier_prefix_char(ch) {
                return None;
            }
        }
        Some(query_str)
    }
}

/// App-owned completion UI plus the active request session.
#[derive(Debug, Default)]
pub struct CompletionState {
    popup: CompletionPopup,
    session: Option<CompletionSession>,
}

impl CompletionState {
    pub fn is_open(&self) -> bool {
        self.popup.is_open()
    }

    pub fn popup(&self) -> &CompletionPopup {
        &self.popup
    }

    pub fn popup_mut(&mut self) -> &mut CompletionPopup {
        &mut self.popup
    }

    pub fn request_id(&self) -> Option<u64> {
        self.session.as_ref().map(|session| session.request_id)
    }

    pub fn prefix_char_range(&self) -> Option<Range<usize>> {
        self.session
            .as_ref()
            .map(|session| session.prefix_char_range.clone())
    }

    pub(crate) fn session(&self) -> Option<&CompletionSession> {
        self.session.as_ref()
    }

    pub fn matches_request(&self, id: u64) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.request_id == id)
    }

    pub fn is_for_path(&self, path: &Path) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.path == path)
    }

    pub fn dismiss(&mut self) {
        self.popup.dismiss();
        self.session = None;
    }

    pub fn begin_session(&mut self, session: CompletionSession) {
        self.session = Some(session);
        self.popup.begin_loading();
    }

    pub fn is_stale_for(&self, active: &Path, buffer: &TextBuffer) -> bool {
        let Some(session) = &self.session else {
            return true;
        };
        session.path != active
            || buffer.cursor() != session.cursor
            || buffer.revision() != session.revision
            || buffer.lsp_version != session.lsp_version
    }

    pub fn try_refine_with_buffer(&mut self, buffer: &TextBuffer) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        if let Some(query) = session.get_refinement_query(buffer) {
            self.popup.query = query;
            self.popup.refilter();
            true
        } else {
            false
        }
    }

    pub fn try_accept_response(
        &mut self,
        id: u64,
        items: Vec<LspCompletionItem>,
        active: &Path,
        buffer: &TextBuffer,
    ) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        if session.request_id != id || session.path != active {
            return false;
        }
        if buffer.cursor() != session.cursor
            || buffer.revision() != session.revision
            || buffer.lsp_version != session.lsp_version
        {
            return false;
        }
        self.popup.loading = false;
        self.popup.items = items;
        self.popup.query = buffer
            .char_range_to_string(session.prefix_char_range.clone())
            .unwrap_or_default();
        self.popup.refilter();
        true
    }

    pub fn popup_area_id(&self) -> Id {
        match &self.session {
            Some(session) => Id::new(("lsp_completion_popup", &session.path, session.request_id)),
            None => Id::new("lsp_completion_popup"),
        }
    }

    pub fn show(
        &mut self,
        ctx: &Context,
        anchor: Rect,
        palette: SemanticPalette,
    ) -> CompletionPopupOutput {
        if !self.is_open() {
            return CompletionPopupOutput::default();
        }
        self.popup
            .show_popup(ctx, anchor, self.popup_area_id(), palette)
    }

    /// Poll keyboard input while the popup is open. Returns `(consumed, event)`.
    pub fn poll_keyboard_event(&mut self, ctx: &Context) -> (bool, Option<CompletionPopupEvent>) {
        if !self.is_open() {
            return (false, None);
        }

        let mut consumed = false;
        let mut event = None;
        ctx.input_mut(|input| {
            if input.consume_key(Modifiers::NONE, Key::Escape) {
                event = Some(CompletionPopupEvent::Dismissed);
                consumed = true;
            } else if input.consume_key(Modifiers::NONE, Key::ArrowUp) {
                self.popup_mut().move_selection(-1);
                consumed = true;
            } else if input.consume_key(Modifiers::NONE, Key::ArrowDown) {
                self.popup_mut().move_selection(1);
                consumed = true;
            } else if input.consume_key(Modifiers::NONE, Key::PageUp) {
                self.popup_mut().move_selection(-5);
                consumed = true;
            } else if input.consume_key(Modifiers::NONE, Key::PageDown) {
                self.popup_mut().move_selection(5);
                consumed = true;
            } else if input.consume_key(Modifiers::NONE, Key::Enter)
                || input.consume_key(Modifiers::NONE, Key::Tab)
            {
                let popup = self.popup();
                if !popup.loading && !popup.filtered_indices.is_empty() {
                    let original_idx = popup.filtered_indices[popup.selected];
                    event = Some(CompletionPopupEvent::Accepted {
                        index: original_idx,
                    });
                }
                consumed = true;
            }
        });
        (consumed, event)
    }

    pub fn show_loading_at_cursor(
        &mut self,
        ctx: &Context,
        palette: SemanticPalette,
    ) -> CompletionPopupOutput {
        if !self.popup.loading {
            return CompletionPopupOutput::default();
        }
        let anchor = ctx.input(|input| {
            input
                .pointer
                .hover_pos()
                .map(|pos| Rect::from_min_size(pos, Vec2::ZERO))
                .unwrap_or_else(|| {
                    let screen = ctx.screen_rect();
                    Rect::from_center_size(screen.center(), Vec2::ZERO)
                })
        });
        self.popup
            .show_popup(ctx, anchor, self.popup_area_id(), palette)
    }

    #[cfg(test)]
    pub fn set_session_for_test(&mut self, session: CompletionSession) {
        self.session = Some(session);
    }
}

#[derive(Debug, Default)]
pub struct CompletionPopup {
    pub items: Vec<LspCompletionItem>,
    pub filtered_indices: Vec<usize>,
    pub matched_indices: Vec<Vec<usize>>,
    pub selected: usize,
    pub loading: bool,
    pub query: String,
}

impl CompletionPopup {
    pub fn is_open(&self) -> bool {
        self.loading || !self.items.is_empty()
    }

    pub fn dismiss(&mut self) {
        self.items.clear();
        self.filtered_indices.clear();
        self.matched_indices.clear();
        self.selected = 0;
        self.loading = false;
        self.query.clear();
    }

    pub fn begin_loading(&mut self) {
        self.items.clear();
        self.filtered_indices.clear();
        self.matched_indices.clear();
        self.selected = 0;
        self.loading = true;
        self.query.clear();
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.filtered_indices.is_empty() {
            return;
        }
        let max_index = self.filtered_indices.len().saturating_sub(1) as isize;
        let next = (self.selected as isize + delta).clamp(0, max_index) as usize;
        self.selected = next;
    }

    pub fn selected_item(&self) -> Option<&LspCompletionItem> {
        self.filtered_indices
            .get(self.selected)
            .and_then(|&idx| self.items.get(idx))
    }

    pub fn refilter(&mut self) {
        if self.items.is_empty() {
            self.filtered_indices.clear();
            self.matched_indices.clear();
            self.selected = 0;
            return;
        }

        if self.query.is_empty() {
            self.filtered_indices = (0..self.items.len()).collect();
            self.matched_indices = vec![Vec::new(); self.items.len()];
            self.selected = self.selected.min(self.items.len().saturating_sub(1));
            return;
        }

        let query_chars: Vec<char> = self.query.chars().collect();
        let mut candidates = Vec::new();

        for (idx, item) in self.items.iter().enumerate() {
            let match_target = item.filter_text.as_deref().unwrap_or(&item.label);
            let target_chars: Vec<char> = match_target.chars().collect();

            if is_subsequence_case_insensitive(&query_chars, &target_chars) {
                if let Some((indices, align_score)) =
                    find_best_alignment(&query_chars, &target_chars)
                {
                    let category = if match_target == self.query {
                        MatchCategory::ExactCaseSensitive
                    } else if match_target.eq_ignore_ascii_case(&self.query) {
                        MatchCategory::ExactCaseInsensitive
                    } else if match_target.starts_with(&self.query) {
                        MatchCategory::PrefixCaseSensitive
                    } else if match_target
                        .to_lowercase()
                        .starts_with(&self.query.to_lowercase())
                    {
                        MatchCategory::PrefixCaseInsensitive
                    } else if match_target.contains(&self.query) {
                        MatchCategory::ContiguousSubstringCaseSensitive
                    } else if match_target
                        .to_lowercase()
                        .contains(&self.query.to_lowercase())
                    {
                        MatchCategory::ContiguousSubstringCaseInsensitive
                    } else {
                        MatchCategory::Subsequence
                    };

                    let score = MatchScore {
                        category,
                        alignment: align_score,
                        target_len: target_chars.len(),
                        original_index: idx,
                    };

                    candidates.push((idx, score, indices));
                }
            }
        }

        candidates.sort_by(|a, b| a.1.cmp(&b.1));

        self.filtered_indices.clear();
        self.matched_indices.clear();
        for (idx, _, indices) in candidates {
            self.filtered_indices.push(idx);
            self.matched_indices.push(indices);
        }

        if self.filtered_indices.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self
                .selected
                .min(self.filtered_indices.len().saturating_sub(1));
        }
    }

    fn show_popup(
        &mut self,
        ctx: &Context,
        anchor: Rect,
        area_id: Id,
        palette: SemanticPalette,
    ) -> CompletionPopupOutput {
        let mut event = None;
        let mut popup_rect = Rect::NOTHING;
        #[cfg(test)]
        let mut row_hit_rects = Vec::new();
        let available = ctx.available_rect();
        let max_width = POPUP_MAX_WIDTH.min(available.width());

        let estimated_height = if self.loading && self.items.is_empty() {
            ROW_HEIGHT + POPUP_CHROME_HEIGHT
        } else if self.filtered_indices.is_empty() {
            ROW_HEIGHT + POPUP_CHROME_HEIGHT
        } else {
            let visible_items = self.filtered_indices.len().min(MAX_VISIBLE_ITEMS);
            (ROW_HEIGHT * visible_items as f32 + 8.0) + POPUP_CHROME_HEIGHT
        };
        let popup_pos = completion_popup_position(anchor, available, estimated_height);

        egui::Area::new(area_id)
            .order(Order::Foreground)
            .fixed_pos(popup_pos)
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(8.0)
                    .show(ui, |ui| {
                        ui.set_min_width(POPUP_MIN_WIDTH.min(max_width));
                        ui.set_max_width(max_width);
                        ui.label(RichText::new("Completions").strong());
                        ui.separator();
                        if self.loading && self.items.is_empty() {
                            ui.weak("Loading completions…");
                            ui.label(
                                RichText::new("↑↓ navigate · Enter/Tab accept · Esc dismiss")
                                    .weak(),
                            );
                            ctx.request_repaint_after(Duration::from_millis(100));
                            return;
                        }
                        if self.items.is_empty() {
                            ui.weak("No completions available");
                            return;
                        }
                        if self.filtered_indices.is_empty() {
                            ui.weak("No matching completions");
                        } else {
                            ScrollArea::vertical()
                                .max_height(MAX_POPUP_HEIGHT)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    for (filtered_idx, &index) in
                                        self.filtered_indices.iter().enumerate()
                                    {
                                        let selected = filtered_idx == self.selected;
                                        let item = &self.items[index];
                                        let matched = &self.matched_indices[filtered_idx];
                                        let response = completion_item_row(
                                            ui, item, matched, selected, palette,
                                        );
                                        #[cfg(test)]
                                        row_hit_rects.push(response.rect);
                                        if selected {
                                            response.scroll_to_me(Some(egui::Align::Center));
                                        }
                                        if response.clicked() {
                                            self.selected = filtered_idx;
                                            event = Some(CompletionPopupEvent::Accepted { index });
                                        }
                                    }
                                });
                        }
                        if let Some(item) = self.selected_item() {
                            ui.separator();
                            if let Some(detail) = &item.detail {
                                ui.add(Label::new(RichText::new(detail).weak()).truncate(true));
                            }
                            if let Some(kind) = &item.kind {
                                ui.add(
                                    Label::new(RichText::new(kind).small().weak()).truncate(true),
                                );
                            }
                        }
                        ui.label(
                            RichText::new("↑↓ navigate · Enter/Tab accept · Esc dismiss").weak(),
                        );
                    });
                popup_rect = ui.min_rect();
            });

        CompletionPopupOutput {
            event,
            popup_rect,
            #[cfg(test)]
            row_hit_rects,
        }
    }
}

fn highlighted_label_layout_job(
    label: &str,
    matched_indices: &[usize],
    font_id: egui::FontId,
    default_color: egui::Color32,
    highlight_color: egui::Color32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let chars: Vec<char> = label.chars().collect();

    let is_matched = |idx: usize| matched_indices.contains(&idx);

    let mut i = 0;
    while i < chars.len() {
        let start = i;
        let match_state = is_matched(i);
        while i < chars.len() && is_matched(i) == match_state {
            i += 1;
        }
        let chunk: String = chars[start..i].iter().collect();
        let color = if match_state {
            highlight_color
        } else {
            default_color
        };
        job.append(
            &chunk,
            0.0,
            egui::TextFormat {
                font_id: font_id.clone(),
                color,
                italics: match_state,
                ..Default::default()
            },
        );
    }
    job
}

fn completion_item_row(
    ui: &mut Ui,
    item: &LspCompletionItem,
    matched_indices: &[usize],
    selected: bool,
    palette: SemanticPalette,
) -> Response {
    let row_width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(row_width, ROW_HEIGHT), Sense::click());
    if ui.is_rect_visible(rect) {
        if selected {
            let selection = ui.visuals().selection;
            ui.painter()
                .rect(rect, 0.0, selection.bg_fill, selection.stroke);
        }
        ui.allocate_ui_at_rect(rect, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(completion_kind_color(item.kind.as_deref(), palette), "●");

                let text_color = if selected {
                    ui.visuals().strong_text_color()
                } else {
                    ui.visuals().text_color()
                };
                let highlight_color = palette.completion_keyword;
                let font_id = egui::TextStyle::Body.resolve(ui.style());

                let label_job = highlighted_label_layout_job(
                    &item.label,
                    matched_indices,
                    font_id,
                    text_color,
                    highlight_color,
                );
                ui.add(Label::new(label_job).truncate(true));

                if let Some(detail) = &item.detail {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(Label::new(RichText::new(detail).weak().small()).truncate(true));
                    });
                }
            });
        });
    }
    response
}

fn clamp_popup_x(anchor_left: f32, available: Rect) -> f32 {
    let max_x = (available.right() - POPUP_MIN_WIDTH).max(available.left());
    anchor_left.clamp(available.left(), max_x)
}

/// Prefer below the caret; flip above when the popup would not fit underneath.
fn completion_popup_position(anchor: Rect, available: Rect, estimated_height: f32) -> Pos2 {
    let x = clamp_popup_x(anchor.left(), available);
    let below_y = anchor.left_bottom().y + POPUP_VERTICAL_GAP;
    let space_below = available.bottom() - below_y;
    if space_below >= estimated_height {
        pos2(x, below_y)
    } else {
        pos2(
            x,
            anchor.left_top().y - POPUP_VERTICAL_GAP - estimated_height,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum MatchCategory {
    ExactCaseSensitive = 0,
    ExactCaseInsensitive = 1,
    PrefixCaseSensitive = 2,
    PrefixCaseInsensitive = 3,
    ContiguousSubstringCaseSensitive = 4,
    ContiguousSubstringCaseInsensitive = 5,
    Subsequence = 6,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AlignmentScore {
    gaps: usize,
    first_idx: usize,
    neg_boundary_matches: isize,
    neg_consecutive_matches: isize,
    span: usize,
}

impl Ord for AlignmentScore {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.gaps
            .cmp(&other.gaps)
            .then(self.first_idx.cmp(&other.first_idx))
            .then(self.neg_boundary_matches.cmp(&other.neg_boundary_matches))
            .then(
                self.neg_consecutive_matches
                    .cmp(&other.neg_consecutive_matches),
            )
            .then(self.span.cmp(&other.span))
    }
}

impl PartialOrd for AlignmentScore {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MatchScore {
    category: MatchCategory,
    alignment: AlignmentScore,
    target_len: usize,
    original_index: usize,
}

impl Ord for MatchScore {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.category
            .cmp(&other.category)
            .then(self.alignment.cmp(&other.alignment))
            .then(self.target_len.cmp(&other.target_len))
            .then(self.original_index.cmp(&other.original_index))
    }
}

impl PartialOrd for MatchScore {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

fn is_boundary(target: &[char], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let curr = target[i];
    let prev = target[i - 1];

    if curr.is_uppercase() && prev.is_lowercase() {
        return true;
    }

    if prev == '_'
        || prev == '-'
        || prev == '.'
        || prev == '/'
        || prev == '\\'
        || prev == ':'
        || prev.is_whitespace()
    {
        return true;
    }

    false
}

fn chars_eq_case_insensitive(a: char, b: char) -> bool {
    if a == b {
        return true;
    }
    a.to_lowercase().eq(b.to_lowercase())
}

fn is_subsequence_case_insensitive(query: &[char], target: &[char]) -> bool {
    let mut q_idx = 0;
    for &t_char in target {
        if q_idx < query.len() {
            if chars_eq_case_insensitive(query[q_idx], t_char) {
                q_idx += 1;
            }
        } else {
            break;
        }
    }
    q_idx == query.len()
}

fn find_best_alignment(query: &[char], target: &[char]) -> Option<(Vec<usize>, AlignmentScore)> {
    if query.is_empty() || target.is_empty() || query.len() > target.len() {
        return None;
    }

    let mut memo = vec![vec![None; target.len()]; query.len()];

    fn search(
        q_idx: usize,
        t_idx: usize,
        query: &[char],
        target: &[char],
        memo: &mut [Vec<Option<Option<(Vec<usize>, AlignmentScore)>>>],
    ) -> Option<(Vec<usize>, AlignmentScore)> {
        if q_idx == query.len() {
            return Some((
                Vec::new(),
                AlignmentScore {
                    gaps: 0,
                    first_idx: 0,
                    neg_boundary_matches: 0,
                    neg_consecutive_matches: 0,
                    span: 0,
                },
            ));
        }
        if t_idx == target.len() {
            return None;
        }

        if let Some(cached) = &memo[q_idx][t_idx] {
            return cached.clone();
        }

        let mut best: Option<(Vec<usize>, AlignmentScore)> = None;

        // Option 1: Skip target[t_idx]
        if let Some((indices, score)) = search(q_idx, t_idx + 1, query, target, memo) {
            best = Some((indices, score));
        }

        // Option 2: Match target[t_idx]
        let q_char = query[q_idx];
        let t_char = target[t_idx];
        if chars_eq_case_insensitive(q_char, t_char) {
            if let Some((indices, sub_score)) = search(q_idx + 1, t_idx + 1, query, target, memo) {
                let is_first = q_idx == 0;
                let is_consecutive = !is_first && !indices.is_empty() && indices[0] == t_idx + 1;
                let is_gap = !is_first && !indices.is_empty() && indices[0] > t_idx + 1;

                let is_bound = is_boundary(target, t_idx);

                let boundary_val = if is_bound { 1 } else { 0 };
                let consecutive_val = if is_consecutive { 1 } else { 0 };
                let gap_val = if is_gap { 1 } else { 0 };

                let first_idx = if is_first { t_idx } else { sub_score.first_idx };
                let last_idx = if indices.is_empty() {
                    t_idx
                } else {
                    *indices.last().unwrap_or(&t_idx)
                };
                let span = last_idx - first_idx + 1;

                let new_score = AlignmentScore {
                    gaps: sub_score.gaps + gap_val,
                    first_idx,
                    neg_boundary_matches: sub_score.neg_boundary_matches - boundary_val,
                    neg_consecutive_matches: sub_score.neg_consecutive_matches - consecutive_val,
                    span,
                };

                let mut new_indices = Vec::with_capacity(indices.len() + 1);
                new_indices.push(t_idx);
                new_indices.extend(indices);

                let candidate = (new_indices, new_score);
                if let Some((_, ref best_score)) = best {
                    if candidate.1 < *best_score {
                        best = Some(candidate);
                    }
                } else {
                    best = Some(candidate);
                }
            }
        }

        memo[q_idx][t_idx] = Some(best.clone());
        best
    }

    search(0, 0, query, target, &mut memo)
}

fn completion_kind_color(kind: Option<&str>, palette: SemanticPalette) -> Color32 {
    match kind {
        Some("Function") | Some("Method") => palette.completion_function,
        Some("Struct") | Some("Enum") | Some("Interface") => palette.completion_type,
        Some("Field") | Some("Property") => palette.completion_field,
        Some("Variable") | Some("Constant") => palette.completion_variable,
        Some("Module") => palette.completion_module,
        Some("Keyword") => palette.completion_keyword,
        _ => palette.muted_text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_buffer(text: &str) -> TextBuffer {
        let mut buffer = TextBuffer::default();
        buffer.insert_at_cursor(text).unwrap();
        buffer
    }

    #[test]
    fn completion_popup_model_blocks_pointer_hover_when_open() {
        assert!(!CompletionPopupModel::closed().blocks_pointer_hover());
        assert!(CompletionPopupModel::open().blocks_pointer_hover());
        assert!(CompletionPopupModel::from_open(true).blocks_pointer_hover());
    }

    #[test]
    fn empty_results_do_not_open_the_popup() {
        let path = PathBuf::from("src/main.rs");
        let buffer = test_buffer("fn main() {}\n");
        let mut state = CompletionState::default();
        state.begin_session(CompletionSession {
            path: path.clone(),
            request_id: 1,
            revision: buffer.revision(),
            lsp_version: buffer.lsp_version,
            cursor: buffer.cursor(),
            prefix_char_range: 0..0,
        });
        assert!(
            state.is_open(),
            "loading may show progress until results arrive"
        );

        assert!(state.try_accept_response(1, vec![], &path, &buffer));
        assert!(!state.is_open());
        assert!(state.popup().items.is_empty());
        assert!(!state.popup().loading);
    }

    #[test]
    fn stale_responses_are_ignored() {
        let path = PathBuf::from("src/main.rs");
        let buffer = test_buffer("fn main() {}\n");
        let mut state = CompletionState::default();
        state.begin_session(CompletionSession {
            path: path.clone(),
            request_id: 1,
            revision: buffer.revision(),
            lsp_version: buffer.lsp_version,
            cursor: buffer.cursor(),
            prefix_char_range: 0..0,
        });

        let item = LspCompletionItem {
            label: "main".to_owned(),
            ..Default::default()
        };

        assert!(!state.try_accept_response(2, vec![item.clone()], &path, &buffer));
        assert!(state.popup().loading);
        assert!(state.popup().items.is_empty());

        let mut edited = test_buffer("fn main() {}\n");
        edited.insert_at_cursor("x").unwrap();
        assert!(!state.try_accept_response(1, vec![item], &path, &edited));
        assert!(state.popup().loading);
        assert!(state.popup().items.is_empty());
    }

    #[test]
    fn popup_area_id_incorporates_file_and_request() {
        let path = PathBuf::from("src/main.rs");
        let mut state = CompletionState::default();
        state.begin_session(CompletionSession {
            path: path.clone(),
            request_id: 7,
            revision: 0,
            lsp_version: 1,
            cursor: CursorPosition { line: 0, col: 0 },
            prefix_char_range: 0..0,
        });

        let id = state.popup_area_id();
        assert_eq!(id, Id::new(("lsp_completion_popup", &path, 7_u64)));
        assert_ne!(id, Id::new(("lsp_completion_popup", &path, 8_u64)));
    }

    #[test]
    fn popup_prefers_below_caret_when_space_allows() {
        let anchor = Rect::from_min_max(pos2(100.0, 200.0), pos2(108.0, 220.0));
        let available = Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 600.0));
        let pos = completion_popup_position(anchor, available, 100.0);
        assert_eq!(pos, pos2(100.0, 224.0));
    }

    #[test]
    fn popup_clamps_horizontally_to_viewport() {
        let anchor = Rect::from_min_max(pos2(350.0, 200.0), pos2(358.0, 220.0));
        let available = Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 600.0));
        let pos = completion_popup_position(anchor, available, 100.0);
        let max_x = (400.0 - POPUP_MIN_WIDTH).max(0.0);
        assert_eq!(pos.x, max_x);
        assert_eq!(pos.y, 224.0);
    }

    #[test]
    fn popup_flips_above_caret_when_space_below_is_tight() {
        let anchor = Rect::from_min_max(pos2(100.0, 500.0), pos2(108.0, 520.0));
        let available = Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 540.0));
        let estimated_height = MAX_POPUP_HEIGHT + POPUP_CHROME_HEIGHT;
        let pos = completion_popup_position(anchor, available, estimated_height);
        assert_eq!(
            pos,
            pos2(100.0, 500.0 - POPUP_VERTICAL_GAP - estimated_height)
        );
    }

    #[test]
    fn selection_clamps_at_list_ends() {
        let mut popup = CompletionPopup {
            items: vec![
                LspCompletionItem {
                    label: "a".to_owned(),
                    ..Default::default()
                },
                LspCompletionItem {
                    label: "b".to_owned(),
                    ..Default::default()
                },
            ],
            selected: 0,
            loading: false,
            ..Default::default()
        };
        popup.refilter();

        popup.move_selection(-1);
        assert_eq!(popup.selected, 0);
        popup.move_selection(1);
        assert_eq!(popup.selected, 1);
        popup.move_selection(1);
        assert_eq!(popup.selected, 1);
        popup.move_selection(-5);
        assert_eq!(popup.selected, 0);
    }

    fn sample_item(label: &str) -> LspCompletionItem {
        LspCompletionItem {
            label: label.to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn insert_text_falls_back_correctly_at_acceptance_time() {
        use crate::lsp::types::LspTextEdit;

        let mut item = LspCompletionItem {
            label: "main".to_owned(),
            insert_text: Some("inserted".to_owned()),
            ..Default::default()
        };
        assert_eq!(completion_acceptance_insert_text(&item), "inserted");

        item.insert_text = None;
        assert_eq!(completion_acceptance_insert_text(&item), "main");

        item.insert_text = Some(String::new());
        assert_eq!(completion_acceptance_insert_text(&item), "main");

        item.label = "println!".to_owned();
        item.insert_text = Some("println!(\"{}\", )".to_owned());
        item.detail = Some("macro".to_owned());
        assert_eq!(
            completion_acceptance_insert_text(&item),
            "println!(\"{}\", )"
        );

        item.insert_text = Some("ignored-on-text-edit-path".to_owned());
        item.text_edit = Some(LspTextEdit {
            line_start: 0,
            col_start: 0,
            line_end: 0,
            col_end: 0,
            new_text: "println!()".to_owned(),
        });
        assert_eq!(
            completion_acceptance_insert_text(&item),
            "ignored-on-text-edit-path",
            "helper still resolves plain-path text; text_edit precedence is enforced in apply_completion_insertion"
        );
    }

    fn simulate_popup_click(
        ctx: &Context,
        state: &mut CompletionState,
        anchor: Rect,
        palette: SemanticPalette,
        click_pos: Pos2,
        screen: Rect,
    ) -> CompletionPopupOutput {
        let modifiers = egui::Modifiers::NONE;
        let frames = [
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
                    button: egui::PointerButton::Primary,
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
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers,
                }],
                modifiers,
                ..Default::default()
            },
        ];

        let mut output = CompletionPopupOutput::default();
        for input in frames {
            let _ = ctx.run(input, |ctx| {
                output = state.show(ctx, anchor, palette);
            });
        }
        output
    }

    #[test]
    fn clicking_completion_row_selects_and_accepts() {
        use crate::settings::Theme;
        use crate::theme::built_in_theme;

        let palette = built_in_theme(Theme::Dark, None).palette.semantic;
        let mut state = CompletionState {
            popup: CompletionPopup {
                items: vec![
                    sample_item("alpha"),
                    sample_item("beta"),
                    sample_item("gamma"),
                ],
                selected: 0,
                loading: false,
                ..Default::default()
            },
            ..Default::default()
        };
        state.popup.refilter();

        let anchor = Rect::from_min_max(pos2(100.0, 200.0), pos2(108.0, 220.0));
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
        let ctx = Context::default();

        let mut layout_output = CompletionPopupOutput::default();
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                layout_output = state.show(ctx, anchor, palette);
            },
        );
        assert!(layout_output.row_hit_rects.len() > 1);

        let output = simulate_popup_click(
            &ctx,
            &mut state,
            anchor,
            palette,
            layout_output.row_hit_rects[1].center(),
            screen,
        );

        assert_eq!(
            output.event,
            Some(CompletionPopupEvent::Accepted { index: 1 })
        );
        assert_eq!(state.popup().selected, 1);
    }

    #[test]
    fn escape_dismisses_without_editing() {
        let mut state = CompletionState {
            popup: CompletionPopup {
                items: vec![sample_item("alpha")],
                selected: 0,
                loading: false,
                ..Default::default()
            },
            ..Default::default()
        };
        state.popup.refilter();

        let ctx = Context::default();
        let _ = ctx.run(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ctx| {
                let (consumed, event) = state.poll_keyboard_event(ctx);
                assert!(consumed);
                assert_eq!(event, Some(CompletionPopupEvent::Dismissed));
            },
        );
    }

    #[test]
    fn enter_and_tab_accept() {
        for key in [Key::Enter, Key::Tab] {
            let mut state = CompletionState {
                popup: CompletionPopup {
                    items: vec![sample_item("alpha"), sample_item("beta")],
                    selected: 1,
                    loading: false,
                    ..Default::default()
                },
                ..Default::default()
            };
            state.popup.refilter();

            let ctx = Context::default();
            let _ = ctx.run(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ctx| {
                    let (consumed, event) = state.poll_keyboard_event(ctx);
                    assert!(consumed, "{key:?} should be consumed");
                    assert_eq!(event, Some(CompletionPopupEvent::Accepted { index: 1 }));
                },
            );
        }
    }

    #[test]
    fn outside_click_emits_dismissed_event() {
        let popup_rect = Rect::from_min_max(pos2(100.0, 200.0), pos2(380.0, 360.0));
        let outside_pos = pos2(10.0, 10.0);
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
        let ctx = Context::default();

        let mut event = None;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                events: vec![egui::Event::PointerButton {
                    pos: outside_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ctx| {
                event = completion_outside_click_event(ctx, popup_rect);
            },
        );

        assert_eq!(event, Some(CompletionPopupEvent::Dismissed));
    }

    #[test]
    fn pressing_inside_popup_does_not_dismiss_before_release() {
        use crate::settings::Theme;
        use crate::theme::built_in_theme;

        let palette = built_in_theme(Theme::Dark, None).palette.semantic;
        let mut state = CompletionState {
            popup: CompletionPopup {
                items: vec![sample_item("alpha"), sample_item("beta")],
                selected: 0,
                loading: false,
                ..Default::default()
            },
            ..Default::default()
        };
        state.popup.refilter();

        let anchor = Rect::from_min_max(pos2(100.0, 200.0), pos2(108.0, 220.0));
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0));
        let ctx = Context::default();

        let mut layout_output = CompletionPopupOutput::default();
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                layout_output = state.show(ctx, anchor, palette);
            },
        );
        let click_pos = layout_output.row_hit_rects[1].center();
        let modifiers = egui::Modifiers::NONE;
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
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers,
                }],
                modifiers,
                ..Default::default()
            },
        ] {
            let mut output = CompletionPopupOutput::default();
            let _ = ctx.run(input, |ctx| {
                output = state.show(ctx, anchor, palette);
            });
            assert!(state.is_open());
            assert_eq!(output.event, None);
            assert!(output.popup_rect.is_positive());
        }
    }
}
