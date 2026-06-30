//! Project template wizard: generates new Rust projects from a template.
//!
//! Supports the "Cargo binary" template initially. The wizard is shown as
//! a modal window triggered by File > New Project.

use std::path::{Path, PathBuf};

/// Available project templates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateKind {
    /// `cargo new --bin`-equivalent
    RustBinary,
}

impl TemplateKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::RustBinary => "Rust — Binary (Cargo)",
        }
    }
    pub fn all() -> &'static [Self] {
        &[Self::RustBinary]
    }
}

/// State for the "New Project" wizard modal.
#[derive(Debug, Clone)]
pub struct NewProjectState {
    pub open: bool,
    pub name: String,
    pub target_dir: String,
    pub template: TemplateKind,
    pub init_git: bool,
    pub open_after: bool,
    /// Feedback message shown inside the wizard.
    pub feedback: Option<String>,
    pub success_path: Option<PathBuf>,
}

impl Default for NewProjectState {
    fn default() -> Self {
        Self {
            open: false,
            name: String::new(),
            target_dir: String::new(),
            template: TemplateKind::RustBinary,
            init_git: true,
            open_after: true,
            feedback: None,
            success_path: None,
        }
    }
}

impl NewProjectState {
    pub fn open(&mut self) {
        *self = Self {
            open: true,
            ..Self::default()
        };
    }

    /// Validate inputs. Returns `Err` with a human-readable message.
    pub fn validate(&self) -> Result<(), String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("Project name cannot be empty.".to_owned());
        }
        if !is_valid_ident(name) {
            return Err(
                "Project name must start with a letter or underscore and contain only \
                 letters, digits, hyphens, and underscores."
                    .to_owned(),
            );
        }
        let dir = self.target_dir.trim();
        if dir.is_empty() {
            return Err("Target directory cannot be empty.".to_owned());
        }
        Ok(())
    }

    /// Absolute path where the project root will be created.
    pub fn project_path(&self) -> PathBuf {
        Path::new(self.target_dir.trim()).join(self.name.trim())
    }
}

/// Create a new project from the wizard state.
/// Returns the project root path on success.
pub fn create_project(state: &NewProjectState) -> Result<PathBuf, String> {
    state.validate()?;

    let root = state.project_path();

    // Guard against overwriting non-empty directories
    if root.exists() {
        let is_empty = std::fs::read_dir(&root)
            .map(|mut d| d.next().is_none())
            .unwrap_or(false);
        if !is_empty {
            return Err(format!(
                "Directory '{}' already exists and is not empty.",
                root.display()
            ));
        }
    }

    match state.template {
        TemplateKind::RustBinary => create_rust_binary(&root, state)?,
    }

    Ok(root)
}

// ─── Template generators ──────────────────────────────────────────────────────

fn create_rust_binary(root: &Path, state: &NewProjectState) -> Result<(), String> {
    let name = state.name.trim();

    // Create directory structure
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir)
        .map_err(|e| format!("Could not create project directory: {e}"))?;

    // Cargo.toml
    let cargo_toml = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[dependencies]
"#
    );
    write_file(&root.join("Cargo.toml"), &cargo_toml)?;

    // src/main.rs
    let main_rs = r#"fn main() {
    println!("Hello, world!");
}
"#;
    write_file(&src_dir.join("main.rs"), main_rs)?;

    // .gitignore
    let gitignore = "/target\n";
    write_file(&root.join(".gitignore"), gitignore)?;

    // README.md
    let readme = format!("# {name}\n\nA new Rust project.\n");
    write_file(&root.join("README.md"), &readme)?;

    // Optionally run `git init`
    if state.init_git {
        // Only try — don't fail if git is not on PATH
        let _ = std::process::Command::new("git")
            .arg("init")
            .current_dir(root)
            .output();
    }

    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn write_file(path: &Path, content: &str) -> Result<(), String> {
    std::fs::write(path, content).map_err(|e| format!("Could not write {}: {e}", path.display()))
}

/// Validate a Cargo project name: starts with letter/underscore, rest are
/// alphanumeric, hyphens, or underscores.
fn is_valid_ident(name: &str) -> bool {
    let mut chars = name.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    if !first.is_alphabetic() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}

