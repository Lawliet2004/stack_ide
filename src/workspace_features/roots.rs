//! Feature 2 — Multiple Workspace Roots
//!
//! Manages a `Vec<PathBuf>` of workspace roots.  Persists them to
//! `~/.stack_ide/workspace_roots.json`.  Provides a context menu per root:
//! Remove, Open in Terminal, Reveal in File Explorer.

use std::fs;
use std::path::{Path, PathBuf};

// ─── Persistence ─────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Debug, Default, Clone)]
struct RootsFile {
    roots: Vec<String>,
}

/// Returns the workspace roots file path.
pub fn roots_file_path() -> Option<PathBuf> {
    directories::BaseDirs::new()
        .map(|b| b.home_dir().join(".stack_ide").join("workspace_roots.json"))
}

/// Load the persisted workspace roots list.
///
/// Falls back to `[initial_root]` if the file does not exist.
pub fn load_roots(initial_root: Option<&Path>) -> Vec<PathBuf> {
    if let Some(path) = roots_file_path() {
        if path.is_file() {
            if let Ok(json) = fs::read_to_string(&path) {
                if let Ok(f) = serde_json::from_str::<RootsFile>(&json) {
                    let roots: Vec<PathBuf> = f
                        .roots
                        .into_iter()
                        .map(PathBuf::from)
                        .filter(|p| p.is_dir())
                        .collect();
                    if !roots.is_empty() {
                        return roots;
                    }
                }
            }
        }
    }
    // No file or empty — seed with the initial root if provided.
    if let Some(root) = initial_root {
        if root.is_dir() {
            let roots = vec![root.to_path_buf()];
            save_roots(&roots);
            return roots;
        }
    }
    Vec::new()
}

/// Persist the workspace roots list to `~/.stack_ide/workspace_roots.json`.
pub fn save_roots(roots: &[PathBuf]) {
    let Some(path) = roots_file_path() else { return };
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("workspace roots: could not create dir: {e}");
            return;
        }
    }
    let file = RootsFile {
        roots: roots.iter().map(|p| p.to_string_lossy().to_string()).collect(),
    };
    match serde_json::to_string_pretty(&file) {
        Ok(json) => {
            if let Err(e) = fs::write(&path, json) {
                eprintln!("workspace roots: save error: {e}");
            }
        }
        Err(e) => eprintln!("workspace roots: serialize error: {e}"),
    }
}

// ─── Mutation helpers ─────────────────────────────────────────────────────────

/// Append `path` to `roots` if not already present.  Saves to disk.
pub fn add_root(roots: &mut Vec<PathBuf>, path: PathBuf) {
    if !roots.contains(&path) {
        roots.push(path);
        save_roots(roots);
    }
}

/// Remove `path` from `roots`.  Saves to disk.  Does not delete any files.
pub fn remove_root(roots: &mut Vec<PathBuf>, path: &Path) {
    roots.retain(|r| r != path);
    save_roots(roots);
}

// ─── OS operations ────────────────────────────────────────────────────────────

/// Open a terminal at `path` (platform-appropriate shell).
pub fn open_in_terminal(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", "cmd.exe"])
            .current_dir(path)
            .spawn()
            .unwrap_or_else(|e| { eprintln!("open terminal: {e}"); std::process::exit(0) });
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", "cmd.exe"])
            .current_dir(path)
            .spawn();
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Try common terminal emulators.
        for term in &["gnome-terminal", "xterm", "konsole", "xfce4-terminal"] {
            if std::process::Command::new(term)
                .current_dir(path)
                .spawn()
                .is_ok()
            {
                return;
            }
        }
    }
}

/// Reveal `path` in the OS native file explorer.
pub fn reveal_in_explorer(path: &Path) {
    if let Err(e) = open::that(path) {
        eprintln!("workspace roots: reveal in explorer: {e}");
    }
}

// ─── UI context-menu action ───────────────────────────────────────────────────

/// Actions that the per-root context menu can return.
#[derive(Debug, Clone, PartialEq)]
pub enum RootContextAction {
    Remove(PathBuf),
    OpenInTerminal(PathBuf),
    RevealInExplorer(PathBuf),
}

