use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use egui::{vec2, Align2, Color32, FontId, Key, Sense, WidgetInfo, WidgetType};

use crate::git::FileStatus;

const MAX_DEPTH: usize = 6;
// Compact, VS Code-style row metrics. ROW_HEIGHT is the single source of
// vertical density for every list item (root, folders and files) and is paired
// with zero inter-row spacing in `render`, so rows sit flush against each other.
const ROW_HEIGHT: f32 = 22.0;
const FONT_SIZE: f32 = 13.0;
const INDENT_WIDTH: f32 = 14.0;
const TOOLBAR_HEIGHT: f32 = 30.0;
const ICON_SIZE: f32 = 20.0;

// Compact tree-row layout metrics. The row is split into three fixed columns
// (chevron, icon, text) so that every item at the same depth lines up cleanly.
const ROW_LEFT_PAD: f32 = 8.0;
const CHEVRON_W: f32 = 14.0;
const TREE_ICON: f32 = 16.0;
const ICON_TEXT_GAP: f32 = 6.0;
const ROW_ROUNDING: f32 = 5.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateKind {
    File,
    Folder,
}

#[derive(Debug, Clone)]
pub struct CreateState {
    pub kind: CreateKind,
    pub target_dir: PathBuf,
    pub name: String,
}

pub enum FsNode {
    File {
        name: String,
        path: PathBuf,
    },
    Dir {
        name: String,
        path: PathBuf,
        children: Vec<FsNode>,
        expanded: bool,
        loaded: bool,
    },
}
impl FsNode {
    pub fn path(&self) -> &Path {
        match self {
            FsNode::File { path, .. } => path,
            FsNode::Dir { path, .. } => path,
        }
    }
}


#[derive(Default)]
pub struct FileTree {
    pub root: Option<FsNode>,
    pub root_path: Option<PathBuf>,
    pub create_state: Option<CreateState>,
    pub selected_path: Option<PathBuf>,
    pub roots: Vec<PathBuf>,
}

pub enum FileTreeAction {
    None,
    Open(PathBuf),
}

impl FileTree {
    pub fn load(&mut self, path: PathBuf) -> io::Result<()> {
        self.roots = vec![path];
        self.rebuild_virtual_root()
    }

