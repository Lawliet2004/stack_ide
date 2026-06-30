//! Zen mode and distraction-free mode state management.
//!
//! Zen mode: hides all panels, centers the editor at max 800px, fades in/out.
//! Distraction-free mode: hides UI chrome (status bar, tab bar, line numbers,
//! gutter, minimap) but keeps full width and file tree accessible.
//!
//! Both modes are global IDE state and independent of each other. When both are
//! active, zen mode takes visual priority (it hides everything zen hides plus more).
//!
//! Chord shortcut state for Ctrl+K then Z is also tracked here.

use std::time::Instant;

/// Configuration options for zen / distraction-free mode.
#[derive(Debug, Clone)]
pub struct ZenModeConfig {
    /// Maximum content width in zen mode (pixels).
    pub max_width: f32,
    /// Whether to hide line numbers in zen mode.
    pub hide_line_numbers: bool,
    /// Transition duration in milliseconds.
    pub transition_ms: f32,
    /// Whether distraction-free mode enables typewriter scroll.
    pub typewriter_mode: bool,
}

impl Default for ZenModeConfig {
    fn default() -> Self {
        Self {
            max_width: 800.0,
            hide_line_numbers: true,
            transition_ms: 150.0,
            typewriter_mode: false,
        }
    }
}

/// State for the pending chord shortcut (Ctrl+K → Z).
#[derive(Debug, Clone)]
pub struct ChordState {
    pub key: egui::Key,
    pub modifiers: egui::Modifiers,
    pub started_at: Instant,
}

/// Combined zen/distraction-free/typewriter mode state stored on `BlueIdeApp`.
#[derive(Debug, Clone)]
pub struct ZenState {
    /// Whether zen mode is fully active.
    pub zen_mode: bool,
    /// Transition progress: 0.0 = panels fully visible, 1.0 = panels fully hidden.
    pub zen_transition_t: f32,
    /// Whether distraction-free mode is active (independent of zen mode).
    pub distraction_free: bool,
    /// Whether typewriter scroll is active (distraction-free sub-feature).
    pub typewriter_mode: bool,
    /// Pending chord: Some while waiting for the second key of a chord shortcut.
    pub chord_pending: Option<ChordState>,
    /// Configuration (max_width, transition_ms, etc.).
    pub config: ZenModeConfig,
}

impl Default for ZenState {
    fn default() -> Self {
        Self {
            zen_mode: false,
            zen_transition_t: 0.0,
            distraction_free: false,
            typewriter_mode: false,
            chord_pending: None,
            config: ZenModeConfig::default(),
        }
    }
}

/// Chord timeout: if the second key is not pressed within 1 second, cancel.
const CHORD_TIMEOUT_SECS: f32 = 1.0;

impl ZenState {
    /// Process keyboard input for zen mode (Ctrl+K then Z) and distraction-free
    /// mode (Ctrl+Shift+F11). Call this every frame before rendering.
    ///
    /// Returns `true` if any mode was toggled.
    pub fn handle_input(&mut self, ctx: &egui::Context, has_modal: bool) -> bool {
        if has_modal {
            self.chord_pending = None;
            return false;
        }

        let mut toggled = false;

        // Check chord timeout
        if let Some(ref chord) = self.chord_pending {
            if chord.started_at.elapsed().as_secs_f32() > CHORD_TIMEOUT_SECS {
                self.chord_pending = None;
            }
        }

        // Ctrl+Shift+F11 → distraction-free mode
        let ctrl_shift_f11 = ctx.input_mut(|i| {
            i.consume_key(
                egui::Modifiers {
                    ctrl: true,
                    shift: true,
                    ..egui::Modifiers::NONE
                },
                egui::Key::F11,
            )
        });
        if ctrl_shift_f11 {
            self.distraction_free = !self.distraction_free;
            toggled = true;
        }

        // Escape exits zen mode (when not blocked by a popup)
        let escape = ctx.input_mut(|i| {
            i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
        });
        if escape && self.zen_mode {
            self.zen_mode = false;
            toggled = true;
        }

        // Ctrl+K chord — arm the chord
        let ctrl_k = ctx.input_mut(|i| {
            i.consume_key(
                egui::Modifiers {
                    ctrl: true,
                    ..egui::Modifiers::NONE
                },
                egui::Key::K,
            )
        });
        if ctrl_k {
            self.chord_pending = Some(ChordState {
                key: egui::Key::K,
                modifiers: egui::Modifiers {
                    ctrl: true,
                    ..egui::Modifiers::NONE
                },
                started_at: Instant::now(),
            });
        }

        // Z after Ctrl+K → toggle zen mode
        if self.chord_pending.is_some() {
            let z_pressed = ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Z)
            });
            if z_pressed {
                self.chord_pending = None;
                self.zen_mode = !self.zen_mode;
                toggled = true;
            }
        }

        toggled
    }

    /// Advance the zen mode transition. Call every frame with `delta_time`.
    pub fn update_transition(&mut self, delta_time: f32) {
        let target = if self.zen_mode { 1.0_f32 } else { 0.0_f32 };
        if (self.zen_transition_t - target).abs() < 0.001 {
            self.zen_transition_t = target;
            return;
        }
        let speed = if self.config.transition_ms > 0.0 {
            delta_time * 1000.0 / self.config.transition_ms
        } else {
            1.0
        };
        if self.zen_mode {
            self.zen_transition_t = (self.zen_transition_t + speed).min(1.0);
        } else {
            self.zen_transition_t = (self.zen_transition_t - speed).max(0.0);
        }
    }

    /// Whether the file tree should be shown.
    pub fn show_file_tree(&self) -> bool {
        // zen mode hides it; distraction-free keeps it
        self.zen_transition_t < 0.5
    }

    /// Whether the status bar should be shown.
    pub fn show_status_bar(&self) -> bool {
        self.zen_transition_t < 0.5 && !self.distraction_free
    }

    /// Whether the tab bar should be shown.
    pub fn show_tab_bar(&self) -> bool {
        self.zen_transition_t < 0.5 && !self.distraction_free
    }

    /// Whether the breadcrumb bar should be shown.
    pub fn show_breadcrumb(&self) -> bool {
        // Breadcrumb is hidden in zen mode but kept in distraction-free
        self.zen_transition_t < 0.5
    }

    /// Whether the minimap should be shown.
    pub fn show_minimap(&self) -> bool {
        self.zen_transition_t < 0.5 && !self.distraction_free
    }

    /// Whether the gutter (line numbers, diff gutter, fold gutter) should be shown.
    pub fn show_gutter(&self) -> bool {
        if self.zen_mode || self.zen_transition_t > 0.5 {
            return !self.config.hide_line_numbers;
        }
        !self.distraction_free
    }

    /// Whether the bottom panels should be shown.
    pub fn show_bottom_panels(&self) -> bool {
        self.zen_transition_t < 0.5
    }

    /// Whether the menu bar should be shown.
    pub fn show_menu_bar(&self) -> bool {
        self.zen_transition_t < 0.5
    }

    /// Editor content width (for zen mode centering).
    pub fn editor_content_width(&self, pane_width: f32) -> f32 {
        if !self.zen_mode && self.zen_transition_t < 0.5 {
            return pane_width;
        }
        pane_width.min(self.config.max_width)
    }

    /// Opacity for panels (for smooth fade transition).
    pub fn panel_opacity(&self) -> f32 {
        1.0 - self.zen_transition_t
    }
}
