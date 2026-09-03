use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const SUPPORTED_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub recent_workspaces: Vec<PathBuf>,
    #[serde(default)]
    pub appearance: AppearanceSettings,
    #[serde(default)]
    pub editor: EditorSettings,
    #[serde(default)]
    pub folding: FoldingSettings,
    #[serde(default)]
    pub panels: PanelSettings,
    #[serde(default)]
    pub lsp: LspSettings,
    #[serde(default)]
    pub ui: UiSettings,
    #[serde(default)]
    pub assistant: AssistantSettings,
    #[serde(default)]
    pub debug: DebugSettings,
}

/// AI assistant panel configuration. The dependency-free provider pipes the
/// prompt (plus optional file/selection context) through a user-configured
/// shell command, e.g. `ollama run llama3.1` or any OpenAI-compatible CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantSettings {
    /// Shell command template. Supported placeholders: `{prompt}`, `{file}`,
    /// `{selection}`, `{language}`. Empty disables the provider.
    #[serde(default)]
    pub command: String,
}

impl Default for AssistantSettings {
    fn default() -> Self {
        Self {
            command: String::new(),
        }
    }
}

/// DAP/debugger configuration. The adapter command is user-supplied so no
/// specific debugger is hardcoded; the launch request is passed through as-is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DebugSettings {
    #[serde(default)]
    pub adapter_path: String,
    #[serde(default)]
    pub adapter_args: Vec<String>,
    #[serde(default)]
    pub launch_args: serde_json::Value,
}

impl Default for DebugSettings {
    fn default() -> Self {
        Self {
            adapter_path: String::new(),
            adapter_args: Vec::new(),
            launch_args: serde_json::Value::Null,
        }
    }
}

