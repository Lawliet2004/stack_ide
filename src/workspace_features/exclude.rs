//! Feature 8 — Exclude Patterns
//!
//! Reads/writes `[project_root]/.stack_ide/exclude.json`.
//! Implements custom pattern matching (no glob crates):
//!   - `target/`   → directory named "target" at any depth
//!   - `*.lock`    → any file ending in ".lock" at any depth
//!   - `exact`     → file or dir with that exact name at any depth
//!   - `src/gen/`  → that exact relative path from the project root
//!
//! Provides a right-side panel ("Exclude Patterns") with add/delete/save/reset.

use std::fs;
use std::path::{Path, PathBuf};

// ─── Defaults ────────────────────────────────────────────────────────────────

pub fn default_patterns() -> Vec<String> {
    vec![
        "target/".to_string(),
        "*.lock".to_string(),
        ".git/".to_string(),
        "node_modules/".to_string(),
    ]
}

fn reset_patterns() -> Vec<String> {
    vec![
        "target/".to_string(),
        "*.lock".to_string(),
        ".git/".to_string(),
        "node_modules/".to_string(),
    ]
}

// ─── Persistence ─────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default)]
struct ExcludeFile {
    exclude: Vec<String>,
}

/// Returns `[project_root]/.stack_ide/exclude.json`.
pub fn exclude_path(project_root: &Path) -> PathBuf {
    project_root.join(".stack_ide").join("exclude.json")
}

/// Load exclude patterns from disk.  Creates the file with defaults if absent.
pub fn load_patterns(project_root: &Path) -> Vec<String> {
    let path = exclude_path(project_root);
    if path.is_file() {
        if let Ok(json) = fs::read_to_string(&path) {
            if let Ok(f) = serde_json::from_str::<ExcludeFile>(&json) {
                return f.exclude;
            }
        }
    }
    // Create with defaults.
    let defaults = default_patterns();
    let _ = save_patterns(project_root, &defaults);
    defaults
}

/// Persist patterns to disk.
pub fn save_patterns(project_root: &Path, patterns: &[String]) -> std::io::Result<()> {
    let path = exclude_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let f = ExcludeFile {
        exclude: patterns.to_vec(),
    };
    let json = serde_json::to_string_pretty(&f).map_err(std::io::Error::other)?;
    fs::write(path, json)
}

// ─── Pattern matching ─────────────────────────────────────────────────────────

/// Returns `true` when `entry_path` (relative or absolute) should be hidden
/// by `pattern`, given `project_root`.
///
/// Matching rules (no external glob crates):
/// 1. Pattern ending with `/`  → matches any directory whose name equals the
///    prefix at any depth.
/// 2. Pattern starting with `*` → suffix match on the filename.
/// 3. Pattern containing `/` in the middle → exact relative path match from root.
/// 4. Everything else → exact filename match at any depth.
pub fn pattern_matches(pattern: &str, path: &Path, project_root: &Path) -> bool {
    let rel = path
        .strip_prefix(project_root)
        .unwrap_or(path);
    let rel_str = rel.to_string_lossy().replace('\\', "/");

    if pattern.ends_with('/') {
        // Directory name match at any depth.
        let dir_name = &pattern[..pattern.len() - 1];
        for component in rel.components() {
            if let std::path::Component::Normal(c) = component {
                if c.to_string_lossy() == dir_name {
                    return true;
                }
            }
        }
        return false;
    }

    if let Some(suffix) = pattern.strip_prefix('*') {
        // Suffix match on filename.
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        return file_name.ends_with(suffix);
    }

    if pattern.contains('/') {
        // Exact relative path from root.
        let pat_norm = pattern.replace('\\', "/");
        return rel_str == pat_norm || rel_str.starts_with(&format!("{pat_norm}/"));
    }

    // Exact name match at any depth.
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    file_name == pattern
}

/// Returns `true` when `path` should be excluded by any of `patterns`.
pub fn is_excluded(path: &Path, project_root: &Path, patterns: &[String]) -> bool {
    patterns.iter().any(|p| pattern_matches(p, path, project_root))
}

/// Count how many files/directories under `root` would be hidden by `patterns`.
pub fn count_hidden(root: &Path, patterns: &[String]) -> usize {
    let mut count = 0;
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if is_excluded(&path, root, patterns) {
                count += 1;
            } else if path.is_dir() {
                count += count_hidden(&path, patterns);
            }
        }
    }
    count
}

// ─── UI Panel state ───────────────────────────────────────────────────────────

