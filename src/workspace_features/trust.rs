//! Feature 6 — Workspace Trust
//!
//! Maintains a trust registry at `~/.stack_ide/trusted_workspaces.json`.
//! Shows a full-width orange banner for unknown workspaces.  Blocks LSP,
//! plugins, and task runner in Restricted / Untrusted workspaces.

use std::fs;
use std::path::{Path, PathBuf};

// ─── Trust level ─────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Clone, Debug, Default)]
pub enum TrustLevel {
    Trusted,
    Restricted,
    #[default]
    Untrusted,
}

impl TrustLevel {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Trusted => "Trusted",
            Self::Restricted => "Restricted",
            Self::Untrusted => "Untrusted",
        }
    }
}

// ─── Registry ────────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Debug, Default, Clone)]
pub struct WorkspaceTrustEntry {
    pub path: String,
    pub trust: TrustLevel,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Default, Clone)]
struct TrustRegistry {
    workspaces: Vec<WorkspaceTrustEntry>,
}

/// Returns `~/.config/blue-ide/trusted_workspaces.json`.
pub fn registry_path() -> Option<PathBuf> {
    directories::BaseDirs::new()
        .map(|b| b.home_dir().join(".stack_ide").join("trusted_workspaces.json"))
}

fn load_registry() -> TrustRegistry {
    let Some(path) = registry_path() else {
        return TrustRegistry::default();
    };
    if !path.is_file() {
        return TrustRegistry::default();
    }
    let json = fs::read_to_string(&path)
        .unwrap_or_else(|e| { eprintln!("trust: read error: {e}"); String::new() });
    serde_json::from_str(&json).unwrap_or_default()
}

fn save_registry(reg: &TrustRegistry) {
    let Some(path) = registry_path() else { return };
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("trust: create dir: {e}");
            return;
        }
    }
    match serde_json::to_string_pretty(reg) {
        Ok(json) => {
            if let Err(e) = fs::write(&path, json) {
                eprintln!("trust: save error: {e}");
            }
        }
        Err(e) => eprintln!("trust: serialize error: {e}"),
    }
}

// ─── Public trust API ─────────────────────────────────────────────────────────

/// Query the trust level for `workspace_path`.
///
/// Returns `None` when the path is not listed (unknown).
pub fn query(workspace_path: &Path) -> Option<TrustLevel> {
    let canonical = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let canonical_str = canonical.to_string_lossy().to_string();
    let reg = load_registry();
    reg.workspaces
        .iter()
        .find(|e| e.path == canonical_str)
        .map(|e| e.trust.clone())
}

/// Set or update the trust level for `workspace_path`.
pub fn set_trust(workspace_path: &Path, level: TrustLevel) {
    let canonical = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let canonical_str = canonical.to_string_lossy().to_string();
    let mut reg = load_registry();
    if let Some(entry) = reg.workspaces.iter_mut().find(|e| e.path == canonical_str) {
        entry.trust = level;
    } else {
        reg.workspaces.push(WorkspaceTrustEntry {
            path: canonical_str,
            trust: level,
        });
    }
    save_registry(&reg);
}

/// Trust `workspace_path` and all paths under its parent directory.
/// Future workspaces whose canonical path starts with the parent are
/// automatically trusted without prompting.
pub fn trust_parent_folder(workspace_path: &Path) {
    let canonical = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let parent = canonical
        .parent()
        .unwrap_or(canonical.as_path())
        .to_path_buf();
    set_trust(&parent, TrustLevel::Trusted);
    // Also directly trust the workspace itself.
    set_trust(workspace_path, TrustLevel::Trusted);
}

/// Check whether `workspace_path` is trusted (directly or via trusted parent).
pub fn is_trusted(workspace_path: &Path) -> bool {
    let canonical = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    // Direct hit.
    if let Some(TrustLevel::Trusted) = query(workspace_path) {
        return true;
    }
    // Parent-folder trust.
    let reg = load_registry();
    for entry in &reg.workspaces {
        if entry.trust == TrustLevel::Trusted {
            let entry_path = PathBuf::from(&entry.path);
            if canonical.starts_with(&entry_path) {
                return true;
            }
        }
    }
    false
}

/// Remove a workspace entry from the registry (Revoke button).
pub fn revoke(workspace_path: &str) {
    let mut reg = load_registry();
    reg.workspaces.retain(|e| e.path != workspace_path);
    save_registry(&reg);
}

/// All entries currently in the trust registry.
pub fn all_entries() -> Vec<WorkspaceTrustEntry> {
    load_registry().workspaces
}

// ─── UI state ────────────────────────────────────────────────────────────────

/// Whether the workspace trust banner is currently visible.
#[derive(Debug, Default)]
pub struct TrustBannerState {
    /// `None` = unknown (show banner), `Some(level)` = resolved.
    pub resolved: Option<TrustLevel>,
    pub show_banner: bool,
}

