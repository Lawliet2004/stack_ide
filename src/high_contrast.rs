//! High Contrast Theme (Feature 3)
//!
//! Creates an egui::Visuals struct called high_contrast_theme()
//! meeting WCAG AA minimum 4.5:1 contrast ratio against background.

use egui::{Color32, Stroke, Visuals};

/// Creates the high contrast visuals.
pub fn high_contrast_theme() -> Visuals {
    let mut visuals = Visuals::dark();
    visuals.dark_mode = true;
    
    // Base colors
    let bg = Color32::from_rgb(0x00, 0x00, 0x00);           // #000000
    let text = Color32::from_rgb(0xFF, 0xFF, 0xFF);         // #FFFFFF
    let button_bg = Color32::from_rgb(0x1A, 0x1A, 0x1A);    // #1A1A1A
    let button_text = Color32::from_rgb(0xFF, 0xFF, 0x00);  // #FFFF00
    let selected_bg = Color32::from_rgb(0x00, 0x5F, 0xFF);  // #005FFF
    let selected_text = Color32::from_rgb(0xFF, 0xFF, 0xFF); // #FFFFFF
    let error_color = Color32::from_rgb(0xFF, 0x40, 0x40);  // #FF4040
    let warning_color = Color32::from_rgb(0xFF, 0xA5, 0x00); // #FFA500
    let border = Color32::from_rgb(0x44, 0x44, 0x44);
    
    visuals.override_text_color = Some(text);
    visuals.panel_fill = bg;
    visuals.window_fill = bg;
    visuals.extreme_bg_color = bg;
    visuals.faint_bg_color = button_bg;
    visuals.code_bg_color = bg;
    visuals.hyperlink_color = button_text;
    visuals.warn_fg_color = warning_color;
    visuals.error_fg_color = error_color;
    visuals.selection.bg_fill = selected_bg;
    visuals.selection.stroke = Stroke::new(1.0, selected_text);
    visuals.window_stroke = Stroke::new(1.0, border);
    visuals.widgets.noninteractive.bg_fill = bg;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, border);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, text);
    visuals.widgets.inactive.bg_fill = button_bg;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, border);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, button_text);
    visuals.widgets.hovered.bg_fill = selected_bg;
    visuals.widgets.hovered.bg_stroke = Stroke::NONE;
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.5, selected_text);
    visuals.widgets.active.bg_fill = selected_bg;
    visuals.widgets.active.bg_stroke = Stroke::NONE;
    visuals.widgets.active.fg_stroke = Stroke::new(1.5, selected_text);
    visuals.widgets.open.bg_fill = selected_bg;
    visuals.widgets.open.bg_stroke = Stroke::NONE;
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, selected_text);
    
    visuals
}