/// Show the New Project wizard inside an egui window.
///
/// Returns the created project path when the user confirms and creation succeeds.
pub fn show_wizard(
    ctx: &egui::Context,
    state: &mut NewProjectState,
    palette: crate::theme::SemanticPalette,
) -> Option<PathBuf> {
    if !state.open {
        return None;
    }

    let mut confirmed = false;
    let mut cancelled = false;

    egui::Window::new("New Project")
        .id(egui::Id::new("new_project_wizard"))
        .collapsible(false)
        .resizable(false)
        .default_width(440.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, -40.0])
        .show(ctx, |ui| {
            ui.add_space(4.0);

            // Template selector
            ui.label("Template:");
            egui::ComboBox::from_id_source("project_template")
                .selected_text(state.template.label())
                .show_ui(ui, |ui| {
                    for &kind in TemplateKind::all() {
                        ui.selectable_value(&mut state.template, kind, kind.label());
                    }
                });

            ui.add_space(8.0);

            // Project name
            ui.label("Project name:");
            ui.add(
                egui::TextEdit::singleline(&mut state.name)
                    .desired_width(360.0)
                    .hint_text("my_project"),
            );

            ui.add_space(8.0);

            // Target directory
            ui.label("Location:");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut state.target_dir)
                        .desired_width(290.0)
                        .hint_text("Choose directory…"),
                );
                if ui.button("Browse…").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        state.target_dir = dir.to_string_lossy().to_string();
                    }
                }
            });

            // Preview path
            if !state.name.trim().is_empty() && !state.target_dir.trim().is_empty() {
                let preview = state.project_path();
                ui.colored_label(
                    palette.muted_text,
                    format!("Will create: {}", preview.display()),
                );
            }

            ui.add_space(8.0);
            ui.checkbox(&mut state.init_git, "Initialize Git repository");
            ui.checkbox(&mut state.open_after, "Open project after creation");

            ui.add_space(8.0);

            if let Some(msg) = &state.feedback {
                let color = if state.success_path.is_some() {
                    palette.success
                } else {
                    palette.error
                };
                ui.colored_label(color, msg.as_str());
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Create").clicked() {
                    confirmed = true;
                }
                if ui.button("Cancel").clicked() {
                    cancelled = true;
                }
            });
        });

    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        cancelled = true;
    }

    if confirmed {
        match create_project(state) {
            Ok(path) => {
                state.feedback = Some(format!(
                    "Project created at {}",
                    path.display()
                ));
                state.success_path = Some(path.clone());
                if !state.open_after {
                    state.open = false;
                }
                return Some(path);
            }
            Err(msg) => {
                state.feedback = Some(msg);
                state.success_path = None;
            }
        }
    }

    if cancelled {
        state.open = false;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("blue_ide_template_test_{nonce}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn creates_rust_binary_structure() {
        let base = temp_dir();
        let state = NewProjectState {
            open: true,
            name: "my_app".to_owned(),
            target_dir: base.to_string_lossy().to_string(),
            template: TemplateKind::RustBinary,
            init_git: false,
            open_after: false,
            feedback: None,
            success_path: None,
        };

        let root = create_project(&state).expect("create project");
        assert!(root.join("Cargo.toml").exists());
        assert!(root.join("src/main.rs").exists());
        assert!(root.join(".gitignore").exists());
        assert!(root.join("README.md").exists());

        let cargo = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(cargo.contains("my_app"));

        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn rejects_empty_name() {
        let state = NewProjectState {
            name: "".to_owned(),
            target_dir: "/tmp".to_owned(),
            ..NewProjectState::default()
        };
        assert!(state.validate().is_err());
    }

    #[test]
    fn rejects_invalid_ident() {
        let state = NewProjectState {
            name: "123bad".to_owned(),
            target_dir: "/tmp".to_owned(),
            ..NewProjectState::default()
        };
        assert!(state.validate().is_err());
    }

    #[test]
    fn accepts_valid_ident_with_hyphens() {
        let state = NewProjectState {
            name: "my-app".to_owned(),
            target_dir: "/tmp".to_owned(),
            ..NewProjectState::default()
        };
        assert!(state.validate().is_ok());
    }
}
