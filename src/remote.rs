//! Remote file editing over SSH/SFTP.
//!
//! Uses background threads for all blocking I/O. The UI reads results via
//! non-blocking `try_recv` on a crossbeam channel. No passwords are stored;
//! credentials are solicited at connection time only.

use std::collections::HashMap;
use std::path::{PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{bounded, Receiver, Sender};
use serde::{Deserialize, Serialize};

// ─── Identity types ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RemoteConnectionId(pub u64);

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

fn new_conn_id() -> RemoteConnectionId {
    RemoteConnectionId(NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed))
}

/// Authentication method for a connection profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuthMethod {
    /// Path to a private key file; password/passphrase asked at runtime.
    PrivateKey { key_path: PathBuf },
    /// Password auth: no stored password — user is prompted at runtime.
    Password,
    /// Try the SSH agent first, then fall back to `Password`.
    Agent,
}

/// A stored SSH connection profile (no secrets).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshProfile {
    pub id: RemoteConnectionId,
    pub label: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
    pub remote_root: String,
}

impl Default for SshProfile {
    fn default() -> Self {
        Self {
            id: new_conn_id(),
            label: String::new(),
            host: String::new(),
            port: 22,
            username: String::new(),
            auth: AuthMethod::Agent,
            remote_root: "/".into(),
        }
    }
}

/// A remote file path, tied to a connection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RemoteFileId {
    pub connection_id: RemoteConnectionId,
    pub path: String,
}

/// File location: local or remote.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileLocation {
    Local(PathBuf),
    Remote(RemoteFileId),
}

impl std::fmt::Display for FileLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileLocation::Local(p) => write!(f, "{}", p.display()),
            FileLocation::Remote(r) => write!(f, "ssh:{}:{}", r.connection_id.0, r.path),
        }
    }
}

// ─── Messages between background worker and UI ────────────────────────────────

#[derive(Debug)]
pub enum RemoteRequest {
    ListDir { path: String },
    ReadFile { path: String },
    WriteFile { path: String, content: Vec<u8> },
    Disconnect,
}

#[derive(Debug)]
pub enum RemoteEvent {
    /// Directory listing succeeded.
    DirListing { path: String, entries: Vec<DirEntry> },
    /// File read succeeded.
    FileContent { path: String, content: Vec<u8> },
    /// File write succeeded.
    FileSaved { path: String },
    /// Connection established.
    Connected,
    /// Connection closed.
    Disconnected,
    /// An error occurred.
    Error { context: String, message: String },
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
}

// ─── Connection state ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    Error,
}

pub struct RemoteConnection {
    pub id: RemoteConnectionId,
    pub profile: SshProfile,
    pub status: ConnectionStatus,
    pub event_rx: Receiver<RemoteEvent>,
    request_tx: Sender<RemoteRequest>,
    /// Loaded directory tree cached for the explorer.
    pub dir_cache: HashMap<String, Vec<DirEntry>>,
}

impl RemoteConnection {
    /// Send a request to the background worker.
    pub fn send(&self, req: RemoteRequest) {
        let _ = self.request_tx.try_send(req);
    }

    /// Poll for incoming events without blocking.
    pub fn poll(&mut self) {
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                RemoteEvent::Connected => self.status = ConnectionStatus::Connected,
                RemoteEvent::Disconnected => self.status = ConnectionStatus::Disconnected,
                RemoteEvent::Error { .. } => self.status = ConnectionStatus::Error,
                RemoteEvent::DirListing { path, entries } => {
                    self.dir_cache.insert(path, entries);
                }
                _ => {}
            }
        }
    }
}

