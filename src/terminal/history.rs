//! Feature 5 — Command History Browser (Ctrl+R).
//!
//! Opens a floating modal window that shows shell history merged with
//! commands executed inside stack_ide terminal sessions.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Return the path to ~/.stack_ide/terminal_history.txt, creating the
/// parent directory if it does not yet exist.
pub fn stack_ide_history_path() -> Option<PathBuf> {
    let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
    let dir = home.join(".stack_ide");
    if !dir.exists() {
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| {
            eprintln!("[history] Failed to create ~/.stack_ide: {}", e);
        });
    }
    Some(dir.join("terminal_history.txt"))
}

/// Append one command to ~/.stack_ide/terminal_history.txt.
pub fn append_command(cmd: &str) {
    let path = match stack_ide_history_path() {
        Some(p) => p,
        None => return,
    };
    let line = format!("{}\n", cmd.trim());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    } else {
        eprintln!("[history] Failed to open {}", path.display());
    }
}

/// Load history from the stack_ide history file.
fn load_stack_ide_history() -> Vec<String> {
    let path = match stack_ide_history_path() {
        Some(p) => p,
        None => return Vec::new(),
    };
    read_lines_from_file(&path)
}

/// Load zsh/bash/fish shell history from the user's home directory.
fn load_shell_history() -> Vec<String> {
    let home = match directories::BaseDirs::new() {
        Some(b) => b.home_dir().to_path_buf(),
        None => return Vec::new(),
    };

    // Try each history file in priority order
    let candidates = [
        home.join(".zsh_history"),
        home.join(".bash_history"),
        home.join(".fish").join("fish_history"),
    ];

    for candidate in &candidates {
        if candidate.exists() {
            if candidate.extension().and_then(|e| e.to_str()) == Some("history")
                && candidate
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n == "fish_history")
                    .unwrap_or(false)
            {
                return load_fish_history(candidate);
            }
            return read_lines_from_file(candidate);
        }
    }

    Vec::new()
}

/// Parse fish_history YAML-like format — extract `cmd:` lines.
fn load_fish_history(path: &Path) -> Vec<String> {
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[history] Failed to read fish history: {}", e);
            return Vec::new();
        }
    };
    content
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("cmd: ") {
                Some(rest.to_owned())
            } else {
                None
            }
        })
        .collect()
}

/// Read a text file and return its non-empty lines.
fn read_lines_from_file(path: &Path) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(s) => s.lines().filter(|l| !l.trim().is_empty()).map(|l| l.to_owned()).collect(),
        Err(e) => {
            eprintln!("[history] Failed to read {}: {}", path.display(), e);
            Vec::new()
        }
    }
}

/// Build the merged, deduplicated history list (most recent first).
pub fn load_merged_history() -> Vec<HistoryEntry> {
    let mut seen = std::collections::HashSet::new();
    let mut result: Vec<HistoryEntry> = Vec::new();

    // Stack IDE history is most recent — add first so deduplication favours it
    for cmd in load_stack_ide_history().into_iter().rev() {
        if seen.insert(cmd.clone()) {
            result.push(HistoryEntry {
                command: cmd,
                source: "stack_ide".to_owned(),
            });
        }
    }

    // Shell history
    for cmd in load_shell_history().into_iter().rev() {
        if seen.insert(cmd.clone()) {
            result.push(HistoryEntry {
                command: cmd,
                source: "shell".to_owned(),
            });
        }
    }

    result
}

/// One entry in the history list.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub command: String,
    pub source: String,
}

/// All UI state for the history browser modal.
pub struct HistoryBrowserState {
    pub open: bool,
    pub filter: String,
    /// Cached full list (reloaded when the modal opens).
    pub entries: Vec<HistoryEntry>,
    /// Index of the currently keyboard-selected entry.
    pub selected: usize,
    /// Command to paste into the active terminal (set when user picks an item).
    pub pending_paste: Option<String>,
}

