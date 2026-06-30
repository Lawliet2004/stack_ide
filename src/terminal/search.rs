//! Feature 4 — Terminal Search (Ctrl+F).
//!
//! A search overlay displayed at the top-right of the terminal panel.
//! Searches case-insensitively across the full scrollback buffer.

/// A single match location: (absolute line index into all_lines, byte range in that line).
#[derive(Debug, Clone)]
pub struct SearchMatch {
    /// Index into the flattened `[scrollback; screen_lines]` line array.
    pub line_index: usize,
    pub byte_start: usize,
    pub byte_end: usize,
}

/// All UI state for the terminal search feature.
#[derive(Default)]
pub struct TerminalSearchState {
    /// Whether the search bar is visible.
    pub visible: bool,
    /// Current query text.
    pub query: String,
    /// All matches found in the last scan.
    pub matches: Vec<SearchMatch>,
    /// Index of the currently highlighted match (into `matches`).
    pub current_match: usize,
    /// Whether the match list is stale and needs re-scanning.
    needs_rescan: bool,
}

impl TerminalSearchState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open the search bar.
    pub fn open(&mut self) {
        self.visible = true;
        self.needs_rescan = true;
    }

    /// Close the search bar and clear all state.
    pub fn close(&mut self) {
        self.visible = false;
        self.query.clear();
        self.matches.clear();
        self.current_match = 0;
        self.needs_rescan = false;
    }

    /// Mark the match list as stale (query or buffer changed).
    pub fn invalidate(&mut self) {
        self.needs_rescan = true;
    }

    /// Scan `lines` (all scrollback + screen lines) for the current query.
    pub fn rescan(&mut self, lines: &[String]) {
        self.matches.clear();
        self.needs_rescan = false;

        if self.query.is_empty() {
            return;
        }

        let query_lower = self.query.to_lowercase();

        for (line_idx, line) in lines.iter().enumerate() {
            let line_lower = line.to_lowercase();
            let mut search_from = 0usize;

            while let Some(rel_pos) = line_lower[search_from..].find(&query_lower) {
                let byte_start = search_from + rel_pos;
                let byte_end = byte_start + query_lower.len();
                self.matches.push(SearchMatch {
                    line_index: line_idx,
                    byte_start,
                    byte_end,
                });
                search_from = byte_end;
            }
        }

        // Clamp current_match after re-scan
        if self.matches.is_empty() {
            self.current_match = 0;
        } else {
            self.current_match = self.current_match.min(self.matches.len() - 1);
        }
    }

    /// Move to the next match (wraps around).
    pub fn next_match(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.current_match = (self.current_match + 1) % self.matches.len();
    }

    /// Move to the previous match (wraps around).
    pub fn prev_match(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        if self.current_match == 0 {
            self.current_match = self.matches.len() - 1;
        } else {
            self.current_match -= 1;
        }
    }

    /// The line index of the currently selected match, if any.
    pub fn current_match_line(&self) -> Option<usize> {
        self.matches.get(self.current_match).map(|m| m.line_index)
    }

    /// Whether a rescan is needed.
    pub fn is_stale(&self) -> bool {
        self.needs_rescan
    }
}

/// Yellow highlight for non-selected matches.
pub const MATCH_BG: egui::Color32 = egui::Color32::from_rgb(255, 215, 0);
/// Orange highlight for the selected match.
pub const CURRENT_MATCH_BG: egui::Color32 = egui::Color32::from_rgb(255, 140, 0);

/// Draw search highlights for all matches visible in the current row range.
///
/// `visible_range` — the range of absolute line indices currently rendered.
/// `line_tops` — maps visible line index → Y coordinate of its top edge.
pub fn draw_search_highlights(
    painter: &egui::Painter,
    state: &TerminalSearchState,
    visible_range: std::ops::Range<usize>,
    left_x: f32,
    char_width: f32,
    line_height: f32,
    line_tops: &dyn Fn(usize) -> f32, // absolute line index → y
) {
    for (match_idx, m) in state.matches.iter().enumerate() {
        if !visible_range.contains(&m.line_index) {
            continue;
        }
        let is_current = match_idx == state.current_match;
        let bg = if is_current { CURRENT_MATCH_BG } else { MATCH_BG };

        // Convert byte range to character columns (approximate: assume ASCII / UTF-8 same width)
        let col_start = m.byte_start;
        let col_end = m.byte_end;
        let x0 = left_x + col_start as f32 * char_width;
        let x1 = left_x + col_end as f32 * char_width;
        let y = line_tops(m.line_index);

        let rect =
            egui::Rect::from_min_max(egui::pos2(x0, y), egui::pos2(x1, y + line_height));
        painter.rect_filled(rect, 0.0, bg);
    }
}

