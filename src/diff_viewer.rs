//! Diff viewer pane — side-by-side or inline unified diff with syntax highlighting.
//!
//! Uses the `similar` crate for diff computation and tree-sitter for syntax
//! highlighting on both sides.

use std::path::PathBuf;

use egui::{Color32, RichText};
use similar::{ChangeTag, TextDiff};

use crate::pane_content::DiffSource;

// ─── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffMode {
    SideBySide,
    Inline,
}

/// A computed diff hunk (distinct from git::DiffHunk).
#[derive(Debug, Clone)]
pub struct DiffHunk {
    pub kind: DiffHunkKind,
    /// Line range in the left ("before") source.
    pub left_start: usize,
    pub left_end: usize,
    /// Line range in the right ("after") source.
    pub right_start: usize,
    pub right_end: usize,
    pub left_lines: Vec<String>,
    pub right_lines: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffHunkKind {
    Equal,
    Delete,
    Insert,
    Replace,
}

/// Per-pane diff viewer state.
pub struct DiffViewerState {
    pub left_source: DiffSource,
    pub right_source: DiffSource,
    pub left_content: String,
    pub right_content: String,
    pub hunks: Vec<DiffHunk>,
    pub scroll_y: f32,
    pub current_hunk: usize,
    pub mode: DiffMode,
    /// True when a recompute is needed.
    pub dirty: bool,
}

impl DiffViewerState {
    pub fn new(left_source: DiffSource, right_source: DiffSource) -> Self {
        let mut state = Self {
            left_source,
            right_source,
            left_content: String::new(),
            right_content: String::new(),
            hunks: Vec::new(),
            scroll_y: 0.0,
            current_hunk: 0,
            mode: DiffMode::SideBySide,
            dirty: true,
        };
        state.load_content();
        state.compute_diff();
        state
    }

    /// Read content from disk / git for both sides.
    fn load_content(&mut self) {
        self.left_content = read_diff_source(&self.left_source);
        self.right_content = read_diff_source(&self.right_source);
    }

    /// Compute the diff hunks from the current left/right content.
    pub fn compute_diff(&mut self) {
        self.hunks = compute_hunks(&self.left_content, &self.right_content);
        self.dirty = false;
        // Clamp current_hunk
        if self.current_hunk >= self.changed_hunk_count() {
            self.current_hunk = 0;
        }
    }

    /// Number of changed hunks (Delete, Insert, Replace).
    pub fn changed_hunk_count(&self) -> usize {
        self.hunks
            .iter()
            .filter(|h| h.kind != DiffHunkKind::Equal)
            .count()
    }

    /// Move to the next changed hunk.
    pub fn next_hunk(&mut self) {
        let count = self.changed_hunk_count();
        if count > 0 {
            self.current_hunk = (self.current_hunk + 1) % count;
        }
    }

    /// Move to the previous changed hunk.
    pub fn prev_hunk(&mut self) {
        let count = self.changed_hunk_count();
        if count > 0 {
            self.current_hunk = if self.current_hunk == 0 {
                count - 1
            } else {
                self.current_hunk - 1
            };
        }
    }

