//! Task runner system: config, execution, and problem matching.

pub mod config;
pub mod problem_matcher;
pub mod runner;

// Re-export top-level types used by app.rs
pub use runner::{TaskStatus, TaskHandle, TaskLine};
pub use config::TaskConfig;

use std::collections::HashMap;
use std::path::Path;

/// UI-level state for the task panel; lives in BlueIdeApp.
#[derive(Default)]
pub struct TaskPanelState {
    /// Tasks loaded from tasks.toml.
    pub tasks: HashMap<String, TaskConfig>,
    /// Currently-running (or just-finished) task handle.
    pub running: Option<TaskHandle>,
    /// Name of the last task that was run.
    pub last_task: Option<String>,
}

impl TaskPanelState {
    /// Reload tasks.toml from the workspace root.
    pub fn reload(&mut self, workspace_root: &Path) {
        match config::TasksFile::load(workspace_root) {
            Ok(tf) => self.tasks = tf.tasks,
            Err(e) => eprintln!("tasks: {e}"),
        }
    }

    /// Poll the running task handle for new output.
    pub fn poll(&mut self) {
        if let Some(h) = &mut self.running {
            h.poll();
        }
    }

    /// Spawn the named task.  Callers must verify workspace trust before calling.
    pub fn run(&mut self, name: &str, workspace_root: &Path) {
        self.run_with_env(name, workspace_root, &[]);
    }

    /// Spawn the named task with extra environment variables.
    pub fn run_with_env(&mut self, name: &str, workspace_root: &Path, extra_env: &[(String, String)]) {
        if let Some(cfg) = self.tasks.get(name).cloned() {
            if let Some(h) = &self.running {
                h.cancel();
            }
            self.running = Some(runner::spawn_task_with_env(name, &cfg, workspace_root, extra_env));
            self.last_task = Some(name.to_string());
        }
    }

    /// Cancel the running task.
    pub fn cancel(&self) {
        if let Some(h) = &self.running {
            h.cancel();
        }
    }

    /// Re-run the last task.
    pub fn rerun_last(&mut self, workspace_root: &Path) {
        if let Some(name) = self.last_task.clone() {
            self.run(&name, workspace_root);
        }
    }
}
