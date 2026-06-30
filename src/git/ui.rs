//! Git UI rendering: diff gutter bars, source-control panel, branch picker, and
//! inline blame gutter.

use std::path::PathBuf;

use egui::{Align2, Color32, Rect};

use crate::git::{
    BlameLine, CommitInfo, ConflictSides, DiffHunk, FileStatus, GitRepo, HunkKind, NetworkProgress,
    NetworkStage, Resolution, TagInfo,
};

/// Actions emitted by `render_git_panel` for the app shell to apply.
#[derive(Debug, Clone)]
pub enum GitPanelAction {
    None,
    Stage(PathBuf),
    Unstage(PathBuf),
    Commit(String),
    ShowBranchPicker,
    // ── Remote operations ──
    Fetch,
    Pull,
    Push,
    // ── Modal launchers ──
    ShowLog,
    ShowTags,
    ShowConflicts,
    // ── Stash ──
    StashSave {
        message: String,
        include_untracked: bool,
    },
    StashApply(usize),
    StashPop(usize),
    StashDrop(usize),
}

/// Paint 3px diff indicator bars in the gutter gap between line numbers and text.
///
/// `gutter_x` is the right edge of the line-number gutter. `text_origin` is the
/// top-left corner of the text area (used to derive vertical line positions).
pub fn render_diff_gutters(
    painter: &egui::Painter,
    hunks: &[DiffHunk],
    gutter_x: f32,
    text_origin: egui::Pos2,
    line_height: f32,
) {
    let w = 3.0; // gutter bar width in pixels
    for hunk in hunks {
        let color = match hunk.kind {
            HunkKind::Added => Color32::from_rgb(40, 180, 40),
            HunkKind::Removed => Color32::from_rgb(220, 50, 50),
            HunkKind::Modified => Color32::from_rgb(30, 130, 220),
        };
        let y0 = text_origin.y + hunk.line_start as f32 * line_height;
        let y1 = y0 + hunk.line_count as f32 * line_height;
        let rect = Rect::from_min_max(
            egui::pos2(gutter_x - w - 2.0, y0),
            egui::pos2(gutter_x - 2.0, y1),
        );
        painter.rect_filled(rect, 0.0, color);

        // Removed hunks: show a 2px line at the boundary.
        if hunk.kind == HunkKind::Removed {
            painter.line_segment(
                [
                    egui::pos2(gutter_x - 8.0, y0),
                    egui::pos2(gutter_x - 2.0, y0),
                ],
                egui::Stroke::new(2.0, color),
            );
        }
    }
}

/// Render the source-control side panel.
///
/// Returns a `GitPanelAction` describing any staging, unstaging, or commit the
/// app should perform. The panel is self-contained and does not mutate `GitRepo`
/// directly.
pub fn render_git_panel(
    ui: &mut egui::Ui,
    git: &mut GitRepo,
    commit_msg: &mut String,
    stash_msg: &mut String,
) -> GitPanelAction {
    let mut action = GitPanelAction::None;

    // Branch switcher.
    ui.horizontal(|ui| {
        ui.label("⎇");
        ui.label(&git.branch);
        if ui.small_button("⌄").clicked() {
            action = GitPanelAction::ShowBranchPicker;
        }
    });

    // Remote operation toolbar.
    ui.horizontal(|ui| {
        if ui.button("⟳ Fetch").on_hover_text("Fetch from remote").clicked() {
            action = GitPanelAction::Fetch;
        }
        if ui.button("⬇ Pull").on_hover_text("Pull from remote").clicked() {
            action = GitPanelAction::Pull;
        }
        if ui.button("⬆ Push").on_hover_text("Push to remote").clicked() {
            action = GitPanelAction::Push;
        }
    });

    // Secondary toolbar: log, tags, conflicts.
    ui.horizontal(|ui| {
        if ui.button("History").on_hover_text("Show commit log").clicked() {
            action = GitPanelAction::ShowLog;
        }
        if ui.button("Tags").on_hover_text("Manage tags").clicked() {
            action = GitPanelAction::ShowTags;
        }
        let conflicts = git.conflicted_paths();
        if !conflicts.is_empty() {
            let label = format!("⚠ Conflicts ({})", conflicts.len());
            if ui
                .button(egui::RichText::new(label).color(Color32::from_rgb(220, 120, 40)))
                .on_hover_text("Resolve merge conflicts")
                .clicked()
            {
                action = GitPanelAction::ShowConflicts;
            }
        }
    });
    ui.separator();

    // Split files into staged and unstaged groups.
    let mut staged: Vec<(PathBuf, FileStatus)> = Vec::new();
    let mut unstaged: Vec<(PathBuf, FileStatus)> = Vec::new();
    for (path, status) in &git.status_map {
        if *status == FileStatus::Unmodified {
            continue;
        }
        if git.is_staged(path) {
            staged.push((path.clone(), status.clone()));
        } else {
            unstaged.push((path.clone(), status.clone()));
        }
    }

    // Staged changes.
    ui.collapsing("Staged", |ui| {
        if staged.is_empty() {
            ui.weak("No staged changes");
        } else {
            for (path, status) in staged {
                render_file_row(ui, &path, &status, true, &mut action);
            }
        }
    });

    // Unstaged changes.
    ui.collapsing("Changes", |ui| {
        if unstaged.is_empty() {
            ui.weak("No changes");
        } else {
            for (path, status) in unstaged {
                render_file_row(ui, &path, &status, false, &mut action);
            }
        }
    });

    ui.separator();

    // Commit box.
    ui.label("Commit message");
    ui.text_edit_multiline(commit_msg);
    let has_staged = !git.staged_paths.is_empty();
    if !has_staged {
        ui.weak("No staged changes");
    }
    ui.add_enabled(
        has_staged && !commit_msg.is_empty(),
        egui::Button::new("Commit"),
    )
    .clicked()
    .then(|| {
        if has_staged && !commit_msg.is_empty() {
            action = GitPanelAction::Commit(commit_msg.clone());
        }
    });

    ui.separator();

    // Stash section.
    render_stash_section(ui, git, stash_msg, &mut action);

    action
}

