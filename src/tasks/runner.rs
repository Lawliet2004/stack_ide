//! Task runner: spawn tasks in background threads, stream output.


use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Instant;

use crossbeam_channel::{bounded, Receiver, Sender};

use super::config::TaskConfig;
use super::problem_matcher::{match_rustc, ProblemMatch};

// ─── Status ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    Idle,
    Running,
    Success,
    Failed(i32),  // exit code
    Cancelled,
    Error(String),
}

impl TaskStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Success => "success",
            Self::Failed(_) => "failed",
            Self::Cancelled => "cancelled",
            Self::Error(_) => "error",
        }
    }
    pub fn is_terminal(&self) -> bool { !matches!(self, Self::Running) }
}

// ─── Output line ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TaskLine {
    pub task_name: String,
    pub text: String,
    pub is_stderr: bool,
}

// ─── Internal channel events ──────────────────────────────────────────────────

enum RawEvent {
    Line(TaskLine),
    Finished(TaskStatus),
    Diagnostics(Vec<ProblemMatch>),
}

// ─── Task handle (public) ─────────────────────────────────────────────────────

pub struct TaskHandle {
    pub task_name: String,
    pub status: TaskStatus,
    pub started_at: Instant,
    pub output_lines: Vec<TaskLine>,
    pub diagnostics: Vec<ProblemMatch>,
    receiver: Receiver<RawEvent>,
    cancel_tx: Sender<()>,
}

impl TaskHandle {
    /// Drain any pending events from the background thread. Call every frame.
    pub fn poll(&mut self) {
        while let Ok(ev) = self.receiver.try_recv() {
            match ev {
                RawEvent::Line(l) => self.output_lines.push(l),
                RawEvent::Diagnostics(d) => self.diagnostics = d,
                RawEvent::Finished(s) => self.status = s,
            }
        }
    }

    pub fn is_running(&self) -> bool { self.status == TaskStatus::Running }
    pub fn cancel(&self) { let _ = self.cancel_tx.try_send(()); }
}

// ─── Spawn ────────────────────────────────────────────────────────────────────

/// Spawn a task in a background thread. Callers must check workspace trust.
pub fn spawn_task(name: &str, config: &TaskConfig, workspace_root: &Path) -> TaskHandle {
    spawn_task_with_env(name, config, workspace_root, &[])
}

/// Spawn a task with additional environment variables injected on top of the config's own env.
pub fn spawn_task_with_env(
    name: &str,
    config: &TaskConfig,
    workspace_root: &Path,
    extra_env: &[(String, String)],
) -> TaskHandle {
    let (etx, erx) = bounded::<RawEvent>(512);
    let (ctx, crx) = bounded::<()>(1);

    let name_owned = name.to_string();
    let name_for_handle = name_owned.clone();   // kept for TaskHandle return value
    let config = config.clone();
    let root = workspace_root.to_path_buf();
    let tx = etx;
    let extra_env_owned: Vec<(String, String)> = extra_env.to_vec();

    thread::spawn(move || {
        let cwd = if config.cwd.trim() == "." || config.cwd.is_empty() {
            root.clone()
        } else {
            root.join(&config.cwd)
        };

        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &config.env { cmd.env(k, v); }
        // Inject project-level env vars (from .stack_ide/env.json)
        for (k, v) in &extra_env_owned { cmd.env(k, v); }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(RawEvent::Finished(TaskStatus::Error(e.to_string())));
                return;
            }
        };

        let stdout = child.stdout.take().expect("stdout");
        let stderr = child.stderr.take().expect("stderr");

        // Channel to collect all lines for problem matching.
        let (ltx, lrx) = bounded::<String>(512);
        let matcher = config.problem_matcher.clone();
        let root_pm = root.clone();
        let task_name = name_owned.clone();

        // stdout reader thread.
        let tx2 = tx.clone();
        let ltx2 = ltx.clone();
        let tn2 = task_name.clone();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().flatten() {
                let _ = tx2.send(RawEvent::Line(TaskLine {
                    task_name: tn2.clone(), text: line.clone(), is_stderr: false,
                }));
                let _ = ltx2.send(line);
            }
        });

        // stderr reader thread.
        let tx3 = tx.clone();
        let tn3 = task_name.clone();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().flatten() {
                let _ = tx3.send(RawEvent::Line(TaskLine {
                    task_name: tn3.clone(), text: line.clone(), is_stderr: true,
                }));
                let _ = ltx.send(line);
            }
        });

        // Wait for the process, checking for cancel.
        let status = loop {
            if crx.try_recv().is_ok() {
                let _ = child.kill();
                break TaskStatus::Cancelled;
            }
            match child.try_wait() {
                Ok(Some(exit)) => {
                    let code = exit.code().unwrap_or(-1);
                    break if code == 0 { TaskStatus::Success } else { TaskStatus::Failed(code) };
                }
                Ok(None) => thread::sleep(std::time::Duration::from_millis(50)),
                Err(e) => break TaskStatus::Error(e.to_string()),
            }
        };

        // Give reader threads a moment to flush remaining lines.
        thread::sleep(std::time::Duration::from_millis(120));

        // Run the problem matcher.
        if matches!(status, TaskStatus::Success | TaskStatus::Failed(_)) {
            if matcher.as_deref() == Some("rustc") {
                let mut lines: Vec<String> = Vec::new();
                while let Ok(l) = lrx.try_recv() { lines.push(l); }
                let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
                let diags = match_rustc(&refs, &root_pm);
                if !diags.is_empty() {
                    let _ = tx.send(RawEvent::Diagnostics(diags));
                }
            }
        }

        let _ = tx.send(RawEvent::Finished(status));
    });

    TaskHandle {
        task_name: name_for_handle,
        status: TaskStatus::Running,
        started_at: Instant::now(),
        output_lines: Vec::new(),
        diagnostics: Vec::new(),
        receiver: erx,
        cancel_tx: ctx,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn echo_task_succeeds() {
        let cfg = TaskConfig {
            command: if cfg!(windows) { "cmd".into() } else { "echo".into() },
            args: if cfg!(windows) {
                vec!["/c".into(), "echo".into(), "hi".into()]
            } else {
                vec!["hi".into()]
            },
            cwd: ".".into(),
            problem_matcher: None,
            env: HashMap::new(),
        };
        let mut h = spawn_task("t", &cfg, &std::env::temp_dir());
        let deadline = std::time::Instant::now();
        loop {
            h.poll();
            if h.status.is_terminal() {
                assert_eq!(h.status, TaskStatus::Success);
                return;
            }
            if deadline.elapsed().as_secs() > 5 { panic!("task did not finish"); }
            thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}