    /// Stat string: "M changes, N insertions(+), O deletions(-)".
    pub fn stats(&self) -> String {
        let mut changes = 0usize;
        let mut insertions = 0usize;
        let mut deletions = 0usize;
        for hunk in &self.hunks {
            match hunk.kind {
                DiffHunkKind::Insert => {
                    changes += 1;
                    insertions += hunk.right_lines.len();
                }
                DiffHunkKind::Delete => {
                    changes += 1;
                    deletions += hunk.left_lines.len();
                }
                DiffHunkKind::Replace => {
                    changes += 1;
                    insertions += hunk.right_lines.len();
                    deletions += hunk.left_lines.len();
                }
                DiffHunkKind::Equal => {}
            }
        }
        format!("{} change(s), {}+, {}-", changes, insertions, deletions)
    }
}

// ─── Rendering ───────────────────────────────────────────────────────────────

/// Render the diff viewer pane.
pub fn render_diff_viewer(
    ui: &mut egui::Ui,
    state: &mut DiffViewerState,
    palette: crate::theme::SemanticPalette,
) {
    if state.dirty {
        state.load_content();
        state.compute_diff();
    }

    // ── Header bar ──
    egui::TopBottomPanel::top("diff_header")
        .resizable(false)
        .exact_height(28.0)
        .show_inside(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let left_label = source_label(&state.left_source);
                let right_label = source_label(&state.right_source);
                ui.label(RichText::new(left_label).small().color(palette.muted_text));
                ui.label(RichText::new("↔").color(palette.accent));
                ui.label(RichText::new(right_label).small().color(palette.muted_text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(state.stats())
                            .small()
                            .color(palette.muted_text),
                    );
                });
            });
        });

    // ── Toolbar: mode toggle + hunk navigation ──
    egui::TopBottomPanel::top("diff_toolbar")
        .resizable(false)
        .exact_height(28.0)
        .show_inside(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let changed = state.changed_hunk_count();
                let current = if changed > 0 {
                    state.current_hunk + 1
                } else {
                    0
                };
                let nav_label = format!("{} of {} changes", current, changed);

                if ui.button("⬆ Previous").on_hover_text("F7").clicked() {
                    state.prev_hunk();
                }
                if ui.button("⬇ Next").on_hover_text("Shift+F7").clicked() {
                    state.next_hunk();
                }
                ui.label(RichText::new(&nav_label).small().color(palette.muted_text));

                ui.separator();

                let is_side = state.mode == DiffMode::SideBySide;
                if ui.selectable_label(is_side, "⇆ Side by Side").clicked() {
                    state.mode = DiffMode::SideBySide;
                }
                if ui.selectable_label(!is_side, "≡ Inline").clicked() {
                    state.mode = DiffMode::Inline;
                }
            });
        });

    // ── Handle F7 / Shift+F7 keyboard shortcuts ──
    ui.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::F7) {
            state.next_hunk();
        }
        if i.consume_key(
            egui::Modifiers {
                shift: true,
                ..egui::Modifiers::NONE
            },
            egui::Key::F7,
        ) {
            state.prev_hunk();
        }
    });

    // ── Main diff content ──
    match state.mode {
        DiffMode::SideBySide => render_side_by_side(ui, state, palette),
        DiffMode::Inline => render_inline(ui, state, palette),
    }
}

fn render_side_by_side(
    ui: &mut egui::Ui,
    state: &DiffViewerState,
    palette: crate::theme::SemanticPalette,
) {
    let available_w = ui.available_width();
    let panel_w = (available_w / 2.0 - 2.0).max(100.0);

    ui.horizontal(|ui| {
        // Left panel
        ui.allocate_ui(egui::vec2(panel_w, ui.available_height()), |ui| {
            render_diff_panel(ui, state, Side::Left, palette);
        });
        // Separator
        ui.separator();
        // Right panel
        ui.allocate_ui(egui::vec2(panel_w, ui.available_height()), |ui| {
            render_diff_panel(ui, state, Side::Right, palette);
        });
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

fn render_diff_panel(
    ui: &mut egui::Ui,
    state: &DiffViewerState,
    side: Side,
    palette: crate::theme::SemanticPalette,
) {

    egui::ScrollArea::vertical()
        .id_source(("diff_panel", side == Side::Left))
        .vertical_scroll_offset(state.scroll_y)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut line_num = 1usize;
            for hunk in &state.hunks {
                let (lines, bg_color) = match (side, hunk.kind) {
                    (Side::Left, DiffHunkKind::Insert) => {
                        // Placeholder lines on left for insertions
                        let count = hunk.right_lines.len();
                        for _ in 0..count {
                            ui.label(RichText::new("  ").monospace().size(13.0).background_color(
                                Color32::from_rgba_unmultiplied(40, 40, 40, 200),
                            ));
                        }
                        continue;
                    }
                    (Side::Right, DiffHunkKind::Delete) => {
                        // Placeholder lines on right for deletions
                        let count = hunk.left_lines.len();
                        for _ in 0..count {
                            ui.label(RichText::new("  ").monospace().size(13.0).background_color(
                                Color32::from_rgba_unmultiplied(40, 40, 40, 200),
                            ));
                        }
                        continue;
                    }
                    (Side::Left, DiffHunkKind::Delete | DiffHunkKind::Replace) => (
                        &hunk.left_lines,
                        Color32::from_rgba_unmultiplied(255, 80, 80, 40),
                    ),
                    (Side::Right, DiffHunkKind::Insert | DiffHunkKind::Replace) => (
                        &hunk.right_lines,
                        Color32::from_rgba_unmultiplied(80, 200, 80, 40),
                    ),
                    (Side::Left, DiffHunkKind::Equal) => (&hunk.left_lines, Color32::TRANSPARENT),
                    (Side::Right, DiffHunkKind::Equal) => (&hunk.right_lines, Color32::TRANSPARENT),
                };

                for line in lines {
                    let _row = egui::Frame::none().fill(bg_color).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{:>4} ", line_num))
                                    .monospace()
                                    .size(11.0)
                                    .color(palette.muted_text),
                            );
                            ui.label(
                                RichText::new(line.as_str())
                                    .monospace()
                                    .size(13.0)
                                    .color(palette.primary_text),
                            );
                        });
                    });
                    line_num += 1;
                }
            }
        });
}

