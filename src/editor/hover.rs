//! LSP documentation hover: popup rendering, request-session types, app-layer gates,
//! and display policy for parsed hover text (raw JSON / debug output rejection).
//!
//! Hover sessions and `LspClient` live in `app.rs`. This module emits
//! `HoverPopupEvent` (dismissed) and renders documentation popups; it does not
//! own or call `LspClient`. Wire flattening lives in `lsp/transport.rs` (see hover
//! parsing considerations there). Does not perform editor hit-testing, diagnostic
//! tooltips (`widget.rs`), or JSON-RPC framing.
//!
//! Tests: `cargo test --lib editor::hover` (display policy, popup layout, gates).
//! | Pointer hover e2e (app) | `pointer_hover_sends_a_debounced_real_lsp_hover_request_and_displays_documentation` | `cargo test --lib pointer_hover_sends_a_debounced_real` |
//! | Diagnostic vs LSP hover coexistence (app) | `diagnostic_tooltips_and_lsp_hover_coexist_according_to_the_specified_precedence` | `cargo test --lib diagnostic_tooltips_and_lsp_hover` |
//! | Stale async responses (app) | `stale_asynchronous_responses_cannot_affect_the_current_editor_state` | `cargo test --lib stale_asynchronous_responses_cannot` |
//! | Hover debounce delay | `HOVER_REST_DELAY_SECS` | `cargo test --lib hover_request_is_debounced` |
//! | Never: replace diagnostic tooltips with LSP hover | `replace_diagnostic_tooltips_with_lsp_hover` | `cargo test --lib replace_diagnostic_tooltips_with_lsp_hover` |
//! | Never: hard-coded completion/hover mock data | `render_completion_or_hover_using_hard_coded_mock_data` | `cargo test --lib render_completion_or_hover_using_hard_coded_mock_data` |
//! | Long docs wrap/scroll in window | `long_hover_documentation_wraps_or_scrolls_within_the_window` | `cargo test --lib long_hover_documentation_wraps_or_scrolls` |
//! Wire-shape regressions: `cargo test --lib lsp::transport::tests::hover`.
//! Popup outside-click tests use `egui::RawInput`; see **Editor/UI state tests** in
//! `editor/widget.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use egui::{
    pos2, text::LayoutJob, Context, FontId, Id, Label, Order, Pos2, Rect, RichText, ScrollArea,
    TextFormat, Ui, Vec2,
};

use crate::editor::buffer::{CursorPosition, TextBuffer};
use crate::editor::completion::CompletionPopupModel;
use crate::editor::position::LspPosition;

/// Resting pointer over rendered source glyphs (not gutter, not past line end).
///
/// Logical `position` is a Rust character index; `app.rs` encodes it to UTF-16
/// for LSP hover requests;
/// `token_rect` anchors the documentation popup in screen space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HoveredSourcePosition {
    pub position: CursorPosition,
    pub token_rect: Rect,
}

impl HoveredSourcePosition {
    pub fn cursor_position(self) -> CursorPosition {
        self.position
    }

    #[cfg(test)]
    pub fn for_test(position: CursorPosition, token_rect: Rect) -> Self {
        Self {
            position,
            token_rect,
        }
    }
}

/// Per-frame LSP hover popup handoff from the editor widget to the app shell.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HoverPopupModel {
    pub hovered_source: Option<HoveredSourcePosition>,
    pub diagnostic_tooltip_active: bool,
}

impl HoverPopupModel {
    pub const fn none() -> Self {
        Self {
            hovered_source: None,
            diagnostic_tooltip_active: false,
        }
    }

    pub fn popup_anchor(self) -> Option<Rect> {
        self.hovered_source.map(|source| source.token_rect)
    }

    pub const fn allows_lsp_hover(self) -> bool {
        !self.diagnostic_tooltip_active
    }
}

/// True when hover text looks like raw JSON or Rust debug output — never show in the popup.
pub(crate) fn is_undisplayable_hover_text(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.contains("\"jsonrpc\"")
        && (trimmed.contains("\"result\"") || trimmed.contains("\"error\""))
    {
        return true;
    }
    if looks_like_rust_debug_hover_text(trimmed) {
        return true;
    }
    looks_like_raw_json_hover_text(trimmed)
}

fn looks_like_raw_json_hover_text(text: &str) -> bool {
    match text.chars().next() {
        Some('{') | Some('[') => serde_json::from_str::<serde_json::Value>(text).is_ok(),
        _ => false,
    }
}

fn looks_like_rust_debug_hover_text(text: &str) -> bool {
    text.starts_with("Some(")
        || text.starts_with("Ok(")
        || text.starts_with("Err(")
        || text.starts_with("None")
        || text.starts_with("Hover {")
        || text.starts_with("HoverResult {")
        || text.starts_with("Value::")
        || text.starts_with("Value {")
}

pub const HOVER_REST_DELAY_SECS: f64 = 0.35;

/// LSP hover may appear only when the diagnostic tooltip is not active this frame.
pub fn lsp_hover_allowed(diagnostic_tooltip_active: bool) -> bool {
    !diagnostic_tooltip_active
}

/// App-layer gates that suppress deferred LSP hover even when the editor reports source text.
///
/// Completion is checked first because it has highest priority over all other hover UI.
pub fn apply_lsp_hover_gates(
    handoff: HoverPopupModel,
    completion_popup: CompletionPopupModel,
    blocked_by_other_overlay: bool,
) -> Option<HoveredSourcePosition> {
    if completion_popup.blocks_pointer_hover()
        || blocked_by_other_overlay
        || !handoff.allows_lsp_hover()
    {
        None
    } else {
        handoff.hovered_source
    }
}

