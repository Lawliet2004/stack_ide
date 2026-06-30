//! Feature 4 — Project Templates
//!
//! Multi-step "New Project Wizard" modal with 5 Rust project templates.
//! Generates complete file trees and optionally runs `git init`.

use std::path::{Path, PathBuf};
use std::process::Command;

// ─── Template kinds ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateKind {
    RustBinary,
    RustLibrary,
    RustWorkspace,
    RustCliTool,
    RustWebServer,
}

impl TemplateKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::RustBinary => "Rust Binary",
            Self::RustLibrary => "Rust Library",
            Self::RustWorkspace => "Rust Workspace",
            Self::RustCliTool => "Rust CLI Tool",
            Self::RustWebServer => "Rust Web Server",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::RustBinary => "A runnable Rust application",
            Self::RustLibrary => "A Rust library crate",
            Self::RustWorkspace => "A multi-crate Cargo workspace",
            Self::RustCliTool => "Binary with clap argument parsing",
            Self::RustWebServer => "Binary with axum HTTP server",
        }
    }

    pub fn all() -> &'static [Self] {
        &[
            Self::RustBinary,
            Self::RustLibrary,
            Self::RustWorkspace,
            Self::RustCliTool,
            Self::RustWebServer,
        ]
    }
}

// ─── Rust Edition ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RustEdition {
    E2021,
    E2024,
    E2018,
}

impl RustEdition {
    pub fn label(self) -> &'static str {
        match self {
            Self::E2021 => "2021",
            Self::E2024 => "2024",
            Self::E2018 => "2018",
        }
    }
    pub fn all() -> &'static [Self] {
        &[Self::E2021, Self::E2024, Self::E2018]
    }
}

impl Default for RustEdition {
    fn default() -> Self {
        Self::E2021
    }
}

// ─── Wizard step ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WizardStep {
    #[default]
    ChooseTemplate,
    Configure,
    Confirm,
}

// ─── Wizard state ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TemplateWizardState {
    pub open: bool,
    pub step: WizardStep,
    pub selected_template: TemplateKind,
    pub project_name: String,
    pub location: String,
    pub edition: RustEdition,
    pub init_git: bool,
    /// Inline validation error for the project name field.
    pub name_error: Option<String>,
    /// Feedback message shown in the confirm step.
    pub feedback: Option<String>,
    /// Path of the created project (set on success).
    pub created_path: Option<PathBuf>,
}

impl Default for TemplateWizardState {
    fn default() -> Self {
        let location = directories::BaseDirs::new()
            .map(|b| b.home_dir().join("Projects").to_string_lossy().to_string())
            .unwrap_or_else(|| "~/Projects".to_string());
        Self {
            open: false,
            step: WizardStep::default(),
            selected_template: TemplateKind::RustBinary,
            project_name: String::new(),
            location,
            edition: RustEdition::default(),
            init_git: true,
            name_error: None,
            feedback: None,
            created_path: None,
        }
    }
}

impl TemplateWizardState {
    pub fn open(&mut self) {
        *self = Self {
            open: true,
            ..Self::default()
        };
    }

    pub fn project_path(&self) -> PathBuf {
        Path::new(&self.location).join(&self.project_name)
    }

    pub fn validate_name(&mut self) -> bool {
        let name = self.project_name.trim();
        if name.is_empty() {
            self.name_error = Some("Project name cannot be empty.".to_owned());
            return false;
        }
        let valid = regex::Regex::new(r"^[a-z][a-z0-9_-]*$")
            .map(|re| re.is_match(name))
            .unwrap_or(false);
        if !valid {
            self.name_error = Some(
                "Name must match ^[a-z][a-z0-9_-]*$ (lowercase, start with a letter)."
                    .to_owned(),
            );
            return false;
        }
        self.name_error = None;
        true
    }

