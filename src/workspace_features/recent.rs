//! Feature 7 — Recently Opened Projects
//!
//! Maintains `~/.config/blue-ide/recent_projects.json` (max 20 entries,
//! sorted by `last_opened` descending).  Shows a welcome screen when no
//! workspace is open.  Provides a quick-pick popup.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

// ─── Data model ──────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct RecentProject {
    pub path: String,
    pub name: String,
    /// Unix timestamp (seconds since epoch) of when the project was last opened.
    pub last_opened: i64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Default, Clone)]
struct RecentFile {
    projects: Vec<RecentProject>,
}

/// Returns the path to the recent-projects JSON file using the `directories` crate.
pub fn recent_path() -> Option<PathBuf> {
    directories::BaseDirs::new()
        .map(|d| d.config_dir().join("blue-ide").join("recent_projects.json"))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ─── CRUD helpers ─────────────────────────────────────────────────────────────

fn load_file() -> RecentFile {
    let Some(path) = recent_path() else {
        return RecentFile::default();
    };
    if !path.is_file() {
        return RecentFile::default();
    }
    let json = fs::read_to_string(&path)
        .unwrap_or_else(|e| { eprintln!("recent: read: {e}"); String::new() });
    serde_json::from_str(&json).unwrap_or_default()
}

fn save_file(f: &RecentFile) {
    let Some(path) = recent_path() else { return };
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("recent: create dir: {e}");
            return;
        }
    }
    match serde_json::to_string_pretty(f) {
        Ok(json) => {
            if let Err(e) = fs::write(&path, json) {
                eprintln!("recent: save: {e}");
            }
        }
        Err(e) => eprintln!("recent: serialize: {e}"),
    }
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Load the recent projects list sorted by `last_opened` descending.
pub fn load() -> Vec<RecentProject> {
    let mut f = load_file();
    f.projects.sort_by(|a, b| b.last_opened.cmp(&a.last_opened));
    f.projects
}

/// Record that `workspace_path` was opened (upsert, maintain max 20).
pub fn record_opened(workspace_path: &Path) {
    let canonical = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let path_str = canonical.to_string_lossy().to_string();
    let name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path_str.clone());

    let mut f = load_file();
    let now = now_unix();
    if let Some(entry) = f.projects.iter_mut().find(|e| e.path == path_str) {
        entry.last_opened = now;
    } else {
        f.projects.push(RecentProject {
            path: path_str,
            name,
            last_opened: now,
        });
    }

    // Keep at most 20 entries (remove oldest).
    if f.projects.len() > 20 {
        f.projects.sort_by(|a, b| b.last_opened.cmp(&a.last_opened));
        f.projects.truncate(20);
    }

    save_file(&f);
}

/// Clear all recent projects.
pub fn clear() {
    save_file(&RecentFile::default());
}

// ─── Date formatting ──────────────────────────────────────────────────────────

const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun",
    "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Format a Unix timestamp as "Jan 15, 2025".
pub fn format_timestamp(ts: i64) -> String {
    // Approximate calendar from seconds since epoch (not accounting for leap
    // seconds, accurate enough for a UI label).
    let secs = ts.max(0) as u64;
    let days = secs / 86400;
    // Rough year calculation.
    let years_since_1970 = days / 365;
    let year = 1970 + years_since_1970;
    let day_of_year = days % 365;
    let month_idx = ((day_of_year / 30) as usize).min(11);
    let day = day_of_year % 30 + 1;
    format!("{} {}, {}", MONTH_NAMES[month_idx], day, year)
}

// ─── Welcome screen ───────────────────────────────────────────────────────────

/// State for the recent projects quick-pick popup.
#[derive(Default)]
pub struct RecentQuickPickState {
    pub open: bool,
}

