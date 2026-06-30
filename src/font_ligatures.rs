//! Font Ligature Support (Feature 1)
//!
//! Loads a ligature-capable font (FiraCode-Regular.ttf) using egui's FontDefinitions
//! and registers it as the named family "ligature_code".

use egui::{FontData, FontDefinitions, FontFamily};

/// Loads the embedded FiraCode font and adds it to the font definitions as "ligature_code".
pub fn load_font_ligatures(font_defs: &mut FontDefinitions) {
    let font_bytes = include_bytes!("FiraCode-Regular.ttf");
    let font_data = FontData::from_owned(font_bytes.to_vec());
    
    font_defs.font_data.insert("ligature_code".to_owned(), font_data);
    font_defs
        .families
        .entry(FontFamily::Name("ligature_code".into()))
        .or_default()
        .insert(0, "ligature_code".to_owned());
}
