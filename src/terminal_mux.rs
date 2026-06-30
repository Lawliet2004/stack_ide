//! Terminal multiplexer: manages multiple named terminal sessions.
//!
//! Each session wraps an existing `TerminalPane` with a user-visible name.
//! The multiplexer tracks the active session and provides session lifecycle.

use std::path::PathBuf;

use crate::terminal::{ShellKind, TerminalPane};

/// A uniquely identified terminal session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TerminalSessionId(pub usize);

/// One named terminal session.
pub struct TerminalSession {
    pub id: TerminalSessionId,
    pub name: String,
    pub pane: TerminalPane,
    pub cwd: Option<PathBuf>,
    pub shell: ShellKind,
}

impl TerminalSession {
    pub fn new(id: TerminalSessionId, name: String, cwd: Option<PathBuf>, shell: ShellKind) -> Self {
        let pane = TerminalPane::with_shell(cwd.clone(), shell);
        Self { id, name, pane, cwd, shell }
    }

    /// Default display title: custom name if set, else the shell label.
    pub fn display_name(&self) -> &str {
        if !self.name.is_empty() {
            &self.name
        } else {
            self.shell.label()
        }
    }
}

/// Manages multiple terminal sessions with an active selection.
#[derive(Default)]
pub struct TerminalMux {
    sessions: Vec<TerminalSession>,
    active: Option<usize>,    // index into `sessions`
    next_id: usize,
}

impl TerminalMux {
    pub fn new() -> Self {
        Self::default()
    }

    /// All sessions (ordered).
    pub fn sessions(&self) -> &[TerminalSession] {
        &self.sessions
    }

    /// Mutable reference to all sessions.
    pub fn sessions_mut(&mut self) -> &mut [TerminalSession] {
        &mut self.sessions
    }

    /// Index of the active session.
    pub fn active_index(&self) -> Option<usize> {
        self.active
    }

    /// The active session, if any.
    pub fn active_session(&self) -> Option<&TerminalSession> {
        self.active.and_then(|i| self.sessions.get(i))
    }

    /// The active session (mutable).
    pub fn active_session_mut(&mut self) -> Option<&mut TerminalSession> {
        let i = self.active?;
        self.sessions.get_mut(i)
    }

    /// Create a new terminal session and make it active.
    pub fn new_session(&mut self, cwd: Option<PathBuf>, shell: ShellKind) -> TerminalSessionId {
        let id = TerminalSessionId(self.next_id);
        self.next_id += 1;
        let name = format!("{}: {}", self.sessions.len() + 1, shell.label());
        let session = TerminalSession::new(id, name, cwd, shell);
        self.sessions.push(session);
        self.active = Some(self.sessions.len() - 1);
        id
    }

    /// Switch the active session to `index`.
    pub fn set_active(&mut self, index: usize) {
        if index < self.sessions.len() {
            self.active = Some(index);
        }
    }

    /// Close the session at `index`. If it was active, select the adjacent one.
    pub fn close(&mut self, index: usize) {
        if index >= self.sessions.len() {
            return;
        }
        self.sessions.remove(index);
        if self.sessions.is_empty() {
            self.active = None;
        } else {
            let new_index = index.min(self.sessions.len() - 1);
            self.active = Some(new_index);
        }
    }

    /// Rename a session by index.
    pub fn rename(&mut self, index: usize, new_name: String) {
        if let Some(s) = self.sessions.get_mut(index) {
            s.name = new_name;
        }
    }

    /// Poll all sessions (drain PTY output into their buffers).
    pub fn poll_all(&mut self) {
        for session in &mut self.sessions {
            session.pane.poll();
        }
    }

    /// Ensure at least one session exists.
    pub fn ensure_session(&mut self, cwd: Option<PathBuf>) {
        if self.sessions.is_empty() {
            self.new_session(cwd, ShellKind::default_shell());
        }
    }

