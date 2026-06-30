//! Feature 6 — Environment Variable Editor.
//!
//! Provides a side-panel UI to manage per-project environment variables stored
//! in `[project_root]/.stack_ide/env.json`. Enabled variables are injected into
//! every new terminal session and task/run command.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

// ─── Data model ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvVar {
    pub key: String,
    pub value: String,
    pub enabled: bool,
}

impl EnvVar {
    pub fn empty() -> Self {
        Self {
            key: String::new(),
            value: String::new(),
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvFile {
    pub variables: Vec<EnvVar>,
}

impl EnvFile {
    pub fn empty() -> Self {
        Self { variables: Vec::new() }
    }

    /// Path to the env.json file for the given project root.
    pub fn path_for(project_root: &Path) -> PathBuf {
        project_root.join(".stack_ide").join("env.json")
    }

    /// Load from disk, or return an empty file if the path does not exist.
    pub fn load(project_root: &Path) -> Self {
        let path = Self::path_for(project_root);
        if !path.exists() {
            return Self::empty();
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
                eprintln!("[env_editor] Failed to parse {}: {}", path.display(), e);
                Self::empty()
            }),
            Err(e) => {
                eprintln!("[env_editor] Failed to read {}: {}", path.display(), e);
                Self::empty()
            }
        }
    }

    /// Save to `[project_root]/.stack_ide/env.json`.
    pub fn save(&self, project_root: &Path) -> Result<(), String> {
        let path = Self::path_for(project_root);
        let dir = path.parent().expect("env.json always has a parent");
        if !dir.exists() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("Failed to create directory: {}", e))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialise env vars: {}", e))?;
        std::fs::write(&path, json).map_err(|e| format!("Failed to write {}: {}", path.display(), e))
    }

    /// Return only the enabled variables as (key, value) pairs.
    pub fn enabled_vars(&self) -> Vec<(String, String)> {
        self.variables
            .iter()
            .filter(|v| v.enabled && !v.key.is_empty())
            .map(|v| (v.key.clone(), v.value.clone()))
            .collect()
    }
}

// ─── UI state ────────────────────────────────────────────────────────────────

pub struct EnvEditorState {
    /// Whether the side panel is visible.
    pub open: bool,
    /// Working copy of the variables being edited.
    pub draft: Vec<EnvVar>,
    /// Per-row validation error messages (indexed by row).
    pub row_errors: Vec<Option<String>>,
    /// Feedback label shown after a successful save.
    save_feedback: Option<(String, Instant)>,
    /// Project root currently being edited.
    pub project_root: Option<PathBuf>,
}

impl EnvEditorState {
    pub fn new() -> Self {
        Self {
            open: false,
            draft: Vec::new(),
            row_errors: Vec::new(),
            save_feedback: None,
            project_root: None,
        }
    }

    /// Open the panel, loading variables from disk.
    pub fn open_for(&mut self, project_root: PathBuf) {
        let file = EnvFile::load(&project_root);
        self.draft = file.variables;
        self.row_errors = vec![None; self.draft.len()];
        self.project_root = Some(project_root);
        self.open = true;
        self.save_feedback = None;
    }

    /// Validate all rows. Returns `true` if all pass.
    fn validate(&mut self) -> bool {
        self.row_errors = vec![None; self.draft.len()];
        let mut ok = true;
        for (i, var) in self.draft.iter().enumerate() {
            if var.key.is_empty() {
                self.row_errors[i] = Some("Key must not be empty".to_owned());
                ok = false;
            } else if var.key.contains(' ') {
                self.row_errors[i] = Some("Key must not contain spaces".to_owned());
                ok = false;
            } else if var.key.contains('=') {
                self.row_errors[i] = Some("Key must not contain '='".to_owned());
                ok = false;
            }
        }
        ok
    }

    /// Save the current draft to disk. Returns an error string on failure.
    pub fn save(&mut self) -> Result<(), String> {
        if !self.validate() {
            return Err("Fix validation errors before saving".to_owned());
        }
        let root = self.project_root.as_ref().ok_or("No project root set")?;
        let file = EnvFile { variables: self.draft.clone() };
        file.save(root)?;
        self.save_feedback = Some(("Saved ✓".to_owned(), Instant::now()));
        Ok(())
    }

