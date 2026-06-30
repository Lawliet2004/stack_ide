//! Shared error placeholder renderer for all non-text pane content types.
//!
//! Shows a centered "⚠" with a title, detail, and an "Open as text" button.
//! Returns `true` when the "Open as text" button is clicked.

/// Render a consistent error state inside `ui` for a content pane that failed to load.
///
/// Returns `true` when the user clicks "Open as text".
pub fn render_content_error(ui: &mut egui::Ui, title: &str, detail: &str) -> bool {
    let mut open_as_text = false;

    ui.centered_and_justified(|ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() / 3.0);
            ui.label(
                egui::RichText::new("⚠")
                    .size(48.0)
                    .color(egui::Color32::from_rgb(255, 140, 0)),
            );
            ui.add_space(8.0);
            ui.label(egui::RichText::new(title).strong().size(16.0));
            if !detail.is_empty() {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(detail).size(13.0).weak());
            }
            ui.add_space(16.0);
            if ui.button("Open as text").clicked() {
                open_as_text = true;
            }
        });
    });

    open_as_text
}
