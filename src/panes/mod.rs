pub mod focus;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use serde::{Deserialize, Serialize};

pub use focus::FocusState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PaneId(pub u64);

impl PaneId {
    pub fn next() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        PaneId(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PaneTree {
    Leaf {
        id: PaneId,
        active: Option<PathBuf>,
        tabs: Vec<PathBuf>,
    },
    HSplit {
        left: Box<PaneTree>,
        right: Box<PaneTree>,
        ratio: f32,
    },
    VSplit {
        top: Box<PaneTree>,
        bottom: Box<PaneTree>,
        ratio: f32,
    },
}

impl PaneTree {
    pub fn single() -> Self {
        PaneTree::Leaf {
            id: PaneId::next(),
            active: None,
            tabs: Vec::new(),
        }
    }

    pub fn split_h(&mut self, target: PaneId) -> bool {
        self.split(target, SplitDirection::Horizontal)
    }

    pub fn split_v(&mut self, target: PaneId) -> bool {
        self.split(target, SplitDirection::Vertical)
    }

    fn split(&mut self, target: PaneId, direction: SplitDirection) -> bool {
        match self {
            PaneTree::Leaf { id, active, tabs } if *id == target => {
                // Replace the target leaf with a split whose first child preserves the
                // original pane id, so focus and existing references remain valid.
                let current = PaneTree::Leaf {
                    id: *id,
                    active: active.clone(),
                    tabs: tabs.clone(),
                };
                let new_pane = PaneTree::Leaf {
                    id: PaneId::next(),
                    active: active.clone(),
                    tabs: active.iter().cloned().collect(),
                };
                *self = match direction {
                    SplitDirection::Horizontal => PaneTree::HSplit {
                        left: Box::new(current),
                        right: Box::new(new_pane),
                        ratio: 0.5,
                    },
                    SplitDirection::Vertical => PaneTree::VSplit {
                        top: Box::new(current),
                        bottom: Box::new(new_pane),
                        ratio: 0.5,
                    },
                };
                true
            }
            PaneTree::Leaf { .. } => false,
            PaneTree::HSplit { left, right, .. } => {
                left.split(target, direction) || right.split(target, direction)
            }
            PaneTree::VSplit { top, bottom, .. } => {
                top.split(target, direction) || bottom.split(target, direction)
            }
        }
    }

    pub fn close_pane(&mut self, target: PaneId) -> CloseResult {
        match self {
            PaneTree::Leaf { id, .. } => {
                if *id == target {
                    CloseResult::RemoveSelf
                } else {
                    CloseResult::Unchanged
                }
            }
            PaneTree::HSplit { left, right, .. } => {
                // If one child disappears, bubble its sibling upward to collapse the split.
                match left.close_pane(target) {
                    CloseResult::RemoveSelf => CloseResult::Replace(right.clone()),
                    CloseResult::Replace(replacement) => {
                        *left = replacement;
                        CloseResult::Unchanged
                    }
                    CloseResult::Unchanged => match right.close_pane(target) {
                        CloseResult::RemoveSelf => CloseResult::Replace(left.clone()),
                        CloseResult::Replace(replacement) => {
                            *right = replacement;
                            CloseResult::Unchanged
                        }
                        CloseResult::Unchanged => CloseResult::Unchanged,
                    },
                }
            }
            PaneTree::VSplit { top, bottom, .. } => {
                // If one child disappears, bubble its sibling upward to collapse the split.
                match top.close_pane(target) {
                    CloseResult::RemoveSelf => CloseResult::Replace(bottom.clone()),
                    CloseResult::Replace(replacement) => {
                        *top = replacement;
                        CloseResult::Unchanged
                    }
                    CloseResult::Unchanged => match bottom.close_pane(target) {
                        CloseResult::RemoveSelf => CloseResult::Replace(top.clone()),
                        CloseResult::Replace(replacement) => {
                            *bottom = replacement;
                            CloseResult::Unchanged
                        }
                        CloseResult::Unchanged => CloseResult::Unchanged,
                    },
                }
            }
        }
    }

    pub fn find_leaf_mut(&mut self, target: PaneId) -> Option<&mut PaneTree> {
        match self {
            PaneTree::Leaf { id, .. } => {
                if *id == target {
                    Some(self)
                } else {
                    None
                }
            }
            PaneTree::HSplit { left, right, .. } => left
                .find_leaf_mut(target)
                .or_else(|| right.find_leaf_mut(target)),
            PaneTree::VSplit { top, bottom, .. } => top
                .find_leaf_mut(target)
                .or_else(|| bottom.find_leaf_mut(target)),
        }
    }

    pub fn find_leaf(&self, target: PaneId) -> Option<&PaneTree> {
        match self {
            PaneTree::Leaf { id, .. } => {
                if *id == target {
                    Some(self)
                } else {
                    None
                }
            }
            PaneTree::HSplit { left, right, .. } => {
                left.find_leaf(target).or_else(|| right.find_leaf(target))
            }
            PaneTree::VSplit { top, bottom, .. } => {
                top.find_leaf(target).or_else(|| bottom.find_leaf(target))
            }
        }
    }

    pub fn active_in_pane(&self, pane_id: PaneId) -> Option<&PathBuf> {
        match self.find_leaf(pane_id) {
            Some(PaneTree::Leaf { active, .. }) => active.as_ref(),
            _ => None,
        }
    }

    pub fn active_in_pane_mut(&mut self, pane_id: PaneId) -> Option<&mut Option<PathBuf>> {
        match self.find_leaf_mut(pane_id) {
            Some(PaneTree::Leaf { active, .. }) => Some(active),
            _ => None,
        }
    }

    pub fn all_leaf_ids(&self) -> Vec<PaneId> {
        match self {
            PaneTree::Leaf { id, .. } => vec![*id],
            PaneTree::HSplit { left, right, .. } => {
                let mut ids = left.all_leaf_ids();
                ids.extend(right.all_leaf_ids());
                ids
            }
            PaneTree::VSplit { top, bottom, .. } => {
                let mut ids = top.all_leaf_ids();
                ids.extend(bottom.all_leaf_ids());
                ids
            }
        }
    }

    pub fn open_in_pane(&mut self, pane_id: PaneId, path: PathBuf) {
        if let Some(PaneTree::Leaf { active, tabs, .. }) = self.find_leaf_mut(pane_id) {
            if !tabs.contains(&path) {
                tabs.push(path.clone());
            }
            *active = Some(path);
        }
    }

    pub fn close_tab_in_pane(&mut self, pane_id: PaneId, path: &Path) {
        if let Some(PaneTree::Leaf { active, tabs, .. }) = self.find_leaf_mut(pane_id) {
            tabs.retain(|tab| tab != path);
            if active.as_deref() == Some(path) {
                *active = tabs.last().cloned();
            }
        }
    }

    pub fn any_pane_has(&self, path: &Path) -> bool {
        match self {
            PaneTree::Leaf { active, tabs, .. } => {
                active.as_deref() == Some(path) || tabs.iter().any(|tab| tab == path)
            }
            PaneTree::HSplit { left, right, .. } => {
                left.any_pane_has(path) || right.any_pane_has(path)
            }
            PaneTree::VSplit { top, bottom, .. } => {
                top.any_pane_has(path) || bottom.any_pane_has(path)
            }
        }
    }

    pub fn remove_tab_from_all_panes(&mut self, path: &Path) {
        match self {
            PaneTree::Leaf { active, tabs, .. } => {
                tabs.retain(|tab| tab != path);
                if active.as_deref() == Some(path) {
                    *active = tabs.last().cloned();
                }
            }
            PaneTree::HSplit { left, right, .. } => {
                left.remove_tab_from_all_panes(path);
                right.remove_tab_from_all_panes(path);
            }
            PaneTree::VSplit { top, bottom, .. } => {
                top.remove_tab_from_all_panes(path);
                bottom.remove_tab_from_all_panes(path);
            }
        }
    }

    pub fn render(
        &mut self,
        ui: &mut egui::Ui,
        focus: &mut FocusState,
        actions: &mut Vec<PaneAction>,
        render_leaf: &mut impl FnMut(
            &mut egui::Ui,
            PaneId,
            &mut Option<PathBuf>,
            &mut Vec<PathBuf>,
            &mut FocusState,
            &mut Vec<PaneAction>,
        ),
    ) {
        match self {
            PaneTree::Leaf { id, active, tabs } => {
                render_leaf(ui, *id, active, tabs, focus, actions);
            }
            PaneTree::HSplit { left, right, ratio } => {
                let total_w = ui.available_width();
                if total_w < 200.0 {
                    left.render(ui, focus, actions, render_leaf);
                    return;
                }
                let splitter = 4.0;
                let left_w = ((total_w - splitter) * *ratio).max(100.0);
                let right_w = (total_w - splitter - left_w).max(100.0);

                ui.horizontal(|ui| {
                    ui.allocate_ui(egui::vec2(left_w, ui.available_height()), |ui| {
                        left.render(ui, focus, actions, render_leaf);
                    });
                    let response = ui.allocate_response(
                        egui::vec2(splitter, ui.available_height()),
                        egui::Sense::drag(),
                    );
                    ui.painter().rect_filled(
                        response.rect,
                        0.0,
                        if response.hovered() {
                            egui::Color32::from_rgb(80, 120, 200)
                        } else {
                            egui::Color32::from_rgb(50, 50, 50)
                        },
                    );
                    if response.hovered() || response.dragged() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }
                    if response.dragged() {
                        *ratio = (*ratio + response.drag_delta().x / total_w).clamp(0.1, 0.9);
                    }
                    ui.allocate_ui(egui::vec2(right_w, ui.available_height()), |ui| {
                        right.render(ui, focus, actions, render_leaf);
                    });
                });
            }
            PaneTree::VSplit { top, bottom, ratio } => {
                let total_h = ui.available_height();
                if total_h < 200.0 {
                    top.render(ui, focus, actions, render_leaf);
                    return;
                }
                let splitter = 4.0;
                let top_h = ((total_h - splitter) * *ratio).max(100.0);
                let bottom_h = (total_h - splitter - top_h).max(100.0);

                ui.vertical(|ui| {
                    ui.allocate_ui(egui::vec2(ui.available_width(), top_h), |ui| {
                        top.render(ui, focus, actions, render_leaf);
                    });
                    let response = ui.allocate_response(
                        egui::vec2(ui.available_width(), splitter),
                        egui::Sense::drag(),
                    );
                    ui.painter().rect_filled(
                        response.rect,
                        0.0,
                        if response.hovered() {
                            egui::Color32::from_rgb(80, 120, 200)
                        } else {
                            egui::Color32::from_rgb(50, 50, 50)
                        },
                    );
                    if response.hovered() || response.dragged() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                    }
                    if response.dragged() {
                        *ratio = (*ratio + response.drag_delta().y / total_h).clamp(0.1, 0.9);
                    }
                    ui.allocate_ui(egui::vec2(ui.available_width(), bottom_h), |ui| {
                        bottom.render(ui, focus, actions, render_leaf);
                    });
                });
            }
        }
    }
}