    /// Tree lines shown in the Confirm step.
    pub fn preview_tree(&self) -> Vec<String> {
        let n = self.project_name.trim();
        let mut lines = vec![
            format!("{}/", n),
            "  Cargo.toml".to_string(),
            "  src/".to_string(),
        ];
        match self.selected_template {
            TemplateKind::RustBinary | TemplateKind::RustCliTool | TemplateKind::RustWebServer => {
                lines.push("    main.rs".to_string());
            }
            TemplateKind::RustLibrary => {
                lines.push("    lib.rs".to_string());
            }
            TemplateKind::RustWorkspace => {
                lines.pop(); // remove src/
                lines.push(format!("  crates/{n}_core/src/lib.rs"));
                lines.push(format!("  crates/{n}_cli/src/main.rs"));
            }
        }
        if self.init_git {
            lines.push("  .gitignore".to_string());
        }
        lines.push("  README.md".to_string());
        lines
    }
}

// ─── File generation ──────────────────────────────────────────────────────────

fn write(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, content)
        .map_err(|e| format!("Could not write {}: {e}", path.display()))
}

fn gitignore_content() -> &'static str {
    "/target\nCargo.lock\n.stack_ide/\n"
}

fn readme_content(name: &str) -> String {
    format!("# {name}\n\nGenerated by stack_ide.\n")
}

pub fn create_project(state: &TemplateWizardState) -> Result<PathBuf, String> {
    let name = state.project_name.trim();
    let root = state.project_path();
    let edition = state.edition.label();

    if root.exists() {
        return Err(format!("Directory already exists: {}", root.display()));
    }

    match state.selected_template {
        TemplateKind::RustBinary => {
            let cargo = format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"{edition}\"\n\n[[bin]]\nname = \"{name}\"\npath = \"src/main.rs\"\n"
            );
            write(&root.join("Cargo.toml"), &cargo)?;
            write(
                &root.join("src").join("main.rs"),
                "fn main() {\n    println!(\"Hello, world!\");\n}\n",
            )?;
        }
        TemplateKind::RustLibrary => {
            let cargo = format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"{edition}\"\n\n[lib]\nname = \"{name}\"\npath = \"src/lib.rs\"\n"
            );
            write(&root.join("Cargo.toml"), &cargo)?;
            let lib_rs = format!(
                "pub fn hello() -> &'static str {{\n    \"Hello from {name}!\"\n}}\n\n#[cfg(test)]\nmod tests {{\n    use super::*;\n\n    #[test]\n    fn it_works() {{\n        assert_eq!(hello(), \"Hello from {name}!\");\n    }}\n}}\n"
            );
            write(&root.join("src").join("lib.rs"), &lib_rs)?;
        }
        TemplateKind::RustWorkspace => {
            let workspace_toml = format!(
                "[workspace]\nmembers = [\n    \"crates/{name}_core\",\n    \"crates/{name}_cli\",\n]\nresolver = \"2\"\n"
            );
            write(&root.join("Cargo.toml"), &workspace_toml)?;
            // core crate
            let core_cargo = format!(
                "[package]\nname = \"{name}_core\"\nversion = \"0.1.0\"\nedition = \"{edition}\"\n\n[lib]\npath = \"src/lib.rs\"\n"
            );
            write(
                &root.join("crates").join(format!("{name}_core")).join("Cargo.toml"),
                &core_cargo,
            )?;
            write(
                &root.join("crates").join(format!("{name}_core")).join("src").join("lib.rs"),
                "pub fn hello() -> &'static str { \"hello from core\" }\n",
            )?;
            // cli crate
            let cli_cargo = format!(
                "[package]\nname = \"{name}_cli\"\nversion = \"0.1.0\"\nedition = \"{edition}\"\n\n[[bin]]\nname = \"{name}_cli\"\npath = \"src/main.rs\"\n"
            );
            write(
                &root.join("crates").join(format!("{name}_cli")).join("Cargo.toml"),
                &cli_cargo,
            )?;
            write(
                &root.join("crates").join(format!("{name}_cli")).join("src").join("main.rs"),
                "fn main() {\n    println!(\"Hello from cli!\");\n}\n",
            )?;
        }
        TemplateKind::RustCliTool => {
            let cargo = format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"{edition}\"\n\n[[bin]]\nname = \"{name}\"\npath = \"src/main.rs\"\n\n[dependencies]\nclap = {{ version = \"4\", features = [\"derive\"] }}\n"
            );
            write(&root.join("Cargo.toml"), &cargo)?;
            let main_rs = format!(
                "use clap::Parser;\n\n#[derive(Parser)]\n#[command(name = \"{name}\", about = \"A CLI tool\")]\nstruct Args {{\n    #[arg(short, long)]\n    verbose: bool,\n}}\n\nfn main() {{\n    let args = Args::parse();\n    if args.verbose {{\n        println!(\"Verbose mode enabled\");\n    }}\n}}\n"
            );
            write(&root.join("src").join("main.rs"), &main_rs)?;
        }
        TemplateKind::RustWebServer => {
            let cargo = format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"{edition}\"\n\n[[bin]]\nname = \"{name}\"\npath = \"src/main.rs\"\n\n[dependencies]\naxum = \"0.7\"\ntokio = {{ version = \"1\", features = [\"full\"] }}\n"
            );
            write(&root.join("Cargo.toml"), &cargo)?;
            let main_rs = "use axum::{routing::get, Router};\n\n#[tokio::main]\nasync fn main() {\n    let app = Router::new().route(\"/\", get(|| async { \"Hello, World!\" }));\n    let listener = tokio::net::TcpListener::bind(\"0.0.0.0:3000\").await.unwrap();\n    println!(\"Listening on http://0.0.0.0:3000\");\n    axum::serve(listener, app).await.unwrap();\n}\n";
            write(&root.join("src").join("main.rs"), main_rs)?;
        }
    }

    // Common files for all templates (except workspace which has no top-level src/).
    if state.init_git {
        write(&root.join(".gitignore"), gitignore_content())?;
    }
    write(&root.join("README.md"), &readme_content(name))?;

    // Run git init.
    if state.init_git {
        let _ = Command::new("git")
            .arg("init")
            .current_dir(&root)
            .output();
    }

    Ok(root)
}

