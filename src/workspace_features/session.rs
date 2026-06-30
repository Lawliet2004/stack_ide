//! Feature 1 — Session Restore
//!
//! Persists open files, scroll positions, cursor positions, active tab, and
//! pane layout to `[project_root]/.stack_ide/session.json`.  On startup the
//! session is restored; missing files are silently skipped.

use std::fs;
use std::path::{Path, PathBuf};

// ─── Public types ─────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct SessionState {
    pub open_files: Vec<OpenFileState>,
    /// Index into `open_files` that was active at save time.
    pub active_tab_index: usize,
    pub pane_layout: PaneLayout,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct OpenFileState {
    /// Absolute path as a string.
    pub absolute_path: String,
    /// First visible line number (0-indexed).
    pub scroll_line: usize,
    /// Cursor line (0-indexed).
    pub cursor_line: usize,
    /// Cursor column (0-indexed).
    pub cursor_col: usize,
    /// True if the buffer had unsaved changes.
    pub is_dirty: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum PaneLayout {
    Single,
    SplitHorizontal { left_file: String, right_file: String },
    SplitVertical { top_file: String, bottom_file: String },
}

// ─── Session file path ────────────────────────────────────────────────────────

/// Returns `.stack_ide/session.json` under `project_root`.
pub fn session_path(project_root: &Path) -> PathBuf {
    project_root.join(".stack_ide").join("session.json")
}

// ─── Save ─────────────────────────────────────────────────────────────────────

/// Persist `state` to disk.  Creates `.stack_ide/` if needed.
pub fn save(project_root: &Path, state: &SessionState) -> std::io::Result<()> {
    let path = session_path(project_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json =
        serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    // Atomic write: write to temp file then rename.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(tmp, path)
}

// ─── Load ─────────────────────────────────────────────────────────────────────

/// Load the session for `project_root`.
///
/// Returns `None` when no session file exists.
/// Files listed in the session that no longer exist on disk are silently dropped.
pub fn load(project_root: &Path) -> Option<SessionState> {
    let path = session_path(project_root);
    if !path.exists() {
        return None;
    }
    let json = fs::read_to_string(&path)
        .unwrap_or_else(|e| { eprintln!("session: read error: {e}"); String::new() });
    if json.is_empty() {
        return None;
    }
    let mut state: SessionState = serde_json::from_str(&json)
        .unwrap_or_else(|e| { eprintln!("session: parse error: {e}"); return SessionState::default(); });

    // Drop files that no longer exist on disk.
    state.open_files.retain(|f| Path::new(&f.absolute_path).is_file());

    // Clamp active_tab_index so it remains in bounds.
    if !state.open_files.is_empty() && state.active_tab_index >= state.open_files.len() {
        state.active_tab_index = state.open_files.len() - 1;
    }

    Some(state)
}

// ─── Clear ────────────────────────────────────────────────────────────────────

/// Delete the session file (File → "Clear Session").
pub fn clear(project_root: &Path) {
    let path = session_path(project_root);
    if path.exists() {
        fs::remove_file(&path)
            .unwrap_or_else(|e| eprintln!("session: could not delete: {e}"));
    }
}

// ─── Default ─────────────────────────────────────────────────────────────────

impl Default for SessionState {
    fn default() -> Self {
        Self {
            open_files: Vec::new(),
            active_tab_index: 0,
            pane_layout: PaneLayout::Single,
        }
    }
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
        let p = std::env::temp_dir().join(format!("ws_session_test_{n}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn save_and_load_roundtrip() {
        let root = tmp_dir();
        // Create a real file so the load filter keeps it.
        let f = root.join("main.rs");
        fs::write(&f, "fn main(){}").unwrap();

        let state = SessionState {
            open_files: vec![OpenFileState {
                absolute_path: f.to_string_lossy().to_string(),
                scroll_line: 3,
                cursor_line: 5,
                cursor_col: 10,
                is_dirty: false,
            }],
            active_tab_index: 0,
            pane_layout: PaneLayout::Single,
        };
        save(&root, &state).unwrap();
        let loaded = load(&root).unwrap();
        assert_eq!(loaded.active_tab_index, 0);
        assert_eq!(loaded.open_files[0].cursor_line, 5);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_files_are_silently_dropped() {
        let root = tmp_dir();
        let state = SessionState {
            open_files: vec![OpenFileState {
                absolute_path: "/nonexistent/path/file.rs".into(),
                scroll_line: 0,
                cursor_line: 0,
                cursor_col: 0,
                is_dirty: false,
            }],
            active_tab_index: 0,
            pane_layout: PaneLayout::Single,
        };
        save(&root, &state).unwrap();
        let loaded = load(&root).unwrap();
        assert!(loaded.open_files.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn clear_removes_file() {
        let root = tmp_dir();
        let state = SessionState::default();
        save(&root, &state).unwrap();
        assert!(session_path(&root).exists());
        clear(&root);
        assert!(!session_path(&root).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn no_session_file_returns_none() {
        let root = tmp_dir();
        assert!(load(&root).is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