    pub fn rebuild_virtual_root(&mut self) -> io::Result<()> {
        if self.roots.is_empty() {
            self.root = None;
            self.root_path = None;
            return Ok(());
        }
        if self.roots.len() == 1 {
            let rpath = &self.roots[0];
            let children = read_children(rpath, 1, Some(rpath))?;
            let name = rpath
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| rpath.display().to_string());
            self.root = Some(FsNode::Dir {
                name,
                path: rpath.clone(),
                children,
                expanded: true,
                loaded: true,
            });
            self.root_path = Some(rpath.clone());
            return Ok(());
        }
        let mut children = Vec::new();
        for rpath in &self.roots {
            let name = rpath
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| rpath.display().to_string());
            let root_children = read_children(rpath, 1, Some(rpath))?;
            children.push(FsNode::Dir {
                name,
                path: rpath.clone(),
                children: root_children,
                expanded: true,
                loaded: true,
            });
        }
        self.root = Some(FsNode::Dir {
            name: "Workspace".to_owned(),
            path: PathBuf::from("workspace://"),
            children,
            expanded: true,
            loaded: true,
        });
        self.root_path = Some(PathBuf::from("workspace://"));
        Ok(())
    }

    pub fn add_root(&mut self, path: PathBuf) -> io::Result<()> {
        if !self.roots.contains(&path) {
            self.roots.push(path);
        }
        self.rebuild_virtual_root()
    }

    pub fn remove_root(&mut self, path: &Path) -> io::Result<()> {
        self.roots.retain(|r| r != path);
        self.rebuild_virtual_root()
    }

    pub fn start_create(&mut self, kind: CreateKind, target_dir: PathBuf) {
        self.create_state = Some(CreateState {
            kind,
            target_dir,
            name: String::new(),
        });
    }

    pub fn cancel_create(&mut self) {
        self.create_state = None;
    }

    /// Confirms the current create operation. Returns the path of a created file when it should be
    /// opened automatically.
    pub fn confirm_create(&mut self) -> io::Result<Option<PathBuf>> {
        let Some(state) = self.create_state.take() else {
            return Ok(None);
        };
        let trimmed = state.name.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        let new_path = state.target_dir.join(trimmed);
        match state.kind {
            CreateKind::File => {
                if let Some(parent) = new_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&new_path, "")?;
            }
            CreateKind::Folder => {
                fs::create_dir_all(&new_path)?;
            }
        }

        if !self.roots.is_empty() {
            let _ = self.rebuild_virtual_root();
        } else if let Some(root_path) = &self.root_path {
            let _ = self.load(root_path.clone());
        }

        if state.kind == CreateKind::File {
            Ok(Some(new_path))
        } else {
            Ok(None)
        }
    }

    pub fn toggle_dir(&mut self, path: &Path) -> io::Result<()> {
        let Some(root) = self.root.as_mut() else {
            return Ok(());
        };
        let Some((node, depth)) = find_dir_mut(root, path, 0) else {
            return Ok(());
        };

        let FsNode::Dir {
            path,
            children,
            expanded,
            loaded,
            ..
        } = node
        else {
            return Ok(());
        };

        if !*expanded && !*loaded {
            let loaded_children = if depth < MAX_DEPTH {
                let owner = find_owner_root(&self.roots, path);
                read_children(path, depth + 1, owner.as_deref())?
            } else {
                Vec::new()
            };
            *children = loaded_children;
            *loaded = true;
        }
        *expanded = !*expanded;
        Ok(())
    }

    pub fn render(
        &mut self,
        ui: &mut egui::Ui,
        active_path: Option<&Path>,
        file_statuses: &HashMap<PathBuf, FileStatus>,
    ) -> io::Result<FileTreeAction> {
        // Keep the tree dense: no inter-row gap and no minimum interactive
        // height inflating each allocated row. This makes row height fully
        // governed by ROW_HEIGHT regardless of the surrounding panel style.
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.spacing_mut().interact_size.y = 0.0;

        let toolbar_response = self.render_toolbar(ui);
        if let Some(kind) = toolbar_response.clicked_kind {
            let target_dir = self.root_path.clone().unwrap_or_else(|| PathBuf::from("."));
            self.start_create(kind, target_dir);
        }

        if let Some(ref mut state) = self.create_state {
            render_create_row(ui, state);
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                self.cancel_create();
            } else if ui.input(|i| i.key_pressed(Key::Enter)) {
                match self.confirm_create() {
                    Ok(Some(created_file)) => {
                        return Ok(FileTreeAction::Open(created_file));
                    }
                    Ok(None) => {}
                    Err(error) => return Err(error),
                }
            }
        }

        let mut clicked_file = None;
        let mut clicked_dir = None;

        let is_focused = ui.memory(|m| m.has_focus(egui::Id::new("file_tree")));
        if is_focused {
            let mut visible_nodes = Vec::new();
            if let Some(ref root) = self.root {
                Self::collect_visible_nodes(root, &mut visible_nodes);
            }

            let current_idx = self.selected_path.as_ref().and_then(|p| {
                visible_nodes.iter().position(|node| node.path() == p)
            });

            ui.input(|input| {
                if input.key_pressed(Key::ArrowDown) {
                    let next_idx = match current_idx {
                        Some(idx) => (idx + 1).min(visible_nodes.len().saturating_sub(1)),
                        None => 0,
                    };
                    if next_idx < visible_nodes.len() {
                        self.selected_path = Some(visible_nodes[next_idx].path().to_path_buf());
                    }
                } else if input.key_pressed(Key::ArrowUp) {
                    let prev_idx = match current_idx {
                        Some(idx) => idx.saturating_sub(1),
                        None => 0,
                    };
                    if prev_idx < visible_nodes.len() {
                        self.selected_path = Some(visible_nodes[prev_idx].path().to_path_buf());
                    }
                } else if input.key_pressed(Key::ArrowRight) {
                    if let Some(idx) = current_idx {
                        if let FsNode::Dir { path, expanded: false, .. } = &visible_nodes[idx] {
                            clicked_dir = Some(path.clone());
                        }
                    }
                } else if input.key_pressed(Key::ArrowLeft) {
                    if let Some(idx) = current_idx {
                        if let FsNode::Dir { path, expanded: true, .. } = &visible_nodes[idx] {
                            clicked_dir = Some(path.clone());
                        }
                    }
                } else if input.key_pressed(Key::Enter) {
                    if let Some(idx) = current_idx {
                        match &visible_nodes[idx] {
                            FsNode::File { path, .. } => {
                                clicked_file = Some(path.clone());
                            }
                            FsNode::Dir { path, .. } => {
                                clicked_dir = Some(path.clone());
                            }
                        }
                    }
                }
            });
        }

        if let Some(root) = self.root.as_ref() {
            if root.path() == Path::new("workspace://") {
                if let FsNode::Dir { children, .. } = root {
                    for child in children {
                        render_node(
                            ui,
                            child,
                            self.selected_path.as_deref().or(active_path),
                            file_statuses,
                            0,
                            &mut clicked_file,
                            &mut clicked_dir,
                        );
                    }
                }
            } else {
                render_node(
                    ui,
                    root,
                    self.selected_path.as_deref().or(active_path),
                    file_statuses,
                    0,
                    &mut clicked_file,
                    &mut clicked_dir,
                );
            }
        }
        if let Some(path) = clicked_file.clone() {
            self.selected_path = Some(path);
        }
        if let Some(path) = clicked_dir.clone() {
            self.selected_path = Some(path.clone());
            self.toggle_dir(&path)?;
        }
        Ok(clicked_file
            .map(FileTreeAction::Open)
            .unwrap_or(FileTreeAction::None))
    }

    fn collect_visible_nodes<'a>(node: &'a FsNode, list: &mut Vec<&'a FsNode>) {
        if node.path() == Path::new("workspace://") {
            if let FsNode::Dir { children, .. } = node {
                for child in children {
                    Self::collect_visible_nodes_rec(child, list);
                }
            }
        } else {
            Self::collect_visible_nodes_rec(node, list);
        }
    }

    fn collect_visible_nodes_rec<'a>(node: &'a FsNode, list: &mut Vec<&'a FsNode>) {
        list.push(node);
        if let FsNode::Dir { children, expanded: true, .. } = node {
            for child in children {
                Self::collect_visible_nodes_rec(child, list);
            }
        }
    }

    fn render_toolbar(&self, ui: &mut egui::Ui) -> ToolbarOutput {
        let available_width = ui.available_width();
        let (toolbar_rect, _) = ui.allocate_exact_size(
            vec2(available_width.max(1.0), TOOLBAR_HEIGHT),
            Sense::hover(),
        );
        let mut output = ToolbarOutput::default();

        let icon_size = egui::vec2(ICON_SIZE, ICON_SIZE);
        let padding = 4.0;
        let spacing = 2.0;

        let file_icon_rect = egui::Rect::from_min_size(
            egui::pos2(
                toolbar_rect.right() - 2.0 * (ICON_SIZE + spacing) - padding,
                toolbar_rect.center().y - ICON_SIZE / 2.0,
            ),
            icon_size,
        );
        let folder_icon_rect = egui::Rect::from_min_size(
            egui::pos2(
                toolbar_rect.right() - (ICON_SIZE + spacing) - padding,
                toolbar_rect.center().y - ICON_SIZE / 2.0,
            ),
            icon_size,
        );

        let file_response = ui.interact(file_icon_rect, ui.id().with("new_file"), Sense::click());
        let folder_response =
            ui.interact(folder_icon_rect, ui.id().with("new_folder"), Sense::click());

        let text_color = ui.visuals().text_color();
        let hover_color = ui.visuals().widgets.hovered.weak_bg_fill;
        let active_color = ui.visuals().widgets.active.weak_bg_fill;

        for (response, rect, label) in [
            (&file_response, file_icon_rect, "New file"),
            (&folder_response, folder_icon_rect, "New folder"),
        ] {
            if response.hovered() || response.is_pointer_button_down_on() {
                let fill = if response.is_pointer_button_down_on() {
                    active_color
                } else {
                    hover_color
                };
                ui.painter().rect_filled(rect.shrink(2.0), 2.0, fill);
            }
            response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, label));
        }

        paint_file_icon(ui.painter(), file_icon_rect.shrink(5.0), text_color);
        paint_folder_icon(ui.painter(), folder_icon_rect.shrink(5.0), text_color);

        if file_response.clicked() {
            output.clicked_kind = Some(CreateKind::File);
        }
        if folder_response.clicked() {
            output.clicked_kind = Some(CreateKind::Folder);
        }

        file_response.on_hover_text("New file");
        folder_response.on_hover_text("New folder");

        output
    }
}