#[derive(Debug)]
pub enum CloseResult {
    Unchanged,
    RemoveSelf,
    Replace(Box<PaneTree>),
}

#[derive(Debug)]
pub enum PaneAction {
    CloseTab { pane: PaneId, path: PathBuf },
    SplitH { pane: PaneId },
    SplitV { pane: PaneId },
    ClosePane { pane: PaneId },
    FocusPane { pane: PaneId },
    OpenInPane { pane: PaneId, path: PathBuf },
}

#[derive(Clone, Copy)]
enum SplitDirection {
    Horizontal,
    Vertical,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> PathBuf {
        PathBuf::from(name)
    }

    #[test]
    fn split_mirrors_active_file_without_cloning_buffers() {
        let mut tree = PaneTree::single();
        let root = tree.all_leaf_ids()[0];
        tree.open_in_pane(root, path("main.rs"));

        assert!(tree.split_h(root));
        let ids = tree.all_leaf_ids();
        assert_eq!(ids.len(), 2);
        assert_eq!(tree.active_in_pane(ids[0]), Some(&path("main.rs")));
        assert_eq!(tree.active_in_pane(ids[1]), Some(&path("main.rs")));
    }

    #[test]
    fn close_pane_collapses_to_sibling_but_single_root_can_be_preserved_by_caller() {
        let mut tree = PaneTree::single();
        let root = tree.all_leaf_ids()[0];
        assert!(tree.split_v(root));
        let ids = tree.all_leaf_ids();

        let CloseResult::Replace(replacement) = tree.close_pane(ids[1]) else {
            panic!("closing a split child should replace the split with its sibling");
        };
        tree = *replacement;

        assert_eq!(tree.all_leaf_ids(), vec![ids[0]]);
        assert!(matches!(tree.close_pane(ids[0]), CloseResult::RemoveSelf));
    }

    #[test]
    fn tab_lists_are_independent_and_any_pane_has_tracks_references() {
        let mut tree = PaneTree::single();
        let root = tree.all_leaf_ids()[0];
        let first = path("first.rs");
        let second = path("second.rs");
        tree.open_in_pane(root, first.clone());
        tree.split_h(root);
        let ids = tree.all_leaf_ids();

        tree.open_in_pane(ids[1], second.clone());
        tree.close_tab_in_pane(ids[0], &first);

        assert!(tree.any_pane_has(&first));
        assert!(tree.any_pane_has(&second));
        tree.close_tab_in_pane(ids[1], &first);
        assert!(!tree.any_pane_has(&first));
    }

    #[test]
    fn focus_cycles_in_leaf_order() {
        let mut tree = PaneTree::single();
        let root = tree.all_leaf_ids()[0];
        tree.split_h(root);
        let ids = tree.all_leaf_ids();
        let mut focus = FocusState::new(ids[0]);

        focus.cycle_next(&tree);
        assert_eq!(focus.active_pane, ids[1]);
        focus.cycle_next(&tree);
        assert_eq!(focus.active_pane, ids[0]);
        focus.cycle_prev(&tree);
        assert_eq!(focus.active_pane, ids[1]);
    }
}
