//! Diff computation against HEAD using `git2`.
//!
//! `diff_file_against_head` builds an in-memory diff between the blob at HEAD for a
//! path and the current buffer text (which may be unsaved). The result is a list of
//! hunks suitable for painting in the editor gutter.

use std::path::PathBuf;

use git2::{DiffLineType, DiffOptions, Oid, Repository};

/// Classification of a changed region.
#[derive(Debug, Clone, PartialEq)]
pub enum HunkKind {
    Added,
    Removed,
    Modified,
}

/// A consecutive run of changed lines in the current file.
#[derive(Debug, Clone)]
pub struct DiffHunk {
    pub kind: HunkKind,
    /// 0-indexed starting line in the current file.
    pub line_start: usize,
    pub line_count: usize,
}

/// Per-file diff result.
#[derive(Debug, Clone)]
pub struct FileDiff {
    pub path: PathBuf,
    pub hunks: Vec<DiffHunk>,
}

/// Diff `path` (with current buffer content `text`) against the HEAD commit.
///
/// Returns an empty vector when:
/// - the repository cannot be read,
/// - the path is not inside the repo workdir,
/// - the file is untracked,
/// - or any git2 call fails.
///
/// For tracked files we diff the HEAD blob against an in-memory blob built from
/// `text`. For files that do not exist in HEAD we diff against an empty blob, so
/// every line appears as an addition.
pub fn diff_file_against_head(repo: &Repository, path: &PathBuf, text: &str) -> Vec<DiffHunk> {
    // repo.workdir() returns the path to the working directory of the repository.
    // The path we are diffing must live inside this working directory.
    let root = match repo.workdir() {
        Some(root) => root,
        None => return Vec::new(),
    };
    if !path.starts_with(root) {
        return Vec::new();
    }

    // repo.head() returns a Reference to the current HEAD (e.g. branch reference or commit).
    // reference.peel_to_tree() dereferences/resolves the HEAD reference down to the underlying
    // Tree object (representing the snapshot of files in the commit) so we can locate the file at HEAD.
    // HEAD may be missing in an empty repository (e.g., right after `git init`).
    let head_tree = match repo
        .head()
        .ok()
        .and_then(|reference| reference.peel_to_tree().ok())
    {
        Some(tree) => tree,
        None => return Vec::new(),
    };

    // Relative path is what git's tree and diff APIs expect.
    let rel = match path.strip_prefix(root) {
        Ok(rel) => rel,
        Err(_) => return Vec::new(),
    };
    let rel_str = match rel.to_str() {
        Some(s) => s,
        None => return Vec::new(),
    };

    // repo.status_file(rel) queries the status of a single file in the repository index and working directory.
    // Untracked files (status `is_wt_new()`) have no HEAD content and no index entry; the gutter stays empty.
    if let Ok(status) = repo.status_file(rel) {
        if status.is_wt_new() {
            return Vec::new();
        }
    }

    // head_tree.get_path(rel) looks up the entry at the specified relative path in the HEAD Tree.
    // If the file exists in the HEAD tree, we retrieve its unique Git Object ID (Oid) to find the blob.
    let head_blob = match head_tree.get_path(rel) {
        Ok(entry) => entry.id(),
        Err(_) => Oid::zero(),
    };

    // repo.blob(bytes) creates a new in-memory Git blob containing the specified byte slice.
    // This allows us to perform a standard diff against the current unsaved text in the editor buffer
    // without needing to save the content to disk first.
    let new_blob_id = match repo.blob(text.as_bytes()) {
        Ok(oid) => oid,
        Err(error) => {
            eprintln!(
                "git2: failed to create in-memory blob for {}: {error}",
                path.display()
            );
            return Vec::new();
        }
    };

    // repo.find_blob(oid) fetches the Git Blob object corresponding to the given Oid.
    // If the file didn't exist at HEAD, we'll diff against an empty/None blob (treating all lines as additions).
    let old_blob = if head_blob.is_zero() {
        None
    } else {
        match repo.find_blob(head_blob) {
            Ok(blob) => Some(blob),
            Err(error) => {
                eprintln!(
                    "git2: failed to read HEAD blob for {}: {error}",
                    path.display()
                );
                return Vec::new();
            }
        }
    };

    // Retrieve the newly created in-memory blob representing the editor buffer contents.
    let new_blob = match repo.find_blob(new_blob_id) {
        Ok(blob) => Some(blob),
        Err(error) => {
            eprintln!(
                "git2: failed to find new blob for {}: {error}",
                path.display()
            );
            return Vec::new();
        }
    };

    // DiffOptions allows configuration of diff behavior.
    // opts.pathspec(rel_str) limits the diff process strictly to the path of interest.
    let mut opts = DiffOptions::new();
    opts.pathspec(rel_str);

    // Collect every added/deleted line reported by git2. `diff_blobs` in git2 0.18
    // invokes callbacks directly rather than returning a `Diff` object.
    let mut lines: Vec<(HunkKind, usize)> = Vec::new();
    let mut line_cb = |_delta: git2::DiffDelta<'_>,
                       _hunk: Option<git2::DiffHunk<'_>>,
                       line: git2::DiffLine<'_>| {
        // line.origin_value() returns the DiffLineType (e.g. Addition, Deletion, Context).
        let kind = match line.origin_value() {
            DiffLineType::Addition => HunkKind::Added,
            DiffLineType::Deletion => HunkKind::Removed,
            // Context, headers, and other markers do not contribute to hunks.
            _ => return true,
        };

        // `new_lineno` is 1-indexed in the new blob; for deletions it is None.
        // `old_lineno` is 1-indexed in the old blob; for additions it is None.
        let anchor = match kind {
            HunkKind::Added => line.new_lineno().unwrap_or(1) as usize,
            HunkKind::Removed => line.old_lineno().unwrap_or(1) as usize,
            HunkKind::Modified => unreachable!(),
        };

        lines.push((kind, anchor));
        true
    };

    // repo.diff_blobs() performs a line-by-line diff between two git blobs.
    // It calls line_cb for each line of difference, passing diff options to restrict path/filtering.
    if let Err(error) = repo.diff_blobs(
        old_blob.as_ref(),
        Some(rel_str),
        new_blob.as_ref(),
        Some(rel_str),
        Some(&mut opts),
        None,
        None,
        None,
        Some(&mut line_cb),
    ) {
        eprintln!("git2: failed to diff blobs for {}: {error}", path.display());
        return Vec::new();
    }

    group_lines(lines)
}

