//! Feature 5 — Task Runner
//!
//! Reads `tasks.toml`, shows a "Task Runner" modal (Ctrl+Shift+T), spawns
//! tasks, and streams output to a dedicated "Tasks" bottom panel.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use egui::{Color32, RichText};

// Colours used in the Tasks panel output.
const COLOR_STDOUT: Color32 = Color32::WHITE;
const COLOR_STDERR: Color32 = Color32::from_rgb(255, 64, 64);
const COLOR_SUCCESS: Color32 = Color32::from_rgb(78, 201, 176);
const COLOR_FAILURE: Color32 = Color32::from_rgb(255, 64, 64);
const COLOR_DURATION: Color32 = Color32::from_rgb(255, 165, 0);

// ─── tasks.toml schema ────────────────────────────────────────────────────────

/// One task entry from `tasks.toml`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct TaskDef {
    pub name: String,
    pub command: String,
    #[serde(default = "default_cwd")]
    pub working_dir: String,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub description: String,
}

fn default_cwd() -> String {
    ".".to_string()
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default)]
pub struct TasksToml {
    #[serde(default, rename = "task")]
    pub tasks: Vec<TaskDef>,
}

impl TasksToml {
    /// Load `tasks.toml` from `workspace_root`.
    pub fn load(workspace_root: &Path) -> Result<Self, String> {
        let path = workspace_root.join("tasks.toml");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("tasks.toml parse error: {e}"))
    }

    /// Write the default starter `tasks.toml` to `workspace_root`.
    pub fn write_starter(workspace_root: &Path) -> Result<(), String> {
        let path = workspace_root.join("tasks.toml");
        let content = r#"[[task]]
name = "build"
command = "cargo build"
working_dir = "."
description = "Build the project"

[[task]]
name = "test"
command = "cargo test"
working_dir = "."
description = "Run all tests"

[[task]]
name = "fmt"
command = "cargo fmt"
working_dir = "."
description = "Format source code"
"#;
        std::fs::write(&path, content)
            .map_err(|e| format!("Could not write {}: {e}", path.display()))
    }
}

// ─── Task runner modal state ──────────────────────────────────────────────────

/// Line of task output.
#[derive(Debug, Clone)]
pub struct TaskOutputLine {
    pub text: String,
    pub is_stderr: bool,
    pub is_status: bool,
    pub color: Color32,
}

/// Events sent from the background thread.
#[derive(Debug)]
pub enum TaskEvent {
    Line { text: String, is_stderr: bool },
    Done { exit_code: i32 },
}

/// A running task's output channel and metadata.
pub struct RunningTask {
    pub name: String,
    pub started: std::time::Instant,
    pub receiver: crossbeam_channel::Receiver<TaskEvent>,
    pub child: std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>>,
    pub finished: bool,
}

/// State for the task runner: modal open flag, loaded tasks, running handle.
#[derive(Default)]
pub struct TaskRunnerState {
    pub modal_open: bool,
    pub tasks: Vec<TaskDef>,
    pub selected_index: Option<usize>,
    /// Output lines shown in the Tasks panel.
    pub output: Vec<TaskOutputLine>,
    /// Currently running child process (if any).
    pub running: Option<RunningTask>,
    /// True if `tasks.toml` was not found at the last load.
    pub no_tasks_file: bool,
}

impl TaskRunnerState {
    /// Reload `tasks.toml` from `workspace_root`.
    pub fn reload(&mut self, workspace_root: &Path) {
        match TasksToml::load(workspace_root) {
            Ok(f) => {
                self.tasks = f.tasks;
                self.no_tasks_file = false;
            }
            Err(_) => {
                self.tasks.clear();
                self.no_tasks_file = true;
            }
        }
    }

