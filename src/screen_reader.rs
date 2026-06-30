//! Screen Reader Hints (Feature 4)
//!
//! Adds .on_hover_text(...) AND response.labelled_by(...) using
//! egui's AccessKit integration to interactive elements.

use egui::{Response, Ui};

/// Associates an interactive element response with hover text and an accessibility label.
/// Generates an invisible label containing the accessibility text to satisfy labelled_by.
///
/// The hidden label is added inside a detached child UI placed over the
/// element's own rect. `add_visible(false, ..)` still *allocates* layout space,
/// so adding it directly to `ui` would advance the cursor and inject phantom
/// gaps between rows (very visible in vertical lists like the file tree). A
/// child UI yields the accessibility id without disturbing the parent layout.
pub fn label_element(ui: &mut Ui, response: Response, hover_text: &str, a11y_label: &str) -> Response {
    let mut child = ui.child_ui(response.rect, *ui.layout());
    let label_response = child.add_visible(false, egui::Label::new(a11y_label));
    response.on_hover_text(hover_text).labelled_by(label_response.id)
}
