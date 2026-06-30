pub mod keybinds;
pub mod theme;

use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub use keybinds::KeybindingsConfig;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub editor: EditorConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub lsp: LspConfig,
    #[serde(default)]
    pub git: GitConfig,
    #[serde(default)]
    pub terminal: TerminalConfig,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub keybindings: KeybindingsConfig,
    #[serde(default)]
    pub plugins: PluginsConfig,
}

macro_rules! config_struct {
    ($name:ident { $($field:ident: $ty:ty = $value:expr => $default:path),* $(,)? }) => {
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(default)]
        pub struct $name { $(pub $field: $ty),* }
        impl Default for $name { fn default() -> Self { Self { $($field: $value),* } } }
    };
}

#[allow(dead_code)]
fn default_font_size() -> f32 {
    14.0
}
#[allow(dead_code)]
fn default_font_family() -> String {
    "monospace".into()
}
#[allow(dead_code)]
fn default_line_height() -> f32 {
    1.4
}
#[allow(dead_code)]
fn default_tab_size() -> u32 {
    4
}
#[allow(dead_code)]
fn default_true() -> bool {
    true
}
#[allow(dead_code)]
fn default_false() -> bool {
    false
}
#[allow(dead_code)]
fn default_theme() -> String {
    "dark".into()
}
#[allow(dead_code)]
fn default_200() -> f32 {
    200.0
}
#[allow(dead_code)]
fn default_280() -> f32 {
    280.0
}
#[allow(dead_code)]
fn default_100() -> f32 {
    100.0
}
#[allow(dead_code)]
fn default_500() -> u64 {
    500
}
#[allow(dead_code)]
fn default_terminal_font_size() -> f32 {
    13.0
}
#[allow(dead_code)]
fn default_scrollback_lines() -> usize {
    10_000
}
#[allow(dead_code)]
fn default_empty() -> String {
    String::new()
}
#[allow(dead_code)]
fn default_vec() -> Vec<String> {
    Vec::new()
}
#[allow(dead_code)]
fn default_sticky_scroll_max_lines() -> usize {
    5
}
#[allow(dead_code)]
fn default_multi_cursor_modifier() -> String {
    "Alt".into()
}
#[allow(dead_code)]
fn default_indent_guide_color() -> String {
    "#2a2a2a".into()
}
#[allow(dead_code)]
fn default_indent_guide_active_color() -> String {
    "#404040".into()
}

config_struct!(EditorConfig {
 font_size:f32=14.0=>default_font_size, font_family:String="monospace".into()=>default_font_family, line_height:f32=1.4=>default_line_height,
 tab_size:u32=4=>default_tab_size, use_spaces:bool=true=>default_true, word_wrap:bool=false=>default_false, show_whitespace:bool=false=>default_false,
 highlight_line:bool=true=>default_true, scroll_past_end:bool=true=>default_true, auto_indent:bool=true=>default_true,
 auto_close_pairs:bool=true=>default_true, trim_trailing_whitespace:bool=true=>default_true
 ,show_indent_guides:bool=true=>default_true, sticky_scroll:bool=true=>default_true,
 bracket_colorization:bool=true=>default_true, bracket_matching:bool=true=>default_true,
 sticky_scroll_max_lines:usize=5=>default_sticky_scroll_max_lines,
 multi_cursor_modifier:String="Alt".into()=>default_multi_cursor_modifier,
 indent_guide_color:String="#2a2a2a".into()=>default_indent_guide_color,
 indent_guide_active_color:String="#404040".into()=>default_indent_guide_active_color
});
config_struct!(UiConfig {
 theme:String="dark".into()=>default_theme, show_minimap:bool=true=>default_true, show_file_tree:bool=true=>default_true,
 show_outline:bool=true=>default_true, show_breadcrumbs:bool=false=>default_false, show_status_bar:bool=true=>default_true,
 show_git_gutter:bool=true=>default_true, file_tree_width:f32=200.0=>default_200, outline_width:f32=200.0=>default_200,
 terminal_height:f32=280.0=>default_280, minimap_width:f32=100.0=>default_100
});
config_struct!(LspConfig { rust_analyzer_path:String=String::new()=>default_empty, enable_diagnostics:bool=true=>default_true,
 enable_completions:bool=true=>default_true, enable_hover:bool=true=>default_true, enable_inlay_hints:bool=false=>default_false,
 diagnostic_delay_ms:u64=500=>default_500 });
config_struct!(GitConfig { enabled:bool=true=>default_true, show_diff_gutter:bool=true=>default_true,
 show_blame_inline:bool=false=>default_false, auto_refresh:bool=true=>default_true });
config_struct!(TerminalConfig { shell:String=String::new()=>default_empty, font_size:f32=13.0=>default_terminal_font_size,
 scrollback_lines:usize=10_000=>default_scrollback_lines });
config_struct!(SearchConfig { case_sensitive:bool=false=>default_false, use_regex:bool=false=>default_false,
 respect_gitignore:bool=true=>default_true });
config_struct!(PluginsConfig { enabled:bool=true=>default_true, disabled_plugins:Vec<String>=Vec::new()=>default_vec });

#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    Parse(toml::de::Error),
}
impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::Parse(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for LoadError {}

pub fn config_path() -> io::Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|d| d.config_dir().join("blue-ide").join("config.toml"))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "configuration directory unavailable",
            )
        })
}

/// Loads existing configuration. A malformed file is returned as an error and is never changed.
/// A missing file is initialized with the complete default schema.
pub fn load_or_create(path: &Path) -> Result<Config, LoadError> {
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(LoadError::Parse),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let value = Config::default();
            save(path, &value).map_err(LoadError::Io)?;
            Ok(value)
        }
        Err(e) => Err(LoadError::Io(e)),
    }
}

pub fn save(path: &Path, config: &Config) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(config)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text)?;
    fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn path(name: &str) -> PathBuf {
        let thread_name = std::thread::current()
            .name()
            .unwrap_or("t")
            .replace(":", "_");
        std::env::temp_dir().join(format!(
            "blue_config_{}_{}_{}.toml",
            name,
            std::process::id(),
            thread_name
        ))
    }
    #[test]
    fn missing_file_is_created_with_complete_defaults() {
        let p = path("missing");
        let _ = fs::remove_file(&p);
        let c = load_or_create(&p).unwrap();
        assert_eq!(c, Config::default());
        let s = fs::read_to_string(&p).unwrap();
        assert!(s.contains("[editor]"));
        assert!(s.contains("[plugins]"));
        let _ = fs::remove_file(p);
    }
    #[test]
    fn malformed_file_is_not_overwritten() {
        let p = path("broken");
        fs::write(&p, "[editor\n").unwrap();
        assert!(matches!(load_or_create(&p), Err(LoadError::Parse(_))));
        assert_eq!(fs::read_to_string(&p).unwrap(), "[editor\n");
        let _ = fs::remove_file(p);
    }
    #[test]
    fn missing_nested_fields_use_defaults() {
        let c: Config = toml::from_str("[editor]\nfont_size=18.0").unwrap();
        assert_eq!(c.editor.font_size, 18.0);
        assert_eq!(c.editor.tab_size, 4);
        assert!(c.ui.show_minimap);
    }
}
