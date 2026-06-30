/// Find/Replace panel rendered inside the editor viewport.
///
/// Two rendering modes:
/// - **Overlay** (`show`): compact floating panel anchored to the top-right
///   corner of the `CentralPanel`.  Used for single-file Find/Replace.
/// - **Bottom panel** (`show_bottom_panel`): full-width docked panel at the
///   bottom of the window, similar to the terminal.  Used for project-wide
///   search so results have more room.
///
/// # Features
/// - Glob filter rows (include / exclude) with live validation.
/// - Project results rendered with virtual scrolling (`show_rows`) — handles
///   50 000+ matches without stalling the frame loop.
/// - Results grouped by file with a collapsible ▶/▼ header per group.
/// - Replace All gated behind `SearchState::request_replace_confirm()`; the
///   caller in `app.rs` renders the confirmation dialog.
/// - Match spans highlighted in yellow using `egui::text::LayoutJob`.
use egui::{Color32, Key, Modifiers, Rect, RichText, TextEdit, Ui, Vec2};

use crate::search::{SearchMode, SearchScope, SearchState};
use crate::theme::SemanticPalette;

// ---------------------------------------------------------------------------
// Bottom-panel output (superset of PanelOutput used by the bottom panel)
// ---------------------------------------------------------------------------

/// Output produced by `show_bottom_panel` each frame.
///
/// Because the bottom panel hosts its own egui `TopBottomPanel`, the caller
/// does not need a `panel_rect` — interactions happen within the panel itself.
#[derive(Debug, Default)]
pub struct BottomPanelOutput {
    /// Navigate to the next match (Enter / F3 from the query field).
    pub next_match: bool,
    /// Navigate to the previous match (Shift+Enter / Shift+F3).
    pub prev_match: bool,
    /// Replace the currently selected match.
    pub replace_one: bool,
    /// Replace all matches (shows confirmation dialog in caller).
    pub replace_all: bool,
    /// The user closed the panel (× button or Escape).
    pub closed: bool,
    /// The query or search options changed — caller should restart search.
    pub query_changed: bool,
    /// A project result row was clicked; value is the index into
    /// `SearchState::project_matches`.
    pub project_result_clicked: Option<usize>,
    /// Caller should request focus on the query `TextEdit` next frame.
    pub want_focus: bool,
}

/// Input from the panel that the app must act on this frame.
#[derive(Debug)]
pub struct PanelOutput {
    /// Navigate to the next match.
    pub next_match: bool,
    /// Navigate to the previous match.
    pub prev_match: bool,
    /// Replace the current active match.
    pub replace_one: bool,
    /// Replace all matches in the current scope (after confirmation).
    pub replace_all: bool,
    /// The panel was closed by the user.
    pub closed: bool,
    /// The query or options changed and matches should be recomputed.
    pub query_changed: bool,
    /// A project-scope result was clicked; contains the result index.
    pub project_result_clicked: Option<usize>,
    /// Screen rectangle of the find/replace overlay when visible.
    pub panel_rect: Rect,
}

impl Default for PanelOutput {
    fn default() -> Self {
        Self {
            next_match: false,
            prev_match: false,
            replace_one: false,
            replace_all: false,
            closed: false,
            query_changed: false,
            project_result_clicked: None,
            panel_rect: Rect::NOTHING,
        }
    }
}

// ---------------------------------------------------------------------------
// Virtual-scrolling helper types
// ---------------------------------------------------------------------------

/// A flattened row in the results list, used for virtual scrolling.
///
/// We build this flat list from the grouped matches so `show_rows` can index
/// rows by integer without knowing about grouping.
#[derive(Clone)]
enum ResultRow {
    /// A collapsible file-group header.
    FileHeader {
        /// Index of the *first* `SearchMatch` belonging to this group.
        first_match_idx: usize,
        /// Number of matches in the group (used for the count badge).
        match_count: usize,
    },
    /// A match entry belonging to a file group.
    MatchEntry {
        /// Index into `SearchState::project_matches`.
        match_idx: usize,
    },
}

