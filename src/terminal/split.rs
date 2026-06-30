//! Feature 2 — Terminal Split: side-by-side terminal panes.
//!
//! When `is_split` is `true` the terminal panel is divided into two equal-width
//! columns. Each column shows an independent session selected from the shared
//! `SessionManager`. A focused-pane indicator (1px #005FFF border) tracks the
//! last-clicked column.

/// Which of the two split panes is currently focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SplitFocus {
    #[default]
    Left,
    Right,
}

/// All state needed to manage the split-terminal layout.
pub struct SplitState {
    /// Whether the split view is currently active.
    pub is_split: bool,
    /// Session index shown in the left (or single) pane.
    pub left_session: usize,
    /// Session index shown in the right pane (only meaningful when `is_split`).
    pub right_session: usize,
    /// Which pane has keyboard focus.
    pub focused_pane: SplitFocus,
}

impl SplitState {
    pub fn new() -> Self {
        Self {
            is_split: false,
            left_session: 0,
            right_session: 0,
            focused_pane: SplitFocus::Left,
        }
    }

    /// Enable split view. If only one session exists the right pane reuses it.
    pub fn enable_split(&mut self, session_count: usize) {
        self.is_split = true;
        self.right_session = if session_count > 1 {
            self.left_session
                .wrapping_add(1)
                .min(session_count.saturating_sub(1))
        } else {
            0
        };
    }

    /// Merge back to a single pane, keeping the left session.
    pub fn disable_split(&mut self) {
        self.is_split = false;
        self.focused_pane = SplitFocus::Left;
    }

    /// The session index for the currently focused pane.
    pub fn focused_session_index(&self) -> usize {
        match self.focused_pane {
            SplitFocus::Left => self.left_session,
            SplitFocus::Right => {
                if self.is_split {
                    self.right_session
                } else {
                    self.left_session
                }
            }
        }
    }

    /// Clamp session indices after a session is removed.
    pub fn clamp_indices(&mut self, session_count: usize) {
        if session_count == 0 {
            self.left_session = 0;
            self.right_session = 0;
            return;
        }
        let max = session_count - 1;
        self.left_session = self.left_session.min(max);
        self.right_session = self.right_session.min(max);
    }
}

impl Default for SplitState {
    fn default() -> Self {
        Self::new()
    }
}

/// Render the "⊟ Split" / "Unsplit" toolbar buttons.
///
/// Returns `true` if split was toggled this frame.
pub fn render_split_buttons(ui: &mut egui::Ui, state: &mut SplitState, session_count: usize) -> bool {
    if state.is_split {
        if ui
            .add(egui::Button::new("⊟ Unsplit").frame(true))
            .on_hover_text("Merge back to a single terminal pane")
            .clicked()
        {
            state.disable_split();
            return true;
        }
    } else {
        if ui
            .add(egui::Button::new("⊟ Split").frame(true))
            .on_hover_text("Split terminal into two side-by-side panes")
            .clicked()
        {
            state.enable_split(session_count);
            return true;
        }
    }
    false
}

/// Render a session selector dropdown for a single pane.
///
/// Returns the newly selected index if the user changed it.
pub fn render_session_selector(
    ui: &mut egui::Ui,
    label: &str,
    current: usize,
    sessions: &[crate::terminal::session::TerminalSession],
    palette: crate::theme::SemanticPalette,
) -> Option<usize> {
    let mut selected: Option<usize> = None;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).small().color(palette.muted_text));
        let current_name = sessions
            .get(current)
            .map(|s| s.name.as_str())
            .unwrap_or("—");
        egui::ComboBox::from_id_source(label)
            .selected_text(current_name)
            .show_ui(ui, |ui| {
                for (i, s) in sessions.iter().enumerate() {
                    if ui.selectable_label(i == current, &s.name).clicked() {
                        selected = Some(i);
                    }
                }
            });
    });
    selected
}

/// Focal-blue colour used for the pane focus border.
pub const FOCUS_BORDER_COLOR: egui::Color32 = egui::Color32::from_rgb(0, 95, 255);

/// Draw a 1px focus border around `rect` using the painter.
pub fn draw_focus_border(ui: &egui::Ui, rect: egui::Rect) {
    ui.painter().rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0, FOCUS_BORDER_COLOR),
    );
}
