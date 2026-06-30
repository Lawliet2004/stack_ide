//! `.editorconfig` support.
//!
//! Searches upward from a file path, collects all applicable `.editorconfig`
//! sections (stopping at `root = true`), and merges them into a single
//! `EditorConfigSettings` for the file. Settings closer to the file take
//! precedence over higher-level ones.

use std::path::Path;

/// Resolved per-file indentation style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndentStyle {
    Space,
    Tab,
}

/// Resolved line-ending convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
    Cr,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::CrLf => "CRLF",
            LineEnding::Cr => "CR",
        }
    }
}

/// Merged settings applicable to one file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EditorConfigSettings {
    pub indent_style: Option<IndentStyle>,
    pub indent_size: Option<usize>,
    pub tab_width: Option<usize>,
    pub end_of_line: Option<LineEnding>,
    pub charset: Option<String>,
    pub trim_trailing_whitespace: Option<bool>,
    pub insert_final_newline: Option<bool>,
}

impl EditorConfigSettings {
    /// The effective tab/indent width — `indent_size` wins, then `tab_width`.
    pub fn effective_indent_size(&self) -> Option<usize> {
        self.indent_size.or(self.tab_width)
    }

    /// Status-bar label: e.g. `"Spaces: 4 | LF | UTF-8"`.
    pub fn status_label(&self) -> String {
        let indent = match (self.indent_style, self.effective_indent_size()) {
            (Some(IndentStyle::Tab), Some(sz)) => format!("Tab ({sz})"),
            (Some(IndentStyle::Tab), None) => "Tab".to_string(),
            (_, Some(sz)) => format!("Spaces: {sz}"),
            _ => String::new(),
        };
        let eol = self
            .end_of_line
            .map(|e| e.as_str())
            .unwrap_or("LF")
            .to_string();
        let charset = self
            .charset
            .clone()
            .unwrap_or_else(|| "UTF-8".to_string())
            .to_uppercase();

        [indent, eol, charset]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

/// Load and resolve `.editorconfig` settings for `file_path`.
///
/// Walks upward from `file_path`'s directory. Closer rules win over higher ones.
/// Stops as soon as a file with `root = true` is found.
pub fn resolve(file_path: &Path) -> EditorConfigSettings {
    let mut layers: Vec<EditorConfigSettings> = Vec::new();

    let mut dir = match file_path.parent() {
        Some(d) => d.to_path_buf(),
        None => return EditorConfigSettings::default(),
    };

    loop {
        let ec_path = dir.join(".editorconfig");
        if ec_path.is_file() {
            if let Ok(text) = std::fs::read_to_string(&ec_path) {
                let rel = file_path
                    .strip_prefix(&dir)
                    .unwrap_or(file_path)
                    .to_string_lossy()
                    .replace('\\', "/");
                let (is_root, settings) = parse_for_file(&text, &rel);
                layers.push(settings);
                if is_root {
                    break;
                }
            }
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => break,
        }
    }

    // Merge: first layer (closest to the file) wins.
    let mut result = EditorConfigSettings::default();
    for layer in layers {
        if result.indent_style.is_none() {
            result.indent_style = layer.indent_style;
        }
        if result.indent_size.is_none() {
            result.indent_size = layer.indent_size;
        }
        if result.tab_width.is_none() {
            result.tab_width = layer.tab_width;
        }
        if result.end_of_line.is_none() {
            result.end_of_line = layer.end_of_line;
        }
        if result.charset.is_none() {
            result.charset = layer.charset;
        }
        if result.trim_trailing_whitespace.is_none() {
            result.trim_trailing_whitespace = layer.trim_trailing_whitespace;
        }
        if result.insert_final_newline.is_none() {
            result.insert_final_newline = layer.insert_final_newline;
        }
    }
    result
}

/// Parse one `.editorconfig` file and return `(is_root, settings_for_rel_path)`.
fn parse_for_file(text: &str, rel_path: &str) -> (bool, EditorConfigSettings) {
    let mut is_root = false;
    let mut settings = EditorConfigSettings::default();
    let mut in_matching_section = false;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            let pattern = &line[1..line.len() - 1];
            let matched = if !pattern.contains('/') {
                glob_matches(pattern, rel_path) || glob_matches(&format!("**/{pattern}"), rel_path)
            } else {
                glob_matches(pattern, rel_path)
            };
            in_matching_section = matched;
            continue;
        }

        if let Some((key, value)) = split_kv(line) {
            // Top-level `root` is not inside any section.
            if !in_matching_section && key == "root" {
                if value == "true" {
                    is_root = true;
                }
                continue;
            }

            if !in_matching_section {
                continue;
            }

            match key {
                "indent_style" => {
                    settings.indent_style = match value {
                        "space" => Some(IndentStyle::Space),
                        "tab" => Some(IndentStyle::Tab),
                        _ => None,
                    }
                }
                "indent_size" => {
                    if value == "tab" {
                        // "tab" means use the tab_width value
                        settings.indent_size = None;
                    } else {
                        settings.indent_size = value.parse().ok();
                    }
                }
                "tab_width" => {
                    settings.tab_width = value.parse().ok();
                }
                "end_of_line" => {
                    settings.end_of_line = match value {
                        "lf" => Some(LineEnding::Lf),
                        "crlf" => Some(LineEnding::CrLf),
                        "cr" => Some(LineEnding::Cr),
                        _ => None,
                    }
                }
                "charset" => {
                    settings.charset = Some(value.to_string());
                }
                "trim_trailing_whitespace" => {
                    settings.trim_trailing_whitespace = match value {
                        "true" => Some(true),
                        "false" => Some(false),
                        _ => None,
                    }
                }
                "insert_final_newline" => {
                    settings.insert_final_newline = match value {
                        "true" => Some(true),
                        "false" => Some(false),
                        _ => None,
                    }
                }
                _ => {}
            }
        }
    }