/// Render the stash list and a save box inside the git panel.
fn render_stash_section(
    ui: &mut egui::Ui,
    git: &mut GitRepo,
    stash_msg: &mut String,
    action: &mut GitPanelAction,
) {
    let entries = git.stash_list();
    egui::CollapsingHeader::new(format!("Stashes ({})", entries.len()))
        .default_open(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(stash_msg)
                        .hint_text("Stash message")
                        .desired_width(120.0),
                );
                if ui.small_button("Stash").on_hover_text("Stash changes").clicked() {
                    *action = GitPanelAction::StashSave {
                        message: stash_msg.clone(),
                        include_untracked: true,
                    };
                }
            });

            if entries.is_empty() {
                ui.weak("No stashes");
            }
            for entry in entries {
                ui.horizontal(|ui| {
                    ui.label(format!("[{}] {}", entry.short_oid, entry.message))
                        .on_hover_text(&entry.message);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("✖").on_hover_text("Drop").clicked() {
                            *action = GitPanelAction::StashDrop(entry.index);
                        }
                        if ui.small_button("Pop").on_hover_text("Apply and drop").clicked() {
                            *action = GitPanelAction::StashPop(entry.index);
                        }
                        if ui.small_button("Apply").on_hover_text("Apply and keep").clicked() {
                            *action = GitPanelAction::StashApply(entry.index);
                        }
                    });
                });
            }
        });
}

fn render_file_row(
    ui: &mut egui::Ui,
    path: &PathBuf,
    status: &FileStatus,
    staged: bool,
    action: &mut GitPanelAction,
) {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let prefix = status_prefix(status);
    ui.horizontal(|ui| {
        let label_color = if *status == FileStatus::Conflicted {
            Color32::from_rgb(220, 50, 50)
        } else {
            ui.visuals().text_color()
        };
        ui.colored_label(label_color, format!("{} {}", prefix, name))
            .on_hover_text(path.to_string_lossy().as_ref());

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let button_label = if staged { "−" } else { "+" };
            if ui.small_button(button_label).clicked() {
                *action = if staged {
                    GitPanelAction::Unstage(path.clone())
                } else {
                    GitPanelAction::Stage(path.clone())
                };
            }
        });
    });
}

fn status_prefix(s: &FileStatus) -> &'static str {
    match s {
        FileStatus::Modified => "M",
        FileStatus::Added => "A",
        FileStatus::Deleted => "D",
        FileStatus::Untracked => "?",
        FileStatus::Conflicted => "!",
        _ => " ",
    }
}

/// Render a centered branch-picker modal. Returns the selected branch name, if any.
pub fn render_branch_picker(
    ctx: &egui::Context,
    branches: &[String],
    current: &str,
    query: &mut String,
) -> Option<String> {
    let mut result = None;
    egui::Window::new("Switch branch")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .fixed_size([300.0, 400.0])
        .show(ctx, |ui| {
            ui.text_edit_singleline(query);
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                for branch in branches {
                    if !query.is_empty() && !branch.contains(query.as_str()) {
                        continue;
                    }
                    let active = branch == current;
                    if ui.selectable_label(active, branch).clicked() {
                        result = Some(branch.clone());
                    }
                }
            });
        });
    result
}

/// Width of the inline blame gutter in pixels.
pub const BLAME_GUTTER_WIDTH: f32 = 120.0;