/// Build a flat `Vec<ResultRow>` from the current project matches, respecting
/// collapsed state.  This is called every frame while results are visible.
fn build_result_rows(state: &SearchState) -> Vec<ResultRow> {
    let matches = &state.project_matches;
    let mut rows: Vec<ResultRow> = Vec::with_capacity(matches.len() + 16);
    let mut i = 0usize;

    while i < matches.len() {
        let path = &matches[i].path;
        let group_start = i;

        // Count how many consecutive matches share this path.
        while i < matches.len() && &matches[i].path == path {
            i += 1;
        }
        let match_count = i - group_start;

        rows.push(ResultRow::FileHeader {
            first_match_idx: group_start,
            match_count,
        });

        if !state.is_file_collapsed(path) {
            for mi in group_start..i {
                rows.push(ResultRow::MatchEntry { match_idx: mi });
            }
        }
    }

    rows
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Render the Find/Replace panel and return the actions requested.
///
/// Must be called from within an `egui::CentralPanel` (or equivalent) so that
/// the `Area` anchors relative to the editor viewport.
pub fn show(
    ui: &mut Ui,
    state: &mut SearchState,
    file_match_count: usize,
    palette: SemanticPalette,
) -> PanelOutput {
    let mut output = PanelOutput::default();

    if !state.visible {
        return output;
    }

    // -----------------------------------------------------------------------
    // Overlay Area anchored to the top-right of the editor content rect
    // -----------------------------------------------------------------------
    let panel_width = if state.mode == SearchMode::Replace {
        480.0_f32
    } else {
        400.0_f32
    };

    let area_response = egui::Area::new(egui::Id::new("find_replace_panel"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::RIGHT_TOP, Vec2::new(-12.0, 8.0))
        .show(ui.ctx(), |ui| {
            egui::Frame::window(&ui.ctx().style())
                .inner_margin(egui::Margin::same(8.0))
                .show(ui, |ui| {
                    ui.set_width(panel_width);
                    render_panel_contents(ui, state, file_match_count, palette, &mut output);
                });
        });

    // Consume Escape only when no higher-priority modal is present.
    // The app checks has_modal() before calling this function.
    if ui
        .ctx()
        .input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
    {
        output.closed = true;
    }

    output.panel_rect = area_response.response.rect;
    output
}

// ---------------------------------------------------------------------------
// Panel content
// ---------------------------------------------------------------------------

fn render_panel_contents(
    ui: &mut Ui,
    state: &mut SearchState,
    file_match_count: usize,
    palette: SemanticPalette,
    output: &mut PanelOutput,
) {
    render_query_row(ui, state, file_match_count, palette, output);
    render_options_row(ui, state, output);

    // Regex compile error
    if let Some(err) = &state.compile_error {
        ui.colored_label(palette.error, format!("⚠ Regex error: {err}"));
    }

    // Glob filter rows (collapsible to save vertical space)
    render_glob_filters(ui, state, output);

    // Replace row (only in Replace mode)
    if state.mode == SearchMode::Replace {
        render_replace_row(ui, state, file_match_count, palette, output);
    }

    // Project search: progress + results
    if state.query.scope == SearchScope::Project {
        render_project_results(ui, state, palette, output);
    }
}

// ---------------------------------------------------------------------------
// Query row
// ---------------------------------------------------------------------------

fn render_query_row(
    ui: &mut Ui,
    state: &mut SearchState,
    file_match_count: usize,
    palette: SemanticPalette,
    output: &mut PanelOutput,
) {
    ui.horizontal(|ui| {
        ui.set_width(ui.available_width());

        // Search input
        let query_id = egui::Id::new("find_replace_query");
        let te = TextEdit::singleline(&mut state.query.query)
            .id(query_id)
            .hint_text("Find")
            .desired_width(180.0);
        let query_response = ui.add(te);

        if state.want_query_focus {
            query_response.request_focus();
            state.want_query_focus = false;
        }
        if state.want_query_select_all && query_response.has_focus() {
            // Select all text in the TextEdit.
            if let Some(mut te_state) = egui::text_edit::TextEditState::load(ui.ctx(), query_id) {
                te_state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(state.query.query.chars().count()),
                    )));
                te_state.store(ui.ctx(), query_id);
            }
            state.want_query_select_all = false;
        }

        if query_response.changed() {
            output.query_changed = true;
        }

        // Navigate Enter / Shift+Enter from the query field.
        if query_response.has_focus() {
            let shift_enter = ui.ctx().input_mut(|i| {
                i.consume_key(
                    Modifiers {
                        shift: true,
                        ..Modifiers::NONE
                    },
                    Key::Enter,
                )
            });
            let enter = ui
                .ctx()
                .input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
            if shift_enter {
                output.prev_match = true;
            } else if enter {
                output.next_match = true;
            }
        }

        // Match counter
        let total = match state.query.scope {
            SearchScope::File => file_match_count,
            SearchScope::Project => state.project_matches.len(),
        };
        let counter_text = if !state.query.is_non_empty() {
            String::new()
        } else if total == 0 {
            "No results".to_owned()
        } else {
            let idx = state.active_index.map(|i| i + 1).unwrap_or(0);
            format!("{idx} of {total}")
        };
        let counter_color = if total == 0 && state.query.is_non_empty() {
            palette.error
        } else {
            ui.visuals().text_color()
        };
        ui.label(RichText::new(counter_text).color(counter_color).small());

        // Navigation buttons
        let prev_btn = ui
            .add_enabled(
                total > 0,
                egui::Button::new("▲")
                    .min_size(egui::vec2(22.0, 22.0))
                    .frame(true),
            )
            .on_hover_text("Previous match (Shift+Enter / Shift+F3)");
        if prev_btn.clicked() {
            output.prev_match = true;
        }

        let next_btn = ui
            .add_enabled(
                total > 0,
                egui::Button::new("▼")
                    .min_size(egui::vec2(22.0, 22.0))
                    .frame(true),
            )
            .on_hover_text("Next match (Enter / F3)");
        if next_btn.clicked() {
            output.next_match = true;
        }

        // Close button
        if ui
            .button(RichText::new("✕").small())
            .on_hover_text("Close (Escape)")
            .clicked()
        {
            output.closed = true;
        }
    });
}

