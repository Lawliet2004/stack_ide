//! Feature 3 — .editorconfig Support
//!
//! Re-exports and extends the existing `crate::editorconfig` implementation
//! with the additional behaviour required by the feature spec:
//! * Apply settings when a file is opened (tab width, indent style)
//! * Apply settings on save (EOL normalisation, trailing whitespace, final newline, charset)
//! * Status-bar label
//!
//! The heavy lifting (file parsing, upward walk, glob matching) is already
//! implemented in `src/editorconfig.rs`; this module is the integration layer
//! that lives under `src/workspace/`.

pub use crate::editorconfig::{
    resolve, EditorConfigSettings, IndentStyle, LineEnding,
};

use std::path::Path;

// ─── Apply on open ────────────────────────────────────────────────────────────

/// Settings to apply to the editor when a file is opened.
#[derive(Debug, Clone, Default)]
pub struct AppliedEditorConfig {
    /// Effective indent width (spaces or tab stop width).
    pub indent_size: Option<usize>,
    /// Whether to use spaces (`true`) or tabs (`false`).
    pub use_spaces: Option<bool>,
}

/// Resolve `.editorconfig` settings for `file_path` and return the subset
/// that should be applied immediately when the file opens.
pub fn settings_for_file(file_path: &Path) -> AppliedEditorConfig {
    let ec = resolve(file_path);
    AppliedEditorConfig {
        indent_size: ec.effective_indent_size(),
        use_spaces: ec.indent_style.map(|s| s == IndentStyle::Space),
    }
}

// ─── Apply on save ────────────────────────────────────────────────────────────

/// Transform file content according to the `.editorconfig` rules that apply
/// at save time: EOL normalisation, trailing-whitespace trimming, final newline.
///
/// Returns the (possibly modified) content as a `String`.
pub fn apply_on_save(content: &str, settings: &EditorConfigSettings) -> String {
    let mut result = content.to_owned();

    // 1. Trim trailing whitespace per line.
    if settings.trim_trailing_whitespace == Some(true) {
        // Preserve whether the content ends with a newline.
        let ends_with_newline = result.ends_with('\n');
        result = result
            .lines()
            .map(|line| line.trim_end())
            .collect::<Vec<_>>()
            .join("\n");
        if ends_with_newline && !result.ends_with('\n') {
            result.push('\n');
        }
    }

    // 2. Ensure exactly one trailing newline.
    if settings.insert_final_newline == Some(true) {
        if !result.ends_with('\n') {
            result.push('\n');
        }
        // Remove double trailing newlines.
        while result.ends_with("\n\n") {
            result.pop();
        }
    }

    // 3. Normalise line endings.
    if let Some(eol) = settings.end_of_line {
        // First normalise everything to \n, then convert.
        result = result.replace("\r\n", "\n").replace('\r', "\n");
        result = match eol {
            LineEnding::Lf => result,
            LineEnding::CrLf => result.replace('\n', "\r\n"),
            LineEnding::Cr => result.replace('\n', "\r"),
        };
    }

    result
}

// ─── Charset helpers ──────────────────────────────────────────────────────────

/// Returns `true` when the content should be saved with a UTF-8 BOM.
pub fn needs_bom(settings: &EditorConfigSettings) -> bool {
    settings
        .charset
        .as_deref()
        .map(|c| c.to_lowercase() == "utf-8-bom")
        .unwrap_or(false)
}

/// Prepend a UTF-8 BOM byte sequence to `content` if required.
pub fn encode_with_charset(content: &str, settings: &EditorConfigSettings) -> Vec<u8> {
    let mut bytes: Vec<u8> = Vec::with_capacity(content.len() + 3);
    if needs_bom(settings) {
        bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    bytes.extend_from_slice(content.as_bytes());
    bytes
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_with_eol(eol: LineEnding) -> EditorConfigSettings {
        EditorConfigSettings {
            end_of_line: Some(eol),
            ..Default::default()
        }
    }

    #[test]
    fn lf_normalises_crlf() {
        let s = settings_with_eol(LineEnding::Lf);
        let out = apply_on_save("a\r\nb\r\n", &s);
        assert_eq!(out, "a\nb\n");
    }

    #[test]
    fn crlf_converts_lf() {
        let s = settings_with_eol(LineEnding::CrLf);
        let out = apply_on_save("a\nb\n", &s);
        assert_eq!(out, "a\r\nb\r\n");
    }

    #[test]
    fn trim_trailing_whitespace() {
        let mut s = EditorConfigSettings::default();
        s.trim_trailing_whitespace = Some(true);
        let out = apply_on_save("hello   \nworld  \n", &s);
        assert_eq!(out, "hello\nworld\n");
    }

    #[test]
    fn insert_final_newline() {
        let mut s = EditorConfigSettings::default();
        s.insert_final_newline = Some(true);
        let out = apply_on_save("hello", &s);
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn bom_flag_detected() {
        let mut s = EditorConfigSettings::default();
        s.charset = Some("utf-8-bom".to_string());
        assert!(needs_bom(&s));
        let bytes = encode_with_charset("hi", &s);
        assert_eq!(&bytes[..3], &[0xEF, 0xBB, 0xBF]);
    }
}