/// State for the "Exclude Patterns" right-side panel.
#[derive(Default)]
pub struct ExcludePanelState {
    pub open: bool,
    /// Editable copy of the patterns while the panel is open.
    pub patterns: Vec<String>,
    /// Text in the "+ Add Pattern" field.
    pub new_pattern: String,
    /// Feedback message (e.g. "Saved.").
    pub feedback: Option<String>,
}

impl ExcludePanelState {
    /// Open the panel and load the current patterns.
    pub fn open_for(&mut self, project_root: &Path) {
        self.patterns = load_patterns(project_root);
        self.new_pattern.clear();
        self.feedback = None;
        self.open = true;
    }
}

// ─── Panel UI ────────────────────────────────────────────────────────────────

/// Render the "Exclude Patterns" right-side panel.
///
/// Returns `true` when the user clicked "Save" (caller should refresh the file tree).
pub fn render_exclude_panel(
    ctx: &egui::Context,
    state: &mut ExcludePanelState,
    project_root: &Path,
    palette: crate::theme::SemanticPalette,
) -> bool {
    if !state.open {
        return false;
    }

    let mut saved = false;

    egui::SidePanel::right("exclude_patterns_panel")
        .default_width(280.0)
        .show(ctx, |ui| {
            ui.heading("Exclude Patterns");
            ui.add_space(6.0);

            let mut to_delete: Option<usize> = None;

            egui::ScrollArea::vertical()
                .max_height(ui.available_height() - 140.0)
                .show(ui, |ui| {
                    for (i, pat) in state.patterns.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(pat.as_str());
                            if ui.small_button("🗑").clicked() {
                                to_delete = Some(i);
                            }
                        });
                    }
                });

            if let Some(i) = to_delete {
                state.patterns.remove(i);
            }

            ui.add_space(8.0);
            ui.separator();
            ui.label("+ Add Pattern:");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut state.new_pattern)
                        .desired_width(180.0)
                        .hint_text("e.g. *.tmp"),
                );
                if ui.button("Add").clicked() && !state.new_pattern.trim().is_empty() {
                    let pat = state.new_pattern.trim().to_string();
                    if !state.patterns.contains(&pat) {
                        state.patterns.push(pat);
                    }
                    state.new_pattern.clear();
                }
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    if let Err(e) = save_patterns(project_root, &state.patterns) {
                        eprintln!("exclude: save error: {e}");
                        state.feedback = Some("Error saving patterns.".to_string());
                    } else {
                        state.feedback = Some("Saved.".to_string());
                        saved = true;
                    }
                }
                if ui.button("Reset to Defaults").clicked() {
                    state.patterns = reset_patterns();
                    state.feedback = Some("Reset to defaults (click Save to apply).".to_string());
                }
                if ui.button("✕").clicked() {
                    state.open = false;
                }
            });

            if let Some(msg) = &state.feedback {
                ui.colored_label(palette.muted_text, msg.as_str());
            }
        });

    saved
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_dir() -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("ws_exclude_test_{n}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn directory_pattern_matches_at_any_depth() {
        let root = tmp_dir();
        let p = root.join("a").join("target").join("foo.rs");
        assert!(pattern_matches("target/", &p, &root));
        let q = root.join("src").join("main.rs");
        assert!(!pattern_matches("target/", &q, &root));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn star_pattern_matches_suffix() {
        let root = tmp_dir();
        let p = root.join("Cargo.lock");
        assert!(pattern_matches("*.lock", &p, &root));
        let q = root.join("main.rs");
        assert!(!pattern_matches("*.lock", &q, &root));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_name_match_at_any_depth() {
        let root = tmp_dir();
        let p = root.join("a").join("b").join(".env");
        assert!(pattern_matches(".env", &p, &root));
        let q = root.join("main.rs");
        assert!(!pattern_matches(".env", &q, &root));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn relative_path_pattern() {
        let root = tmp_dir();
        let p = root.join("src").join("generated");
        assert!(pattern_matches("src/generated", &p, &root));
        let q = root.join("src").join("main.rs");
        assert!(!pattern_matches("src/generated", &q, &root));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn save_and_load_roundtrip() {
        let root = tmp_dir();
        let patterns = vec!["*.tmp".to_string(), "dist/".to_string()];
        save_patterns(&root, &patterns).unwrap();
        let loaded = load_patterns(&root);
        assert_eq!(loaded, patterns);
        fs::remove_dir_all(root).unwrap();
    }
}