#[derive(Default)]
struct ToolbarOutput {
    clicked_kind: Option<CreateKind>,
}

fn render_create_row(ui: &mut egui::Ui, state: &mut CreateState) {
    let available_width = ui.available_width();
    let (rect, _) =
        ui.allocate_exact_size(vec2(available_width.max(1.0), ROW_HEIGHT), Sense::hover());
    ui.painter().rect_filled(
        rect,
        0.0,
        ui.visuals().selection.bg_fill.linear_multiply(0.35),
    );

    let prompt = match state.kind {
        CreateKind::File => "New file: ",
        CreateKind::Folder => "New folder: ",
    };
    let prompt_x = rect.left() + 4.0 + INDENT_WIDTH;
    let prompt_galley = ui.painter().layout(
        prompt.to_string(),
        FontId::proportional(FONT_SIZE),
        ui.visuals().text_color(),
        f32::INFINITY,
    );
    let prompt_width = prompt_galley.size().x;
    ui.painter().galley(
        egui::pos2(prompt_x, rect.center().y - prompt_galley.size().y / 2.0),
        prompt_galley,
        ui.visuals().text_color(),
    );

    let input_x = prompt_x + prompt_width + 4.0;
    let input_width = rect.right() - input_x - 4.0;
    let input_rect = egui::Rect::from_min_size(
        egui::pos2(input_x, rect.center().y - ROW_HEIGHT / 2.0 + 2.0),
        egui::vec2(input_width.max(1.0), ROW_HEIGHT - 4.0),
    );

    ui.painter()
        .rect_filled(input_rect, 2.0, ui.visuals().extreme_bg_color);
    ui.painter().rect_stroke(
        input_rect,
        2.0,
        egui::Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color),
    );

    let text_galley = ui.painter().layout(
        state.name.clone(),
        FontId::proportional(FONT_SIZE),
        ui.visuals().text_color(),
        f32::INFINITY,
    );
    ui.painter().galley(
        egui::pos2(
            input_rect.min.x + 4.0,
            input_rect.center().y - text_galley.size().y / 2.0,
        ),
        text_galley,
        ui.visuals().text_color(),
    );

    // Register an actual interactive widget for the input so the focus target
    // exists in the accessibility tree. Requesting focus on an unregistered id
    // makes accesskit panic (focus node missing from the tree).
    let input_id = ui.id().with("create_input");
    let response = ui.interact(input_rect, input_id, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::TextEdit, &state.name));
    response.request_focus();
    ui.input(|input| {
        for event in &input.events {
            if let egui::Event::Text(text) = event {
                if !text.contains('\n') && !text.contains('\r') {
                    state.name.push_str(text);
                }
            } else if let egui::Event::Key {
                key: Key::Backspace,
                pressed: true,
                repeat: _,
                modifiers,
                physical_key: _,
            } = event
            {
                if modifiers.ctrl || modifiers.command {
                    state.name.clear();
                } else if !state.name.is_empty() {
                    state.name.pop();
                }
            }
        }
    });
}

