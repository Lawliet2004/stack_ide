use crate::lsp::types::{OutlineNode, SymbolKind};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentKind {
    File,
    Symbol(SymbolKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreadcrumbSegment {
    pub label: String,
    pub kind: SegmentKind,
    pub line: Option<usize>,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct BreadcrumbState {
    pub segments: Vec<BreadcrumbSegment>,
    pub open_dropdown: Option<usize>,
    pub dropdown_items: Vec<BreadcrumbSegment>,
    pub last_cursor_line: usize,
    pub last_active_file: Option<PathBuf>,
    pub focused_segment: Option<usize>,
    pub dropdown_selected_idx: Option<usize>,
}

impl Default for BreadcrumbState {
    fn default() -> Self {
        Self {
            segments: Vec::new(),
            open_dropdown: None,
            dropdown_items: Vec::new(),
            last_cursor_line: usize::MAX, // starts invalid to trigger first recompute
            last_active_file: None,
            focused_segment: None,
            dropdown_selected_idx: None,
        }
    }
}

pub struct OutlinePanel {
    pub nodes: HashMap<PathBuf, Vec<OutlineNode>>,
    pub pending_request: Option<PathBuf>,
    pub show: bool,
    pub width: f32,
    pub filter: String,
    pub last_cursor_line: Option<usize>,
    pub current_symbol_line: Option<usize>,
    pub needs_scroll_to_symbol: bool,
    pub last_active_file: Option<PathBuf>,
    pub selected_row_index: Option<usize>,
    pub needs_scroll_to_selected: bool,
}

impl OutlinePanel {
    pub fn new(settings: &crate::settings::Settings) -> Self {
        Self {
            nodes: HashMap::new(),
            pending_request: None,
            show: false,
            width: settings.panels.outline_panel_width.unwrap_or(200.0),
            filter: String::new(),
            last_cursor_line: None,
            current_symbol_line: None,
            needs_scroll_to_symbol: false,
            last_active_file: None,
            selected_row_index: None,
            needs_scroll_to_selected: false,
        }
    }
}