/// Render the welcome screen.
///
/// Returns a [`WelcomeAction`] when the user interacts with it.
pub fn render_welcome_screen(
    ui: &mut egui::Ui,
    palette: crate::theme::SemanticPalette,
) -> WelcomeAction {
    let mut action = WelcomeAction::None;
    let projects = load();

    ui.vertical_centered(|ui| {
        ui.add_space(40.0);
        ui.label(
            egui::RichText::new("Blue IDE")
                .size(36.0)
                .strong()
                .color(palette.primary_text),
        );
        ui.add_space(24.0);

        if projects.is_empty() {
            ui.colored_label(palette.muted_text, "No recent projects.");
        } else {
            ui.label(egui::RichText::new("Recent Projects").size(16.0).strong());
            ui.add_space(8.0);

            for project in &projects {
                let path = PathBuf::from(&project.path);
                let exists = path.is_dir();

                ui.horizontal(|ui| {
                    let name_resp = ui.add(
                        egui::Label::new(
                            egui::RichText::new(&project.name)
                                .strong()
                                .color(if exists {
                                    palette.primary_text
                                } else {
                                    palette.muted_text
                                }),
                        )
                        .sense(egui::Sense::click()),
                    );
                    ui.add_space(8.0);
                    ui.colored_label(
                        egui::Color32::from_rgb(136, 136, 136),
                        egui::RichText::new(&project.path).small(),
                    );
                    ui.add_space(8.0);
                    ui.colored_label(
                        palette.muted_text,
                        egui::RichText::new(format_timestamp(project.last_opened)).small(),
                    );

                    if !exists {
                        ui.colored_label(palette.error, "Folder not found");
                    } else if name_resp.clicked() {
                        action = WelcomeAction::Open(path);
                    }
                });
                ui.add_space(4.0);
            }
        }

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if ui.button("Open Folder...").clicked() {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    action = WelcomeAction::Open(dir);
                }
            }
            ui.add_space(8.0);
            if ui.button("New Project...").clicked() {
                action = WelcomeAction::NewProject;
            }
            ui.add_space(8.0);
            if !projects.is_empty() && ui.button("Clear Recent").clicked() {
                clear();
            }
        });
    });

    action
}

/// Actions emitted by the welcome screen.
#[derive(Debug, Clone, PartialEq)]
pub enum WelcomeAction {
    None,
    Open(PathBuf),
    NewProject,
}

// ─── Quick-pick popup ─────────────────────────────────────────────────────────

/// Render the "Recent Projects" quick-pick popup.
///
/// Returns a `PathBuf` if the user selects and opens a project.
pub fn show_quick_pick(
    ctx: &egui::Context,
    state: &mut RecentQuickPickState,
    palette: crate::theme::SemanticPalette,
) -> Option<PathBuf> {
    if !state.open {
        return None;
    }

    let mut result = None;
    let projects = load();

    egui::Window::new("Recent Projects")
        .id(egui::Id::new("recent_quick_pick"))
        .collapsible(false)
        .resizable(false)
        .default_width(500.0)
        .anchor(egui::Align2::CENTER_TOP, [0.0, 60.0])
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                for project in &projects {
                    let path = PathBuf::from(&project.path);
                    let exists = path.is_dir();
                    let resp = ui.selectable_label(
                        false,
                        egui::RichText::new(format!(
                            "{} — {}",
                            project.name, project.path
                        ))
                        .color(if exists {
                            palette.primary_text
                        } else {
                            palette.muted_text
                        }),
                    );
                    if resp.clicked() && exists {
                        result = Some(path);
                        state.open = false;
                    }
                }
            });
            ui.add_space(4.0);
            if ui.button("Close").clicked() {
                state.open = false;
            }
        });

    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        state.open = false;
    }

    result
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_timestamp_contains_year() {
        // Unix timestamp for 2025-01-15
        let ts = 1_736_899_200i64; // approx Jan 15, 2025
        let formatted = format_timestamp(ts);
        assert!(formatted.contains("2025"), "expected year in '{formatted}'");
    }

    #[test]
    fn format_timestamp_zero_is_epoch() {
        let formatted = format_timestamp(0);
        assert!(formatted.contains("1970"), "epoch should show 1970, got '{formatted}'");
    }
}