/// Connect to an SSH host using the `ssh` CLI on the PATH as a subprocess.
///
/// Full libssh2 integration is left as a future enhancement. This stub uses
/// `ssh -T` to verify connectivity and performs SFTP-like operations via
/// `scp`/`ssh cat` commands — good enough for file reading/writing without
/// requiring additional crates.
pub fn connect(
    profile: SshProfile,
    password_hint: Option<String>,
) -> RemoteConnection {
    let id = profile.id;
    let (event_tx, event_rx) = bounded::<RemoteEvent>(64);
    let (request_tx, request_rx) = bounded::<RemoteRequest>(64);

    let profile_clone = profile.clone();
    let tx = event_tx;

    std::thread::spawn(move || {
        let _ = password_hint; // Would be used for SSH_ASKPASS / expect script
        let host = &profile_clone.host;
        let user = &profile_clone.username;
        let port = profile_clone.port;

        // Probe connectivity with a no-op SSH command.
        let probe = std::process::Command::new("ssh")
            .args([
                "-p", &port.to_string(),
                "-o", "BatchMode=yes",
                "-o", "StrictHostKeyChecking=accept-new",
                "-o", "ConnectTimeout=10",
                &format!("{user}@{host}"),
                "echo connected",
            ])
            .output();

        match probe {
            Ok(out) if out.status.success() => {
                let _ = tx.send(RemoteEvent::Connected);
            }
            Ok(out) => {
                let msg = String::from_utf8_lossy(&out.stderr).to_string();
                let _ = tx.send(RemoteEvent::Error {
                    context: "connect".into(),
                    message: msg,
                });
                return;
            }
            Err(e) => {
                let _ = tx.send(RemoteEvent::Error {
                    context: "connect".into(),
                    message: format!("ssh not found or failed: {e}"),
                });
                return;
            }
        }

        // Process requests from the UI.
        while let Ok(req) = request_rx.recv() {
            match req {
                RemoteRequest::Disconnect => break,

                RemoteRequest::ListDir { path } => {
                    let out = std::process::Command::new("ssh")
                        .args([
                            "-p", &port.to_string(),
                            &format!("{user}@{host}"),
                            &format!("ls -1aF {path}"),
                        ])
                        .output();
                    match out {
                        Ok(o) if o.status.success() => {
                            let text = String::from_utf8_lossy(&o.stdout);
                            let entries = text
                                .lines()
                                .filter(|l| !l.is_empty() && *l != "." && *l != "..")
                                .map(|l| {
                                    let is_dir = l.ends_with('/');
                                    let name = l.trim_end_matches(['/', '*', '@', '|', '=', '%']).to_string();
                                    DirEntry { name, is_dir, size: None }
                                })
                                .collect();
                            let _ = tx.send(RemoteEvent::DirListing { path, entries });
                        }
                        _ => {
                            let _ = tx.send(RemoteEvent::Error {
                                context: "listdir".into(),
                                message: "failed to list directory".into(),
                            });
                        }
                    }
                }

                RemoteRequest::ReadFile { path } => {
                    let out = std::process::Command::new("ssh")
                        .args([
                            "-p", &port.to_string(),
                            &format!("{user}@{host}"),
                            &format!("cat {path}"),
                        ])
                        .output();
                    match out {
                        Ok(o) if o.status.success() => {
                            let _ = tx.send(RemoteEvent::FileContent {
                                path,
                                content: o.stdout,
                            });
                        }
                        Ok(o) => {
                            let _ = tx.send(RemoteEvent::Error {
                                context: "read".into(),
                                message: String::from_utf8_lossy(&o.stderr).into_owned(),
                            });
                        }
                        Err(e) => {
                            let _ = tx.send(RemoteEvent::Error {
                                context: "read".into(),
                                message: e.to_string(),
                            });
                        }
                    }
                }

                RemoteRequest::WriteFile { path, content } => {
                    // Write via `ssh "cat > remote_path"` piping content to stdin.
                    let mut child = match std::process::Command::new("ssh")
                        .args([
                            "-p", &port.to_string(),
                            &format!("{user}@{host}"),
                            &format!("cat > {path}"),
                        ])
                        .stdin(std::process::Stdio::piped())
                        .spawn()
                    {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = tx.send(RemoteEvent::Error {
                                context: "write".into(),
                                message: e.to_string(),
                            });
                            continue;
                        }
                    };

                    let success = if let Some(stdin) = child.stdin.as_mut() {
                        use std::io::Write;
                        stdin.write_all(&content).is_ok()
                    } else {
                        false
                    };
                    let exit = child.wait().map(|s| s.success()).unwrap_or(false);

                    if success && exit {
                        let _ = tx.send(RemoteEvent::FileSaved { path });
                    } else {
                        let _ = tx.send(RemoteEvent::Error {
                            context: "write".into(),
                            message: "remote write failed".into(),
                        });
                    }
                }
            }
        }

        let _ = tx.send(RemoteEvent::Disconnected);
    });

    RemoteConnection {
        id,
        profile,
        status: ConnectionStatus::Connecting,
        event_rx,
        request_tx,
        dir_cache: HashMap::new(),
    }
}

// ─── Remote explorer UI state ─────────────────────────────────────────────────

pub struct RemoteExplorerState {
    pub profiles: Vec<SshProfile>,
    pub connections: HashMap<RemoteConnectionId, RemoteConnection>,
    pub show: bool,
    pub show_add_profile: bool,
    pub edit_profile: SshProfile,
    /// Last error message.
    pub error_message: Option<String>,
    /// Pending file read: (connection_id, remote_path).
    pub pending_open: Option<(RemoteConnectionId, String)>,
}

impl Default for RemoteExplorerState {
    fn default() -> Self {
        Self {
            profiles: Vec::new(),
            connections: HashMap::new(),
            show: false,
            show_add_profile: false,
            edit_profile: SshProfile::default(),
            error_message: None,
            pending_open: None,
        }
    }
}

impl RemoteExplorerState {
    pub fn poll(&mut self) {
        for conn in self.connections.values_mut() {
            conn.poll();
        }
        // Surface errors.
        for conn in self.connections.values() {
            if conn.status == ConnectionStatus::Error {
                self.error_message = Some(format!(
                    "SSH connection error: {}",
                    conn.profile.label
                ));
            }
        }
    }