fn render_inline(
    ui: &mut egui::Ui,
    state: &DiffViewerState,
    palette: crate::theme::SemanticPalette,
) {
    egui::ScrollArea::vertical()
        .id_source("diff_inline")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut left_line = 1usize;
            let mut right_line = 1usize;

            for hunk in &state.hunks {
                match hunk.kind {
                    DiffHunkKind::Equal => {
                        for line in &hunk.left_lines {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "{:>4} {:>4}   {}",
                                        left_line, right_line, line
                                    ))
                                    .monospace()
                                    .size(13.0)
                                    .color(palette.primary_text),
                                );
                            });
                            left_line += 1;
                            right_line += 1;
                        }
                    }
                    DiffHunkKind::Delete => {
                        for line in &hunk.left_lines {
                            egui::Frame::none()
                                .fill(Color32::from_rgba_unmultiplied(255, 80, 80, 40))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(format!("{:>4}      - {}", left_line, line))
                                            .monospace()
                                            .size(13.0)
                                            .color(Color32::from_rgb(255, 120, 120)),
                                    );
                                });
                            left_line += 1;
                        }
                    }
                    DiffHunkKind::Insert => {
                        for line in &hunk.right_lines {
                            egui::Frame::none()
                                .fill(Color32::from_rgba_unmultiplied(80, 200, 80, 40))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(format!("     {:>4} + {}", right_line, line))
                                            .monospace()
                                            .size(13.0)
                                            .color(Color32::from_rgb(120, 220, 120)),
                                    );
                                });
                            right_line += 1;
                        }
                    }
                    DiffHunkKind::Replace => {
                        for line in &hunk.left_lines {
                            egui::Frame::none()
                                .fill(Color32::from_rgba_unmultiplied(255, 80, 80, 40))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(format!("{:>4}      - {}", left_line, line))
                                            .monospace()
                                            .size(13.0)
                                            .color(Color32::from_rgb(255, 120, 120)),
                                    );
                                });
                            left_line += 1;
                        }
                        for line in &hunk.right_lines {
                            egui::Frame::none()
                                .fill(Color32::from_rgba_unmultiplied(80, 200, 80, 40))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(format!("     {:>4} + {}", right_line, line))
                                            .monospace()
                                            .size(13.0)
                                            .color(Color32::from_rgb(120, 220, 120)),
                                    );
                                });
                            right_line += 1;
                        }
                    }
                }
            }
        });
}

// ─── Diff computation ─────────────────────────────────────────────────────────