    /// Number of sessions.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

/// Render the terminal multiplexer UI inside a parent `Ui`.
///
/// Shows a tab bar on the right and the active terminal in the main area.
pub fn render_mux(
    ui: &mut egui::Ui,
    mux: &mut TerminalMux,
    cwd: Option<&PathBuf>,
    palette: crate::theme::SemanticPalette,
    ligatures_enabled: bool,
    ligature_renderer: Option<&mut crate::text::ligature::LigatureRenderer>,
) {
    if mux.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label("No terminal open");
        });
        return;
    }

    // Right-hand session list panel
    egui::SidePanel::right("terminal_mux_sessions")
        .resizable(false)
        .exact_width(150.0)
        .show_inside(ui, |ui| {
            ui.add_space(4.0);
            egui::ScrollArea::vertical().show(ui, |ui| {
                let len = mux.sessions.len();
                let mut to_close: Option<usize> = None;
                let active = mux.active_index();
                for i in 0..len {
                    let name = mux.sessions[i].display_name().to_owned();
                    let selected = active == Some(i);
                    ui.horizontal(|ui| {
                        if ui.selectable_label(selected, &name).clicked() {
                            mux.set_active(i);
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("×").color(palette.muted_text),
                                )
                                .frame(false),
                            )
                            .on_hover_text("Close terminal")
                            .clicked()
                        {
                            to_close = Some(i);
                        }
                    });
                }
                if let Some(i) = to_close {
                    mux.close(i);
                }
            });

            ui.add_space(4.0);
            ui.separator();
            if ui.button("+ New").clicked() {
                mux.new_session(cwd.cloned(), ShellKind::default_shell());
            }
        });

    // Main terminal content
    egui::CentralPanel::default()
        .frame(egui::Frame::none())
        .show_inside(ui, |ui| {
            let active_idx = match mux.active_index() {
                Some(i) => i,
                None => return,
            };

            // Compute terminal size
            let panel_width = ui.available_width();
            let panel_height = ui.available_height();
            let font_id = egui::FontId::monospace(13.0);
            let char_width = ui.fonts(|f| f.glyph_width(&font_id, 'M')).max(1.0);
            let line_height = (ui.text_style_height(&egui::TextStyle::Monospace) + 2.0).max(1.0);
            let new_cols = ((panel_width / char_width) as u16).max(40);
            let new_rows = ((panel_height / line_height) as u16).max(2);

            let session = &mut mux.sessions[active_idx];
            session.pane.resize(new_rows, new_cols);

            let response = crate::terminal::renderer::render_terminal(
                ui,
                &mut session.pane.buffer,
                font_id,
                ligatures_enabled,
                ligature_renderer,
            );

            if response.has_focus() || ui.memory(|m| m.has_focus(ui.id())) {
                ui.input(|i| {
                    for event in &i.events {
                        match event {
                            egui::Event::Text(s) => {
                                session.pane.write_str(s);
                            }
                            egui::Event::Key { key, pressed: true, modifiers, .. } => {
                                handle_terminal_key_mux(&mut session.pane, *key, *modifiers);
                            }
                            _ => {}
                        }
                    }
                });
            }
        });
}

fn handle_terminal_key_mux(
    term: &mut TerminalPane,
    key: egui::Key,
    mods: egui::Modifiers,
) {
    if mods.ctrl {
        match key {
            egui::Key::C => { term.write(b"\x03"); return; }
            egui::Key::D => { term.write(b"\x04"); return; }
            egui::Key::L => { term.write(b"\x0c"); return; }
            egui::Key::Z => { term.write(b"\x1a"); return; }
            _ => {}
        }
    }
    let bytes: &[u8] = match key {
        egui::Key::Enter    => b"\r",
        egui::Key::Backspace => b"\x7f",
        egui::Key::Tab      => b"\t",
        egui::Key::Escape   => b"\x1b",
        egui::Key::ArrowUp  => b"\x1b[A",
        egui::Key::ArrowDown => b"\x1b[B",
        egui::Key::ArrowRight => b"\x1b[C",
        egui::Key::ArrowLeft  => b"\x1b[D",
        egui::Key::Home     => b"\x1b[H",
        egui::Key::End      => b"\x1b[F",
        egui::Key::PageUp   => b"\x1b[5~",
        egui::Key::PageDown => b"\x1b[6~",
        egui::Key::Delete   => b"\x1b[3~",
        _ => return,
    };
    term.write(bytes);
}