    pub fn connect_profile(&mut self, profile_id: RemoteConnectionId) {
        if self.connections.contains_key(&profile_id) {
            return;
        }
        if let Some(profile) = self.profiles.iter().find(|p| p.id == profile_id).cloned() {
            let conn = connect(profile, None);
            self.connections.insert(profile_id, conn);
        }
    }

    pub fn disconnect_profile(&mut self, profile_id: RemoteConnectionId) {
        if let Some(conn) = self.connections.get(&profile_id) {
            conn.send(RemoteRequest::Disconnect);
        }
        self.connections.remove(&profile_id);
    }
}

/// Render the remote explorer sidebar section.
/// Returns `Some((connection_id, remote_path))` when the user clicks a file.
pub fn render_remote_explorer(
    ui: &mut egui::Ui,
    state: &mut RemoteExplorerState,
) -> Option<(RemoteConnectionId, String)> {
    let mut open_file = None;

    ui.horizontal(|ui| {
        ui.strong("Remote");
        if ui.small_button("+ Add SSH").clicked() {
            state.show_add_profile = true;
            state.edit_profile = SshProfile::default();
        }
    });

    for profile in &state.profiles.clone() {
        let connected = state
            .connections
            .get(&profile.id)
            .map(|c| c.status == ConnectionStatus::Connected)
            .unwrap_or(false);
        let connecting = state
            .connections
            .get(&profile.id)
            .map(|c| c.status == ConnectionStatus::Connecting)
            .unwrap_or(false);

        ui.horizontal(|ui| {
            let label = if connected {
                format!("🔗 {}", profile.label)
            } else {
                format!("○ {}", profile.label)
            };
            ui.label(&label);
            if connecting {
                ui.spinner();
            }
            if connected {
                if ui.small_button("✖").clicked() {
                    state.disconnect_profile(profile.id);
                }
            } else if ui.small_button("Connect").clicked() {
                state.connect_profile(profile.id);
            }
        });

        if connected {
            if let Some(conn) = state.connections.get(&profile.id) {
                let root = profile.remote_root.clone();
                let entries = conn.dir_cache.get(&root).cloned().unwrap_or_default();
                if entries.is_empty() {
                    // Request a listing.
                    conn.send(RemoteRequest::ListDir { path: root.clone() });
                    ui.indent("remote_dir", |ui| {
                        ui.spinner();
                    });
                } else {
                    ui.indent("remote_dir", |ui| {
                        for entry in &entries {
                            if !entry.is_dir {
                                let path = format!("{}/{}", root.trim_end_matches('/'), entry.name);
                                if ui.selectable_label(false, &entry.name).clicked() {
                                    open_file = Some((profile.id, path));
                                }
                            }
                        }
                    });
                }
            }
        }
    }

    if let Some(ref msg) = state.error_message.clone() {
        ui.colored_label(egui::Color32::from_rgb(220, 80, 80), msg);
    }

    // Add-profile form.
    if state.show_add_profile {
        egui::Window::new("Add SSH Connection")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_size([380.0, 280.0])
            .show(ui.ctx(), |ui| {
                egui::Grid::new("ssh_profile_grid")
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Label:");
                        ui.text_edit_singleline(&mut state.edit_profile.label);
                        ui.end_row();
                        ui.label("Host:");
                        ui.text_edit_singleline(&mut state.edit_profile.host);
                        ui.end_row();
                        ui.label("Port:");
                        let mut port_str = state.edit_profile.port.to_string();
                        ui.text_edit_singleline(&mut port_str);
                        if let Ok(p) = port_str.parse::<u16>() {
                            state.edit_profile.port = p;
                        }
                        ui.end_row();
                        ui.label("Username:");
                        ui.text_edit_singleline(&mut state.edit_profile.username);
                        ui.end_row();
                        ui.label("Remote root:");
                        ui.text_edit_singleline(&mut state.edit_profile.remote_root);
                        ui.end_row();
                    });

                ui.horizontal(|ui| {
                    let valid = !state.edit_profile.host.is_empty()
                        && !state.edit_profile.username.is_empty();
                    if ui
                        .add_enabled(valid, egui::Button::new("Add"))
                        .clicked()
                    {
                        let p = state.edit_profile.clone();
                        state.profiles.push(p);
                        state.show_add_profile = false;
                    }
                    if ui.button("Cancel").clicked() {
                        state.show_add_profile = false;
                    }
                });
            });
    }

    open_file
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_location_display() {
        let local = FileLocation::Local(PathBuf::from("/home/user/main.rs"));
        assert!(local.to_string().contains("main.rs"));

        let remote = FileLocation::Remote(RemoteFileId {
            connection_id: RemoteConnectionId(1),
            path: "/srv/app/main.rs".into(),
        });
        assert!(remote.to_string().contains("main.rs"));
    }

    #[test]
    fn ssh_profile_default_port_is_22() {
        let p = SshProfile::default();
        assert_eq!(p.port, 22);
    }

    #[test]
    fn remote_explorer_profile_list_is_initially_empty() {
        let state = RemoteExplorerState::default();
        assert!(state.profiles.is_empty());
        assert!(state.connections.is_empty());
    }
}