    /// Poll the running task for new output lines.  Call every frame.
    pub fn poll(&mut self) {
        let Some(running) = &mut self.running else {
            return;
        };
        if running.finished {
            return;
        }
        while let Ok(ev) = running.receiver.try_recv() {
            match ev {
                TaskEvent::Line { text, is_stderr } => {
                    self.output.push(TaskOutputLine {
                        text,
                        is_stderr,
                        is_status: false,
                        color: if is_stderr { COLOR_STDERR } else { COLOR_STDOUT },
                    });
                }
                TaskEvent::Done { exit_code } => {
                    let elapsed = running.started.elapsed().as_secs_f64();
                    let (msg, color) = if exit_code == 0 {
                        (
                            format!("✓ Task '{}' finished (exit code: 0)", running.name),
                            COLOR_SUCCESS,
                        )
                    } else {
                        (
                            format!("✗ Task '{}' failed (exit code: {exit_code})", running.name),
                            COLOR_FAILURE,
                        )
                    };
                    self.output.push(TaskOutputLine {
                        text: msg,
                        is_stderr: false,
                        is_status: true,
                        color,
                    });
                    self.output.push(TaskOutputLine {
                        text: format!("Duration: {elapsed:.2}s"),
                        is_stderr: false,
                        is_status: true,
                        color: COLOR_DURATION,
                    });
                    running.finished = true;
                }
            }
        }
    }

    /// Spawn the task at `index`.
    pub fn run_task(
        &mut self,
        index: usize,
        workspace_root: &Path,
        extra_env: &[(String, String)],
    ) {
        let Some(task) = self.tasks.get(index) else {
            return;
        };
        let task = task.clone();
        self.output.clear();
        self.output.push(TaskOutputLine {
            text: format!("▶ Running: {}", task.name),
            is_stderr: false,
            is_status: true,
            color: COLOR_SUCCESS,
        });

        // Build working directory.
        let cwd = if task.working_dir.trim() == "." || task.working_dir.is_empty() {
            workspace_root.to_path_buf()
        } else {
            workspace_root.join(&task.working_dir)
        };

        let (tx, rx) = crossbeam_channel::bounded::<TaskEvent>(512);
        let name_clone = task.name.clone();
        let extra_env_owned: Vec<(String, String)> = extra_env.to_vec();
        let task_env = task.env.clone();

        // Parse command into owned program + args strings (avoids borrow issues).
        let parts: Vec<String> = task
            .command
            .split_whitespace()
            .map(|s| s.to_owned())
            .collect();
        let (program, args): (String, Vec<String>) = if parts.is_empty() {
            ("echo".to_owned(), vec!["(empty command)".to_owned()])
        } else {
            (parts[0].clone(), parts[1..].to_vec())
        };

        let child_arc: std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let child_arc_clone = child_arc.clone();

        std::thread::spawn(move || {
            use std::io::BufRead;
            use std::process::Stdio;

            let mut cmd = std::process::Command::new(&program);
            cmd.args(&args)
                .current_dir(&cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for (k, v) in &task_env {
                cmd.env(k, v);
            }
            for (k, v) in &extra_env_owned {
                cmd.env(k, v);
            }

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(TaskEvent::Line {
                        text: format!("Error spawning '{}': {e}", program),
                        is_stderr: true,
                    });
                    let _ = tx.send(TaskEvent::Done { exit_code: -1 });
                    return;
                }
            };

            let stdout = child.stdout.take().expect("stdout");
            let stderr = child.stderr.take().expect("stderr");

            // Drop the placeholder — child is now in scope.
            drop(child_arc_clone);

            let tx2 = tx.clone();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(stdout).lines().flatten() {
                    let _ = tx2.send(TaskEvent::Line { text: line, is_stderr: false });
                }
            });
            let tx3 = tx.clone();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(stderr).lines().flatten() {
                    let _ = tx3.send(TaskEvent::Line { text: line, is_stderr: true });
                }
            });

            let _ = name_clone; // keep alive
            let exit_code = match child.wait() {
                Ok(status) => status.code().unwrap_or(-1),
                Err(_) => -1,
            };
            std::thread::sleep(std::time::Duration::from_millis(80));
            let _ = tx.send(TaskEvent::Done { exit_code });
        });

        self.running = Some(RunningTask {
            name: task.name.clone(),
            started: std::time::Instant::now(),
            receiver: rx,
            child: child_arc,
            finished: false,
        });
        self.modal_open = false;
    }

    /// Kill the running task.
    pub fn stop(&mut self) {
        if let Some(running) = &mut self.running {
            running.finished = true;
            if let Ok(mut guard) = running.child.lock() {
                if let Some(child) = guard.as_mut() {
                    let _ = child.kill();
                }
            }
        }
    }
}