// ---------------------------------------------------------------------------
// Options row
// ---------------------------------------------------------------------------

fn render_options_row(ui: &mut Ui, state: &mut SearchState, output: &mut PanelOutput) {
    ui.horizontal(|ui| {
        let regex_label = if state.query.use_regex {
            ".*  ✓"
        } else {
            ".*"
        };
        let regex_btn = ui
            .selectable_label(state.query.use_regex, regex_label)
            .on_hover_text("Use regular expression");
        if regex_btn.clicked() {
            state.query.use_regex = !state.query.use_regex;
            output.query_changed = true;
        }

        let case_label = if state.query.case_sensitive {
            "Aa ✓"
        } else {
            "Aa"
        };
        let case_btn = ui
            .selectable_label(state.query.case_sensitive, case_label)
            .on_hover_text("Case sensitive");
        if case_btn.clicked() {
            state.query.case_sensitive = !state.query.case_sensitive;
            output.query_changed = true;
        }

        ui.separator();

        // Scope selector
        let file_active = state.query.scope == SearchScope::File;
        if ui
            .selectable_label(file_active, "Current File")
            .on_hover_text("Search the active file")
            .clicked()
            && !file_active
        {
            state.query.scope = SearchScope::File;
            output.query_changed = true;
        }
        let proj_active = state.query.scope == SearchScope::Project;
        if ui
            .selectable_label(proj_active, "Project")
            .on_hover_text("Search all project files")
            .clicked()
            && !proj_active
        {
            state.query.scope = SearchScope::Project;
            output.query_changed = true;
        }
    });
}

// ---------------------------------------------------------------------------
// Glob filter rows
// ---------------------------------------------------------------------------