    /// Return enabled variables to inject into spawned sessions/tasks.
    pub fn enabled_vars(&self) -> Vec<(String, String)> {
        self.draft
            .iter()
            .filter(|v| v.enabled && !v.key.is_empty())
            .map(|v| (v.key.clone(), v.value.clone()))
            .collect()
    }
}

impl Default for EnvEditorState {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Rendering ───────────────────────────────────────────────────────────────

/// Render the Env Variables side panel.
///
/// This should be called inside an `egui::SidePanel::right(...)` closure.
pub fn render_env_editor(
    ui: &mut egui::Ui,
    state: &mut EnvEditorState,
    project_name: &str,
) {
    if !state.open {
        return;
    }

    ui.heading(format!("Env Variables — {}", project_name));
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);

    // Expire save feedback after 2 s
    if let Some((_, ts)) = &state.save_feedback {
        if ts.elapsed().as_secs_f32() >= 2.0 {
            state.save_feedback = None;
        }
    }

    // Table header
    egui::Grid::new("env_table_header")
        .min_col_width(20.0)
        .spacing([4.0, 2.0])
        .show(ui, |ui| {
            ui.label(egui::RichText::new("On").small().strong());
            ui.label(egui::RichText::new("Key").small().strong());
            ui.label(egui::RichText::new("Value").small().strong());
            ui.label(egui::RichText::new("").small());
            ui.end_row();
        });

    ui.separator();

    let mut to_delete: Option<usize> = None;

    // Ensure row_errors is the right length
    while state.row_errors.len() < state.draft.len() {
        state.row_errors.push(None);
    }
    state.row_errors.truncate(state.draft.len());

    egui::ScrollArea::vertical()
        .max_height(300.0)
        .show(ui, |ui| {
            let n = state.draft.len();
            for i in 0..n {
                egui::Grid::new(format!("env_row_{}", i))
                    .min_col_width(20.0)
                    .spacing([4.0, 2.0])
                    .show(ui, |ui| {
                        // Enabled checkbox
                        ui.checkbox(&mut state.draft[i].enabled, "");

                        // Key input
                        let key_resp = ui.add(
                            egui::TextEdit::singleline(&mut state.draft[i].key)
                                .desired_width(140.0)
                                .hint_text("KEY"),
                        );
                        if key_resp.changed() {
                            state.row_errors[i] = None;
                        }

                        // Value input
                        ui.add(
                            egui::TextEdit::singleline(&mut state.draft[i].value)
                                .desired_width(180.0)
                                .hint_text("value"),
                        );

                        // Delete button
                        if ui
                            .add(
                                egui::Button::new("🗑")
                                    .frame(false)
                                    .min_size(egui::vec2(20.0, 20.0)),
                            )
                            .on_hover_text("Remove this variable")
                            .clicked()
                        {
                            to_delete = Some(i);
                        }

                        ui.end_row();
                    });

                // Inline validation error
                if let Some(err) = &state.row_errors[i] {
                    ui.colored_label(egui::Color32::from_rgb(255, 60, 60), err);
                }
            }
        });

    if let Some(idx) = to_delete {
        state.draft.remove(idx);
        state.row_errors.remove(idx);
    }

    ui.add_space(4.0);

    // Add variable button
    if ui.button("+ Add Variable").clicked() {
        state.draft.push(EnvVar::empty());
        state.row_errors.push(None);
    }

    ui.add_space(8.0);
    ui.separator();

    // Save button and feedback
    ui.horizontal(|ui| {
        if ui.button("Save").clicked() {
            if let Err(e) = state.save() {
                // Show the error inline — we don't have a toast system
                eprintln!("[env_editor] Save failed: {}", e);
            }
        }

        if let Some((msg, _)) = &state.save_feedback {
            ui.colored_label(egui::Color32::from_rgb(80, 200, 80), msg);
        }
    });
}