// ─── Modal UI ─────────────────────────────────────────────────────────────────

/// Render the Task Runner modal.
///
/// Returns `Some((index, workspace_root))` when the user selects and launches
/// a task (caller should call `state.run_task(index, &root, env)`).
pub fn show_task_runner_modal(
    ctx: &egui::Context,
    state: &mut TaskRunnerState,
    workspace_root: Option<&Path>,
    palette: crate::theme::SemanticPalette,
) -> Option<(usize, PathBuf)> {
    if !state.modal_open {
        return None;
    }

    let mut result = None;

    egui::Window::new("Task Runner")
        .id(egui::Id::new("task_runner_modal"))
        .collapsible(false)
        .resizable(false)
        .default_width(480.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            let root = match workspace_root {
                Some(r) => r.to_path_buf(),
                None => {
                    ui.label("No workspace root open.");
                    if ui.button("Close").clicked() {
                        state.modal_open = false;
                    }
                    return;
                }
            };

            if state.no_tasks_file {
                ui.colored_label(
                    palette.warning,
                    format!(
                        "No tasks.toml found. Create one at {}",
                        root.join("tasks.toml").display()
                    ),
                );
                ui.add_space(6.0);
                if ui.button("Create tasks.toml").clicked() {
                    if let Err(e) = TasksToml::write_starter(&root) {
                        eprintln!("tasks: {e}");
                    } else {
                        state.reload(&root);
                    }
                }
                ui.add_space(6.0);
                if ui.button("Close").clicked() {
                    state.modal_open = false;
                }
                return;
            }

            ui.heading("Select a task to run:");
            ui.add_space(6.0);

            egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                for (i, task) in state.tasks.iter().enumerate() {
                    let selected = state.selected_index == Some(i);
                    let resp = ui.selectable_label(
                        selected,
                        RichText::new(format!(
                            "{} — {}",
                            task.name,
                            task.description
                        )),
                    );
                    if resp.clicked() {
                        state.selected_index = Some(i);
                    }
                    if resp.double_clicked() {
                        result = Some((i, root.clone()));
                    }
                }
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let can_run = state.selected_index.is_some();
                if ui
                    .add_enabled(can_run, egui::Button::new("▶ Run Task"))
                    .clicked()
                {
                    if let Some(idx) = state.selected_index {
                        result = Some((idx, root.clone()));
                    }
                }
                if ui.button("Close").clicked() {
                    state.modal_open = false;
                }
            });

            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if let Some(idx) = state.selected_index {
                    result = Some((idx, root));
                }
            }
        });

    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        state.modal_open = false;
    }

    result
}

// ─── Tasks panel (bottom panel content) ──────────────────────────────────────

/// Render the Tasks panel content inside a pre-allocated `Ui`.
pub fn render_tasks_panel(
    ui: &mut egui::Ui,
    state: &mut TaskRunnerState,
    palette: crate::theme::SemanticPalette,
) {
    let is_running = state
        .running
        .as_ref()
        .map(|r| !r.finished)
        .unwrap_or(false);

    ui.horizontal(|ui| {
        ui.label(RichText::new("Tasks").strong());
        ui.add_space(8.0);
        if is_running {
            if ui
                .add(egui::Button::new("⏹ Stop").fill(palette.error))
                .clicked()
            {
                state.stop();
            }
        }
    });
    ui.separator();

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for line in &state.output {
                ui.colored_label(line.color, &line.text);
            }
        });
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
        let p = std::env::temp_dir().join(format!("ws_tasks_test_{n}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn write_and_load_tasks_toml() {
        let dir = tmp_dir();
        TasksToml::write_starter(&dir).unwrap();
        let f = TasksToml::load(&dir).unwrap();
        assert_eq!(f.tasks.len(), 3);
        assert!(f.tasks.iter().any(|t| t.name == "build"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reload_sets_no_tasks_file_when_missing() {
        let dir = tmp_dir();
        let mut state = TaskRunnerState::default();
        state.reload(&dir);
        assert!(state.no_tasks_file);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