/// Render the collapsible glob filter section.
///
/// When either glob field changes the caller receives `query_changed = true`
/// so the project search is restarted with the new filters.
fn render_glob_filters(ui: &mut Ui, state: &mut SearchState, output: &mut PanelOutput) {
    // Only show glob filters for project-scope search; they have no effect
    // on file-scope search (which operates on the single active buffer).
    if state.query.scope != SearchScope::Project {
        return;
    }

    ui.collapsing("Filters", |ui| {
        ui.horizontal(|ui| {
            ui.label("Include:");
            let resp = ui.add(
                TextEdit::singleline(&mut state.query.include_glob)
                    .id(egui::Id::new("search_include_glob"))
                    .hint_text("e.g. *.rs")
                    .desired_width(160.0),
            );
            if resp.changed() {
                output.query_changed = true;
            }
            // Show a red indicator when the glob pattern is syntactically invalid.
            if !state.query.include_glob.is_empty()
                && glob::Pattern::new(&state.query.include_glob).is_err()
            {
                ui.colored_label(Color32::RED, "⚠ invalid glob");
            }
        });

        ui.horizontal(|ui| {
            ui.label("Exclude:");
            let resp = ui.add(
                TextEdit::singleline(&mut state.query.exclude_glob)
                    .id(egui::Id::new("search_exclude_glob"))
                    .hint_text("e.g. tests/**")
                    .desired_width(160.0),
            );
            if resp.changed() {
                output.query_changed = true;
            }
            if !state.query.exclude_glob.is_empty()
                && glob::Pattern::new(&state.query.exclude_glob).is_err()
            {
                ui.colored_label(Color32::RED, "⚠ invalid glob");
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Replace row
// ---------------------------------------------------------------------------

fn render_replace_row(
    ui: &mut Ui,
    state: &mut SearchState,
    file_match_count: usize,
    palette: SemanticPalette,
    output: &mut PanelOutput,
) {
    ui.horizontal(|ui| {
        let te = TextEdit::singleline(&mut state.query.replacement)
            .id(egui::Id::new("find_replace_replacement"))
            .hint_text("Replace")
            .desired_width(180.0);
        ui.add(te);

        let has_active = state.active_file_match().is_some()
            || (state.query.scope == SearchScope::Project
                && state.active_project_match().is_some());
        let replace_btn = ui
            .add_enabled(has_active, egui::Button::new("Replace"))
            .on_hover_text("Replace this match");
        if replace_btn.clicked() {
            output.replace_one = true;
        }

        let has_matches = match state.query.scope {
            SearchScope::File => file_match_count > 0,
            SearchScope::Project => !state.project_matches.is_empty(),
        };
        let replace_all_btn = ui
            .add_enabled(has_matches, egui::Button::new("Replace All"))
            .on_hover_text("Replace all matches (shows confirmation)");
        if replace_all_btn.clicked() {
            // Gate behind a confirmation dialog — `app.rs` renders it.
            state.request_replace_confirm();
        }
    });

    // Last replace report
    if let Some(report) = &state.last_replace_report {
        let msg = if report.failures.is_empty() {
            format!(
                "Replaced {} occurrence(s) in {} file(s).",
                report.replaced, report.files_affected
            )
        } else {
            format!(
                "Replaced {} in {} file(s); {} error(s).",
                report.replaced,
                report.files_affected,
                report.failures.len()
            )
        };
        let report_color = if report.failures.is_empty() {
            palette.success
        } else {
            palette.warning
        };
        ui.label(RichText::new(msg).small().color(report_color));
    }
}

// ---------------------------------------------------------------------------
// Project results panel — virtual scrolling + file-group collapse
// ---------------------------------------------------------------------------

/// Approximate height of a single result row (used by `show_rows` for layout).
const ROW_HEIGHT: f32 = 20.0;
/// Max height of the scrollable results area.
const RESULTS_MAX_HEIGHT: f32 = 320.0;

fn render_project_results(
    ui: &mut Ui,
    state: &mut SearchState,
    palette: SemanticPalette,
    output: &mut PanelOutput,
) {
    if state.project_searching() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(format!(
                "Searching… {} match(es) so far",
                state.project_matches.len()
            ));
        });
    } else if !state.project_matches.is_empty() {
        ui.separator();

        // Build flat row list respecting collapsed groups.
        // We rebuild this every frame because it is cheap and collapse state
        // may change on any frame.
        let rows = build_result_rows(state);
        let row_count = rows.len();

        egui::ScrollArea::vertical()
            .id_source("project_results_scroll")
            .max_height(RESULTS_MAX_HEIGHT)
            .auto_shrink([false, false])
            .show_rows(ui, ROW_HEIGHT, row_count, |ui, visible_range| {
                // We need to separate the immutable borrow of `state.project_matches`
                // from the mutable borrow of `state.collapsed_files`.
                // Collect the actions that require mutation and apply them after
                // the rendering loop.
                let mut toggle_path: Option<std::path::PathBuf> = None;
                let mut clicked_match_idx: Option<usize> = None;

                for row_idx in visible_range {
                    match &rows[row_idx] {
                        ResultRow::FileHeader {
                            first_match_idx,
                            match_count,
                        } => {
                            let path = state.project_matches[*first_match_idx].path.clone();
                            let collapsed = state.is_file_collapsed(&path);
                            let arrow = if collapsed { "▶" } else { "▼" };
                            let file_name = path
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.display().to_string());

                            ui.horizontal(|ui| {
                                let header_resp = ui
                                    .selectable_label(
                                        false,
                                        RichText::new(format!(
                                            "{arrow} {file_name} ({match_count})"
                                        ))
                                        .strong(),
                                    )
                                    .on_hover_text(path.display().to_string());
                                if header_resp.clicked() {
                                    toggle_path = Some(path);
                                }
                            });
                        }

                        ResultRow::MatchEntry { match_idx } => {
                            let m = &state.project_matches[*match_idx];
                            let is_active = state.active_index == Some(*match_idx);

                            // Build highlighted label using a LayoutJob so match spans
                            // appear with a yellow background.
                            let label_text =
                                format!("{:>4}  {}", m.line + 1, m.line_preview.trim());

                            ui.horizontal(|ui| {
                                ui.add_space(16.0); // indent under file header

                                // Line number badge
                                ui.label(
                                    RichText::new(format!("{:>4}", m.line + 1))
                                        .color(palette.muted_text)
                                        .monospace(),
                                );
                                ui.add_space(4.0);

                                // Line preview with highlighted match byte range.
                                // We highlight the portion of the preview that falls
                                // within `m.col .. m.col + (m.byte_range.end - m.byte_range.start)`.
                                let preview = m.line_preview.trim();
                                let match_len = m.byte_range.end.saturating_sub(m.byte_range.start);
                                let col = m.col;
                                let end = col.saturating_add(match_len).min(preview.len());

                                let mut job = egui::text::LayoutJob::default();

                                macro_rules! append_seg {
                                    ($text:expr, $color:expr, $bg:expr) => {
                                        job.append(
                                            $text,
                                            0.0,
                                            egui::text::TextFormat {
                                                font_id: egui::FontId::monospace(12.5),
                                                color: $color,
                                                background: $bg,
                                                ..Default::default()
                                            },
                                        );
                                    };
                                }

                                let normal_color = if is_active {
                                    ui.visuals().strong_text_color()
                                } else {
                                    ui.visuals().text_color()
                                };
                                let highlight_bg = Color32::from_rgb(255, 210, 0);
                                let no_bg = Color32::TRANSPARENT;

                                if col < preview.len() && end <= preview.len() && col <= end {
                                    if col > 0 {
                                        append_seg!(&preview[..col], normal_color, no_bg);
                                    }
                                    if col < end {
                                        append_seg!(
                                            &preview[col..end],
                                            Color32::BLACK,
                                            highlight_bg
                                        );
                                    }
                                    if end < preview.len() {
                                        append_seg!(&preview[end..], normal_color, no_bg);
                                    }
                                } else {
                                    // Fallback: render plain if offsets are out of range.
                                    append_seg!(preview, normal_color, no_bg);
                                }

                                let row_resp = ui.add(
                                    egui::Label::new(job)
                                        .sense(egui::Sense::click())
                                        .truncate(true),
                                );
                                if row_resp.clicked() {
                                    clicked_match_idx = Some(*match_idx);
                                }
                            });

                            // Subtle separator only between match rows (not after headers).
                            if !is_active {
                                // Skip separator – tight layout looks better.
                            }

                            let _ = label_text; // suppress unused warning
                        }
                    }
                }

                // Apply mutations outside the rendering loop.
                if let Some(path) = toggle_path {
                    state.toggle_file_collapsed(&path);
                }
                if let Some(idx) = clicked_match_idx {
                    output.project_result_clicked = Some(idx);
                }
            });
    } else if state.project_done && state.query.is_non_empty() && state.compile_error.is_none() {
        ui.label(
            RichText::new("No results in project")
                .small()
                .color(palette.muted_text),
        );
    }

    // Failures summary
    if !state.project_failures.is_empty() {
        ui.collapsing(
            format!("{} file(s) could not be read", state.project_failures.len()),
            |ui| {
                for (path, err) in &state.project_failures {
                    ui.label(format!("{}: {err}", path.display()));
                }
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Bottom panel — docked, full-width, project-search focused
// ---------------------------------------------------------------------------

/// Default height of the search bottom panel (pixels).
const SEARCH_PANEL_DEFAULT_HEIGHT: f32 = 280.0;
/// Minimum height the panel can be resized to.
const SEARCH_PANEL_MIN_HEIGHT: f32 = 140.0;
/// Maximum height for the panel.
const SEARCH_PANEL_MAX_HEIGHT: f32 = 600.0;

/// Render a docked bottom panel containing the full workspace search UI.
///
/// This is the primary entry-point for project-wide search.  Call it from
/// `app.rs` in place of (or alongside) `show()`.  It returns a
/// [`BottomPanelOutput`] that the caller should inspect every frame to drive
/// navigation, replacements, and re-searches.
///
/// # Panel identity
/// Uses the `egui` id `"search_bottom_panel"`.  The caller must NOT register
/// another `TopBottomPanel::bottom` with the same id.
///
/// # Virtual scrolling
/// Results are rendered with `ScrollArea::show_rows` so 50 000+ rows never
/// stall the frame loop.  Only the rows that are actually visible on screen
/// are laid out.
///
/// # Cancellation
/// Pressing Escape while the query `TextEdit` is focused — or clicking the ×
/// button — sets `BottomPanelOutput::closed = true`.  The caller is responsible
/// for setting `SearchState::visible = false` and returning focus to the editor.
pub fn show_bottom_panel(
    context: &egui::Context,
    state: &mut SearchState,
    file_match_count: usize,
    palette: SemanticPalette,
) -> BottomPanelOutput {
    let mut output = BottomPanelOutput::default();

    if !state.visible {
        return output;
    }

    egui::TopBottomPanel::bottom("search_bottom_panel")
        .resizable(true)
        .min_height(SEARCH_PANEL_MIN_HEIGHT)
        .max_height(SEARCH_PANEL_MAX_HEIGHT)
        .default_height(SEARCH_PANEL_DEFAULT_HEIGHT)
        .show(context, |ui| {
            output = show_inside(ui, state, file_match_count, palette);
        });

    output
}

pub fn show_inside(
    ui: &mut egui::Ui,
    state: &mut SearchState,
    file_match_count: usize,
    palette: SemanticPalette,
) -> BottomPanelOutput {
    let mut output = BottomPanelOutput::default();
    ui.horizontal(|ui| {
                // Panel title / label
                ui.label(RichText::new("🔍 Search").strong().small());
                ui.separator();

                // ── Query input ────────────────────────────────────────────
                let query_id = egui::Id::new("search_bottom_query");
                let te = TextEdit::singleline(&mut state.query.query)
                    .id(query_id)
                    .hint_text("Search (Enter to run, Shift+Enter for previous)")
                    .desired_width(260.0);
                let query_resp = ui.add(te);

                // Focus management from caller
                if state.want_query_focus {
                    query_resp.request_focus();
                    state.want_query_focus = false;
                }
                if state.want_query_select_all && query_resp.has_focus() {
                    if let Some(mut te_state) =
                        egui::text_edit::TextEditState::load(ui.ctx(), query_id)
                    {
                        te_state
                            .cursor
                            .set_char_range(Some(egui::text::CCursorRange::two(
                                egui::text::CCursor::new(0),
                                egui::text::CCursor::new(state.query.query.chars().count()),
                            )));
                        te_state.store(ui.ctx(), query_id);
                    }
                    state.want_query_select_all = false;
                }

                if query_resp.changed() {
                    output.query_changed = true;
                }

                // Enter / Shift+Enter to cycle matches
                if query_resp.has_focus() {
                    let shift_enter = ui.ctx().input_mut(|i| {
                        i.consume_key(
                            Modifiers {
                                shift: true,
                                ..Modifiers::NONE
                            },
                            Key::Enter,
                        )
                    });
                    let enter = ui
                        .ctx()
                        .input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
                    if shift_enter {
                        output.prev_match = true;
                    } else if enter {
                        // On Enter with project scope, (re-)start the search.
                        if state.query.scope == SearchScope::Project
                            && state.query.is_non_empty()
                            && state.compile_error.is_none()
                        {
                            // Signal the caller to start a new project search.
                            output.query_changed = true;
                        } else {
                            output.next_match = true;
                        }
                    }
                }

                // ── Toggle: Aa (case), .* (regex) ─────────────────────────
                let regex_label = if state.query.use_regex { ".*✓" } else { ".*" };
                if ui
                    .selectable_label(state.query.use_regex, regex_label)
                    .on_hover_text("Use regular expression")
                    .clicked()
                {
                    state.query.use_regex = !state.query.use_regex;
                    output.query_changed = true;
                }

                let case_label = if state.query.case_sensitive {
                    "Aa✓"
                } else {
                    "Aa"
                };
                if ui
                    .selectable_label(state.query.case_sensitive, case_label)
                    .on_hover_text("Case sensitive")
                    .clicked()
                {
                    state.query.case_sensitive = !state.query.case_sensitive;
                    output.query_changed = true;
                }

                ui.separator();

                // ── Scope selector ─────────────────────────────────────────
                let proj_active = state.query.scope == SearchScope::Project;
                if ui
                    .selectable_label(!proj_active, "Current File")
                    .on_hover_text("Search the active file only")
                    .clicked()
                    && proj_active
                {
                    state.query.scope = SearchScope::File;
                    output.query_changed = true;
                }
                if ui
                    .selectable_label(proj_active, "Project")
                    .on_hover_text("Search all project files (respects .gitignore)")
                    .clicked()
                    && !proj_active
                {
                    state.query.scope = SearchScope::Project;
                    output.query_changed = true;
                }

                ui.separator();

                // ── Match counter ──────────────────────────────────────────
                let total = match state.query.scope {
                    SearchScope::File => file_match_count,
                    SearchScope::Project => state.project_matches.len(),
                };
                if state.query.is_non_empty() {
                    let counter_color = if total == 0 {
                        palette.error
                    } else {
                        ui.visuals().text_color()
                    };
                    let idx = state.active_index.map(|i| i + 1).unwrap_or(0);
                    let counter_text = if total == 0 {
                        "No results".to_owned()
                    } else {
                        format!("{idx} / {total}")
                    };
                    ui.label(RichText::new(counter_text).color(counter_color).small());
                }

                // ── Prev / Next navigation ─────────────────────────────────
                let prev_btn = ui
                    .add_enabled(
                        total > 0,
                        egui::Button::new("▲").min_size(egui::vec2(22.0, 22.0)),
                    )
                    .on_hover_text("Previous match (Shift+Enter / Shift+F3)");
                if prev_btn.clicked() {
                    output.prev_match = true;
                }
                let next_btn = ui
                    .add_enabled(
                        total > 0,
                        egui::Button::new("▼").min_size(egui::vec2(22.0, 22.0)),
                    )
                    .on_hover_text("Next match (Enter / F3)");
                if next_btn.clicked() {
                    output.next_match = true;
                }

                ui.separator();

                // ── Replace mode toggle ────────────────────────────────────
                let replace_active = state.mode == SearchMode::Replace;
                if ui
                    .selectable_label(replace_active, "Replace")
                    .on_hover_text("Show replace field (Ctrl+H)")
                    .clicked()
                {
                    state.mode = if replace_active {
                        SearchMode::Find
                    } else {
                        SearchMode::Replace
                    };
                }

                // ── Close button ───────────────────────────────────────────
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(RichText::new("✕").small())
                        .on_hover_text("Close search (Escape)")
                        .clicked()
                    {
                        output.closed = true;
                    }
                });
            });

            // ── Regex compile error ────────────────────────────────────────
            if let Some(err) = &state.compile_error {
                ui.colored_label(palette.error, format!("⚠  Regex error: {err}"));
            }

            // ── Replace row ────────────────────────────────────────────────
            if state.mode == SearchMode::Replace {
                ui.horizontal(|ui| {
                    let te = TextEdit::singleline(&mut state.query.replacement)
                        .id(egui::Id::new("search_bottom_replacement"))
                        .hint_text("Replace with…")
                        .desired_width(260.0);
                    ui.add(te);

                    let has_active = state.active_file_match().is_some()
                        || (state.query.scope == SearchScope::Project
                            && state.active_project_match().is_some());
                    if ui
                        .add_enabled(has_active, egui::Button::new("Replace"))
                        .on_hover_text("Replace this match")
                        .clicked()
                    {
                        output.replace_one = true;
                    }

                    let has_matches = match state.query.scope {
                        SearchScope::File => file_match_count > 0,
                        SearchScope::Project => !state.project_matches.is_empty(),
                    };
                    if ui
                        .add_enabled(has_matches, egui::Button::new("Replace All"))
                        .on_hover_text(
                            "Replace all matches — shows confirmation dialog before writing",
                        )
                        .clicked()
                    {
                        // Gate behind confirmation; `app.rs` renders the dialog.
                        state.request_replace_confirm();
                    }
                });

                // Last replace report
                if let Some(report) = &state.last_replace_report {
                    let msg = if report.failures.is_empty() {
                        format!(
                            "✓ Replaced {} occurrence(s) in {} file(s).",
                            report.replaced, report.files_affected
                        )
                    } else {
                        format!(
                            "⚠ Replaced {} in {} file(s); {} error(s).",
                            report.replaced,
                            report.files_affected,
                            report.failures.len()
                        )
                    };
                    let report_color = if report.failures.is_empty() {
                        palette.success
                    } else {
                        palette.warning
                    };
                    ui.label(RichText::new(msg).small().color(report_color));
                }
            }

            // ── Glob filter row ────────────────────────────────────────────
            // Only relevant for Project scope.
            if state.query.scope == SearchScope::Project {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Include:").small());
                    let inc = ui.add(
                        TextEdit::singleline(&mut state.query.include_glob)
                            .id(egui::Id::new("search_bottom_include"))
                            .hint_text("*.rs")
                            .desired_width(100.0),
                    );
                    if inc.changed() {
                        output.query_changed = true;
                    }
                    if !state.query.include_glob.is_empty()
                        && glob::Pattern::new(&state.query.include_glob).is_err()
                    {
                        ui.colored_label(Color32::RED, "⚠ invalid glob");
                    }

                    ui.separator();

                    ui.label(RichText::new("Exclude:").small());
                    let exc = ui.add(
                        TextEdit::singleline(&mut state.query.exclude_glob)
                            .id(egui::Id::new("search_bottom_exclude"))
                            .hint_text("tests/**")
                            .desired_width(100.0),
                    );
                    if exc.changed() {
                        output.query_changed = true;
                    }
                    if !state.query.exclude_glob.is_empty()
                        && glob::Pattern::new(&state.query.exclude_glob).is_err()
                    {
                        ui.colored_label(Color32::RED, "⚠ invalid glob");
                    }

                    // Progress spinner while background search is running
                    if state.project_searching() {
                        ui.spinner();
                        ui.label(
                            RichText::new(format!(
                                "Searching… {} match(es) so far",
                                state.project_matches.len()
                            ))
                            .small(),
                        );
                    } else if state.project_done
                        && state.project_matches.is_empty()
                        && state.query.is_non_empty()
                        && state.compile_error.is_none()
                    {
                        ui.label(
                            RichText::new("No results in project")
                                .small()
                                .color(palette.muted_text),
                        );
                    }
                });
            }

            ui.separator();

            // ── Results list — virtual scrolling + file-group collapse ─────
            //
            // `build_result_rows` is defined in the file above and operates on
            // `SearchState::project_matches` + `collapsed_files`.  We call it
            // every frame because it is cheap and the collapse state can change
            // on any click.
            //
            // `ScrollArea::show_rows` takes a row count and a height estimate
            // and renders *only the visible rows*, calling our closure for the
            // slice `[first_visible..=last_visible]`.  This keeps frame time
            // constant regardless of result count.
            let rows = if state.query.scope == SearchScope::Project {
                build_result_rows(state)
            } else {
                // For file-scope, synthesise a flat MatchEntry list so the
                // same virtual-scroll infrastructure works.
                (0..state.file_matches.len())
                    .map(|i| ResultRow::MatchEntry { match_idx: i })
                    .collect()
            };
            let row_count = rows.len();

            egui::ScrollArea::vertical()
                .id_source("search_bottom_results")
                .auto_shrink([false, false])
                .show_rows(ui, ROW_HEIGHT, row_count, |ui, visible_range| {
                    // We defer mutations (toggle collapse, click) to after the
                    // rendering loop to avoid simultaneous mutable borrows.
                    let mut toggle_path: Option<std::path::PathBuf> = None;
                    let mut clicked_match_idx: Option<usize> = None;

                    for row_idx in visible_range {
                        match &rows[row_idx] {
                            ResultRow::FileHeader {
                                first_match_idx,
                                match_count,
                            } => {
                                // Look up the path from the project_matches vec.
                                let path = state.project_matches[*first_match_idx].path.clone();
                                let collapsed = state.is_file_collapsed(&path);
                                let arrow = if collapsed { "▶" } else { "▼" };
                                let file_name = path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| path.display().to_string());

                                ui.horizontal(|ui| {
                                    // Relative path prefix (parent directory)
                                    let dir_prefix = path
                                        .parent()
                                        .and_then(|p| p.file_name())
                                        .map(|n| format!("{}/", n.to_string_lossy()))
                                        .unwrap_or_default();

                                    let resp = ui
                                        .selectable_label(
                                            false,
                                            RichText::new(format!(
                                                "{arrow} {dir_prefix}{file_name}  ({match_count})"
                                            ))
                                            .strong(),
                                        )
                                        .on_hover_text(path.display().to_string());
                                    if resp.clicked() {
                                        toggle_path = Some(path);
                                    }
                                });
                            }

                            ResultRow::MatchEntry { match_idx } => {
                                // Resolve the match from the correct source vec.
                                let m = if state.query.scope == SearchScope::Project {
                                    &state.project_matches[*match_idx]
                                } else {
                                    &state.file_matches[*match_idx]
                                };
                                let is_active = state.active_index == Some(*match_idx);

                                // ── Build highlighted LayoutJob ──────────
                                // The match column `m.col` and byte-length
                                // `m.byte_range.len()` tell us which portion of
                                // the preview to highlight yellow.
                                let preview = m.line_preview.trim();
                                let match_len = m.byte_range.end.saturating_sub(m.byte_range.start);
                                let col = m.col.min(preview.len());
                                let end = col.saturating_add(match_len).min(preview.len());

                                let normal_color = if is_active {
                                    ui.visuals().strong_text_color()
                                } else {
                                    ui.visuals().text_color()
                                };
                                let highlight_bg = Color32::from_rgb(255, 210, 0);
                                let no_bg = Color32::TRANSPARENT;

                                let mut job = egui::text::LayoutJob::default();

                                macro_rules! seg {
                                    ($text:expr, $color:expr, $bg:expr) => {
                                        job.append(
                                            $text,
                                            0.0,
                                            egui::text::TextFormat {
                                                font_id: egui::FontId::monospace(12.5),
                                                color: $color,
                                                background: $bg,
                                                ..Default::default()
                                            },
                                        )
                                    };
                                }

                                if col < preview.len() && end <= preview.len() && col <= end {
                                    if col > 0 {
                                        seg!(&preview[..col], normal_color, no_bg);
                                    }
                                    if col < end {
                                        seg!(&preview[col..end], Color32::BLACK, highlight_bg);
                                    }
                                    if end < preview.len() {
                                        seg!(&preview[end..], normal_color, no_bg);
                                    }
                                } else {
                                    seg!(preview, normal_color, no_bg);
                                }

                                ui.horizontal(|ui| {
                                    ui.add_space(16.0); // indent

                                    // Line number
                                    ui.label(
                                        RichText::new(format!("{:>4}", m.line + 1))
                                            .color(palette.muted_text)
                                            .monospace(),
                                    );
                                    ui.add_space(6.0);

                                    let row_resp = ui.add(
                                        egui::Label::new(job)
                                            .sense(egui::Sense::click())
                                            .truncate(true),
                                    );
                                    if row_resp.clicked() {
                                        clicked_match_idx = Some(*match_idx);
                                    }
                                });
                            }
                        }
                    }

                    // Apply deferred mutations.
                    if let Some(path) = toggle_path {
                        state.toggle_file_collapsed(&path);
                    }
                    if let Some(idx) = clicked_match_idx {
                        output.project_result_clicked = Some(idx);
                    }
                });

            // ── Failure summary ────────────────────────────────────────────
            if !state.project_failures.is_empty() {
                ui.collapsing(
                    format!("{} file(s) could not be read", state.project_failures.len()),
                    |ui| {
                        for (path, err) in &state.project_failures {
                            ui.label(format!("{}: {err}", path.display()));
                        }
                    },
                );
            }

            // ── Escape closes the panel ────────────────────────────────────
            if ui
                .ctx()
                .input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
            {
                output.closed = true;
            }

    output
}