/// Render a context menu for a workspace root header.
///
/// Returns `Some(action)` when the user picks a menu item.
pub fn render_root_context_menu(
    ui: &mut egui::Ui,
    root: &Path,
) -> Option<RootContextAction> {
    let mut action = None;
    if ui.button("Remove from Workspace").clicked() {
        action = Some(RootContextAction::Remove(root.to_path_buf()));
        ui.close_menu();
    }
    if ui.button("Open in Terminal").clicked() {
        action = Some(RootContextAction::OpenInTerminal(root.to_path_buf()));
        ui.close_menu();
    }
    if ui.button("Reveal in File Explorer").clicked() {
        action = Some(RootContextAction::RevealInExplorer(root.to_path_buf()));
        ui.close_menu();
    }
    action
}

/// Render the multi-root file tree sidebar.
///
/// Each root is a bold collapsible section header.  Right-clicking a header
/// opens a context menu with the three root-level actions.
///
/// Returns the path the user clicked to open (if any).
pub fn render_roots_sidebar(
    ui: &mut egui::Ui,
    roots: &mut Vec<PathBuf>,
    expanded: &mut std::collections::HashMap<PathBuf, bool>,
    file_tree: &mut crate::filetree::FileTree,
    active_path: Option<&std::path::Path>,
    file_statuses: &std::collections::HashMap<PathBuf, crate::git::FileStatus>,
) -> (Option<PathBuf>, Option<RootContextAction>) {
    let mut open_file = None;
    let mut context_action = None;

    // Clone roots to avoid borrow conflicts during iteration.
    let root_list: Vec<PathBuf> = roots.clone();

    for root in &root_list {
        let is_expanded = *expanded.entry(root.clone()).or_insert(true);
        let label = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| root.display().to_string());

        let marker = if is_expanded { "▼" } else { "▶" };
        let header_text = format!("{marker} {label}");

        let resp = ui.add(
            egui::Button::new(egui::RichText::new(header_text).strong())
                .frame(false)
                .min_size(egui::vec2(ui.available_width(), 22.0)),
        );

        // Context menu on right-click.
        resp.context_menu(|ui| {
            if let Some(act) = render_root_context_menu(ui, root) {
                context_action = Some(act);
            }
        });

        if resp.clicked() {
            let entry = expanded.entry(root.clone()).or_insert(true);
            *entry = !*entry;
        }

        if is_expanded {
            // Render the subtree for this root.
            ui.indent(egui::Id::new(root.as_os_str()), |ui| {
                // Use the existing FileTree render for this subtree.
                // We temporarily set single-root mode via the tree's roots field.
                let prev_roots = file_tree.roots.clone();
                file_tree.roots = vec![root.clone()];
                if let Err(e) = file_tree.rebuild_virtual_root() {
                    eprintln!("workspace roots: rebuild: {e}");
                }
                match file_tree.render(ui, active_path, file_statuses) {
                    Ok(crate::filetree::FileTreeAction::Open(p)) => open_file = Some(p),
                    Ok(crate::filetree::FileTreeAction::None) => {}
                    Err(e) => eprintln!("workspace roots: file tree render: {e}"),
                }
                // Restore the full roots list.
                file_tree.roots = prev_roots;
                if let Err(e) = file_tree.rebuild_virtual_root() {
                    eprintln!("workspace roots: restore rebuild: {e}");
                }
            });
        }
    }

    (open_file, context_action)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_dir() -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("ws_roots_test_{n}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn add_and_remove_root() {
        let a = tmp_dir();
        let b = tmp_dir();
        let mut roots: Vec<PathBuf> = Vec::new();
        add_root(&mut roots, a.clone());
        add_root(&mut roots, a.clone()); // duplicate
        assert_eq!(roots.len(), 1);
        add_root(&mut roots, b.clone());
        assert_eq!(roots.len(), 2);
        remove_root(&mut roots, &a);
        assert_eq!(roots, vec![b.clone()]);
        fs::remove_dir_all(a).unwrap();
        fs::remove_dir_all(b).unwrap();
    }
}