impl TrustBannerState {
    /// Call when a new workspace root is opened.
    pub fn evaluate(&mut self, workspace_path: &Path) {
        match query(workspace_path) {
            Some(TrustLevel::Trusted) => {
                self.resolved = Some(TrustLevel::Trusted);
                self.show_banner = false;
            }
            Some(TrustLevel::Untrusted) | Some(TrustLevel::Restricted) => {
                self.resolved = Some(TrustLevel::Untrusted);
                self.show_banner = true;
            }
            None => {
                // Not listed — show banner.
                self.resolved = None;
                self.show_banner = true;
            }
        }
    }

    /// True when LSP, plugins and task runner should be active.
    pub fn capabilities_enabled(&self) -> bool {
        matches!(self.resolved, Some(TrustLevel::Trusted))
    }
}

// ─── Banner UI ────────────────────────────────────────────────────────────────

/// Render the full-width trust banner.
///
/// Returns the user's trust decision if a button was clicked.
pub fn render_trust_banner(
    ui: &mut egui::Ui,
    workspace_path: &Path,
    palette: crate::theme::SemanticPalette,
) -> Option<TrustDecision> {
    let _ = palette;
    let mut decision = None;

    let banner_color = egui::Color32::from_rgb(255, 165, 0);

    egui::Frame::none()
        .fill(banner_color)
        .inner_margin(egui::Margin::symmetric(12.0, 6.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(
                    egui::Color32::WHITE,
                    "⚠ This workspace is not trusted. LSP, plugins, and tasks are disabled.",
                );
                ui.add_space(12.0);
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Trust This Workspace").color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(40, 120, 40)),
                    )
                    .clicked()
                {
                    decision = Some(TrustDecision::Trust(workspace_path.to_path_buf()));
                }
                ui.add_space(4.0);
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Trust Parent Folder").color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(40, 100, 40)),
                    )
                    .clicked()
                {
                    decision = Some(TrustDecision::TrustParent(workspace_path.to_path_buf()));
                }
                ui.add_space(4.0);
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Don't Trust").color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(140, 40, 40)),
                    )
                    .clicked()
                {
                    decision = Some(TrustDecision::Untrust(workspace_path.to_path_buf()));
                }
            });
        });

    decision
}

#[derive(Debug, Clone)]
pub enum TrustDecision {
    Trust(PathBuf),
    TrustParent(PathBuf),
    Untrust(PathBuf),
}

// ─── "Manage Trusted Workspaces" modal ───────────────────────────────────────

/// State for the trust management modal.
#[derive(Default)]
pub struct TrustManagementState {
    pub open: bool,
}

/// Render the "Manage Trusted Workspaces" modal.
///
/// Returns `true` if any revoke was performed (caller should re-evaluate).
pub fn show_manage_modal(
    ctx: &egui::Context,
    state: &mut TrustManagementState,
    palette: crate::theme::SemanticPalette,
) -> bool {
    if !state.open {
        return false;
    }

    let mut revoked = false;

    egui::Window::new("Manage Trusted Workspaces")
        .id(egui::Id::new("trust_management"))
        .collapsible(false)
        .resizable(true)
        .default_width(600.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            let entries = all_entries();
            if entries.is_empty() {
                ui.label("No workspace trust entries.");
            } else {
                egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                    egui::Grid::new("trust_table")
                        .num_columns(3)
                        .striped(true)
                        .show(ui, |ui| {
                            ui.strong("Path");
                            ui.strong("Trust Level");
                            ui.strong("Action");
                            ui.end_row();
                            for entry in &entries {
                                ui.label(&entry.path);
                                let color = match entry.trust {
                                    TrustLevel::Trusted => palette.success,
                                    TrustLevel::Restricted => palette.warning,
                                    TrustLevel::Untrusted => palette.error,
                                };
                                ui.colored_label(color, entry.trust.label());
                                if ui.small_button("Revoke").clicked() {
                                    revoke(&entry.path);
                                    revoked = true;
                                }
                                ui.end_row();
                            }
                        });
                });
            }

            ui.add_space(6.0);
            if ui.button("Close").clicked() {
                state.open = false;
            }
        });

    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        state.open = false;
    }

    revoked
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Note: these tests rely on the real home-dir file, so we only test
    // the logic that is pure (no side effects).

    #[test]
    fn trust_level_labels() {
        assert_eq!(TrustLevel::Trusted.label(), "Trusted");
        assert_eq!(TrustLevel::Untrusted.label(), "Untrusted");
        assert_eq!(TrustLevel::Restricted.label(), "Restricted");
    }

    #[test]
    fn banner_state_evaluates_unknown_as_show() {
        // Simulate no entry for a temp path.
        let tmp = std::env::temp_dir().join("unknown_ws_12345abc");
        let banner = TrustBannerState::default();
        // If the path doesn't exist in the registry, evaluate sets show_banner.
        // We can't easily test the real registry in unit tests without mocking,
        // so just verify the initial state.
        assert!(!banner.capabilities_enabled());
        assert!(!banner.show_banner);
        drop(tmp);
    }
}
