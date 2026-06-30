//! Keyboard-Only Navigation (Feature 5)
//!
//! Provides the draw_focus_outline helper to draw a visible 2px #005FFF
//! focus outline around whichever element is currently focused.

use egui::{Color32, Id, Rect, Stroke, Ui};

/// Draws a visible 2px #005FFF focus outline around a rect if the specified ID is focused.
pub fn draw_focus_outline(ui: &mut Ui, id: Id, rect: Rect) {
    if ui.memory(|m| m.has_focus(id)) {
        let outline_color = Color32::from_rgb(0x00, 0x5F, 0xFF); // #005FFF
        ui.painter().rect_stroke(
            rect,
            0.0,
            Stroke::new(2.0, outline_color),
        );
    }
}