/// Group a flat list of added/removed lines into consecutive hunks.
///
/// A run that contains both additions and deletions is reported as `Modified`.
/// Runs of only additions or only deletions become `Added` or `Removed`
/// respectively. Line numbers are converted to 0-indexed values.
fn group_lines(lines: Vec<(HunkKind, usize)>) -> Vec<DiffHunk> {
    if lines.is_empty() {
        return Vec::new();
    }

    let mut hunks: Vec<DiffHunk> = Vec::new();

    for (kind, line_num) in lines {
        let line_idx = line_num.saturating_sub(1);

        let mut merged = false;
        if let Some(hunk) = hunks.last_mut() {
            let hunk_end = hunk.line_start + hunk.line_count;

            match (&hunk.kind, &kind) {
                (HunkKind::Added, HunkKind::Added) => {
                    if line_idx == hunk_end {
                        hunk.line_count += 1;
                        merged = true;
                    }
                }
                (HunkKind::Removed, HunkKind::Removed) => {
                    if line_idx == hunk_end {
                        hunk.line_count += 1;
                        merged = true;
                    }
                }
                (HunkKind::Added, HunkKind::Removed) | (HunkKind::Removed, HunkKind::Added) => {
                    if line_idx >= hunk.line_start && line_idx <= hunk_end {
                        hunk.kind = HunkKind::Modified;
                        merged = true;
                    }
                }
                (HunkKind::Modified, _) => {
                    if line_idx >= hunk.line_start && line_idx <= hunk_end {
                        merged = true;
                    } else if line_idx == hunk_end {
                        hunk.line_count += 1;
                        merged = true;
                    }
                }
                _ => {}
            }
        }

        if !merged {
            hunks.push(DiffHunk {
                kind,
                line_start: line_idx,
                line_count: 1,
            });
        }
    }

    hunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_lines_empty() {
        assert!(group_lines(Vec::new()).is_empty());
    }

    #[test]
    fn group_lines_added_run() {
        let lines = vec![
            (HunkKind::Added, 5),
            (HunkKind::Added, 6),
            (HunkKind::Added, 7),
        ];
        let hunks = group_lines(lines);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].kind, HunkKind::Added);
        assert_eq!(hunks[0].line_start, 4);
        assert_eq!(hunks[0].line_count, 3);
    }

    #[test]
    fn group_lines_removed_run() {
        let lines = vec![(HunkKind::Removed, 2), (HunkKind::Removed, 3)];
        let hunks = group_lines(lines);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].kind, HunkKind::Removed);
        assert_eq!(hunks[0].line_start, 1);
        assert_eq!(hunks[0].line_count, 2);
    }

    #[test]
    fn group_lines_mixed_becomes_modified() {
        let lines = vec![
            (HunkKind::Removed, 3),
            (HunkKind::Added, 3),
            (HunkKind::Added, 4),
        ];
        let hunks = group_lines(lines);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].kind, HunkKind::Modified);
    }

    #[test]
    fn group_lines_splits_on_gap() {
        let lines = vec![
            (HunkKind::Added, 1),
            (HunkKind::Added, 2),
            (HunkKind::Added, 5),
            (HunkKind::Added, 6),
        ];
        let hunks = group_lines(lines);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].line_start, 0);
        assert_eq!(hunks[1].line_start, 4);
    }

    #[test]
    fn diff_against_head_reports_modified_and_added_lines() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("blue_ide_git_diff_test_{unique}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let repo = git2::Repository::init(&dir).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();

        let path = dir.join("file.txt");
        std::fs::write(&path, "line one\nline two\nline three\n").unwrap();

        // Commit the initial content.
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        // Diff against modified unsaved buffer text.
        let modified = "line one\nline TWO changed\nline three\nline four new\n";
        let hunks = diff_file_against_head(&repo, &path, modified);

        // Should report one modified region and one added region.
        assert!(!hunks.is_empty());
        assert!(hunks.iter().any(|h| h.kind == HunkKind::Modified));
        assert!(hunks.iter().any(|h| h.kind == HunkKind::Added));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn untracked_file_returns_empty_diff() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("blue_ide_git_untracked_test_{unique}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let repo = git2::Repository::init(&dir).unwrap();
        let path = dir.join("new.txt");
        std::fs::write(&path, "new content\n").unwrap();

        let hunks = diff_file_against_head(&repo, &path, "new content\n");
        assert!(hunks.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