fn paint_file_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let stroke = egui::Stroke::new(1.2, color);
    let corner = 1.5;
    // Document body
    let body = rect.shrink(1.0);
    painter.rect_stroke(body, corner, stroke);
    // Folded corner
    let fold_size = body.width().min(body.height()) * 0.35;
    let top_right = body.right_top();
    painter.line_segment(
        [
            egui::pos2(top_right.x - fold_size, top_right.y),
            egui::pos2(top_right.x, top_right.y + fold_size),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(top_right.x - fold_size, top_right.y),
            egui::pos2(top_right.x - fold_size, top_right.y + fold_size * 0.4),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(top_right.x - fold_size * 0.55, top_right.y + fold_size),
            egui::pos2(top_right.x, top_right.y + fold_size),
        ],
        stroke,
    );
    // Plus sign
    let plus_center = egui::pos2(body.center().x, body.bottom() - body.height() * 0.25);
    let plus_half = body.width().min(body.height()) * 0.12;
    painter.line_segment(
        [
            egui::pos2(plus_center.x - plus_half, plus_center.y),
            egui::pos2(plus_center.x + plus_half, plus_center.y),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(plus_center.x, plus_center.y - plus_half),
            egui::pos2(plus_center.x, plus_center.y + plus_half),
        ],
        stroke,
    );
}

fn paint_folder_icon(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let stroke = egui::Stroke::new(1.2, color);
    let tab_width = rect.width() * 0.35;
    let tab_height = rect.height() * 0.18;
    let body = egui::Rect::from_min_size(
        egui::pos2(rect.min.x, rect.min.y + tab_height - 1.0),
        egui::vec2(rect.width(), rect.height() - tab_height + 1.0),
    );
    // Tab
    painter.line_segment(
        [
            egui::pos2(rect.min.x, rect.min.y + tab_height),
            egui::pos2(rect.min.x + tab_width, rect.min.y + tab_height),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(rect.min.x + tab_width * 0.2, rect.min.y + tab_height),
            egui::pos2(rect.min.x + tab_width * 0.45, rect.min.y),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(rect.min.x + tab_width * 0.45, rect.min.y),
            egui::pos2(rect.min.x + tab_width * 0.85, rect.min.y),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(rect.min.x + tab_width * 0.85, rect.min.y),
            egui::pos2(rect.min.x + tab_width, rect.min.y + tab_height),
        ],
        stroke,
    );
    // Body
    painter.rect_stroke(body, 1.5, stroke);
    // Plus sign
    let plus_center = egui::pos2(body.center().x, body.center().y + body.height() * 0.05);
    let plus_half = rect.width().min(rect.height()) * 0.12;
    painter.line_segment(
        [
            egui::pos2(plus_center.x - plus_half, plus_center.y),
            egui::pos2(plus_center.x + plus_half, plus_center.y),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(plus_center.x, plus_center.y - plus_half),
            egui::pos2(plus_center.x, plus_center.y + plus_half),
        ],
        stroke,
    );
}

fn find_owner_root(roots: &[PathBuf], path: &Path) -> Option<PathBuf> {
    roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
        .cloned()
}

fn read_children(path: &Path, child_depth: usize, root_path: Option<&Path>) -> io::Result<Vec<FsNode>> {
    let matcher = root_path.map(crate::workspace::ExcludeMatcher::load_for_root);
    let mut children = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }

        let file_type = entry.file_type()?;
        if file_type.is_symlink() || (file_type.is_dir() && name == "target") {
            continue;
        }

        let child_path = entry.path();
        if let Some(ref matcher) = matcher {
            if let Some(rp) = root_path {
                if matcher.is_excluded(&child_path, rp) {
                    continue;
                }
            }
        }

        if file_type.is_dir() {
            children.push(FsNode::Dir {
                name,
                path: child_path,
                children: Vec::new(),
                expanded: false,
                loaded: child_depth >= MAX_DEPTH,
            });
        } else if file_type.is_file() {
            children.push(FsNode::File { name, path: child_path });
        }
    }
    children.sort_by(compare_nodes);
    Ok(children)
}