fn default_version() -> u32 {
    SUPPORTED_VERSION
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SUPPORTED_VERSION,
            recent_workspaces: Vec::new(),
            appearance: AppearanceSettings::default(),
            editor: EditorSettings::default(),
            folding: FoldingSettings::default(),
            panels: PanelSettings::default(),
            lsp: LspSettings::default(),
            ui: UiSettings::default(),
            assistant: AssistantSettings::default(),
            debug: DebugSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiSettings {
    #[serde(default = "default_false_setting")]
    pub show_breadcrumbs: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            show_breadcrumbs: default_false_setting(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FoldingSettings {
    #[serde(default)]
    pub collapsed_by_file: HashMap<String, Vec<usize>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppearanceSettings {
    #[serde(default = "default_theme")]
    pub theme: Theme,
    #[serde(default = "default_editor_font_size")]
    pub editor_font_size: f32,
    #[serde(default = "default_ui_scale")]
    pub ui_scale: f32,
    #[serde(default = "default_font_ligatures")]
    pub font_ligatures: bool,
    #[serde(default = "default_high_contrast")]
    pub high_contrast: bool,
}

fn default_high_contrast() -> bool {
    false
}

fn default_theme() -> Theme {
    Theme::Dark
}

fn default_editor_font_size() -> f32 {
    14.0
}

fn default_ui_scale() -> f32 {
    1.0
}

fn default_font_ligatures() -> bool {
    false
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            editor_font_size: default_editor_font_size(),
            ui_scale: default_ui_scale(),
            font_ligatures: default_font_ligatures(),
            high_contrast: default_high_contrast(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Dark,
    Light,
    System,
    Nord,
    Dracula,
    #[serde(rename = "solarized-dark")]
    SolarizedDark,
    #[serde(rename = "one-dark")]
    OneDark,
    #[serde(rename = "one-light")]
    OneLight,
    #[serde(rename = "ayu-dark")]
    AyuDark,
    #[serde(rename = "ayu-mirage")]
    AyuMirage,
    #[serde(rename = "ayu-light")]
    AyuLight,
    #[serde(rename = "gruvbox-dark")]
    GruvboxDark,
    #[serde(rename = "gruvbox-light")]
    GruvboxLight,
    #[serde(rename = "catppuccin-mocha")]
    CatppuccinMocha,
    #[serde(rename = "catppuccin-latte")]
    CatppuccinLatte,
}

impl Theme {
    pub const fn serialized_id(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::System => "system",
            Self::Nord => "nord",
            Self::Dracula => "dracula",
            Self::SolarizedDark => "solarized-dark",
            Self::OneDark => "one-dark",
            Self::OneLight => "one-light",
            Self::AyuDark => "ayu-dark",
            Self::AyuMirage => "ayu-mirage",
            Self::AyuLight => "ayu-light",
            Self::GruvboxDark => "gruvbox-dark",
            Self::GruvboxLight => "gruvbox-light",
            Self::CatppuccinMocha => "catppuccin-mocha",
            Self::CatppuccinLatte => "catppuccin-latte",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Dark => "Default Dark",
            Self::Light => "Default Light",
            Self::System => "System",
            Self::Nord => "Nord",
            Self::Dracula => "Dracula",
            Self::SolarizedDark => "Solarized Dark",
            Self::OneDark => "One Dark",
            Self::OneLight => "One Light",
            Self::AyuDark => "Ayu Dark",
            Self::AyuMirage => "Ayu Mirage",
            Self::AyuLight => "Ayu Light",
            Self::GruvboxDark => "Gruvbox Dark",
            Self::GruvboxLight => "Gruvbox Light",
            Self::CatppuccinMocha => "Catppuccin Mocha",
            Self::CatppuccinLatte => "Catppuccin Latte",
        }
    }

    pub const fn all() -> &'static [Self] {
        &[
            Self::System,
            Self::Dark,
            Self::Light,
            Self::OneDark,
            Self::OneLight,
            Self::AyuDark,
            Self::AyuMirage,
            Self::AyuLight,
            Self::GruvboxDark,
            Self::GruvboxLight,
            Self::CatppuccinMocha,
            Self::CatppuccinLatte,
            Self::Nord,
            Self::Dracula,
            Self::SolarizedDark,
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditorSettings {
    #[serde(default = "default_tab_width")]
    pub tab_width: u32,
    #[serde(default = "default_insert_spaces")]
    pub insert_spaces: bool,
    #[serde(default = "default_false_setting")]
    pub format_on_save: bool,
    #[serde(default)]
    pub inlay_hints: InlayHintSettings,
    #[serde(default = "default_true_setting")]
    pub show_indent_guides: bool,
    #[serde(default = "default_true_setting")]
    pub sticky_scroll: bool,
    #[serde(default = "default_sticky_scroll_max_lines")]
    pub sticky_scroll_max_lines: usize,
    #[serde(default = "default_true_setting")]
    pub bracket_colorization: bool,
    #[serde(default = "default_true_setting")]
    pub bracket_matching: bool,
    #[serde(default = "default_indent_guide_color")]
    pub indent_guide_color: String,
    #[serde(default = "default_indent_guide_active_color")]
    pub indent_guide_active_color: String,
    #[serde(default = "default_multi_cursor_modifier")]
    pub multi_cursor_modifier: String,
    // ─── Large file mode thresholds ───────────────────────────────────────────
    /// File size in KB at which a warning is shown (default 1 MB).
    #[serde(default = "default_large_file_warn_kb")]
    pub large_file_warn_kb: u64,
    /// File size in KB at which large file mode activates (default 5 MB).
    #[serde(default = "default_large_file_mode_kb")]
    pub large_file_mode_kb: u64,
    /// Line count at which a warning is shown (default 50 000).
    #[serde(default = "default_large_file_line_warn")]
    pub large_file_line_warn: usize,
    /// Line count at which large file mode activates (default 100 000).
    #[serde(default = "default_large_file_line_mode")]
    pub large_file_line_mode: usize,
    /// Vim/modal editing (Normal/Insert/Visual). Default off.
    #[serde(default)]
    pub vim_mode: bool,
    /// Auto-save mode: off, after_delay, focus_change.
    #[serde(default)]
    pub auto_save: AutoSaveMode,
    /// Auto-save delay in milliseconds for `after_delay` (default 500).
    #[serde(default = "default_auto_save_delay_ms")]
    pub auto_save_delay_ms: u64,
    /// Render diagnostic messages inline at the end of the line (Zed-style).
    #[serde(default = "default_inline_diagnostics")]
    pub inline_diagnostics: bool,
}

/// When the editor saves dirty buffers automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AutoSaveMode {
    /// Never save automatically (default; Ctrl+S still works).
    #[default]
    #[serde(rename = "off")]
    Off,
    /// Save a dirty buffer once it has been idle for `auto_save_delay_ms`.
    #[serde(rename = "after_delay")]
    AfterDelay,
    /// Save dirty buffers when the editor loses focus.
    #[serde(rename = "focus_change")]
    FocusChange,
}

impl AutoSaveMode {
    pub const fn serialized_id(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::AfterDelay => "after_delay",
            Self::FocusChange => "focus_change",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::AfterDelay => "After Delay",
            Self::FocusChange => "On Focus Change",
        }
    }

    pub const fn all() -> &'static [Self] {
        &[Self::Off, Self::AfterDelay, Self::FocusChange]
    }
}

fn default_auto_save_delay_ms() -> u64 {
    500
}

/// Settings controlling LSP inlay-hint display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InlayHintSettings {
    /// Whether inlay hints are enabled at all.
    #[serde(default = "default_inlay_hints_enabled")]
    pub enabled: bool,
    /// Show type hints (e.g. `: i32` after `let x`).
    #[serde(default = "default_true_setting")]
    pub show_type_hints: bool,
    /// Show parameter name hints (e.g. `width:` before `800`).
    #[serde(default = "default_true_setting")]
    pub show_parameter_hints: bool,
}

fn default_inlay_hints_enabled() -> bool {
    true
}

fn default_true_setting() -> bool {
    true
}
fn default_sticky_scroll_max_lines() -> usize {
    5
}
fn default_indent_guide_color() -> String {
    "#2a2a2a".into()
}
fn default_indent_guide_active_color() -> String {
    "#404040".into()
}
fn default_multi_cursor_modifier() -> String {
    "Alt".into()
}

fn default_large_file_warn_kb() -> u64 { 1024 }
fn default_large_file_mode_kb() -> u64 { 5120 }
fn default_large_file_line_warn() -> usize { 50_000 }
fn default_large_file_line_mode() -> usize { 100_000 }

impl Default for InlayHintSettings {
    fn default() -> Self {
        Self {
            enabled: default_inlay_hints_enabled(),
            show_type_hints: default_true_setting(),
            show_parameter_hints: default_true_setting(),
        }
    }
}

fn default_false_setting() -> bool {
    false
}

fn default_inline_diagnostics() -> bool {
    true
}

fn default_tab_width() -> u32 {
    4
}

fn default_insert_spaces() -> bool {
    true
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            tab_width: default_tab_width(),
            insert_spaces: default_insert_spaces(),
            format_on_save: false,
            inlay_hints: InlayHintSettings::default(),
            auto_save: AutoSaveMode::Off,
            auto_save_delay_ms: default_auto_save_delay_ms(),
            inline_diagnostics: true,
            vim_mode: false,
            show_indent_guides: true,
            sticky_scroll: true,
            sticky_scroll_max_lines: 5,
            bracket_colorization: true,
            bracket_matching: true,
            indent_guide_color: default_indent_guide_color(),
            indent_guide_active_color: default_indent_guide_active_color(),
            multi_cursor_modifier: default_multi_cursor_modifier(),
            large_file_warn_kb: default_large_file_warn_kb(),
            large_file_mode_kb: default_large_file_mode_kb(),
            large_file_line_warn: default_large_file_line_warn(),
            large_file_line_mode: default_large_file_line_mode(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PanelSettings {
    #[serde(default)]
    pub show_file_tree: bool,
    #[serde(default = "default_show_problems")]
    pub show_problems: bool,
    #[serde(default)]
    pub show_terminal: bool,
    #[serde(default)]
    pub outline_panel_width: Option<f32>,
}

fn default_show_problems() -> bool {
    false
}

impl Default for PanelSettings {
    fn default() -> Self {
        Self {
            show_file_tree: false,
            show_problems: default_show_problems(),
            show_terminal: false,
            outline_panel_width: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LspSettings {
    #[serde(default)]
    pub rust: RustLspSettings,
    #[serde(default)]
    pub python: PythonLspSettings,
    #[serde(default)]
    pub typescript: TypeScriptLspSettings,
}

impl LspSettings {
    pub fn is_enabled(&self, server_id: crate::language::LanguageServerId) -> bool {
        match server_id {
            crate::language::LanguageServerId::Rust => self.rust.enabled,
            crate::language::LanguageServerId::Python => self.python.enabled,
            crate::language::LanguageServerId::TypeScript => self.typescript.enabled,
        }
    }

    pub fn server_config(
        &self,
        server_id: crate::language::LanguageServerId,
    ) -> Option<(String, Vec<String>)> {
        match server_id {
            crate::language::LanguageServerId::Rust => {
                if self.rust.enabled {
                    Some((self.rust.command.clone(), self.rust.args.clone()))
                } else {
                    None
                }
            }
            crate::language::LanguageServerId::Python => {
                if self.python.enabled {
                    Some((self.python.command.clone(), self.python.args.clone()))
                } else {
                    None
                }
            }
            crate::language::LanguageServerId::TypeScript => {
                if self.typescript.enabled {
                    Some((
                        self.typescript.command.clone(),
                        self.typescript.args.clone(),
                    ))
                } else {
                    None
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RustLspSettings {
    #[serde(default = "default_rust_enabled")]
    pub enabled: bool,
    #[serde(default = "default_rust_command")]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

fn default_rust_enabled() -> bool {
    true
}

fn default_rust_command() -> String {
    "rust-analyzer".to_owned()
}

impl Default for RustLspSettings {
    fn default() -> Self {
        Self {
            enabled: default_rust_enabled(),
            command: default_rust_command(),
            args: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PythonLspSettings {
    #[serde(default = "default_python_enabled")]
    pub enabled: bool,
    #[serde(default = "default_python_command")]
    pub command: String,
    #[serde(default = "default_python_args")]
    pub args: Vec<String>,
}

fn default_python_enabled() -> bool {
    true
}

fn default_python_command() -> String {
    "pyright-langserver".to_owned()
}

fn default_python_args() -> Vec<String> {
    vec!["--stdio".to_owned()]
}

impl Default for PythonLspSettings {
    fn default() -> Self {
        Self {
            enabled: default_python_enabled(),
            command: default_python_command(),
            args: default_python_args(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypeScriptLspSettings {
    #[serde(default = "default_typescript_enabled")]
    pub enabled: bool,
    #[serde(default = "default_typescript_command")]
    pub command: String,
    #[serde(default = "default_typescript_args")]
    pub args: Vec<String>,
}

fn default_typescript_enabled() -> bool {
    true
}

fn default_typescript_command() -> String {
    "typescript-language-server".to_owned()
}

fn default_typescript_args() -> Vec<String> {
    vec!["--stdio".to_owned()]
}

impl Default for TypeScriptLspSettings {
    fn default() -> Self {
        Self {
            enabled: default_typescript_enabled(),
            command: default_typescript_command(),
            args: default_typescript_args(),
        }
    }
}

#[derive(Debug)]
pub enum SettingsError {
    Io(io::Error),
    Parse(toml::de::Error),
    Validation(ValidationErrors),
    UnsupportedVersion(u32),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {}", e),
            Self::Parse(e) => write!(f, "TOML parse error: {}", e),
            Self::Validation(e) => write!(f, "Validation error: {}", e),
            Self::UnsupportedVersion(v) => {
                write!(
                    f,
                    "Unsupported version {}; expected {}",
                    v, SUPPORTED_VERSION
                )
            }
        }
    }
}

impl std::error::Error for SettingsError {}

impl From<io::Error> for SettingsError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<toml::de::Error> for SettingsError {
    fn from(e: toml::de::Error) -> Self {
        Self::Parse(e)
    }
}

impl From<ValidationErrors> for SettingsError {
    fn from(e: ValidationErrors) -> Self {
        Self::Validation(e)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidationErrors {
    pub errors: Vec<String>,
}

impl fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, error) in self.errors.iter().enumerate() {
            if i > 0 {
                write!(f, "; ")?;
            }
            write!(f, "{}", error)?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

impl Settings {
    pub fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = Vec::new();

        if self.version != SUPPORTED_VERSION {
            errors.push(format!(
                "version must be {}; found {}",
                SUPPORTED_VERSION, self.version
            ));
        }

        if !self.appearance.editor_font_size.is_finite() {
            errors.push("appearance.editor_font_size must be finite".to_owned());
        } else if !(8.0..=48.0).contains(&self.appearance.editor_font_size) {
            errors.push(format!(
                "appearance.editor_font_size must be between 8.0 and 48.0; found {}",
                self.appearance.editor_font_size
            ));
        }

        if !self.appearance.ui_scale.is_finite() {
            errors.push("appearance.ui_scale must be finite".to_owned());
        } else if !(0.5..=3.0).contains(&self.appearance.ui_scale) {
            errors.push(format!(
                "appearance.ui_scale must be between 0.5 and 3.0; found {}",
                self.appearance.ui_scale
            ));
        }

        if !(1..=16).contains(&self.editor.tab_width) {
            errors.push(format!(
                "editor.tab_width must be between 1 and 16; found {}",
                self.editor.tab_width
            ));
        }

        if self.lsp.rust.enabled && self.lsp.rust.command.trim().is_empty() {
            errors.push("lsp.rust.command must not be empty when Rust LSP is enabled".to_owned());
        }
        for (i, arg) in self.lsp.rust.args.iter().enumerate() {
            if arg.contains('\0') {
                errors.push(format!("lsp.rust.args[{}] contains NUL character", i));
            }
        }

        if self.lsp.python.enabled && self.lsp.python.command.trim().is_empty() {
            errors
                .push("lsp.python.command must not be empty when Python LSP is enabled".to_owned());
        }
        for (i, arg) in self.lsp.python.args.iter().enumerate() {
            if arg.contains('\0') {
                errors.push(format!("lsp.python.args[{}] contains NUL character", i));
            }
        }

        if self.lsp.typescript.enabled && self.lsp.typescript.command.trim().is_empty() {
            errors.push(
                "lsp.typescript.command must not be empty when TypeScript LSP is enabled"
                    .to_owned(),
            );
        }
        for (i, arg) in self.lsp.typescript.args.iter().enumerate() {
            if arg.contains('\0') {
                errors.push(format!("lsp.typescript.args[{}] contains NUL character", i));
            }
        }

        if self.assistant.command.contains('\0') {
            errors.push("assistant.command contains NUL character".to_owned());
        }

        if self.editor.auto_save == AutoSaveMode::AfterDelay
            && !(50..=3_600_000).contains(&self.editor.auto_save_delay_ms)
        {
            errors.push(format!(
                "editor.auto_save_delay_ms must be between 50 and 3600000 when auto_save is after_delay; found {}",
                self.editor.auto_save_delay_ms
            ));
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(ValidationErrors { errors })
        }
    }
}

/// Storage and persistence for settings
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    /// Discover the platform-appropriate configuration file path
    pub fn discover() -> Result<Self, SettingsError> {
        let config_dir = directories::ProjectDirs::from("com", "BlueIDE", "Blue IDE")
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "could not determine configuration directory",
                )
            })?
            .config_dir()
            .to_path_buf();

        Ok(Self {
            path: config_dir.join("settings.toml"),
        })
    }

    /// Create a store at an explicit path (for testing)
    pub fn at_path(path: PathBuf) -> Self {
        Self { path }
    }

    /// Get the settings file path
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load settings from disk, or return defaults if the file doesn't exist
    pub fn load(&self) -> Result<Settings, SettingsError> {
        match fs::read_to_string(&self.path) {
            Ok(content) => {
                let settings: Settings = toml::from_str(&content)?;
                if settings.version != SUPPORTED_VERSION {
                    return Err(SettingsError::UnsupportedVersion(settings.version));
                }
                settings.validate()?;
                Ok(settings)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Save settings to disk atomically
    pub fn save(&self, settings: &Settings) -> Result<(), SettingsError> {
        settings.validate()?;

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = toml::to_string_pretty(settings)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        let temp_path = self.path.with_extension("toml.tmp");
        let write_result = fs::write(&temp_path, content);

        match write_result {
            Ok(()) => {
                fs::rename(&temp_path, &self.path)?;
                Ok(())
            }
            Err(e) => {
                let _ = fs::remove_file(&temp_path);
                Err(e.into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_settings_path() -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("blue_ide_settings_test_{}.toml", unique))
    }

    #[test]
    fn default_settings_are_valid() {
        let settings = Settings::default();
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn default_settings_match_schema() {
        let settings = Settings::default();
        assert_eq!(settings.version, 1);
        assert_eq!(settings.appearance.theme, Theme::Dark);
        assert_eq!(settings.appearance.editor_font_size, 14.0);
        assert_eq!(settings.appearance.ui_scale, 1.0);
        assert_eq!(settings.editor.tab_width, 4);
        assert!(settings.editor.insert_spaces);
        assert!(!settings.panels.show_file_tree);
        assert!(!settings.panels.show_problems);
        assert!(!settings.panels.show_terminal);
        assert!(settings.lsp.rust.enabled);
        assert_eq!(settings.lsp.rust.command, "rust-analyzer");
        assert!(settings.lsp.rust.args.is_empty());
    }

    #[test]
    fn empty_toml_inherits_defaults() {
        let toml = "";
        let settings: Settings = toml::from_str(toml).unwrap();
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn minimal_toml_inherits_nested_defaults() {
        let toml = "version = 1";
        let settings: Settings = toml::from_str(toml).unwrap();
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn missing_inline_diagnostics_defaults_to_enabled() {
        // A settings file written before the field existed must default it to
        // the same value as the in-memory EditorSettings::default().
        let toml = "[editor]\n";
        let settings: Settings = toml::from_str(toml).unwrap();
        assert_eq!(
            settings.editor.inline_diagnostics,
            Settings::default().editor.inline_diagnostics
        );
        assert!(settings.editor.inline_diagnostics);
    }

    #[test]
    fn complete_valid_toml_parses() {
        let toml = r#"
version = 1

[appearance]
theme = "light"
editor_font_size = 16.0
ui_scale = 1.2

[editor]
tab_width = 2
insert_spaces = false

[panels]
show_file_tree = true
show_problems = false

[lsp.rust]
enabled = false
command = "rust-analyzer"
args = ["--log-file", "/tmp/ra.log"]
"#;
        let settings: Settings = toml::from_str(toml).unwrap();
        assert!(settings.validate().is_ok());
        assert_eq!(settings.appearance.theme, Theme::Light);
        assert_eq!(settings.appearance.editor_font_size, 16.0);
        assert_eq!(settings.editor.tab_width, 2);
        assert!(!settings.lsp.rust.enabled);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let toml = r#"
version = 1
unknown_field = "ignored"

[appearance]
theme = "dark"
future_feature = 42
"#;
        let settings: Settings = toml::from_str(toml).unwrap();
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn invalid_toml_returns_parse_error() {
        let toml = "version = ";
        let result: Result<Settings, _> = toml::from_str(toml);
        assert!(result.is_err());
    }

    #[test]
    fn unsupported_version_rejected() {
        let toml = "version = 999";
        let settings: Settings = toml::from_str(toml).unwrap();
        let err = settings.validate().unwrap_err();
        assert!(err.errors[0].contains("version must be 1"));
    }

    #[test]
    fn font_size_boundaries() {
        let mut settings = Settings::default();

        settings.appearance.editor_font_size = 8.0;
        assert!(settings.validate().is_ok());

        settings.appearance.editor_font_size = 48.0;
        assert!(settings.validate().is_ok());

        settings.appearance.editor_font_size = 7.9;
        assert!(settings.validate().is_err());

        settings.appearance.editor_font_size = 48.1;
        assert!(settings.validate().is_err());

        settings.appearance.editor_font_size = f32::NAN;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn ui_scale_boundaries() {
        let mut settings = Settings::default();

        settings.appearance.ui_scale = 0.5;
        assert!(settings.validate().is_ok());

        settings.appearance.ui_scale = 3.0;
        assert!(settings.validate().is_ok());

        settings.appearance.ui_scale = 0.4;
        assert!(settings.validate().is_err());

        settings.appearance.ui_scale = 3.1;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn tab_width_boundaries() {
        let mut settings = Settings::default();

        settings.editor.tab_width = 1;
        assert!(settings.validate().is_ok());

        settings.editor.tab_width = 16;
        assert!(settings.validate().is_ok());

        settings.editor.tab_width = 0;
        assert!(settings.validate().is_err());

        settings.editor.tab_width = 17;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn enabled_lsp_with_empty_command_rejected() {
        let mut settings = Settings::default();
        settings.lsp.rust.command = "".to_owned();
        let err = settings.validate().unwrap_err();
        assert!(err
            .errors
            .iter()
            .any(|e| e.contains("command must not be empty")));
    }

    #[test]
    fn disabled_lsp_with_empty_command_accepted() {
        let mut settings = Settings::default();
        settings.lsp.rust.enabled = false;
        settings.lsp.rust.command = "".to_owned();
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn lsp_args_with_nul_rejected() {
        let mut settings = Settings::default();
        settings.lsp.rust.args = vec!["--flag\0malicious".to_owned()];
        let err = settings.validate().unwrap_err();
        assert!(err.errors.iter().any(|e| e.contains("NUL character")));
    }

    #[test]
    fn missing_file_returns_defaults() {
        let path = temp_settings_path();
        let store = SettingsStore::at_path(path.clone());
        let settings = store.load().unwrap();
        assert_eq!(settings, Settings::default());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn save_creates_parent_directory() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("blue_ide_test_{}", unique));
        let path = dir.join("config").join("settings.toml");
        let store = SettingsStore::at_path(path.clone());

        assert!(!dir.exists());
        let settings = Settings::default();
        store.save(&settings).unwrap();
        assert!(path.exists());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_load_roundtrip() {
        let path = temp_settings_path();
        let store = SettingsStore::at_path(path.clone());

        let mut settings = Settings::default();
        settings.appearance.theme = Theme::Light;
        settings.appearance.editor_font_size = 18.0;
        settings.editor.tab_width = 2;
        settings.panels.show_file_tree = true;
        settings.recent_workspaces = vec![PathBuf::from(r"C:\workspace\a")];

        store.save(&settings).unwrap();
        let loaded = store.load().unwrap();

        assert_eq!(loaded, settings);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn recent_workspaces_round_trip_through_store() {
        let path = temp_settings_path();
        let store = SettingsStore::at_path(path.clone());

        let mut settings = Settings::default();
        settings.recent_workspaces = vec![
            PathBuf::from(r"C:\workspace\one"),
            PathBuf::from(r"C:\workspace\two"),
        ];

        store.save(&settings).unwrap();
        let loaded = store.load().unwrap();

        assert_eq!(loaded.recent_workspaces, settings.recent_workspaces);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn save_can_replace_an_existing_settings_file() {
        let path = temp_settings_path();
        let store = SettingsStore::at_path(path.clone());
        let mut settings = Settings::default();
        store.save(&settings).unwrap();

        settings.appearance.theme = Theme::Dracula;
        store.save(&settings).unwrap();

        assert_eq!(store.load().unwrap().appearance.theme, Theme::Dracula);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn legacy_theme_names_deserialize() {
        for (serialized, expected) in [
            ("dark", Theme::Dark),
            ("light", Theme::Light),
            ("system", Theme::System),
        ] {
            let input = format!("[appearance]\ntheme = \"{serialized}\"\n");
            let settings: Settings = toml::from_str(&input).unwrap();
            assert_eq!(settings.appearance.theme, expected);
        }
    }

    #[test]
    fn every_theme_round_trips_through_toml() {
        for theme in Theme::all() {
            let mut settings = Settings::default();
            settings.appearance.theme = *theme;
            let serialized = toml::to_string(&settings).unwrap();
            let deserialized: Settings = toml::from_str(&serialized).unwrap();
            assert_eq!(deserialized.appearance.theme, *theme);
        }
    }

    #[test]
    fn solarized_dark_uses_stable_serialized_name() {
        let mut settings = Settings::default();
        settings.appearance.theme = Theme::SolarizedDark;
        let serialized = toml::to_string(&settings).unwrap();
        assert!(serialized.contains("theme = \"solarized-dark\""));
    }

    #[test]
    fn every_theme_survives_settings_store_roundtrip() {
        for theme in Theme::all() {
            let path = temp_settings_path();
            let store = SettingsStore::at_path(path.clone());
            let mut settings = Settings::default();
            settings.appearance.theme = *theme;

            store.save(&settings).unwrap();
            assert_eq!(store.load().unwrap().appearance.theme, *theme);
            fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn unknown_theme_name_is_a_parse_error() {
        let result = toml::from_str::<Settings>("[appearance]\ntheme = \"unknown\"\n");
        assert!(result.is_err());
    }

    #[test]
    fn invalid_settings_do_not_save() {
        let path = temp_settings_path();
        let store = SettingsStore::at_path(path.clone());

        let mut settings = Settings::default();
        settings.appearance.editor_font_size = 100.0;

        let result = store.save(&settings);
        assert!(result.is_err());
        assert!(!path.exists());
    }

    #[test]
    fn serialization_contains_version_and_sections() {
        let settings = Settings::default();
        let toml = toml::to_string_pretty(&settings).unwrap();
        assert!(toml.contains("version = 1"));
        assert!(toml.contains("[appearance]"));
        assert!(toml.contains("[editor]"));
        assert!(toml.contains("[panels]"));
        assert!(toml.contains("[lsp.rust]"));
    }

    #[test]
    fn invalid_toml_not_overwritten() {
        let path = temp_settings_path();
        fs::write(&path, "invalid {{{ toml").unwrap();

        let store = SettingsStore::at_path(path.clone());
        let result = store.load();
        assert!(result.is_err());

        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "invalid {{{ toml");

        fs::remove_file(path).unwrap();
    }
}