/// Render the search overlay bar at the top-right of `panel_rect`.
///
/// Returns a `SearchBarOutput` indicating what changed this frame.
pub struct SearchBarOutput {
    /// The query text after any edits this frame.
    pub query_changed: bool,
    /// User pressed ∨ or Enter.
    pub next_requested: bool,
    /// User pressed ∧ or Shift+Enter.
    pub prev_requested: bool,
    /// User closed the bar.
    pub closed: bool,
}

pub fn render_search_bar(
    ui: &mut egui::Ui,
    state: &mut TerminalSearchState,
    panel_rect: egui::Rect,
) -> SearchBarOutput {
    let mut out = SearchBarOutput {
        query_changed: false,
        next_requested: false,
        prev_requested: false,
        closed: false,
    };

    if !state.visible {
        return out;
    }

    let bar_width = 320.0_f32;
    let bar_height = 34.0_f32;
    let bar_rect = egui::Rect::from_min_size(
        egui::pos2(panel_rect.right() - bar_width - 8.0, panel_rect.top() + 4.0),
        egui::vec2(bar_width, bar_height),
    );

    // Draw background
    ui.painter().rect_filled(
        bar_rect,
        4.0,
        egui::Color32::from_rgb(30, 30, 30),
    );
    ui.painter().rect_stroke(
        bar_rect,
        4.0,
        egui::Stroke::new(1.0, egui::Color32::from_rgb(70, 70, 70)),
    );

    // Place a child UI inside the bar
    let mut child = ui.child_ui(bar_rect, egui::Layout::left_to_right(egui::Align::Center));
    child.add_space(6.0);

    // Text input
    let old_query = state.query.clone();
    let te = egui::TextEdit::singleline(&mut state.query)
        .hint_text("Search terminal…")
        .desired_width(160.0)
        .font(egui::TextStyle::Small);
    let te_resp = child.add(te);

    if state.query != old_query {
        out.query_changed = true;
        state.invalidate();
    }

    // Handle Enter / Shift+Enter on the text field
    if te_resp.has_focus() {
        let (enter, shift_enter, esc) = child.input(|i| {
            let enter = i.key_pressed(egui::Key::Enter) && !i.modifiers.shift;
            let shift_enter = i.key_pressed(egui::Key::Enter) && i.modifiers.shift;
            let esc = i.key_pressed(egui::Key::Escape);
            (enter, shift_enter, esc)
        });
        if enter {
            out.next_requested = true;
        }
        if shift_enter {
            out.prev_requested = true;
        }
        if esc {
            out.closed = true;
        }
    }

    child.add_space(4.0);

    // Match counter
    let counter = if state.matches.is_empty() {
        "No results".to_owned()
    } else {
        format!("{} of {}", state.current_match + 1, state.matches.len())
    };
    child.label(egui::RichText::new(counter).small().color(
        if state.matches.is_empty() {
            egui::Color32::from_rgb(180, 60, 60)
        } else {
            egui::Color32::from_rgb(180, 180, 180)
        },
    ));

    child.add_space(4.0);

    // ∧ prev
    if child
        .add(egui::Button::new("∧").frame(false).min_size(egui::vec2(18.0, 18.0)))
        .on_hover_text("Previous match (Shift+Enter)")
        .clicked()
    {
        out.prev_requested = true;
    }

    // ∨ next
    if child
        .add(egui::Button::new("∨").frame(false).min_size(egui::vec2(18.0, 18.0)))
        .on_hover_text("Next match (Enter)")
        .clicked()
    {
        out.next_requested = true;
    }

    child.add_space(4.0);

    // ✕ close
    if child
        .add(
            egui::Button::new(
                egui::RichText::new("✕")
                    .color(egui::Color32::from_rgb(180, 180, 180)),
            )
            .frame(false)
            .min_size(egui::vec2(18.0, 18.0)),
        )
        .on_hover_text("Close search (Escape)")
        .clicked()
    {
        out.closed = true;
    }

    out
}