fn compare_nodes(left: &FsNode, right: &FsNode) -> Ordering {
    let (left_is_file, left_name) = node_sort_key(left);
    let (right_is_file, right_name) = node_sort_key(right);
    left_is_file
        .cmp(&right_is_file)
        .then_with(|| left_name.to_lowercase().cmp(&right_name.to_lowercase()))
        .then_with(|| left_name.cmp(right_name))
}

fn node_sort_key(node: &FsNode) -> (bool, &str) {
    match node {
        FsNode::Dir { name, .. } => (false, name),
        FsNode::File { name, .. } => (true, name),
    }
}

fn find_dir_mut<'a>(
    node: &'a mut FsNode,
    target: &Path,
    depth: usize,
) -> Option<(&'a mut FsNode, usize)> {
    let is_target = matches!(node, FsNode::Dir { path, .. } if path == target);
    if is_target {
        return Some((node, depth));
    }
    let FsNode::Dir { children, .. } = node else {
        return None;
    };
    children
        .iter_mut()
        .find_map(|child| find_dir_mut(child, target, depth + 1))
}

fn render_node(
    ui: &mut egui::Ui,
    node: &FsNode,
    active_path: Option<&Path>,
    file_statuses: &HashMap<PathBuf, FileStatus>,
    depth: usize,
    clicked_file: &mut Option<PathBuf>,
    clicked_dir: &mut Option<PathBuf>,
) {
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width().max(1.0), ROW_HEIGHT),
        Sense::click(),
    );
    let (label, path, is_dir, expanded) = match node {
        FsNode::File { name, path } => (name.as_str(), path, false, false),
        FsNode::Dir {
            name,
            path,
            expanded,
            ..
        } => (name.as_str(), path, true, *expanded),
    };
    let _node_id = ui.id().with(path.as_os_str());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, label));

    // Accessibility: add label for screen readers
    let a11y_label = if is_dir {
        format!("Open folder: {label}")
    } else {
        format!("Open file: {label}")
    };
    let response = crate::screen_reader::label_element(ui, response, &a11y_label, &a11y_label);

    let selected = active_path == Some(path.as_path());
    let is_root = depth == 0;

    // Slim, rounded selection / hover highlight that hugs the row bounds.
    let highlight = rect.shrink2(vec2(3.0, 1.0));
    if selected {
        ui.painter()
            .rect_filled(highlight, ROW_ROUNDING, tree_selection_fill());
    } else if response.hovered() {
        ui.painter()
            .rect_filled(highlight, ROW_ROUNDING, tree_hover_fill());
    }

    // Three aligned columns: chevron | icon | text. Every item at the same
    // depth shares the same column x-positions so glyphs line up vertically.
    let indent = depth as f32 * INDENT_WIDTH;
    let col_x = rect.left() + ROW_LEFT_PAD + indent;
    let chevron_center = egui::pos2(col_x + CHEVRON_W * 0.5, rect.center().y);
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(col_x + CHEVRON_W, rect.center().y - TREE_ICON * 0.5),
        egui::vec2(TREE_ICON, TREE_ICON),
    );
    let text_x = icon_rect.right() + ICON_TEXT_GAP;

    let muted = ui.visuals().weak_text_color();
    let conflicted = !is_dir && file_statuses.get(path) == Some(&FileStatus::Conflicted);

    // Faint vertical indentation guides, one per ancestor level (VS Code style).
    // Drawn after the row highlight so they sit beneath the glyphs and read as
    // continuous lines across stacked rows.
    if !selected {
        let guide = Color32::from_white_alpha(13);
        for level in 0..depth {
            let gx = (rect.left() + ROW_LEFT_PAD + level as f32 * INDENT_WIDTH
                + CHEVRON_W * 0.5)
                .round()
                + 0.5;
            ui.painter().line_segment(
                [egui::pos2(gx, rect.top()), egui::pos2(gx, rect.bottom())],
                egui::Stroke::new(1.0, guide),
            );
        }
    }

    // Chevron (folders only). Files leave the column empty so the icon column
    // still aligns across files and folders.
    if is_dir {
        paint_chevron(ui.painter(), chevron_center, expanded, muted);
    }

    // Type icon.
    if is_dir {
        paint_tree_folder(ui.painter(), icon_rect, folder_icon_color());
    } else {
        let icon_color = if conflicted {
            Color32::from_rgb(220, 90, 90)
        } else {
            muted
        };
        crate::file_icons::paint(ui.painter(), icon_rect, label, icon_color);
    }

    // Label. The root is rendered slightly more prominently than its children.
    let text_color = if conflicted {
        Color32::from_rgb(224, 108, 108)
    } else if is_root {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().text_color()
    };
    let font_id = FontId::proportional(if is_root { FONT_SIZE + 0.5 } else { FONT_SIZE });
    ui.painter().text(
        egui::pos2(text_x, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font_id,
        text_color,
    );

    if response.clicked() {
        if is_dir {
            *clicked_dir = Some(path.clone());
        } else {
            *clicked_file = Some(path.clone());
        }
    }

    if let FsNode::Dir {
        children,
        expanded: true,
        ..
    } = node
    {
        for child in children {
            render_node(
                ui,
                child,
                active_path,
                file_statuses,
                depth + 1,
                clicked_file,
                clicked_dir,
            );
        }
    }
}