pub const HOVER_POPUP_VERTICAL_GAP: f32 = 4.0;
pub const HOVER_POPUP_MIN_WIDTH: f32 = 280.0;
pub const HOVER_POPUP_MAX_WIDTH: f32 = 480.0;
/// Scrollable documentation body cap (prose and fenced code blocks).
pub const HOVER_POPUP_MAX_BODY_HEIGHT: f32 = 320.0;
/// Heading, separator, frame margins, and hint space beyond the scroll body.
const HOVER_POPUP_CHROME_HEIGHT: f32 = 40.0;
pub const HOVER_POPUP_MAX_HEIGHT: f32 = HOVER_POPUP_MAX_BODY_HEIGHT + HOVER_POPUP_CHROME_HEIGHT;
/// Monospace size for fenced ``` code blocks in hover documentation.
pub const HOVER_FENCED_CODE_FONT_SIZE: f32 = 12.0;

fn hover_fenced_code_font() -> FontId {
    FontId::monospace(HOVER_FENCED_CODE_FONT_SIZE)
}

fn hover_fenced_code_label_text(code: &str) -> RichText {
    RichText::new(code).font(hover_fenced_code_font())
}

/// User-driven hover popup lifecycle event (read-only documentation popup).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoverPopupEvent {
    Dismissed,
}

/// Outside-click dismiss when the pointer press lands outside the rendered popup bounds.
pub fn hover_outside_click_event(
    ctx: &Context,
    popup_rect: Option<Rect>,
) -> Option<HoverPopupEvent> {
    let clicked_outside = ctx.input(|input| {
        input.pointer.any_pressed()
            && input.pointer.interact_pos().is_none_or(|pos| {
                popup_rect.is_none_or(|rect| !rect.is_positive() || !rect.contains(pos))
            })
    });
    clicked_outside.then_some(HoverPopupEvent::Dismissed)
}

/// Screen-space bounds of the hover popup for one rendered frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoverPopupOutput {
    pub popup_rect: Rect,
}

impl Default for HoverPopupOutput {
    fn default() -> Self {
        Self {
            popup_rect: Rect::NOTHING,
        }
    }
}

/// In-flight hover request context used to reject stale LSP responses.
#[derive(Debug, Clone, PartialEq)]
pub struct HoverRequestSession {
    pub request_id: u64,
    pub path: PathBuf,
    pub position: LspPosition,
    pub revision: u64,
    pub lsp_version: i32,
    /// egui time when the pointer entered this logical source position.
    pub position_entered_at: f64,
    /// Screen-space bounds of the hovered token when the request was sent.
    pub popup_anchor: Rect,
}

/// Effective placement bounds: editor viewport clipped to the window.
pub fn hover_popup_bounds(editor_viewport: Rect, window_available: Rect) -> Rect {
    let intersection = editor_viewport.intersect(window_available);
    if intersection.width() > 0.0 && intersection.height() > 0.0 {
        intersection
    } else {
        window_available
    }
}

pub fn hover_popup_estimated_size(bounds: Rect) -> Vec2 {
    Vec2::new(
        HOVER_POPUP_MAX_WIDTH.min(bounds.width().max(0.0)),
        HOVER_POPUP_MAX_HEIGHT.min(bounds.height().max(0.0)),
    )
}

fn apply_hover_popup_size_limits(ui: &mut Ui, bounds: Rect) {
    let max_width = HOVER_POPUP_MAX_WIDTH.min(bounds.width().max(0.0));
    ui.set_min_width(HOVER_POPUP_MIN_WIDTH.min(max_width));
    ui.set_max_width(max_width);
    let max_height = HOVER_POPUP_MAX_HEIGHT.min(bounds.height().max(0.0));
    if max_height > 0.0 {
        ui.set_max_height(max_height);
    }
}

/// Prefer below the hovered token; flip above or shift sideways to avoid covering it.
pub fn hover_popup_position(anchor: Rect, bounds: Rect, popup_size: Vec2) -> Pos2 {
    let popup_width = popup_size.x.min(bounds.width().max(0.0));
    let popup_height = popup_size.y.min(bounds.height().max(0.0));
    let size = Vec2::new(popup_width.max(0.0), popup_height.max(0.0));

    if size.x <= 0.0 || size.y <= 0.0 {
        return bounds.left_top();
    }

    let x = clamp_hover_popup_x(anchor.left(), bounds, size.x);
    let below_y = anchor.bottom() + HOVER_POPUP_VERTICAL_GAP;
    let above_y = anchor.top() - HOVER_POPUP_VERTICAL_GAP - size.y;

    let below = popup_rect_at(pos2(x, below_y), size);
    if rect_fits_in_bounds(below, bounds) && !below.intersects(anchor) {
        return below.min;
    }

    let above = popup_rect_at(pos2(x, above_y), size);
    if rect_fits_in_bounds(above, bounds) && !above.intersects(anchor) {
        return above.min;
    }

    if let Some(pos) = short_vertical_placement(anchor, bounds, x, size, true) {
        return pos;
    }
    if let Some(pos) = short_vertical_placement(anchor, bounds, x, size, false) {
        return pos;
    }

    let beside_right = popup_rect_at(
        pos2(anchor.right() + HOVER_POPUP_VERTICAL_GAP, anchor.top()),
        size,
    );
    if rect_fits_in_bounds(beside_right, bounds) && !beside_right.intersects(anchor) {
        return beside_right.min;
    }

    let beside_left = popup_rect_at(
        pos2(
            anchor.left() - HOVER_POPUP_VERTICAL_GAP - size.x,
            anchor.top(),
        ),
        size,
    );
    if rect_fits_in_bounds(beside_left, bounds) && !beside_left.intersects(anchor) {
        return beside_left.min;
    }

    let space_below = bounds.bottom() - below_y;
    let space_above = above_y - bounds.top();
    let prefer_below = space_below >= space_above;
    let mut y = if prefer_below { below_y } else { above_y };
    y = y.clamp(bounds.top(), (bounds.bottom() - size.y).max(bounds.top()));
    let mut pos = pos2(x, y);
    let rect = popup_rect_at(pos, size);

    if rect.intersects(anchor) {
        let right_x = anchor.right() + HOVER_POPUP_VERTICAL_GAP;
        if right_x + size.x <= bounds.right() {
            pos.x = right_x;
        } else {
            pos.x = (anchor.left() - HOVER_POPUP_VERTICAL_GAP - size.x).max(bounds.left());
        }
    }

    pos.x = pos
        .x
        .clamp(bounds.left(), (bounds.right() - size.x).max(bounds.left()));
    pos.y = pos
        .y
        .clamp(bounds.top(), (bounds.bottom() - size.y).max(bounds.top()));
    pos
}

