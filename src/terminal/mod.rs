pub mod pty;
pub mod renderer;
pub mod session;
pub mod split;
pub mod links;
pub mod search;
pub mod history;
pub mod env_editor;

pub use pty::{resize_pty, spawn_pty, PtyHandle};
pub use renderer::{render_terminal, Cell, TerminalBuffer};

use std::io::Write;
use std::path::PathBuf;

/// The kind of shell a terminal can run. Used to power the VS Code style
/// shell picker (PowerShell / Command Prompt / Git Bash, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    PowerShell,
    Cmd,
    GitBash,
    /// Platform default shell ($SHELL on Unix, cmd.exe on Windows).
    System,
}

impl ShellKind {
    /// Human friendly label used in tabs and menus.
    pub fn label(self) -> &'static str {
        match self {
            ShellKind::PowerShell => "PowerShell",
            ShellKind::Cmd => "Command Prompt",
            ShellKind::GitBash => "Git Bash",
            ShellKind::System => {
                if cfg!(windows) {
                    "Command Prompt"
                } else {
                    "Shell"
                }
            }
        }
    }

    /// The set of shells offered in the new-terminal dropdown, filtered to
    /// those actually installed on this machine.
    pub fn available() -> Vec<ShellKind> {
        let mut shells = Vec::new();

        #[cfg(windows)]
        {
            if pty::resolve_shell(ShellKind::PowerShell).is_some() {
                shells.push(ShellKind::PowerShell);
            }
            // cmd.exe is always present on Windows.
            shells.push(ShellKind::Cmd);
            if pty::resolve_shell(ShellKind::GitBash).is_some() {
                shells.push(ShellKind::GitBash);
            }
        }

        #[cfg(not(windows))]
        {
            shells.push(ShellKind::System);
            if pty::resolve_shell(ShellKind::Cmd).is_some() {
                // no-op on unix; kept for symmetry
            }
        }

        if shells.is_empty() {
            shells.push(ShellKind::System);
        }
        shells
    }

    /// The default shell to spawn when no explicit choice is made.
    pub fn default_shell() -> ShellKind {
        if cfg!(windows) {
            // Prefer PowerShell on Windows, matching VS Code's default.
            if pty::resolve_shell(ShellKind::PowerShell).is_some() {
                ShellKind::PowerShell
            } else {
                ShellKind::Cmd
            }
        } else {
            ShellKind::System
        }
    }
}

pub struct TerminalPane {
    pub buffer: TerminalBuffer,
    pub pty: Option<PtyHandle>,
    pub receiver: std::sync::mpsc::Receiver<Vec<u8>>,
    pub visible: bool,
    pub height: f32, // panel height, user-resizable
    pub rows: u16,
    pub cols: u16,
    pub shell: ShellKind,
}

impl TerminalPane {
    pub fn new(cwd: Option<PathBuf>) -> Self {
        Self::with_shell(cwd, ShellKind::default_shell())
    }

    pub fn with_shell(cwd: Option<PathBuf>, shell: ShellKind) -> Self {
        Self::with_shell_and_env(cwd, shell, &[])
    }

    /// Create a new pane with a specific shell and pre-set environment variables.
    pub fn with_shell_and_env(cwd: Option<PathBuf>, shell: ShellKind, env_vars: &[(String, String)]) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let rows = 24u16;
        let cols = 220u16;
        let pty = pty::spawn_pty_with_env(rows, cols, cwd, shell, tx, env_vars);
        let buffer = TerminalBuffer::new(rows as usize, cols as usize);
        TerminalPane {
            buffer,
            pty: Some(pty),
            receiver: rx,
            visible: false,
            height: 280.0,
            rows,
            cols,
            shell,
        }
    }

    /// Short label used for the terminal tab (e.g. "PowerShell").
    pub fn title(&self) -> &'static str {
        self.shell.label()
    }

    // Call every frame — drain PTY output into buffer
    pub fn poll(&mut self) {
        while let Ok(data) = self.receiver.try_recv() {
            self.buffer.feed(&data);
        }
    }

    // Send user input to PTY
    pub fn write(&mut self, data: &[u8]) {
        if let Some(pty) = &mut self.pty {
            pty.writer.write_all(data).ok();
            pty.writer.flush().ok();
        }
    }

    pub fn write_str(&mut self, s: &str) {
        self.write(s.as_bytes());
    }

    // Resize PTY and TerminalBuffer
    pub fn resize(&mut self, new_rows: u16, new_cols: u16) {
        if new_rows != self.rows || new_cols != self.cols {
            self.rows = new_rows;
            self.cols = new_cols;
            self.buffer.resize(new_rows as usize, new_cols as usize);
            if let Some(pty) = &self.pty {
                pty::resize_pty(pty.master.as_ref(), new_rows, new_cols);
            }
        }
    }
}
