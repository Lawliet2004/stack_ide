use super::{PaneId, PaneTree};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusState {
    pub active_pane: PaneId,
}

impl FocusState {
    pub const fn new(root_id: PaneId) -> Self {
        Self {
            active_pane: root_id,
        }
    }

    pub fn cycle_next(&mut self, tree: &PaneTree) {
        let ids = tree.all_leaf_ids();
        if ids.is_empty() {
            return;
        }
        let current = ids
            .iter()
            .position(|id| *id == self.active_pane)
            .unwrap_or(0);
        self.active_pane = ids[(current + 1) % ids.len()];
    }

    pub fn cycle_prev(&mut self, tree: &PaneTree) {
        let ids = tree.all_leaf_ids();
        if ids.is_empty() {
            return;
        }
        let current = ids
            .iter()
            .position(|id| *id == self.active_pane)
            .unwrap_or(0);
        self.active_pane = ids[if current == 0 {
            ids.len() - 1
        } else {
            current - 1
        }];
    }
}