impl HistoryBrowserState {
    pub fn new() -> Self {
        Self {
            open: false,
            filter: String::new(),
            entries: Vec::new(),
            selected: 0,
            pending_paste: None,
        }
    }

    /// Open the modal and reload history.
    pub fn open(&mut self) {
        self.open = true;
        self.filter.clear();
        self.selected = 0;
        self.entries = load_merged_history();
    }

    /// Filtered entries based on the current `filter` substring.
    pub fn filtered(&self) -> Vec<HistoryEntry> {
        if self.filter.is_empty() {
            self.entries.clone()
        } else {
            let q = self.filter.to_lowercase();
            self.entries.iter().filter(|e| e.command.to_lowercase().contains(&q)).cloned().collect()
        }
    }
}

impl Default for HistoryBrowserState {
    fn default() -> Self {
        Self::new()
    }
}

/// Render the history browser modal.
///
/// Returns a command string to paste into the active terminal if the user
/// selected one, or `None`.
pub fn render_history_browser(
    ctx: &egui::Context,
    state: &mut HistoryBrowserState,
    palette: crate::theme::SemanticPalette,
) -> Option<String> {
    if !state.open {
        return None;
    }

    let mut paste_command: Option<String> = None;
    let mut should_close = false;
    let mut clear_history = false;

    egui::Window::new("Command History")
        .collapsible(false)
        .resizable(true)
        .default_size([500.0, 400.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            // Filter input
            ui.horizontal(|ui| {
                ui.label("Filter history…");
                ui.add(
                    egui::TextEdit::singleline(&mut state.filter)
                        .desired_width(300.0)
                        .hint_text("Type to filter…"),
                );
            });
            ui.add_space(4.0);

            let filtered = state.filtered();
            let count = filtered.len();

            // Handle arrow-key navigation before rendering the list
            let (up, down, enter) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if up && state.selected > 0 {
                state.selected -= 1;
            }
            if down && state.selected + 1 < count {
                state.selected += 1;
            }
            if enter && !filtered.is_empty() {
                let cmd = filtered[state.selected].command.clone();
                paste_command = Some(cmd);
                should_close = true;
            }
            // Clamp after potential count change from filter update
            if count > 0 && state.selected >= count {
                state.selected = count - 1;
            }

            // Scrollable list
            let _row_height = 20.0_f32;
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .show(ui, |ui| {
                    if filtered.is_empty() {
                        ui.colored_label(palette.muted_text, "No history found");
                    }
                    for (i, entry) in filtered.iter().enumerate() {
                        let is_selected = i == state.selected;
                        ui.horizontal(|ui| {
                            let cmd_resp = ui.selectable_label(
                                is_selected,
                                egui::RichText::new(&entry.command)
                                    .color(palette.primary_text)
                                    .monospace(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new(&entry.source)
                                            .small()
                                            .color(egui::Color32::from_rgb(136, 136, 136)),
                                    );
                                },
                            );
                            if cmd_resp.clicked() {
                                state.selected = i;
                                paste_command = Some(entry.command.clone());
                                should_close = true;
                            }
                        });
                    }
                });

            ui.add_space(8.0);
            ui.separator();

            // Bottom buttons
            ui.horizontal(|ui| {
                if ui.button("Clear History").clicked() {
                    clear_history = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        should_close = true;
                    }
                });
            });
        });

    if clear_history {
        // Clear the stack_ide history file
        if let Some(path) = stack_ide_history_path() {
            std::fs::write(&path, "").unwrap_or_else(|e| {
                eprintln!("[history] Failed to clear history: {}", e);
            });
        }
        state.entries.retain(|e| e.source != "stack_ide");
        state.entries = load_merged_history();
    }

    if should_close {
        state.open = false;
    }

    // Take pending paste set by the selection
    paste_command.or_else(|| state.pending_paste.take())
}