fn short_vertical_placement(
    anchor: Rect,
    bounds: Rect,
    x: f32,
    max_size: Vec2,
    below: bool,
) -> Option<Pos2> {
    let width = max_size.x.min(bounds.width().max(0.0));
    if width <= 0.0 {
        return None;
    }

    let (y, available_height) = if below {
        let y = anchor.bottom() + HOVER_POPUP_VERTICAL_GAP;
        (y, bounds.bottom() - y)
    } else {
        let available = anchor.top() - HOVER_POPUP_VERTICAL_GAP - bounds.top();
        let height = available.min(max_size.y).max(0.0);
        let y = anchor.top() - HOVER_POPUP_VERTICAL_GAP - height;
        (y, available)
    };

    let height = available_height.min(max_size.y).max(0.0);
    if height <= 0.0 {
        return None;
    }

    let rect = popup_rect_at(pos2(x, y), Vec2::new(width, height));
    if rect_fits_in_bounds(rect, bounds) && !rect.intersects(anchor) {
        Some(rect.min)
    } else {
        None
    }
}

fn popup_rect_at(pos: Pos2, size: Vec2) -> Rect {
    Rect::from_min_size(pos, size)
}

fn rect_fits_in_bounds(rect: Rect, bounds: Rect) -> bool {
    rect.min.x >= bounds.min.x
        && rect.min.y >= bounds.min.y
        && rect.max.x <= bounds.max.x
        && rect.max.y <= bounds.max.y
}

fn clamp_hover_popup_x(anchor_left: f32, bounds: Rect, popup_width: f32) -> f32 {
    let max_x = (bounds.right() - popup_width).max(bounds.left());
    anchor_left.clamp(bounds.left(), max_x)
}

impl HoverRequestSession {
    pub fn matches_request(&self, id: u64) -> bool {
        self.request_id == id
    }

    /// True when a newer hover request has replaced the in-flight session.
    pub fn is_superseded_response(&self, id: u64) -> bool {
        self.request_id != id
    }

    pub fn matches_lsp_position(&self, path: &Path, position: LspPosition) -> bool {
        self.path == path && self.position == position
    }

    pub fn pointer_still_at_requested_position(&self, path: &Path, position: LspPosition) -> bool {
        self.matches_lsp_position(path, position)
    }

    /// The hover request's file is still the active editor tab.
    pub fn matches_active_tab(&self, active: &Path) -> bool {
        self.path == active
    }

    pub fn matches_active_file(&self, active: &Path) -> bool {
        self.matches_active_tab(active)
    }

    pub fn matches_buffer_revision(&self, buffer: &TextBuffer) -> bool {
        buffer.revision() == self.revision
    }

    pub fn buffer_snapshot_matches(&self, buffer: &TextBuffer) -> bool {
        self.matches_buffer_revision(buffer) && buffer.lsp_version == self.lsp_version
    }

    pub fn is_stale_for(&self, active: &Path, resting: LspPosition, buffer: &TextBuffer) -> bool {
        active != self.path || resting != self.position || !self.buffer_snapshot_matches(buffer)
    }

    pub fn pointer_still_resting_since_entry(&self, position_entered_at: Option<f64>) -> bool {
        position_entered_at == Some(self.position_entered_at)
    }
}

/// Buffer snapshot captured when accepted hover content is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoverContentSnapshot {
    pub revision: u64,
    pub lsp_version: i32,
}

impl HoverContentSnapshot {
    pub fn from_session(session: &HoverRequestSession) -> Self {
        Self {
            revision: session.revision,
            lsp_version: session.lsp_version,
        }
    }

    pub fn matches_buffer(&self, buffer: &TextBuffer) -> bool {
        buffer.revision() == self.revision && buffer.lsp_version == self.lsp_version
    }
}

pub fn show_hover_loading(
    ctx: &Context,
    anchor: Rect,
    editor_viewport: Option<Rect>,
) -> HoverPopupOutput {
    let output = show_hover_popup(ctx, anchor, editor_viewport, |ui| {
        add_wrapped_hover_label(
            ui,
            RichText::new("Loading documentation from rust-analyzer…").weak(),
        );
    });
    ctx.request_repaint_after(Duration::from_millis(100));
    output
}

pub fn show_hover_documentation(
    ctx: &Context,
    content: &str,
    anchor: Rect,
    editor_viewport: Option<Rect>,
) -> HoverPopupOutput {
    if content.is_empty() || is_undisplayable_hover_text(content) {
        return HoverPopupOutput::default();
    }

    show_hover_popup(ctx, anchor, editor_viewport, |ui| {
        ui.heading("Documentation");
        ui.separator();
        show_hover_documentation_body(ui, content);
    })
}

fn hover_documentation_scroll_area() -> ScrollArea {
    ScrollArea::vertical()
        .id_source("lsp_hover_documentation_body")
        .max_height(HOVER_POPUP_MAX_BODY_HEIGHT)
        // Keep the body height capped; shrink when the content is shorter.
        .auto_shrink([false, true])
}