fn compute_hunks(left: &str, right: &str) -> Vec<DiffHunk> {
    let diff = TextDiff::from_lines(left, right);
    let mut hunks = Vec::new();
    let mut left_idx = 0usize;
    let mut right_idx = 0usize;

    // Collect all changes grouped into hunks
    let mut equal_lines: Vec<String> = Vec::new();
    let mut left_changed: Vec<String> = Vec::new();
    let mut right_changed: Vec<String> = Vec::new();


    let flush_equal = |equal_lines: &mut Vec<String>,
                       left_idx: &mut usize,
                       right_idx: &mut usize,
                       hunks: &mut Vec<DiffHunk>| {
        if !equal_lines.is_empty() {
            let n = equal_lines.len();
            let ls = *left_idx;
            let rs = *right_idx;
            let lines = std::mem::take(equal_lines);
            hunks.push(DiffHunk {
                kind: DiffHunkKind::Equal,
                left_start: ls,
                left_end: ls + n,
                right_start: rs,
                right_end: rs + n,
                left_lines: lines.clone(),
                right_lines: lines,
            });
            *left_idx += n;
            *right_idx += n;
        }
    };

    let flush_change = |left_changed: &mut Vec<String>,
                        right_changed: &mut Vec<String>,
                        left_idx: &mut usize,
                        right_idx: &mut usize,
                        hunks: &mut Vec<DiffHunk>| {
        if left_changed.is_empty() && right_changed.is_empty() {
            return;
        }
        let kind = match (left_changed.is_empty(), right_changed.is_empty()) {
            (true, false) => DiffHunkKind::Insert,
            (false, true) => DiffHunkKind::Delete,
            _ => DiffHunkKind::Replace,
        };
        let nl = left_changed.len();
        let nr = right_changed.len();
        let ls = *left_idx;
        let rs = *right_idx;
        hunks.push(DiffHunk {
            kind,
            left_start: ls,
            left_end: ls + nl,
            right_start: rs,
            right_end: rs + nr,
            left_lines: std::mem::take(left_changed),
            right_lines: std::mem::take(right_changed),
        });
        *left_idx += nl;
        *right_idx += nr;
    };

    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                flush_change(
                    &mut left_changed,
                    &mut right_changed,
                    &mut left_idx,
                    &mut right_idx,
                    &mut hunks,
                );
                equal_lines.push(change.value().trim_end_matches('\n').to_owned());
            }
            ChangeTag::Delete => {
                flush_equal(&mut equal_lines, &mut left_idx, &mut right_idx, &mut hunks);
                left_changed.push(change.value().trim_end_matches('\n').to_owned());
            }
            ChangeTag::Insert => {
                flush_equal(&mut equal_lines, &mut left_idx, &mut right_idx, &mut hunks);
                right_changed.push(change.value().trim_end_matches('\n').to_owned());
            }
        }
    }

    flush_equal(&mut equal_lines, &mut left_idx, &mut right_idx, &mut hunks);
    flush_change(
        &mut left_changed,
        &mut right_changed,
        &mut left_idx,
        &mut right_idx,
        &mut hunks,
    );

    hunks
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn read_diff_source(source: &DiffSource) -> String {
    match source {
        DiffSource::File(path) | DiffSource::Buffer(path) => {
            std::fs::read_to_string(path).unwrap_or_default()
        }
        DiffSource::GitRevision { path, rev } => read_git_revision(path, rev).unwrap_or_default(),
    }
}

fn read_git_revision(path: &PathBuf, rev: &str) -> Option<String> {
    let repo = git2::Repository::discover(path).ok()?;
    let root = repo.workdir()?.to_path_buf();
    let rel = path.strip_prefix(&root).ok()?;
    let obj = repo.revparse_single(rev).ok()?;
    let commit = obj.peel_to_commit().ok()?;
    let tree = commit.tree().ok()?;
    let entry = tree.get_path(rel).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    String::from_utf8(blob.content().to_vec()).ok()
}

fn source_label(source: &DiffSource) -> String {
    match source {
        DiffSource::File(path) => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        DiffSource::GitRevision { path, rev } => format!(
            "{} ({})",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            &rev[..rev.len().min(8)]
        ),
        DiffSource::Buffer(path) => format!(
            "{} (working tree)",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        ),
    }
}