// ─── egui wizard modal ────────────────────────────────────────────────────────

/// Render the 3-step "New Project Wizard" modal.
///
/// Returns the created project root path when the user successfully creates a project.
pub fn show_wizard(
    ctx: &egui::Context,
    state: &mut TemplateWizardState,
    palette: crate::theme::SemanticPalette,
) -> Option<PathBuf> {
    if !state.open {
        return None;
    }

    let mut result = None;

    egui::Window::new("New Project Wizard")
        .id(egui::Id::new("template_wizard"))
        .collapsible(false)
        .resizable(true)
        .default_width(520.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            match state.step {
                WizardStep::ChooseTemplate => {
                    ui.heading("Step 1 — Choose Template");
                    ui.add_space(8.0);
                    let templates = TemplateKind::all();
                    for &kind in templates {
                        let selected = state.selected_template == kind;
                        let card_color = if selected {
                            egui::Color32::from_rgb(0, 95, 255)
                        } else {
                            palette.elevated_background
                        };
                        let frame = egui::Frame::none()
                            .fill(card_color)
                            .stroke(egui::Stroke::new(
                                if selected { 2.0 } else { 1.0 },
                                if selected {
                                    egui::Color32::from_rgb(0, 95, 255)
                                } else {
                                    palette.border
                                },
                            ))
                            .rounding(4.0)
                            .inner_margin(egui::Margin::same(8.0));
                        let resp = frame.show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.strong(kind.name());
                                ui.add_space(8.0);
                                ui.label(
                                    egui::RichText::new(kind.description())
                                        .color(palette.muted_text),
                                );
                            });
                        });
                        if resp.response.interact(egui::Sense::click()).clicked() {
                            state.selected_template = kind;
                        }
                        ui.add_space(4.0);
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Next →").clicked() {
                            state.step = WizardStep::Configure;
                        }
                        if ui.button("Cancel").clicked() {
                            state.open = false;
                        }
                    });
                }
                WizardStep::Configure => {
                    ui.heading("Step 2 — Configure Project");
                    ui.add_space(8.0);

                    ui.label("Project Name:");
                    ui.add(
                        egui::TextEdit::singleline(&mut state.project_name)
                            .desired_width(380.0)
                            .hint_text("my-project"),
                    );
                    if let Some(err) = &state.name_error {
                        ui.colored_label(palette.error, err.as_str());
                    }

                    ui.add_space(6.0);
                    ui.label("Location:");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut state.location)
                                .desired_width(310.0),
                        );
                        if ui.button("Browse...").clicked() {
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                state.location = dir.to_string_lossy().to_string();
                            }
                        }
                    });

                    ui.add_space(6.0);
                    ui.label("Rust Edition:");
                    egui::ComboBox::from_id_source("edition_combo")
                        .selected_text(state.edition.label())
                        .show_ui(ui, |ui| {
                            for &ed in RustEdition::all() {
                                ui.selectable_value(&mut state.edition, ed, ed.label());
                            }
                        });

                    ui.add_space(6.0);
                    ui.checkbox(&mut state.init_git, "Initialize Git Repository");

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("← Back").clicked() {
                            state.step = WizardStep::ChooseTemplate;
                        }
                        if ui.button("Next →").clicked() {
                            if state.validate_name() {
                                state.step = WizardStep::Confirm;
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            state.open = false;
                        }
                    });
                }
                WizardStep::Confirm => {
                    ui.heading("Step 3 — Confirm & Create");
                    ui.add_space(8.0);

                    ui.label(format!("Template:   {}", state.selected_template.name()));
                    ui.label(format!("Name:       {}", state.project_name));
                    ui.label(format!("Location:   {}", state.location));
                    ui.label(format!("Edition:    {}", state.edition.label()));
                    ui.label(format!("Git init:   {}", state.init_git));

                    ui.add_space(8.0);
                    ui.label("Files that will be created:");
                    egui::Frame::none()
                        .fill(palette.editor_background)
                        .inner_margin(egui::Margin::same(6.0))
                        .show(ui, |ui| {
                            for line in state.preview_tree() {
                                ui.label(
                                    egui::RichText::new(line).monospace().color(palette.primary_text),
                                );
                            }
                        });

                    if let Some(msg) = &state.feedback {
                        let color = if state.created_path.is_some() {
                            palette.success
                        } else {
                            palette.error
                        };
                        ui.add_space(4.0);
                        ui.colored_label(color, msg.as_str());
                    }

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("← Back").clicked() {
                            state.step = WizardStep::Configure;
                            state.feedback = None;
                        }
                        let btn = ui.button("Create Project");
                        if btn.clicked() {
                            match create_project(state) {
                                Ok(path) => {
                                    state.feedback = Some(format!(
                                        "Project created at {}",
                                        path.display()
                                    ));
                                    state.created_path = Some(path.clone());
                                    result = Some(path);
                                    state.open = false;
                                }
                                Err(msg) => {
                                    state.feedback = Some(msg);
                                    state.created_path = None;
                                }
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            state.open = false;
                        }
                    });
                }
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
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_dir() -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("ws_tpl_test_{n}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn state_for(base: &Path, template: TemplateKind) -> TemplateWizardState {
        TemplateWizardState {
            project_name: "my-project".into(),
            location: base.to_string_lossy().to_string(),
            edition: RustEdition::E2021,
            init_git: false,
            selected_template: template,
            ..Default::default()
        }
    }

    #[test]
    fn creates_rust_binary() {
        let base = tmp_dir();
        let state = state_for(&base, TemplateKind::RustBinary);
        let root = create_project(&state).unwrap();
        assert!(root.join("Cargo.toml").exists());
        assert!(root.join("src/main.rs").exists());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn creates_rust_library() {
        let base = tmp_dir();
        let state = state_for(&base, TemplateKind::RustLibrary);
        let root = create_project(&state).unwrap();
        assert!(root.join("src/lib.rs").exists());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn creates_rust_workspace() {
        let base = tmp_dir();
        let state = state_for(&base, TemplateKind::RustWorkspace);
        let root = create_project(&state).unwrap();
        assert!(root.join("Cargo.toml").exists());
        assert!(root.join("crates/my-project_core/src/lib.rs").exists());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn rejects_existing_directory() {
        let base = tmp_dir();
        let mut state = state_for(&base, TemplateKind::RustBinary);
        create_project(&state).unwrap();
        state.project_name = "my-project".into(); // same name
        let result = create_project(&state);
        assert!(result.is_err());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn name_validation() {
        let mut state = TemplateWizardState::default();
        state.project_name = "My-Project".into(); // uppercase not allowed
        assert!(!state.validate_name());
        state.project_name = "my-project".into();
        assert!(state.validate_name());
    }
}