fn show_hover_documentation_body(ui: &mut Ui, content: &str) {
    hover_documentation_scroll_area().show(ui, |ui| {
        ui.set_width(ui.available_width());
        render_hover_markdown(ui, content);
    });
}

fn show_hover_popup(
    ctx: &Context,
    anchor: Rect,
    editor_viewport: Option<Rect>,
    add_contents: impl FnOnce(&mut Ui),
) -> HoverPopupOutput {
    let mut popup_rect = Rect::NOTHING;
    let window = ctx.available_rect();
    let editor = editor_viewport.unwrap_or(window);
    let bounds = hover_popup_bounds(editor, window);
    let popup_size = hover_popup_estimated_size(bounds);
    let popup_pos = hover_popup_position(anchor, bounds, popup_size);
    egui::Area::new(Id::new("lsp_hover_popup"))
        .order(Order::Foreground)
        .fixed_pos(popup_pos)
        .constrain(true)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(10.0)
                .show(ui, |ui| {
                    apply_hover_popup_size_limits(ui, bounds);
                    add_contents(ui);
                });
            popup_rect = ui.min_rect();
        });
    HoverPopupOutput { popup_rect }
}

fn add_wrapped_hover_label(ui: &mut Ui, text: impl Into<egui::WidgetText>) {
    ui.add(Label::new(text).wrap(true));
}

fn hover_body_font(ui: &Ui) -> FontId {
    ui.style().text_styles[&egui::TextStyle::Body].clone()
}

/// Minimum markdown support for hover docs: prose segments and fenced code blocks.
/// No external markdown crate — split on ``` only.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HoverMarkdownSegment {
    Prose(String),
    Code(String),
}

fn is_hover_fence_language_tag(line: &str) -> bool {
    !line.is_empty()
        && line
            .chars()
            .all(|c| c.is_ascii_alphabetic() || c == '_' || c == '-' || c.is_ascii_digit())
}

fn strip_hover_fence_language_tag(part: &str) -> &str {
    if let Some((first, rest)) = part.split_once('\n') {
        if is_hover_fence_language_tag(first) {
            return rest;
        }
    }
    part
}

fn split_hover_markdown_segments(content: &str) -> Vec<HoverMarkdownSegment> {
    let parts: Vec<&str> = content.split("```").collect();
    let mut segments = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index % 2 == 1 {
            let code = strip_hover_fence_language_tag(part).trim_end();
            segments.push(HoverMarkdownSegment::Code(code.to_string()));
        } else if !part.is_empty() {
            segments.push(HoverMarkdownSegment::Prose(part.to_string()));
        }
    }
    segments
}

fn is_hover_horizontal_rule(line: &str) -> bool {
    matches!(line.trim(), "---" | "***" | "___")
}

fn hover_heading_level(line: &str) -> usize {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return 0;
    }
    trimmed.chars().take_while(|ch| *ch == '#').count().min(6)
}

/// Strip common inline markdown markers rust-analyzer embeds in hover docs.
fn simplify_hover_inline_markdown(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '*' | '_' if chars.peek() == Some(&ch) => {
                chars.next();
            }
            '[' => {
                let link_text: String = chars.by_ref().take_while(|c| *c != ']').collect();
                if chars.next() == Some('(') {
                    let _: String = chars.by_ref().take_while(|c| *c != ')').collect();
                    output.push_str(&link_text);
                } else {
                    output.push('[');
                    output.push_str(&link_text);
                }
            }
            other => output.push(other),
        }
    }
    output
}

fn hover_prose_layout_job(
    line: &str,
    body_font: &FontId,
    heading_font: &FontId,
    monospace: &FontId,
) -> LayoutJob {
    let heading_level = hover_heading_level(line);
    let prose = if heading_level > 0 {
        line.trim_start().trim_start_matches('#').trim_start()
    } else {
        line
    };

    let mut job = LayoutJob::default();
    let body_format = TextFormat {
        font_id: body_font.clone(),
        ..Default::default()
    };
    let heading_format = TextFormat {
        font_id: heading_font.clone(),
        ..Default::default()
    };
    let code_format = TextFormat {
        font_id: monospace.clone(),
        ..Default::default()
    };

    for (segment_index, segment) in prose.split('`').enumerate() {
        if segment_index % 2 == 1 {
            job.append(segment, 0.0, code_format.clone());
        } else if heading_level > 0 {
            job.append(
                &simplify_hover_inline_markdown(segment),
                0.0,
                heading_format.clone(),
            );
        } else {
            job.append(
                &simplify_hover_inline_markdown(segment),
                0.0,
                body_format.clone(),
            );
        }
    }
    job
}

fn hover_paragraph_break_height(ui: &Ui) -> f32 {
    // Blank lines in markdown prose denote paragraph breaks — make the gap visible.
    ui.spacing().item_spacing.y * 2.0
}

/// Blank lines in prose (from `\n\n` in flattened hover text) become paragraph breaks.
fn render_hover_prose(ui: &mut Ui, prose: &str, body_font: &FontId, monospace: &FontId) {
    let paragraph_gap = hover_paragraph_break_height(ui);
    let mut pending_blank_lines = 0usize;

    // Use split('\n') instead of lines() so trailing blank lines are preserved.
    for line in prose.split('\n') {
        if line.is_empty() {
            pending_blank_lines += 1;
            continue;
        }

        for _ in 0..pending_blank_lines {
            ui.add_space(paragraph_gap);
        }
        pending_blank_lines = 0;
        render_hover_prose_line(ui, line, body_font, monospace);
    }

    for _ in 0..pending_blank_lines {
        ui.add_space(paragraph_gap);
    }
}