    (is_root, settings)
}

fn split_kv(line: &str) -> Option<(&str, &str)> {
    let eq = line.find('=')?;
    let key = line[..eq].trim();
    let value = line[eq + 1..].trim();
    // Strip inline comments.
    let value = value
        .split_once(" ;")
        .or_else(|| value.split_once(" #"))
        .map(|(v, _)| v.trim())
        .unwrap_or(value);
    if key.is_empty() {
        return None;
    }
    Some((key, value))
}

/// Minimal glob matcher supporting `*`, `**`, `?`, and `{a,b}` alternatives.
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    // Expand {a,b} alternatives.
    if let Some(start) = pattern.find('{') {
        if let Some(end) = pattern[start..].find('}') {
            let end = start + end;
            let before = &pattern[..start];
            let after = &pattern[end + 1..];
            let alts = &pattern[start + 1..end];
            return alts
                .split(',')
                .any(|alt| glob_matches(&format!("{before}{alt}{after}"), path));
        }
    }
    glob_match_inner(pattern, path)
}

fn glob_match_inner(pattern: &str, path: &str) -> bool {
    // Convert the pattern chars into a simple NFA-like match.
    let pat: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = path.chars().collect();
    glob_match_slices(&pat, &s)
}

fn glob_match_slices(pat: &[char], s: &[char]) -> bool {
    match (pat, s) {
        ([], []) => true,
        ([], _) => false,
        (['*', '*', rest @ ..], _) => {
            // `**` matches zero or more path segments.
            for i in 0..=s.len() {
                if glob_match_slices(rest, &s[i..]) {
                    return true;
                }
            }
            false
        }
        (['*', rest @ ..], _) => {
            // `*` matches anything except `/`.
            for i in 0..=s.len() {
                if s[..i].contains(&'/') {
                    break;
                }
                if glob_match_slices(rest, &s[i..]) {
                    return true;
                }
            }
            false
        }
        (['?', rest @ ..], [_, s_rest @ ..]) => glob_match_slices(rest, s_rest),
        ([p, rest @ ..], [c, s_rest @ ..]) if p == c => glob_match_slices(rest, s_rest),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_star_matches_single_segment() {
        assert!(glob_matches("*.rs", "main.rs"));
        assert!(!glob_matches("*.rs", "src/main.rs"));
    }

    #[test]
    fn glob_double_star_matches_nested_path() {
        assert!(glob_matches("**/*.rs", "src/main.rs"));
        assert!(glob_matches("**/*.rs", "a/b/c.rs"));
    }

    #[test]
    fn glob_braces_expand_alternatives() {
        assert!(glob_matches("*.{rs,toml}", "Cargo.toml"));
        assert!(glob_matches("*.{rs,toml}", "lib.rs"));
        assert!(!glob_matches("*.{rs,toml}", "main.py"));
    }

    #[test]
    fn parse_basic_editorconfig() {
        let text = "[*.rs]\nindent_style = space\nindent_size = 4\ninsert_final_newline = true\n";
        let (is_root, s) = parse_for_file(text, "src/main.rs");
        assert!(!is_root);
        assert_eq!(s.indent_style, Some(IndentStyle::Space));
        assert_eq!(s.indent_size, Some(4));
        assert_eq!(s.insert_final_newline, Some(true));
    }

    #[test]
    fn root_true_is_detected() {
        let text = "root = true\n[*]\nindent_style = tab\n";
        let (is_root, _) = parse_for_file(text, "file.txt");
        assert!(is_root);
    }

    #[test]
    fn non_matching_section_is_ignored() {
        let text = "[*.toml]\nindent_size = 2\n[*.rs]\nindent_size = 4\n";
        let (_, s) = parse_for_file(text, "src/lib.rs");
        assert_eq!(s.indent_size, Some(4));
    }

    #[test]
    fn status_label_formatting() {
        let s = EditorConfigSettings {
            indent_style: Some(IndentStyle::Space),
            indent_size: Some(4),
            end_of_line: Some(LineEnding::Lf),
            charset: Some("utf-8".to_string()),
            ..Default::default()
        };
        assert_eq!(s.status_label(), "Spaces: 4 | LF | UTF-8");
    }

    #[test]
    fn resolve_finds_config_in_temp_dir() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ec_test_{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".editorconfig"),
            "root = true\n[*.rs]\nindent_size = 2\n",
        )
        .unwrap();
        let result = resolve(&dir.join("main.rs"));
        assert_eq!(result.indent_size, Some(2));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