/// Render the inline blame gutter inside `rect`.
///
/// `scroll_y` is the current vertical scroll offset in pixels. `line_height` is
/// the pixel height of one editor row. `visible_rows` is the number of rows that
/// fit in the viewport.
pub fn render_blame_gutter(
    painter: &egui::Painter,
    rect: Rect,
    blame_lines: &[BlameLine],
    scroll_y: f32,
    line_height: f32,
    visible_rows: usize,
) {
    let first_line = (scroll_y / line_height) as usize;
    let font_id = egui::FontId::monospace(11.0);
    let text_color = Color32::from_rgb(140, 140, 140);

    for i in first_line..(first_line + visible_rows).min(blame_lines.len()) {
        let b = &blame_lines[i];
        let y = rect.top() + i as f32 * line_height - scroll_y;
        let txt = format!("{} {:>10}", b.commit, truncate(&b.author, 10));
        painter.text(
            egui::pos2(rect.left() + 4.0, y),
            Align2::LEFT_TOP,
            &txt,
            font_id.clone(),
            text_color,
        );
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}

// ─── Commit log viewer ──────────────────────────────────────────────────────

/// Action emitted by the commit-log modal.
#[derive(Debug, Clone)]
pub enum LogAction {
    None,
    /// Cherry-pick the commit with this full SHA onto the current branch.
    CherryPick(String),
    /// Create a tag pointing at this commit (opens the tag manager prefilled).
    TagCommit(String),
    Close,
}

/// Render the commit-log modal. `commits` is newest-first.
pub fn render_log_viewer(ctx: &egui::Context, commits: &[CommitInfo]) -> LogAction {
    let mut action = LogAction::None;
    let mut open = true;
    egui::Window::new("Commit history")
        .collapsible(false)
        .resizable(true)
        .open(&mut open)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .default_size([620.0, 460.0])
        .show(ctx, |ui| {
            if commits.is_empty() {
                ui.weak("No commits yet.");
                return;
            }
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for commit in commits {
                        ui.horizontal(|ui| {
                            ui.monospace(
                                egui::RichText::new(&commit.short_oid)
                                    .color(Color32::from_rgb(200, 160, 60)),
                            );
                            ui.label(&commit.summary)
                                .on_hover_text(format!(
                                    "{}\n{} <{}>\n{}",
                                    commit.oid, commit.author, commit.email, commit.message
                                ));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("Cherry-pick").clicked() {
                                        action = LogAction::CherryPick(commit.oid.clone());
                                    }
                                    if ui.small_button("Tag").clicked() {
                                        action = LogAction::TagCommit(commit.oid.clone());
                                    }
                                },
                            );
                        });
                        ui.weak(format!("    {} • {}", commit.author, format_time(commit.time)));
                        ui.separator();
                    }
                });
        });
    if !open {
        return LogAction::Close;
    }
    action
}

// ─── Tag manager ────────────────────────────────────────────────────────────

/// Action emitted by the tag-manager modal.
#[derive(Debug, Clone)]
pub enum TagManagerAction {
    None,
    Create { name: String, message: String },
    Delete(String),
    Push(String),
    Close,
}

/// Render the tag-manager modal. `new_name` / `new_message` hold the create-form
/// input across frames.
pub fn render_tag_manager(
    ctx: &egui::Context,
    tags: &[TagInfo],
    new_name: &mut String,
    new_message: &mut String,
) -> TagManagerAction {
    let mut action = TagManagerAction::None;
    let mut open = true;
    egui::Window::new("Tags")
        .collapsible(false)
        .resizable(true)
        .open(&mut open)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .default_size([460.0, 420.0])
        .show(ctx, |ui| {
            ui.label("Create tag at HEAD");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(new_name)
                        .hint_text("name (e.g. v1.0)")
                        .desired_width(120.0),
                );
                ui.add(
                    egui::TextEdit::singleline(new_message)
                        .hint_text("message (optional → annotated)")
                        .desired_width(180.0),
                );
                if ui
                    .add_enabled(!new_name.is_empty(), egui::Button::new("Create"))
                    .clicked()
                {
                    action = TagManagerAction::Create {
                        name: new_name.clone(),
                        message: new_message.clone(),
                    };
                }
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if tags.is_empty() {
                        ui.weak("No tags");
                    }
                    for tag in tags {
                        ui.horizontal(|ui| {
                            ui.monospace(&tag.short_oid);
                            ui.strong(&tag.name);
                            if !tag.message.is_empty() {
                                ui.weak(&tag.message);
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("✖").on_hover_text("Delete").clicked() {
                                        action = TagManagerAction::Delete(tag.name.clone());
                                    }
                                    if ui.small_button("Push").clicked() {
                                        action = TagManagerAction::Push(tag.name.clone());
                                    }
                                },
                            );
                        });
                    }
                });
        });
    if !open {
        return TagManagerAction::Close;
    }
    action
}