fn render_hover_prose_line(ui: &mut Ui, line: &str, body_font: &FontId, monospace: &FontId) {
    if is_hover_horizontal_rule(line) {
        ui.separator();
        return;
    }

    let heading_font = ui.style().text_styles[&egui::TextStyle::Heading].clone();
    ui.add(
        Label::new(hover_prose_layout_job(
            line,
            body_font,
            &heading_font,
            monospace,
        ))
        .wrap(true),
    );
}

fn render_hover_markdown(ui: &mut Ui, content: &str) {
    let body_font = hover_body_font(ui);
    let inline_code_font = hover_fenced_code_font();
    for segment in split_hover_markdown_segments(content) {
        match segment {
            HoverMarkdownSegment::Code(code) => {
                ui.add_space(2.0);
                add_wrapped_hover_label(ui, hover_fenced_code_label_text(&code));
                ui.add_space(2.0);
            }
            HoverMarkdownSegment::Prose(prose) => {
                render_hover_prose(ui, &prose, &body_font, &inline_code_font);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::buffer::CursorPosition;
    use egui::{FontFamily, Vec2};

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn hover_delay_matches_debounce_spec() {
        assert!(HOVER_REST_DELAY_SECS >= 0.35);
        assert!(HOVER_REST_DELAY_SECS <= 0.5);
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn hover_popup_max_dimensions_are_reasonable() {
        assert!(HOVER_POPUP_MIN_WIDTH > 0.0);
        assert!(HOVER_POPUP_MIN_WIDTH <= HOVER_POPUP_MAX_WIDTH);
        assert!(HOVER_POPUP_MAX_BODY_HEIGHT > 0.0);
        assert!(HOVER_POPUP_MAX_HEIGHT > HOVER_POPUP_MAX_BODY_HEIGHT);
        assert_eq!(
            HOVER_POPUP_MAX_HEIGHT,
            HOVER_POPUP_MAX_BODY_HEIGHT + HOVER_POPUP_CHROME_HEIGHT
        );
    }

    #[test]
    fn hover_popup_estimated_size_respects_max_dimensions() {
        let huge = Rect::from_min_max(Pos2::ZERO, Pos2::new(2000.0, 2000.0));
        let size = hover_popup_estimated_size(huge);
        assert_eq!(size.x, HOVER_POPUP_MAX_WIDTH);
        assert_eq!(size.y, HOVER_POPUP_MAX_HEIGHT);
    }

    #[test]
    fn hover_popup_estimated_size_shrinks_inside_tight_bounds() {
        let tight = Rect::from_min_max(Pos2::ZERO, Pos2::new(240.0, 180.0));
        let size = hover_popup_estimated_size(tight);
        assert_eq!(size.x, 240.0);
        assert_eq!(size.y, 180.0);
    }

    #[test]
    fn hover_documentation_wraps_long_prose_within_popup_width() {
        let ctx = Context::default();
        let long_line = "word ".repeat(80);
        let mut content_width = None;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(HOVER_POPUP_MAX_WIDTH, HOVER_POPUP_MAX_HEIGHT),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.set_width(HOVER_POPUP_MAX_WIDTH);
                show_hover_documentation_body(ui, &long_line);
                content_width = Some(ui.min_rect().width());
            });
        });
        let width = content_width.expect("hover markdown should render");
        assert!(
            width <= HOVER_POPUP_MAX_WIDTH + 1.0,
            "long documentation should wrap instead of widening the popup"
        );
    }

    #[test]
    fn show_hover_documentation_skips_raw_json_and_debug_text() {
        assert!(is_undisplayable_hover_text(
            "{\"kind\":\"markdown\",\"value\":\"hello\"}"
        ));
        assert!(is_undisplayable_hover_text(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"contents\":\"docs\"}}"
        ));
        assert!(is_undisplayable_hover_text("Some(\"documentation\")"));
        assert!(!is_undisplayable_hover_text(
            "Returns the length.\n\n```rust\nfn main() {}\n```"
        ));
    }

    #[test]
    fn hover_unit_display_policy_treats_empty_as_displayable() {
        assert!(!is_undisplayable_hover_text(""));
        assert!(!is_undisplayable_hover_text("   "));
    }

    #[test]
    fn hover_unit_display_policy_rejects_debug_prefixes() {
        assert!(is_undisplayable_hover_text("Ok(\"docs\")"));
        assert!(is_undisplayable_hover_text("Err(\"failed\")"));
        assert!(is_undisplayable_hover_text("None"));
        assert!(is_undisplayable_hover_text("Value::String(\"x\")"));
    }

    #[test]
    fn hover_unit_display_policy_allows_brace_text_that_is_not_json() {
        assert!(!is_undisplayable_hover_text(
            "use std::collections::{HashMap, BTreeMap};"
        ));
    }

    #[test]
    fn hover_fenced_code_sections_use_monospace_styling() {
        let code_font = hover_fenced_code_font();
        assert_eq!(code_font.family, FontFamily::Monospace);
        assert_eq!(code_font.size, HOVER_FENCED_CODE_FONT_SIZE);
        assert_ne!(code_font.family, FontFamily::Proportional);

        let ctx = Context::default();
        let mut body_family = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                body_family = Some(hover_body_font(ui).family);
            });
        });
        assert_ne!(
            body_family.expect("body font"),
            FontFamily::Monospace,
            "prose should stay proportional while fenced code uses monospace"
        );
    }

    #[test]
    fn hover_markdown_minimum_splits_plain_lines_and_fenced_code_blocks() {
        let content = "Returns the number of elements.\n\n```rust\npub fn len(&self) -> usize\n```\n\nSee also `Vec`.";
        let segments = split_hover_markdown_segments(content);
        assert_eq!(segments.len(), 3);
        assert_eq!(
            segments[0],
            HoverMarkdownSegment::Prose("Returns the number of elements.\n\n".to_string())
        );
        assert_eq!(
            segments[1],
            HoverMarkdownSegment::Code("pub fn len(&self) -> usize".to_string())
        );
        assert_eq!(
            segments[2],
            HoverMarkdownSegment::Prose("\n\nSee also `Vec`.".to_string())
        );
    }

    #[test]
    fn hover_markdown_strips_fence_language_tag_without_markdown_crate() {
        assert_eq!(
            strip_hover_fence_language_tag("rust\npub fn main() {}\n"),
            "pub fn main() {}\n"
        );
        assert_eq!(
            strip_hover_fence_language_tag("pub fn main() {}\n"),
            "pub fn main() {}\n"
        );
    }

    fn measure_hover_documentation_body_height(content: &str) -> f32 {
        let ctx = Context::default();
        let mut height = 0.0;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(HOVER_POPUP_MAX_WIDTH, HOVER_POPUP_MAX_HEIGHT),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.set_width(HOVER_POPUP_MAX_WIDTH);
                let output = hover_documentation_scroll_area().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    render_hover_markdown(ui, content);
                });
                height = output.content_size.y;
            });
        });
        height
    }

    #[test]
    fn hover_documentation_preserves_paragraph_breaks() {
        let single_newline = "Paragraph one.\nParagraph two.";
        let paragraph_break = "Paragraph one.\n\nParagraph two.";
        let h_single = measure_hover_documentation_body_height(single_newline);
        let h_paragraph = measure_hover_documentation_body_height(paragraph_break);
        assert!(
            h_paragraph > h_single + 1.0,
            "a blank line between paragraphs should add visible vertical space"
        );

        let double_break = "Paragraph one.\n\n\nParagraph two.";
        let h_double = measure_hover_documentation_body_height(double_break);
        assert!(
            h_double > h_paragraph + 1.0,
            "multiple blank lines should add proportionally more space"
        );
    }

    #[test]
    fn hover_markdown_prose_segment_preserves_internal_paragraph_breaks() {
        let segments = split_hover_markdown_segments("Intro.\n\nDetails.");
        assert_eq!(
            segments,
            vec![HoverMarkdownSegment::Prose(
                "Intro.\n\nDetails.".to_string()
            )]
        );
    }

    #[test]
    fn hover_markdown_plain_prose_only_has_no_code_segments() {
        let segments = split_hover_markdown_segments("Plain documentation line.\nSecond line.");
        assert_eq!(
            segments,
            vec![HoverMarkdownSegment::Prose(
                "Plain documentation line.\nSecond line.".to_string()
            )]
        );
    }

    #[test]
    fn simplify_hover_inline_markdown_strips_rust_analyzer_markers() {
        assert_eq!(
            simplify_hover_inline_markdown("**Parameters**"),
            "Parameters"
        );
        assert_eq!(
            simplify_hover_inline_markdown("Read the [docs](https://doc.rust-lang.org)"),
            "Read the docs"
        );
        assert_eq!(
            simplify_hover_inline_markdown("Use `Vec` for growable storage"),
            "Use `Vec` for growable storage"
        );
    }

    #[test]
    fn hover_documentation_renders_rust_analyzer_payload_cleanly() {
        let ctx = Context::default();
        let content = "```rust\npub fn len(&self) -> usize\n```\n\n\
            ## Parameters\n\n\
            - `self`: the collection\n\n\
            ---\n\n\
            Returns the number of elements.";
        let mut rendered_lines = 0usize;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(HOVER_POPUP_MAX_WIDTH, HOVER_POPUP_MAX_HEIGHT),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.set_width(HOVER_POPUP_MAX_WIDTH);
                show_hover_documentation_body(ui, content);
                rendered_lines = ui.min_rect().height() as usize;
            });
        });
        assert!(
            rendered_lines > 0,
            "typical rust-analyzer hover content should render"
        );
    }

    #[test]
    fn hover_documentation_scrolls_when_content_exceeds_body_height() {
        let ctx = Context::default();
        let long_content = (0..80)
            .map(|index| format!("Line {index}: documentation paragraph with extra detail."))
            .collect::<Vec<_>>()
            .join("\n");
        let mut scroll_metrics = None;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(HOVER_POPUP_MAX_WIDTH, HOVER_POPUP_MAX_HEIGHT),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.set_width(HOVER_POPUP_MAX_WIDTH);
                let output = hover_documentation_scroll_area().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    render_hover_markdown(ui, &long_content);
                });
                scroll_metrics = Some((output.content_size.y, output.inner_rect.height()));
            });
        });
        let (content_height, viewport_height) =
            scroll_metrics.expect("hover documentation body should render");
        assert!(
            content_height > viewport_height,
            "long documentation should exceed the scroll viewport"
        );
        assert!(
            viewport_height <= HOVER_POPUP_MAX_BODY_HEIGHT + 1.0,
            "scroll viewport should stay within the body height cap"
        );
    }

    #[test]
    fn long_hover_documentation_wraps_or_scrolls_within_the_window() {
        let ctx = Context::default();
        let long_line = "word ".repeat(80);
        let long_content = (0..80)
            .map(|index| format!("Line {index}: documentation paragraph with extra detail."))
            .collect::<Vec<_>>()
            .join("\n");
        let mut content_width = None;
        let mut scroll_metrics = None;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(HOVER_POPUP_MAX_WIDTH, HOVER_POPUP_MAX_HEIGHT),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.set_width(HOVER_POPUP_MAX_WIDTH);
                show_hover_documentation_body(ui, &long_line);
                content_width = Some(ui.min_rect().width());

                let output = hover_documentation_scroll_area().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    render_hover_markdown(ui, &long_content);
                });
                scroll_metrics = Some((output.content_size.y, output.inner_rect.height()));
            });
        });

        let width = content_width.expect("wrapped prose should render");
        assert!(
            width <= HOVER_POPUP_MAX_WIDTH + 1.0,
            "long documentation should wrap within the popup width"
        );

        let (content_height, viewport_height) =
            scroll_metrics.expect("scrollable documentation should render");
        assert!(
            content_height > viewport_height,
            "long documentation should scroll when content exceeds the viewport"
        );
        assert!(
            viewport_height <= HOVER_POPUP_MAX_BODY_HEIGHT + 1.0,
            "scroll viewport should stay within the window body height cap"
        );
    }

    #[test]
    fn hover_request_session_tracks_stale_context() {
        let path = PathBuf::from("src/main.rs");
        let mut buffer = TextBuffer::from_text("fn main() {}\n");
        buffer.set_cursor(CursorPosition { line: 0, col: 0 });

        let session = HoverRequestSession {
            request_id: 7,
            path: path.clone(),
            position: LspPosition::new(0, 3),
            revision: buffer.revision(),
            lsp_version: buffer.lsp_version,
            position_entered_at: 12.5,
            popup_anchor: Rect::from_min_size(Pos2::new(80.0, 40.0), Vec2::new(24.0, 18.0)),
        };

        assert!(session.matches_request(7));
        assert!(!session.matches_request(8));
        assert!(!session.is_superseded_response(7));
        assert!(session.is_superseded_response(8));
        assert!(session.matches_lsp_position(&path, LspPosition::new(0, 3)));
        assert!(session.pointer_still_at_requested_position(&path, LspPosition::new(0, 3)));
        assert!(!session.pointer_still_at_requested_position(&path, LspPosition::new(0, 4)));
        assert!(session.matches_active_tab(&path));
        assert!(session.matches_active_file(&path));
        assert!(!session.matches_active_tab(Path::new("src/other.rs")));
        assert!(!session.matches_lsp_position(&path, LspPosition::new(0, 4)));
        assert!(!session.matches_lsp_position(Path::new("src/other.rs"), LspPosition::new(0, 3)));
        assert!(!session.is_stale_for(&path, LspPosition::new(0, 3), &buffer));
        assert!(session.is_stale_for(Path::new("src/other.rs"), LspPosition::new(0, 3), &buffer));

        assert!(session.matches_buffer_revision(&buffer));
        buffer.insert_at_cursor("x").unwrap();
        assert!(!session.matches_buffer_revision(&buffer));
        assert!(!session.buffer_snapshot_matches(&buffer));
        assert!(session.is_stale_for(&path, LspPosition::new(0, 3), &buffer));
        assert!(session.is_stale_for(&path, LspPosition::new(0, 4), &buffer));
    }

    #[test]
    fn hover_content_snapshot_tracks_buffer_revision() {
        let mut buffer = TextBuffer::from_text("fn main() {}\n");
        let session = HoverRequestSession {
            request_id: 1,
            path: PathBuf::from("src/main.rs"),
            position: LspPosition::new(0, 0),
            revision: buffer.revision(),
            lsp_version: buffer.lsp_version,
            position_entered_at: 0.0,
            popup_anchor: Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(12.0, 16.0)),
        };
        let snapshot = HoverContentSnapshot::from_session(&session);

        assert!(snapshot.matches_buffer(&buffer));
        buffer.insert_at_cursor("x").unwrap();
        assert!(!snapshot.matches_buffer(&buffer));
    }

    #[test]
    fn hover_request_session_tracks_position_entry_time() {
        let session = HoverRequestSession {
            request_id: 1,
            path: PathBuf::from("src/main.rs"),
            position: LspPosition::new(0, 0),
            revision: 0,
            lsp_version: 1,
            position_entered_at: 18.0,
            popup_anchor: Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(12.0, 16.0)),
        };
        assert!(session.pointer_still_resting_since_entry(Some(18.0)));
        assert!(!session.pointer_still_resting_since_entry(Some(18.5)));
        assert!(!session.pointer_still_resting_since_entry(None));
    }

    fn test_popup_size(bounds: Rect) -> Vec2 {
        hover_popup_estimated_size(bounds)
    }

    fn popup_at(pos: Pos2, size: Vec2) -> Rect {
        popup_rect_at(pos, size)
    }

    #[test]
    fn hover_popup_prefers_below_hovered_token() {
        let bounds = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(800.0, 600.0));
        let token = Rect::from_min_max(Pos2::new(120.0, 200.0), Pos2::new(156.0, 218.0));
        let size = test_popup_size(bounds);
        let popup_pos = hover_popup_position(token, bounds, size);
        assert_eq!(popup_pos.x, 120.0);
        assert_eq!(popup_pos.y, token.bottom() + HOVER_POPUP_VERTICAL_GAP);
        assert!(!popup_at(popup_pos, size).intersects(token));
    }

    #[test]
    fn hover_popup_uses_short_below_placement_when_full_height_does_not_fit() {
        let bounds = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(800.0, 220.0));
        let token = Rect::from_min_max(Pos2::new(120.0, 180.0), Pos2::new(156.0, 198.0));
        let size = test_popup_size(bounds);
        let popup_pos = hover_popup_position(token, bounds, size);
        assert!(popup_pos.y >= token.bottom() + HOVER_POPUP_VERTICAL_GAP);
        assert!(!popup_at(popup_pos, size).intersects(token));
    }

    #[test]
    fn hover_popup_flips_above_token_when_no_room_remains_below() {
        let bounds = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(800.0, 200.0));
        let token = Rect::from_min_max(Pos2::new(120.0, 182.0), Pos2::new(156.0, 198.0));
        let size = test_popup_size(bounds);
        let popup_pos = hover_popup_position(token, bounds, size);
        assert!(
            popup_pos.y < token.top(),
            "popup must flip above the token when there is no room below"
        );
        let placed_height = (token.top() - HOVER_POPUP_VERTICAL_GAP - popup_pos.y).min(size.y);
        let popup = popup_at(popup_pos, Vec2::new(size.x, placed_height));
        assert!(!popup.intersects(token));
        assert!(rect_fits_in_bounds(popup, bounds));
    }

    #[test]
    fn hover_popup_clamps_horizontally_to_viewport() {
        let bounds = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(300.0, 600.0));
        let token = Rect::from_min_max(Pos2::new(280.0, 100.0), Pos2::new(296.0, 118.0));
        let size = test_popup_size(bounds);
        let popup_pos = hover_popup_position(token, bounds, size);
        assert!(popup_pos.x <= bounds.right() - HOVER_POPUP_MIN_WIDTH);
    }

    #[test]
    fn hover_popup_avoids_obscuring_hovered_token() {
        let bounds = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(800.0, 600.0));
        let token = Rect::from_min_max(Pos2::new(200.0, 300.0), Pos2::new(240.0, 318.0));
        let size = test_popup_size(bounds);
        let popup_pos = hover_popup_position(token, bounds, size);
        assert!(!popup_at(popup_pos, size).intersects(token));
    }

    #[test]
    fn hover_popup_stays_inside_editor_bounds() {
        let editor = Rect::from_min_max(Pos2::new(200.0, 80.0), Pos2::new(1000.0, 700.0));
        let window = Rect::from_min_max(Pos2::ZERO, Pos2::new(1200.0, 800.0));
        let bounds = hover_popup_bounds(editor, window);
        let token = Rect::from_min_max(Pos2::new(220.0, 400.0), Pos2::new(260.0, 418.0));
        let size = Vec2::new(280.0, 200.0);
        let popup_pos = hover_popup_position(token, bounds, size);
        let popup = popup_at(popup_pos, size);
        assert!(rect_fits_in_bounds(popup, bounds));
        assert!(!popup.intersects(token));
    }

    #[test]
    fn hover_popup_clamps_vertically_inside_bounds_without_covering_token() {
        let bounds = Rect::from_min_max(Pos2::new(0.0, 100.0), Pos2::new(800.0, 250.0));
        let token = Rect::from_min_max(Pos2::new(120.0, 180.0), Pos2::new(156.0, 198.0));
        let size = Vec2::new(280.0, 360.0);
        let popup_pos = hover_popup_position(token, bounds, size);
        let short_height = bounds.bottom() - (token.bottom() + HOVER_POPUP_VERTICAL_GAP);
        let popup = popup_at(popup_pos, Vec2::new(size.x, short_height.min(size.y)));
        assert!(rect_fits_in_bounds(popup, bounds));
        assert!(!popup.intersects(token));
        assert!(popup_pos.y >= token.bottom() + HOVER_POPUP_VERTICAL_GAP);
    }

    #[test]
    fn lsp_hover_allowed_only_without_active_diagnostic_tooltip() {
        assert!(lsp_hover_allowed(false));
        assert!(!lsp_hover_allowed(true));
    }

    #[test]
    fn hover_popup_model_allows_lsp_hover_without_diagnostic_tooltip() {
        assert!(HoverPopupModel::none().allows_lsp_hover());
        assert!(!HoverPopupModel {
            hovered_source: None,
            diagnostic_tooltip_active: true,
        }
        .allows_lsp_hover());
    }

    #[test]
    fn outside_click_emits_dismissed_event() {
        let popup_rect = Rect::from_min_max(Pos2::new(100.0, 200.0), Pos2::new(380.0, 360.0));
        let outside_pos = Pos2::new(10.0, 10.0);
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let ctx = Context::default();

        let mut event = None;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                events: vec![egui::Event::PointerButton {
                    pos: outside_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ctx| {
                event = hover_outside_click_event(ctx, Some(popup_rect));
            },
        );

        assert_eq!(event, Some(HoverPopupEvent::Dismissed));
    }

    #[test]
    fn clicking_inside_hover_popup_does_not_emit_event() {
        let popup_rect = Rect::from_min_max(Pos2::new(100.0, 200.0), Pos2::new(380.0, 360.0));
        let inside_pos = popup_rect.center();
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let ctx = Context::default();

        let mut event = None;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                events: vec![egui::Event::PointerButton {
                    pos: inside_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ctx| {
                event = hover_outside_click_event(ctx, Some(popup_rect));
            },
        );

        assert_eq!(event, None);
    }

    #[test]
    fn apply_lsp_hover_gates_suppresses_deferred_hover_under_overlays() {
        use crate::editor::buffer::CursorPosition;

        let source = HoveredSourcePosition::for_test(
            CursorPosition { line: 0, col: 3 },
            Rect::from_min_size(Pos2::new(80.0, 20.0), Vec2::new(24.0, 18.0)),
        );
        let handoff = HoverPopupModel {
            hovered_source: Some(source),
            diagnostic_tooltip_active: false,
        };
        assert!(
            apply_lsp_hover_gates(handoff, CompletionPopupModel::open(), false).is_none(),
            "completion must suppress deferred LSP hover"
        );
        assert!(
            apply_lsp_hover_gates(handoff, CompletionPopupModel::closed(), true).is_none(),
            "other overlays must suppress deferred LSP hover"
        );
        assert!(
            apply_lsp_hover_gates(
                HoverPopupModel {
                    hovered_source: Some(source),
                    diagnostic_tooltip_active: true,
                },
                CompletionPopupModel::closed(),
                false,
            )
            .is_none(),
            "active diagnostic tooltip must suppress deferred LSP hover"
        );
        assert_eq!(
            apply_lsp_hover_gates(handoff, CompletionPopupModel::closed(), false),
            Some(source),
            "source text should reach LSP hover when no overlay blocks it"
        );
    }
}
