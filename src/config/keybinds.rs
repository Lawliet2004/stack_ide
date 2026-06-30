use egui::{Key, Modifiers};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionName {
    SplitHorizontal,
    SplitVertical,
    ToggleTerminal,
    ToggleFileTree,
    ToggleMinimap,
    ToggleOutline,
    Find,
    FindInFiles,
    Replace,
    GoToLine,
    FormatFile,
    ReloadPlugins,
    OpenSettings,
}

impl ActionName {
    pub const ALL: [Self; 13] = [
        Self::SplitHorizontal,
        Self::SplitVertical,
        Self::ToggleTerminal,
        Self::ToggleFileTree,
        Self::ToggleMinimap,
        Self::ToggleOutline,
        Self::Find,
        Self::FindInFiles,
        Self::Replace,
        Self::GoToLine,
        Self::FormatFile,
        Self::ReloadPlugins,
        Self::OpenSettings,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::SplitHorizontal => "Split Horizontal",
            Self::SplitVertical => "Split Vertical",
            Self::ToggleTerminal => "Toggle Terminal",
            Self::ToggleFileTree => "Toggle File Tree",
            Self::ToggleMinimap => "Toggle Minimap",
            Self::ToggleOutline => "Toggle Outline",
            Self::Find => "Find",
            Self::FindInFiles => "Find in Files",
            Self::Replace => "Replace",
            Self::GoToLine => "Go to Line",
            Self::FormatFile => "Format File",
            Self::ReloadPlugins => "Reload Plugins",
            Self::OpenSettings => "Open Settings",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedKeybind {
    pub modifiers: Modifiers,
    pub key: Key,
}
impl ParsedKeybind {
    pub fn is_pressed(self, input: &egui::InputState) -> bool {
        input.modifiers.matches_logically(self.modifiers) && input.key_pressed(self.key)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeybindingsConfig {
    #[serde(default = "split_horizontal")]
    pub split_horizontal: String,
    #[serde(default = "split_vertical")]
    pub split_vertical: String,
    #[serde(default = "toggle_terminal")]
    pub toggle_terminal: String,
    #[serde(default = "toggle_file_tree")]
    pub toggle_file_tree: String,
    #[serde(default = "toggle_minimap")]
    pub toggle_minimap: String,
    #[serde(default = "toggle_outline")]
    pub toggle_outline: String,
    #[serde(default = "find")]
    pub find: String,
    #[serde(default = "find_in_files")]
    pub find_in_files: String,
    #[serde(default = "replace")]
    pub replace: String,
    #[serde(default = "go_to_line")]
    pub go_to_line: String,
    #[serde(default = "format_file")]
    pub format_file: String,
    #[serde(default = "reload_plugins")]
    pub reload_plugins: String,
    #[serde(default = "open_settings")]
    pub open_settings: String,
    #[serde(default = "join_lines")]
    pub join_lines: String,
    #[serde(default = "duplicate_line")]
    pub duplicate_line: String,
    #[serde(default = "move_line_up")]
    pub move_line_up: String,
    #[serde(default = "move_line_down")]
    pub move_line_down: String,
    #[serde(default = "select_all_occurrences")]
    pub select_all_occurrences: String,
    #[serde(default = "expand_selection")]
    pub expand_selection: String,
    #[serde(default = "shrink_selection")]
    pub shrink_selection: String,
    #[serde(default = "undo_history_panel")]
    pub undo_history_panel: String,
    #[serde(default = "add_cursor_above")]
    pub add_cursor_above: String,
    #[serde(default = "add_cursor_below")]
    pub add_cursor_below: String,
}
macro_rules! defaults {($($n:ident=$v:literal),*)=>{$(fn $n()->String{$v.into()})*};}
defaults!(
    split_horizontal = "Ctrl+\\",
    split_vertical = "Ctrl+-",
    toggle_terminal = "Ctrl+`",
    toggle_file_tree = "Ctrl+\\",
    toggle_minimap = "Ctrl+Shift+M",
    toggle_outline = "Ctrl+Shift+O",
    find = "Ctrl+F",
    find_in_files = "Ctrl+Shift+F",
    replace = "Ctrl+H",
    go_to_line = "Ctrl+G",
    format_file = "Ctrl+Shift+I",
    reload_plugins = "Ctrl+Shift+R",
    open_settings = "Ctrl+,",
    join_lines = "Ctrl+J",
    duplicate_line = "Ctrl+Shift+D",
    move_line_up = "Alt+Up",
    move_line_down = "Alt+Down",
    select_all_occurrences = "Ctrl+Shift+L",
    expand_selection = "Alt+Shift+Right",
    shrink_selection = "Alt+Shift+Left",
    undo_history_panel = "Ctrl+Shift+U",
    add_cursor_above = "Ctrl+Alt+Up",
    add_cursor_below = "Ctrl+Alt+Down"
);
impl Default for KeybindingsConfig {
    fn default() -> Self {
        Self {
            split_horizontal: split_horizontal(),
            split_vertical: split_vertical(),
            toggle_terminal: toggle_terminal(),
            toggle_file_tree: toggle_file_tree(),
            toggle_minimap: toggle_minimap(),
            toggle_outline: toggle_outline(),
            find: find(),
            find_in_files: find_in_files(),
            replace: replace(),
            go_to_line: go_to_line(),
            format_file: format_file(),
            reload_plugins: reload_plugins(),
            open_settings: open_settings(),
            join_lines: join_lines(),
            duplicate_line: duplicate_line(),
            move_line_up: move_line_up(),
            move_line_down: move_line_down(),
            select_all_occurrences: select_all_occurrences(),
            expand_selection: expand_selection(),
            shrink_selection: shrink_selection(),
            undo_history_panel: undo_history_panel(),
            add_cursor_above: add_cursor_above(),
            add_cursor_below: add_cursor_below(),
        }
    }
}
impl KeybindingsConfig {
    fn value(&self, a: ActionName) -> &str {
        match a {
            ActionName::SplitHorizontal => &self.split_horizontal,
            ActionName::SplitVertical => &self.split_vertical,
            ActionName::ToggleTerminal => &self.toggle_terminal,
            ActionName::ToggleFileTree => &self.toggle_file_tree,
            ActionName::ToggleMinimap => &self.toggle_minimap,
            ActionName::ToggleOutline => &self.toggle_outline,
            ActionName::Find => &self.find,
            ActionName::FindInFiles => &self.find_in_files,
            ActionName::Replace => &self.replace,
            ActionName::GoToLine => &self.go_to_line,
            ActionName::FormatFile => &self.format_file,
            ActionName::ReloadPlugins => &self.reload_plugins,
            ActionName::OpenSettings => &self.open_settings,
        }
    }
    pub fn parsed_with_defaults(&self) -> HashMap<ActionName, ParsedKeybind> {
        let defaults = Self::default();
        ActionName::ALL
            .into_iter()
            .filter_map(|a| {
                parse(self.value(a))
                    .or_else(|| parse(defaults.value(a)))
                    .map(|b| (a, b))
            })
            .collect()
    }
    pub fn conflict(&self, action: ActionName, candidate: ParsedKeybind) -> Option<ActionName> {
        self.parsed_with_defaults()
            .into_iter()
            .find_map(|(a, b)| (a != action && b == candidate).then_some(a))
    }
}

pub fn parse(text: &str) -> Option<ParsedKeybind> {
    let mut modifiers = Modifiers::default();
    let mut key = None;
    for token in text.split('+') {
        match token.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers.ctrl = true,
            "shift" => modifiers.shift = true,
            "alt" => modifiers.alt = true,
            "command" | "cmd" => modifiers.command = true,
            name => {
                if key.is_some() {
                    return None;
                }
                key = parse_key(name)
            }
        }
    }
    Some(ParsedKeybind {
        modifiers,
        key: key?,
    })
}
fn parse_key(s: &str) -> Option<Key> {
    if s.len() == 1 {
        let c = s.chars().next()?.to_ascii_uppercase();
        if c.is_ascii_alphabetic() {
            return Key::from_name(&c.to_string());
        }
        if c.is_ascii_digit() {
            return Key::from_name(&format!("Num{c}"));
        }
    }
    match s {
        "enter" => Some(Key::Enter),
        "escape" | "esc" => Some(Key::Escape),
        "tab" => Some(Key::Tab),
        "backspace" => Some(Key::Backspace),
        "space" => Some(Key::Space),
        "," => Some(Key::Comma),
        "-" => Some(Key::Minus),
        "`" => Some(Key::Backtick),
        "\\" => Some(Key::Backslash),
        _ => Key::from_name(&s.to_ascii_uppercase()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_modifiers_and_punctuation() {
        let b = parse("Ctrl+Shift+F").unwrap();
        assert!(b.modifiers.ctrl && b.modifiers.shift);
        assert_eq!(b.key, Key::F);
        assert_eq!(parse("Ctrl+,").unwrap().key, Key::Comma);
    }
    #[test]
    fn rejects_multiple_keys() {
        assert!(parse("Ctrl+A+B").is_none());
    }
    #[test]
    fn detects_conflicts() {
        let c = KeybindingsConfig::default();
        assert_eq!(
            c.conflict(ActionName::ToggleFileTree, parse("Ctrl+\\").unwrap()),
            Some(ActionName::SplitHorizontal)
        );
    }
}
