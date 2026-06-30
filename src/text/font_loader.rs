//! Font loading utilities for ligature-capable fonts.
//!
//! Provides functionality to load coding ligature fonts (FiraCode, JetBrainsMono)
//! and register them with egui as a named font family "ligature_code".

use egui::{FontData, FontDefinitions, FontFamily};
use std::path::PathBuf;

/// Loads a ligature-capable font and registers it as "ligature_code" family.
/// Falls back to system monospace if the preferred font is not available.
pub fn load_ligature_font(font_defs: &mut FontDefinitions) {
    // Try to load FiraCode first, then JetBrainsMono, then fall back to system monospace
    let font_data = load_fira_code()
        .or_else(load_jetbrains_mono)
        .or_else(load_system_monospace);

    if let Some(data) = font_data {
        font_defs.font_data.insert("ligature_code".to_owned(), data);
        font_defs
            .families
            .entry(FontFamily::Name("ligature_code".into()))
            .or_default()
            .insert(0, "ligature_code".to_owned());
    }
}

fn get_user_font_dir() -> Option<PathBuf> {
    let base = directories::BaseDirs::new()?;
    #[cfg(target_os = "windows")]
    {
        Some(base.data_local_dir().join("Microsoft\\Windows\\Fonts"))
    }
    #[cfg(target_os = "macos")]
    {
        Some(base.home_dir().join("Library/Fonts"))
    }
    #[cfg(target_os = "linux")]
    {
        Some(base.data_local_dir().join("fonts"))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// Attempts to load FiraCode-Regular from embedded bytes.
fn load_fira_code() -> Option<FontData> {
    let font_bytes = include_bytes!("../FiraCode-Regular.ttf");
    Some(FontData::from_owned(font_bytes.to_vec()))
}

/// Attempts to load JetBrainsMono-Regular from common system locations.
fn load_jetbrains_mono() -> Option<FontData> {
    let mut paths = vec![
        PathBuf::from("/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf"),
        PathBuf::from("/usr/local/share/fonts/JetBrainsMono-Regular.ttf"),
        PathBuf::from("/Library/Fonts/JetBrainsMono-Regular.ttf"),
        PathBuf::from("/System/Library/Fonts/JetBrainsMono-Regular.ttf"),
        PathBuf::from("C:\\Windows\\Fonts\\JetBrainsMono-Regular.ttf"),
    ];
    if let Some(font_dir) = get_user_font_dir() {
        paths.push(font_dir.join("JetBrainsMono-Regular.ttf"));
    }

    for path in paths {
        if let Ok(bytes) = std::fs::read(&path) {
            return Some(FontData::from_owned(bytes));
        }
    }
    None
}

/// Falls back to loading the system default monospace font.
fn load_system_monospace() -> Option<FontData> {
    #[cfg(target_os = "linux")]
    let paths = [
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/liberation2/LiberationMono-Regular.ttf",
        "/usr/share/fonts/truetype/freefont/FreeMono.ttf",
    ];
    #[cfg(target_os = "macos")]
    let paths = [
        "/System/Library/Fonts/Menlo.ttc",
        "/System/Library/Fonts/Monaco.dfont",
        "/Library/Fonts/Courier New.ttf",
    ];
    #[cfg(target_os = "windows")]
    let paths = [
        "C:\\Windows\\Fonts\\consola.ttf",
        "C:\\Windows\\Fonts\\cour.ttf",
        "C:\\Windows\\Fonts\\lucon.ttf",
    ];
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let paths = [] as [&str; 0];

    for path in paths {
        if let Ok(bytes) = std::fs::read(path) {
            return Some(FontData::from_owned(bytes));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::FontDefinitions;

    #[test]
    fn load_ligature_font_does_not_panic() {
        let mut font_defs = FontDefinitions::default();
        load_ligature_font(&mut font_defs);
        // Should not panic regardless of font availability
    }
}