/// Muted blue-gray fill for the selected row.
fn tree_selection_fill() -> Color32 {
    Color32::from_rgba_unmultiplied(116, 132, 170, 56)
}

/// Very subtle lightening for the hovered row.
fn tree_hover_fill() -> Color32 {
    Color32::from_rgba_unmultiplied(255, 255, 255, 14)
}

/// Muted gray-blue used for folder glyphs.
fn folder_icon_color() -> Color32 {
    Color32::from_rgb(118, 131, 156)
}

/// Lowercased file extension for `name`, or an empty string when there is none.
fn file_ext(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Pick a subtle, file-type aware tint for a file glyph, falling back to the
/// muted default color when the extension is unknown. Colors follow the
/// conventional brand/language hues used by common IDE icon themes.
fn file_icon_color(name: &str, default: Color32) -> Color32 {
    match file_ext(name).as_str() {
        // Systems / compiled languages.
        "rs" => Color32::from_rgb(206, 123, 74),
        "go" => Color32::from_rgb(85, 173, 209),
        "c" | "h" => Color32::from_rgb(108, 145, 191),
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => Color32::from_rgb(120, 130, 200),
        "java" => Color32::from_rgb(199, 120, 80),
        "kt" | "kts" => Color32::from_rgb(160, 120, 210),
        "swift" => Color32::from_rgb(222, 120, 80),
        // Scripting languages.
        "py" => Color32::from_rgb(92, 160, 218),
        "rb" => Color32::from_rgb(204, 90, 90),
        "php" => Color32::from_rgb(130, 130, 190),
        "lua" => Color32::from_rgb(90, 120, 200),
        "sh" | "bash" | "zsh" | "ps1" | "bat" | "cmd" => Color32::from_rgb(140, 190, 120),
        // Web / JS ecosystem.
        "js" | "mjs" | "cjs" | "jsx" => Color32::from_rgb(224, 196, 100),
        "ts" | "tsx" => Color32::from_rgb(86, 156, 214),
        "html" | "htm" => Color32::from_rgb(206, 134, 110),
        "css" | "scss" | "sass" | "less" => Color32::from_rgb(118, 160, 206),
        "vue" => Color32::from_rgb(110, 190, 140),
        // Data / config.
        "json" => Color32::from_rgb(214, 188, 110),
        "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "env" => {
            Color32::from_rgb(150, 184, 144)
        }
        "xml" | "csv" | "sql" => Color32::from_rgb(150, 170, 150),
        // Docs / text.
        "md" | "markdown" => Color32::from_rgb(140, 162, 198),
        "txt" | "log" => Color32::from_rgb(150, 158, 170),
        // Images / assets.
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "bmp" | "ico" => {
            Color32::from_rgb(180, 142, 196)
        }
        // Lock / metadata.
        "lock" => Color32::from_rgb(132, 138, 150),
        _ => default,
    }
}

/// Draw a small chevron centered at `center`: pointing down when `expanded`,
/// otherwise pointing right.
fn paint_chevron(painter: &egui::Painter, center: egui::Pos2, expanded: bool, color: Color32) {
    let stroke = egui::Stroke::new(1.4, color);
    let s = 3.0;
    if expanded {
        painter.line_segment(
            [
                egui::pos2(center.x - s, center.y - s * 0.5),
                egui::pos2(center.x, center.y + s * 0.6),
            ],
            stroke,
        );
        painter.line_segment(
            [
                egui::pos2(center.x, center.y + s * 0.6),
                egui::pos2(center.x + s, center.y - s * 0.5),
            ],
            stroke,
        );
    } else {
        painter.line_segment(
            [
                egui::pos2(center.x - s * 0.5, center.y - s),
                egui::pos2(center.x + s * 0.6, center.y),
            ],
            stroke,
        );
        painter.line_segment(
            [
                egui::pos2(center.x + s * 0.6, center.y),
                egui::pos2(center.x - s * 0.5, center.y + s),
            ],
            stroke,
        );
    }
}

