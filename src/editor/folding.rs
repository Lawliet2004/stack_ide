use std::collections::HashSet;

use ropey::Rope;
use tree_sitter::{Node, Tree};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FoldKind {
    Function,
    Impl,
    Struct,
    Enum,
    Mod,
    Comment,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldRange {
    pub start_line: usize,
    pub end_line: usize,
    pub kind: FoldKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FoldState {
    pub available_folds: Vec<FoldRange>,
    pub collapsed: HashSet<usize>,
}

impl FoldState {
    pub fn refresh_from_tree(&mut self, tree: Option<&Tree>, rope: &Rope) {
        let previous_collapsed = self.collapsed.clone();
        self.available_folds.clear();

        if let Some(tree) = tree {
            collect_fold_ranges(tree.root_node(), rope, &mut self.available_folds);
            self.available_folds.sort_by(|left, right| {
                left.start_line
                    .cmp(&right.start_line)
                    .then_with(|| {
                        fold_kind_priority(right.kind).cmp(&fold_kind_priority(left.kind))
                    })
                    .then_with(|| right.end_line.cmp(&left.end_line))
            });
            self.available_folds.dedup_by_key(|range| range.start_line);
        }

        self.collapsed = self
            .available_folds
            .iter()
            .filter_map(|range| {
                previous_collapsed
                    .contains(&range.start_line)
                    .then_some(range.start_line)
            })
            .collect();
    }

    pub fn fold_starting_at(&self, line: usize) -> Option<&FoldRange> {
        self.available_folds
            .binary_search_by_key(&line, |range| range.start_line)
            .ok()
            .map(|index| &self.available_folds[index])
    }

    pub fn collapsed_containing(&self, line: usize) -> Option<&FoldRange> {
        let index = self
            .available_folds
            .partition_point(|range| range.start_line < line);
        self.available_folds[..index].iter().rev().find(|range| {
            self.collapsed.contains(&range.start_line)
                && line > range.start_line
                && line <= range.end_line
        })
    }

    pub fn is_line_visible(&self, line: usize) -> bool {
        self.collapsed_containing(line).is_none()
    }
}

fn fold_kind_priority(kind: FoldKind) -> u8 {
    match kind {
        FoldKind::Other => 0,
        FoldKind::Comment => 1,
        FoldKind::Function | FoldKind::Impl | FoldKind::Struct | FoldKind::Enum | FoldKind::Mod => {
            2
        }
    }
}

fn collect_fold_ranges(node: Node<'_>, rope: &Rope, ranges: &mut Vec<FoldRange>) {
    if let Some(range) = fold_range_for_node(node, rope) {
        ranges.push(range);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_fold_ranges(child, rope, ranges);
    }
}

fn fold_range_for_node(node: Node<'_>, rope: &Rope) -> Option<FoldRange> {
    if node.parent().is_none() {
        return None;
    }

    let start_byte = node.start_byte().min(rope.len_bytes());
    let end_byte = node.end_byte().min(rope.len_bytes());
    let start_line = rope.byte_to_line(start_byte);
    let end_line = rope.byte_to_line(end_byte);

    if end_line <= start_line + 1 {
        return None;
    }

    let kind = match node.kind() {
        "function_item" => FoldKind::Function,
        "impl_item" => FoldKind::Impl,
        "struct_item" => FoldKind::Struct,
        "enum_item" => FoldKind::Enum,
        "mod_item" => FoldKind::Mod,
        "block_comment" => FoldKind::Comment,
        _ => FoldKind::Other,
    };

    if kind == FoldKind::Other && end_line < start_line + 2 {
        return None;
    }

    Some(FoldRange {
        start_line,
        end_line,
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::highlight::Highlighter;
    use crate::settings::Theme;
    use crate::theme::built_in_theme;
    use egui::FontId;

    #[test]
    fn collapsed_containing_uses_real_lines() {
        let mut state = FoldState {
            available_folds: vec![FoldRange {
                start_line: 1,
                end_line: 4,
                kind: FoldKind::Function,
            }],
            collapsed: [1usize].into_iter().collect(),
        };
        assert!(state.is_line_visible(1));
        assert!(!state.is_line_visible(2));
        assert!(!state.is_line_visible(4));
        assert!(state.is_line_visible(5));

        state.collapsed.clear();
        assert!(state.is_line_visible(2));
    }

    #[test]
    fn detects_named_rust_folds_and_skips_two_line_blocks() {
        let source = "struct Item {\n    value: i32,\n}\n\nfn long() {\n    let x = 1;\n    let y = 2;\n}\n\nfn short() {\n}\n";
        let rope = Rope::from_str(source);
        let mut highlighter = Highlighter::new();
        let palette = built_in_theme(Theme::Dark, None).palette.syntax;
        let _ = highlighter.highlight(source, FontId::monospace(14.0), palette);

        let mut state = FoldState::default();
        state.refresh_from_tree(highlighter.tree(), &rope);

        assert!(state.available_folds.iter().any(|range| {
            range.start_line == 0 && range.end_line == 2 && range.kind == FoldKind::Struct
        }));
        assert!(state.available_folds.iter().any(|range| {
            range.start_line == 4 && range.end_line == 7 && range.kind == FoldKind::Function
        }));
        assert!(!state
            .available_folds
            .iter()
            .any(|range| range.start_line == 9));
    }
}
