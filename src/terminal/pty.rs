use super::ShellKind;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::path::PathBuf;
use std::sync::mpsc::Sender;

pub struct PtyHandle {
    pub writer: Box<dyn std::io::Write + Send>,
    pub master: Box<dyn portable_pty::MasterPty + Send>,
}

/// A resolved shell: the executable to launch plus any args it needs.
pub struct ResolvedShell {
    pub program: String,
    pub args: Vec<String>,
}

/// Resolve a [`ShellKind`] to a concrete executable on this machine. Returns
/// `None` when the shell is not installed (used to filter the picker menu).
pub fn resolve_shell(kind: ShellKind) -> Option<ResolvedShell> {
    match kind {
        #[cfg(windows)]
        ShellKind::PowerShell => {
            // Prefer PowerShell 7+ (pwsh), fall back to Windows PowerShell.
            if let Some(pwsh) = find_in_path("pwsh.exe") {
                Some(ResolvedShell {
                    program: pwsh,
                    args: vec!["-NoLogo".into()],
                })
            } else {
                let win_ps = windows_powershell_path();
                if std::path::Path::new(&win_ps).exists() {
                    Some(ResolvedShell {
                        program: win_ps,
                        args: vec!["-NoLogo".into()],
                    })
                } else {
                    None
                }
            }
        }
        #[cfg(not(windows))]
        ShellKind::PowerShell => find_in_path("pwsh").map(|p| ResolvedShell {
            program: p,
            args: vec!["-NoLogo".into()],
        }),

        ShellKind::Cmd => {
            #[cfg(windows)]
            {
                Some(ResolvedShell {
                    program: "cmd.exe".into(),
                    args: Vec::new(),
                })
            }
            #[cfg(not(windows))]
            {
                None
            }
        }

        ShellKind::GitBash => find_git_bash().map(|p| ResolvedShell {
            program: p,
            // --login -i gives an interactive login shell, matching VS Code.
            args: vec!["--login".into(), "-i".into()],
        }),

        ShellKind::System => {
            #[cfg(windows)]
            {
                Some(ResolvedShell {
                    program: "cmd.exe".into(),
                    args: Vec::new(),
                })
            }
            #[cfg(not(windows))]
            {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
                Some(ResolvedShell {
                    program: shell,
                    args: Vec::new(),
                })
            }
        }
    }
}

#[cfg(windows)]
fn windows_powershell_path() -> String {
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    format!(
        "{}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
        system_root
    )
}

/// Search PATH for an executable, returning its full path if found.
fn find_in_path(exe: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(exe);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

/// Locate Git Bash (`bash.exe`) from a Git for Windows install.
fn find_git_bash() -> Option<String> {
    #[cfg(windows)]
    {
        let mut candidates: Vec<PathBuf> = Vec::new();

        for var in ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"] {
            if let Some(base) = std::env::var_os(var) {
                let base = PathBuf::from(base);
                candidates.push(base.join("Git\\bin\\bash.exe"));
                candidates.push(base.join("Programs\\Git\\bin\\bash.exe"));
            }
        }

        for candidate in candidates {
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }

        // Last resort: a `git.exe` on PATH implies a sibling bash.exe.
        if let Some(git) = find_in_path("git.exe") {
            let git_path = PathBuf::from(git);
            if let Some(bin_dir) = git_path.parent() {
                let bash = bin_dir.join("bash.exe");
                if bash.is_file() {
                    return Some(bash.to_string_lossy().into_owned());
                }
                // git.exe is often in <root>\cmd, bash in <root>\bin
                if let Some(root) = bin_dir.parent() {
                    let bash = root.join("bin\\bash.exe");
                    if bash.is_file() {
                        return Some(bash.to_string_lossy().into_owned());
                    }
                }
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        find_in_path("bash")
    }
}

pub fn spawn_pty(
    rows: u16,
    cols: u16,
    cwd: Option<PathBuf>,
    shell: ShellKind,
    to_ui: Sender<Vec<u8>>, // PTY output → UI
) -> PtyHandle {
    spawn_pty_with_env(rows, cols, cwd, shell, to_ui, &[])
}

pub fn spawn_pty_with_env(
    rows: u16,
    cols: u16,
    cwd: Option<PathBuf>,
    shell: ShellKind,
    to_ui: Sender<Vec<u8>>,
    env_vars: &[(String, String)],
) -> PtyHandle {
    let pty_system = native_pty_system();

    let size = PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };

    let pair = pty_system.openpty(size).expect("Failed to open PTY");

    // Resolve the requested shell, falling back to the platform default if it
    // is somehow unavailable.
    let resolved = resolve_shell(shell)
        .or_else(|| resolve_shell(ShellKind::System))
        .unwrap_or(ResolvedShell {
            program: if cfg!(windows) {
                "cmd.exe".into()
            } else {
                "/bin/sh".into()
            },
            args: Vec::new(),
        });

    let mut cmd = CommandBuilder::new(&resolved.program);
    for arg in &resolved.args {
        cmd.arg(arg);
    }

    // Set working dir to current project root if available
    if let Some(ref dir) = cwd {
        cmd.cwd(dir);
    }

    // Inject project environment variables
    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    let _child = pair
        .slave
        .spawn_command(cmd)
        .expect("Failed to spawn shell");

    // Reader thread: PTY stdout → mpsc channel
    let mut reader = pair
        .master
        .try_clone_reader()
        .expect("Failed to clone PTY reader");

    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    to_ui.send(buf[..n].to_vec()).ok();
                }
            }
        }
    });

    PtyHandle {
        writer: pair
            .master
            .take_writer()
            .expect("Failed to take PTY writer"),
        master: pair.master,
    }
}

pub fn resize_pty(master: &dyn portable_pty::MasterPty, rows: u16, cols: u16) {
    master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .ok();
}