// ─── Conflict resolver ──────────────────────────────────────────────────────

/// Render a three-pane (base / ours / theirs) conflict resolver modal.
///
/// `paths` are the repository-relative conflicted files. `selected` is the index
/// of the path whose sides are shown in `sides`. Returns an optional outcome:
/// `Ok(Some(resolution))` to take a whole side, `Ok(None)` for no action, or a
/// path-selection / close signal via the returned struct.
pub struct ConflictResolverOutcome {
    /// User clicked a different path in the list.
    pub select_path: Option<usize>,
    /// User chose to resolve the current path with a whole side.
    pub resolve: Option<Resolution>,
    /// User dismissed the modal.
    pub close: bool,
}

pub fn render_conflict_resolver(
    ctx: &egui::Context,
    paths: &[PathBuf],
    selected: usize,
    sides: &ConflictSides,
) -> ConflictResolverOutcome {
    let mut outcome = ConflictResolverOutcome {
        select_path: None,
        resolve: None,
        close: false,
    };
    let mut open = true;
    egui::Window::new("Resolve conflicts")
        .collapsible(false)
        .resizable(true)
        .open(&mut open)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .default_size([820.0, 560.0])
        .show(ctx, |ui| {
            if paths.is_empty() {
                ui.weak("No conflicts 🎉");
                return;
            }
            // File list.
            ui.horizontal(|ui| {
                ui.label("Conflicted files:");
                for (i, path) in paths.iter().enumerate() {
                    let name = path.to_string_lossy();
                    if ui.selectable_label(i == selected, name.as_ref()).clicked() {
                        outcome.select_path = Some(i);
                    }
                }
            });
            ui.separator();

            // Resolution buttons.
            ui.horizontal(|ui| {
                if ui.button("Take ours").clicked() {
                    outcome.resolve = Some(Resolution::Ours);
                }
                if ui.button("Take theirs").clicked() {
                    outcome.resolve = Some(Resolution::Theirs);
                }
                if ui.button("Take base").clicked() {
                    outcome.resolve = Some(Resolution::Base);
                }
            });
            ui.separator();

            // Three side-by-side panes.
            let pane_w = (ui.available_width() - 20.0) / 3.0;
            ui.horizontal_top(|ui| {
                conflict_pane(ui, "Base", sides.base.as_deref(), pane_w);
                conflict_pane(ui, "Ours", sides.ours.as_deref(), pane_w);
                conflict_pane(ui, "Theirs", sides.theirs.as_deref(), pane_w);
            });
        });
    if !open {
        outcome.close = true;
    }
    outcome
}

fn conflict_pane(ui: &mut egui::Ui, title: &str, content: Option<&str>, width: f32) {
    ui.vertical(|ui| {
        ui.set_width(width);
        ui.strong(title);
        egui::ScrollArea::vertical()
            .id_source(title)
            .max_height(420.0)
            .auto_shrink([false, false])
            .show(ui, |ui| match content {
                Some(text) => {
                    ui.monospace(text);
                }
                None => {
                    ui.weak("(absent)");
                }
            });
    });
}

// ─── Network progress bar ───────────────────────────────────────────────────

/// Render a thin progress strip for an in-flight network operation at the bottom
/// of the git panel.
pub fn render_network_progress(ui: &mut egui::Ui, progress: &NetworkProgress) {
    ui.separator();
    let label = match &progress.stage {
        NetworkStage::Connecting => format!("{}: connecting…", progress.op.label()),
        NetworkStage::Transferring { received, total } => {
            format!("{}: {}/{} objects", progress.op.label(), received, total)
        }
        NetworkStage::Pushing { pushed, total } => {
            format!("{}: {}/{} objects", progress.op.label(), pushed, total)
        }
        NetworkStage::Done(msg) => format!("✓ {}", msg),
        NetworkStage::Failed(msg) => format!("✗ {}", msg),
    };

    let color = match &progress.stage {
        NetworkStage::Failed(_) => Color32::from_rgb(220, 80, 80),
        NetworkStage::Done(_) => Color32::from_rgb(80, 180, 80),
        _ => ui.visuals().text_color(),
    };
    ui.colored_label(color, label);

    if let Some(fraction) = progress.stage.fraction() {
        ui.add(egui::ProgressBar::new(fraction).desired_height(4.0));
    } else if !progress.stage.is_terminal() {
        ui.spinner();
    }
}

/// Format a unix timestamp as a short local date-time string without pulling in
/// a date crate. Falls back to the raw epoch seconds on overflow.
fn format_time(secs: i64) -> String {
    // Minimal civil-time conversion (UTC) good enough for log display.
    if secs <= 0 {
        return "—".to_string();
    }
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // Convert days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        year, month, d, h, m, s
    )
}
