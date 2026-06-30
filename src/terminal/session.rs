//! Feature 1 — Terminal Multiplexer: named, persistent terminal sessions.
//!
//! Each `TerminalSession` wraps a PTY pair and its full scrollback history.
//! `SessionManager` owns the list, tracks the active session, and exposes
//! all session-lifecycle operations (create, close, rename, switch).


use std::path::PathBuf;

use crate::terminal::{ShellKind, TerminalPane};

/// Unique, stable identifier for a terminal session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(pub uuid::Uuid);

impl SessionId {
    pub fn new() -> Self {
        SessionId(uuid::Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

/// One named terminal session that remembers its full output history.
pub struct TerminalSession {
    pub id: SessionId,
    pub name: String,
    /// The live PTY-backed terminal pane.
    pub pane: TerminalPane,
    /// All lines ever printed to this session (never truncated).
    pub scrollback: Vec<String>,
    pub is_active: bool,
    /// Whether the name field is currently being edited inline.
    pub editing_name: bool,
    /// Scratch buffer for the inline name editor.
    pub name_edit_buf: String,
}

impl TerminalSession {
    pub fn new(
        name: String,
        cwd: Option<PathBuf>,
        shell: ShellKind,
        env_vars: &[(String, String)],
    ) -> Self {
        let pane = TerminalPane::with_shell_and_env(cwd, shell, env_vars);
        Self {
            id: SessionId::new(),
            name,
            pane,
            scrollback: Vec::new(),
            is_active: false,
            editing_name: false,
            name_edit_buf: String::new(),
        }
    }

    /// Write bytes to the PTY input.
    pub fn write(&mut self, data: &[u8]) {
        self.pane.write(data);
    }

    /// Write a string to the PTY input.
    pub fn write_str(&mut self, s: &str) {
        self.pane.write_str(s);
    }

    /// Poll PTY output into buffer and capture new scrollback lines.
    pub fn poll(&mut self) {
        self.pane.poll();
    }
}

/// Manages the ordered list of sessions and which one is active.
pub struct SessionManager {
    pub sessions: Vec<TerminalSession>,
    /// Index into `sessions` of the active one.
    pub active_index: Option<usize>,
    /// Counter for default session names ("Session 1", "Session 2", …).
    next_n: usize,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            active_index: None,
            next_n: 1,
        }
    }

    /// Create a new session and make it active. Returns its index.
    pub fn create_session(
        &mut self,
        cwd: Option<PathBuf>,
        shell: ShellKind,
        env_vars: &[(String, String)],
    ) -> usize {
        let name = format!("Session {}", self.next_n);
        self.next_n += 1;
        let mut session = TerminalSession::new(name, cwd, shell, env_vars);
        session.is_active = true;

        // Mark any previously active session as inactive.
        for s in &mut self.sessions {
            s.is_active = false;
        }

        self.sessions.push(session);
        let idx = self.sessions.len() - 1;
        self.active_index = Some(idx);
        idx
    }

    /// Ensure at least one session exists; creates default if empty.
    pub fn ensure_session(&mut self, cwd: Option<PathBuf>, env_vars: &[(String, String)]) {
        if self.sessions.is_empty() {
            self.create_session(cwd, ShellKind::default_shell(), env_vars);
        }
    }

    /// Switch the active session to the given index.
    pub fn set_active(&mut self, index: usize) {
        if index >= self.sessions.len() {
            return;
        }
        for (i, s) in self.sessions.iter_mut().enumerate() {
            s.is_active = i == index;
        }
        self.active_index = Some(index);
    }

    /// Close the session at `index`. Kills the PTY child if possible.
    /// Selects an adjacent session after removal.
    pub fn close_session(&mut self, index: usize) {
        if index >= self.sessions.len() {
            return;
        }
        // Drop the session (kills the pane and writer).
        self.sessions.remove(index);

        if self.sessions.is_empty() {
            self.active_index = None;
            return;
        }

        // Select the previous session, or the next one if there is no previous.
        let new_idx = if index > 0 { index - 1 } else { 0 };
        let new_idx = new_idx.min(self.sessions.len() - 1);
        self.set_active(new_idx);
    }

    /// Rename session at `index`.
    pub fn rename_session(&mut self, index: usize, new_name: String) {
        if let Some(s) = self.sessions.get_mut(index) {
            s.name = new_name;
        }
    }

    /// Borrow the active session immutably.
    pub fn active(&self) -> Option<&TerminalSession> {
        self.active_index.and_then(|i| self.sessions.get(i))
    }

    /// Borrow the active session mutably.
    pub fn active_mut(&mut self) -> Option<&mut TerminalSession> {
        let i = self.active_index?;
        self.sessions.get_mut(i)
    }

    /// Poll all sessions every frame.
    pub fn poll_all(&mut self) {
        for s in &mut self.sessions {
            s.poll();
        }
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Render the session tab bar inside a horizontal `ui`.
///
/// Returns an optional command describing what action should be taken after render:
/// `TabBarAction::SetActive(i)`, `TabBarAction::Close(i)`, `TabBarAction::New`.
pub enum TabBarAction {
    SetActive(usize),
    Close(usize),
    New,
}

pub fn render_session_tabs(
    ui: &mut egui::Ui,
    manager: &mut SessionManager,
    split_state: &mut crate::terminal::split::SplitState,
    palette: crate::theme::SemanticPalette,
) -> Option<TabBarAction> {
    let active_blue = palette.accent;
    let mut action: Option<TabBarAction> = None;

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.spacing_mut().button_padding = egui::vec2(8.0, 2.0);

        let n = manager.sessions.len();
        for i in 0..n {
            let session = &mut manager.sessions[i];
            let is_active = manager.active_index == Some(i);

            if session.editing_name {
                // Inline name editor
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut session.name_edit_buf)
                        .desired_width(90.0)
                        .font(egui::TextStyle::Small),
                );
                if resp.lost_focus() {
                    let key_enter = ui.input(|k| k.key_pressed(egui::Key::Enter));
                    let _key_esc = ui.input(|k| k.key_pressed(egui::Key::Escape));
                    if key_enter {
                        session.name = session.name_edit_buf.clone();
                    }
                    session.editing_name = false;
                    session.name_edit_buf.clear();
                }
                if ui.input(|k| k.key_pressed(egui::Key::Enter)) {
                    session.name = session.name_edit_buf.clone();
                    session.editing_name = false;
                    session.name_edit_buf.clear();
                }
                if ui.input(|k| k.key_pressed(egui::Key::Escape)) {
                    session.editing_name = false;
                    session.name_edit_buf.clear();
                }
            } else {
                // Render tab label
                let label_text = session.name.clone();
                let tab_resp = egui::Frame::none()
                    .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let text = egui::RichText::new(&label_text)
                                .color(if is_active {
                                    palette.primary_text
                                } else {
                                    palette.muted_text
                                })
                                .size(12.0);
                            let resp = ui.label(text);

                            // Close ×
                            let close_resp = ui.add(
                                egui::Button::new(
                                    egui::RichText::new("×")
                                        .size(11.0)
                                        .color(palette.muted_text),
                                )
                                .frame(false)
                                .min_size(egui::vec2(14.0, 14.0)),
                            );
                            (resp, close_resp)
                        })
                        .inner
                    });

                let (label_resp, close_resp) = tab_resp.inner;

                // Blue underline for active tab
                if is_active {
                    let tab_rect = tab_resp.response.rect;
                    ui.painter().line_segment(
                        [
                            egui::pos2(tab_rect.left(), tab_rect.bottom()),
                            egui::pos2(tab_rect.right(), tab_rect.bottom()),
                        ],
                        egui::Stroke::new(2.0, active_blue),
                    );
                }

                if close_resp.clicked() {
                    action = Some(TabBarAction::Close(i));
                } else if label_resp.double_clicked() {
                    // Start inline editing
                    let session = &mut manager.sessions[i];
                    session.name_edit_buf = session.name.clone();
                    session.editing_name = true;
                } else if label_resp.clicked() || tab_resp.response.clicked() {
                    action = Some(TabBarAction::SetActive(i));
                }
            }
        }

        // "+" button
        ui.add_space(4.0);
        if ui
            .add(
                egui::Button::new(egui::RichText::new("+").size(14.0).color(palette.muted_text))
                    .frame(false),
            )
            .on_hover_text("New session")
            .clicked()
        {
            action = Some(TabBarAction::New);
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            crate::terminal::split::render_split_buttons(ui, split_state, manager.sessions.len());
        });
    });

    action
}