/// Draw a compact, filled folder glyph (VS Code / Seti style) inside `rect`.
fn paint_tree_folder(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let tab_h = rect.height() * 0.26;
    let tab_w = rect.width() * 0.46;
    // Raised back tab, drawn slightly darker so it reads behind the body.
    let tab = egui::Rect::from_min_size(
        egui::pos2(rect.left(), rect.top() + tab_h * 0.35),
        egui::vec2(tab_w, tab_h * 1.3),
    );
    painter.rect_filled(
        tab,
        egui::Rounding {
            nw: 2.0,
            ne: 2.0,
            sw: 0.0,
            se: 0.0,
        },
        color.gamma_multiply(0.7),
    );
    // Folder body.
    let body = egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.top() + tab_h),
        egui::pos2(rect.right(), rect.bottom() - 0.5),
    );
    painter.rect_filled(body, egui::Rounding::same(2.0), color);
}

/// Draw a compact, filled document glyph with a dog-eared corner inside `rect`.
fn paint_tree_file(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let body = rect.shrink2(egui::vec2(rect.width() * 0.17, 0.5));
    let fold = body.width() * 0.42;

    let p_tl = body.left_top();
    let p_fold_top = egui::pos2(body.right() - fold, body.top());
    let p_fold_corner = egui::pos2(body.right() - fold, body.top() + fold);
    let p_fold_side = egui::pos2(body.right(), body.top() + fold);
    let p_br = body.right_bottom();
    let p_bl = body.left_bottom();

    // Page body: cutting the top-right corner keeps the polygon convex.
    painter.add(egui::Shape::convex_polygon(
        vec![p_tl, p_fold_top, p_fold_side, p_br, p_bl],
        color,
        egui::Stroke::NONE,
    ));
    // Folded corner, darker, to read as a dog-ear.
    painter.add(egui::Shape::convex_polygon(
        vec![p_fold_top, p_fold_corner, p_fold_side],
        color.gamma_multiply(0.5),
        egui::Stroke::NONE,
    ));
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{FileTree, FsNode};

    struct TestDir(PathBuf);

    use std::path::PathBuf;

    impl TestDir {
        fn new(label: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("blue_ide_{label}_{unique}"));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn dir_children(node: &FsNode) -> &[FsNode] {
        match node {
            FsNode::Dir { children, .. } => children,
            FsNode::File { .. } => panic!("expected directory"),
        }
    }

    fn node_names(nodes: &[FsNode]) -> Vec<&str> {
        nodes
            .iter()
            .map(|node| match node {
                FsNode::File { name, .. } | FsNode::Dir { name, .. } => name.as_str(),
            })
            .collect()
    }

    #[test]
    fn load_reads_one_level_and_filters_and_sorts_entries() {
        let temp = TestDir::new("tree_load");
        fs::create_dir(temp.0.join("z_dir")).unwrap();
        fs::create_dir(temp.0.join("a_dir")).unwrap();
        fs::create_dir(temp.0.join("target")).unwrap();
        fs::write(temp.0.join("b.rs"), "b").unwrap();
        fs::write(temp.0.join("A.rs"), "a").unwrap();
        fs::write(temp.0.join(".hidden"), "hidden").unwrap();
        fs::write(temp.0.join("a_dir").join("nested.rs"), "nested").unwrap();

        let mut tree = FileTree::default();
        tree.load(temp.0.clone()).unwrap();

        let root = tree.root.as_ref().unwrap();
        assert!(matches!(
            root,
            FsNode::Dir {
                expanded: true,
                loaded: true,
                ..
            }
        ));
        let children = dir_children(root);
        assert_eq!(node_names(children), ["a_dir", "z_dir", "A.rs", "b.rs"]);
        assert!(matches!(
            &children[0],
            FsNode::Dir { children, loaded: false, .. } if children.is_empty()
        ));
    }

    #[test]
    fn expanding_loads_a_directory_only_once() {
        let temp = TestDir::new("tree_lazy");
        let src = temp.0.join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("first.rs"), "first").unwrap();

        let mut tree = FileTree::default();
        tree.load(temp.0.clone()).unwrap();
        tree.toggle_dir(&src).unwrap();
        fs::write(src.join("later.rs"), "later").unwrap();
        tree.toggle_dir(&src).unwrap();
        tree.toggle_dir(&src).unwrap();

        let src_node = dir_children(tree.root.as_ref().unwrap()).first().unwrap();
        assert_eq!(node_names(dir_children(src_node)), ["first.rs"]);
        assert!(matches!(
            src_node,
            FsNode::Dir {
                expanded: true,
                loaded: true,
                ..
            }
        ));
    }

    #[test]
    fn depth_six_directories_are_visible_but_not_read() {
        let temp = TestDir::new("tree_depth");
        let mut current = temp.0.clone();
        for depth in 1..=7 {
            current = current.join(format!("d{depth}"));
            fs::create_dir(&current).unwrap();
        }

        let mut tree = FileTree::default();
        tree.load(temp.0.clone()).unwrap();
        current = temp.0.clone();
        for depth in 1..=6 {
            current = current.join(format!("d{depth}"));
            tree.toggle_dir(&current).unwrap();
        }

        let mut node = tree.root.as_ref().unwrap();
        for _ in 1..=6 {
            node = dir_children(node).first().unwrap();
        }
        assert!(matches!(node, FsNode::Dir { children, loaded: true, .. } if children.is_empty()));
    }

    #[test]
    fn failed_load_preserves_the_existing_tree() {
        let temp = TestDir::new("tree_transaction");
        fs::write(temp.0.join("main.rs"), "main").unwrap();
        let mut tree = FileTree::default();
        tree.load(temp.0.clone()).unwrap();

        let missing = temp.0.join("missing");
        assert!(tree.load(missing).is_err());
        assert_eq!(tree.root_path.as_deref(), Some(temp.0.as_path()));
        assert_eq!(
            node_names(dir_children(tree.root.as_ref().unwrap())),
            ["main.rs"]
        );
    }

    #[test]
    fn create_file_adds_file_and_returns_path() {
        let temp = TestDir::new("tree_create_file");
        let mut tree = FileTree::default();
        tree.load(temp.0.clone()).unwrap();

        tree.start_create(super::CreateKind::File, temp.0.clone());
        tree.create_state.as_mut().unwrap().name = "hello.rs".to_string();
        let created = tree.confirm_create().unwrap();

        assert_eq!(created, Some(temp.0.join("hello.rs")));
        assert!(temp.0.join("hello.rs").is_file());
        assert_eq!(
            node_names(dir_children(tree.root.as_ref().unwrap())),
            ["hello.rs"]
        );
    }

    #[test]
    fn create_folder_adds_directory() {
        let temp = TestDir::new("tree_create_folder");
        let mut tree = FileTree::default();
        tree.load(temp.0.clone()).unwrap();

        tree.start_create(super::CreateKind::Folder, temp.0.clone());
        tree.create_state.as_mut().unwrap().name = "src".to_string();
        let created = tree.confirm_create().unwrap();

        assert_eq!(created, None);
        assert!(temp.0.join("src").is_dir());
        assert_eq!(
            node_names(dir_children(tree.root.as_ref().unwrap())),
            ["src"]
        );
    }

    #[test]
    fn create_file_with_nested_path_creates_parent_directories() {
        let temp = TestDir::new("tree_create_nested");
        let mut tree = FileTree::default();
        tree.load(temp.0.clone()).unwrap();

        tree.start_create(super::CreateKind::File, temp.0.clone());
        tree.create_state.as_mut().unwrap().name = "src/main.rs".to_string();
        let created = tree.confirm_create().unwrap();

        assert_eq!(created, Some(temp.0.join("src/main.rs")));
        assert!(temp.0.join("src/main.rs").is_file());
    }

    #[test]
    fn empty_create_name_is_a_no_op() {
        let temp = TestDir::new("tree_create_empty");
        let mut tree = FileTree::default();
        tree.load(temp.0.clone()).unwrap();

        tree.start_create(super::CreateKind::File, temp.0.clone());
        tree.create_state.as_mut().unwrap().name = "   ".to_string();
        let created = tree.confirm_create().unwrap();

        assert_eq!(created, None);
        assert!(dir_children(tree.root.as_ref().unwrap()).is_empty());
    }

    #[test]
    fn cancel_create_clears_state() {
        let temp = TestDir::new("tree_create_cancel");
        let mut tree = FileTree::default();
        tree.load(temp.0.clone()).unwrap();

        tree.start_create(super::CreateKind::File, temp.0.clone());
        tree.cancel_create();

        assert!(tree.create_state.is_none());
    }

    #[test]
    fn test_multi_root_and_excludes() {
        let temp1 = TestDir::new("multi_root_1");
        let temp2 = TestDir::new("multi_root_2");
        
        fs::write(temp1.0.join("file1.rs"), "a").unwrap();
        fs::write(temp1.0.join("ignored.rs"), "ignored").unwrap();
        let blue_dir = temp1.0.join(".blue");
        fs::create_dir(&blue_dir).unwrap();
        fs::write(blue_dir.join("exclude"), "ignored.rs\n").unwrap();

        fs::write(temp2.0.join("file2.rs"), "b").unwrap();

        let mut tree = FileTree::default();
        tree.add_root(temp1.0.clone()).unwrap();
        tree.add_root(temp2.0.clone()).unwrap();

        // Root path should be virtual
        assert_eq!(tree.root_path.as_deref(), Some(std::path::Path::new("workspace://")));
        
        let root = tree.root.as_ref().unwrap();
        let children = dir_children(root);
        
        // Should have 2 children: the two roots
        assert_eq!(children.len(), 2);
        
        // Assert temp1 children (should NOT have ignored.rs)
        let root1_children = dir_children(&children[0]);
        assert_eq!(node_names(root1_children), ["file1.rs"]);
        
        // Assert temp2 children
        let root2_children = dir_children(&children[1]);
        assert_eq!(node_names(root2_children), ["file2.rs"]);
    }
}
