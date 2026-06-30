//! Workspace trust UI: trust prompt modal and trust badge in the status bar.

use crate::workspace::{TrustState, TrustStore, WorkspaceRoot};

/// State for the trust prompt modal (shown when opening an untrusted folder).
#[derive(Debug, Default)]
pub struct TrustPromptState {
    pub open: bool,
    /// Root that is pending a trust decision.
    pub pending_root: Option<std::path::PathBuf>,
    /// User choice (trust / don't trust).
    pub decision: Option<TrustState>,
}

impl TrustPromptState {
    /// Open the trust prompt for a specific folder.
    pub fn prompt(&mut self, path: std::path::PathBuf) {
        self.open = true;
        self.pending_root = Some(path);
        self.decision = None;
    }
}

/// Show the trust prompt modal. Returns the user's decision when they click a button.
pub fn show_trust_prompt(
    ctx: &egui::Context,
    state: &mut TrustPromptState,
    palette: crate::theme::SemanticPalette,
) -> Option<(std::path::PathBuf, TrustState)> {
    if !state.open {
        return None;
    }
    let Some(path) = state.pending_root.clone() else {
        return None;
    };

    let dir_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());

    let mut result = None;

    egui::Window::new("Workspace Trust")
        .id(egui::Id::new("workspace_trust_prompt"))
        .collapsible(false)
        .resizable(false)
        .default_width(420.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, -60.0])
        .show(ctx, |ui| {
            ui.add_space(4.0);
            ui.heading("Do you trust the files in this folder?");
            ui.add_space(8.0);
            ui.label(format!("Folder: {}", path.display()));
            ui.add_space(8.0);
            ui.label(
                "Trusted workspaces can:\n\
                 • Start language servers (LSP)\n\
                 • Run task commands\n\
                 • Execute debugger sessions\n\
                 • Run terminal startup commands",
            );
            ui.add_space(8.0);
            ui.colored_label(
                palette.warning,
                "Untrusted folders can still edit files, search, and browse the file tree.",
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui
                    .button(format!("✔  Trust \"{}\"", dir_name))
                    .clicked()
                {
                    result = Some((path.clone(), TrustState::Trusted));
                    state.open = false;
                    state.decision = Some(TrustState::Trusted);
                }
                if ui.button("✘  Don't Trust").clicked() {
                    result = Some((path.clone(), TrustState::Untrusted));
                    state.open = false;
                    state.decision = Some(TrustState::Untrusted);
                }
            });
        });

    result
}

/// Show the trust badge in the status bar. Returns `true` if clicked.
pub fn show_trust_badge(
    ui: &mut egui::Ui,
    state: TrustState,
    palette: crate::theme::SemanticPalette,
) -> bool {
    let (label, color) = match state {
        TrustState::Trusted => ("⚡ Trusted", palette.success),
        TrustState::Untrusted => ("⚠ Restricted Mode", palette.warning),
    };
    ui.colored_label(color, label)
        .on_hover_text("Click to manage workspace trust")
        .clicked()
}

/// Show a trust management popup (triggered by clicking the status bar badge).
pub fn show_trust_management(
    ctx: &egui::Context,
    root: &WorkspaceRoot,
    trust_store: &mut TrustStore,
    palette: crate::theme::SemanticPalette,
    open: &mut bool,
) {
    if !*open {
        return;
    }

    let current = trust_store.state(root);
    let mut new_state = current;

    egui::Window::new("Workspace Trust")
        .id(egui::Id::new("workspace_trust_management"))
        .collapsible(false)
        .resizable(false)
        .default_width(380.0)
        .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -30.0])
        .show(ctx, |ui| {
            ui.label(format!("Workspace: {}", root.path.display()));
            ui.add_space(8.0);

            let trusted_selected = current == TrustState::Trusted;
            if ui
                .selectable_label(trusted_selected, "✔  Trusted Workspace")
                .clicked()
            {
                new_state = TrustState::Trusted;
            }
            if ui
                .selectable_label(!trusted_selected, "✘  Restricted Mode")
                .clicked()
            {
                new_state = TrustState::Untrusted;
            }

            ui.add_space(8.0);

            match current {
                TrustState::Trusted => {
                    ui.colored_label(
                        palette.success,
                        "LSP, tasks, debugger, and terminal are enabled.",
                    );
                }
                TrustState::Untrusted => {
                    ui.colored_label(
                        palette.warning,
                        "Only file editing, search, and file tree are available.",
                    );
                }
            }

            ui.add_space(8.0);
            if ui.button("Close").clicked() {
                *open = false;
            }
        });

    if new_state != current {
        let _ = trust_store.set(root, new_state);
    }

    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        *open = false;
    }
}
