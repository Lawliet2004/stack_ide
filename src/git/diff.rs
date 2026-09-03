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
    /// 0-indexed starting line in the current file (gutter coordinates).
    pub line_start: usize,
    /// Number of gutter lines occupied in the current file.
    pub line_count: usize,
    /// 0-indexed starting line in the file at HEAD.
    pub old_line_start: usize,
    /// Number of lines at HEAD covered by this hunk.
    pub old_line_count: usize,
    /// 0-indexed starting line in the current working content.
    pub new_line_start: usize,
    /// Number of lines in the current working content covered by this hunk.
    pub new_line_count: usize,
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
pub fn diff_file_against_head(repo: &Repository, path: &PathBuf, text: &str) -> Vec<DiffHunk> {
    let Some(root) = repo.workdir() else {
        return Vec::new();
    };
    if !path.starts_with(root) {
        return Vec::new();
    }
    let Ok(rel) = path.strip_prefix(root) else {
        return Vec::new();
    };
    let Some(rel_str) = rel.to_str() else {
        return Vec::new();
    };

    // Untracked files (status `is_wt_new()`) have no HEAD content and no index
    // entry; the gutter stays empty.
    if let Ok(status) = repo.status_file(rel) {
        if status.is_wt_new() {
            return Vec::new();
        }
    }

    let old_text = old_content(repo, path).unwrap_or_default();
    diff_texts(repo, rel_str, &old_text, text)
}

/// Diff `path` (with current buffer content `text`) against the current index.
///
/// These are the *unstaged* hunks: the parts the user has not yet staged.
pub fn diff_file_against_index(repo: &Repository, path: &PathBuf, text: &str) -> Vec<DiffHunk> {
    let Some(root) = repo.workdir() else {
        return Vec::new();
    };
    if !path.starts_with(root) {
        return Vec::new();
    }
    let Ok(rel) = path.strip_prefix(root) else {
        return Vec::new();
    };
    let Some(rel_str) = rel.to_str() else {
        return Vec::new();
    };
    let old_text = index_content(repo, path).unwrap_or_default();
    diff_texts(repo, rel_str, &old_text, text)
}

/// Diff the index against HEAD for `path`.
///
/// These are the *staged* hunks: the parts the user has added to the index.
pub fn diff_index_against_head(repo: &Repository, path: &PathBuf) -> Vec<DiffHunk> {
    let Some(root) = repo.workdir() else {
        return Vec::new();
    };
    if !path.starts_with(root) {
        return Vec::new();
    }
    let Ok(rel) = path.strip_prefix(root) else {
        return Vec::new();
    };
    let Some(rel_str) = rel.to_str() else {
        return Vec::new();
    };
    let old_text = old_content(repo, path).unwrap_or_default();
    let new_text = index_content(repo, path).unwrap_or_default();
    diff_texts(repo, rel_str, &old_text, &new_text)
}

/// Diff two in-memory text blobs and group the result into hunks.
fn diff_texts(repo: &Repository, rel_str: &str, old_text: &str, new_text: &str) -> Vec<DiffHunk> {
    let old_oid = match repo.blob(old_text.as_bytes()) {
        Ok(oid) => oid,
        Err(error) => {
            eprintln!("git2: failed to create old blob for {}: {error}", rel_str);
            return Vec::new();
        }
    };
    let new_oid = match repo.blob(new_text.as_bytes()) {
        Ok(oid) => oid,
        Err(error) => {
            eprintln!("git2: failed to create new blob for {}: {error}", rel_str);
            return Vec::new();
        }
    };
    let old_blob = match repo.find_blob(old_oid) {
        Ok(blob) => blob,
        Err(error) => {
            eprintln!("git2: failed to read old blob for {}: {error}", rel_str);
            return Vec::new();
        }
    };
    let new_blob = match repo.find_blob(new_oid) {
        Ok(blob) => blob,
        Err(error) => {
            eprintln!("git2: failed to read new blob for {}: {error}", rel_str);
            return Vec::new();
        }
    };

    // DiffOptions allows configuration of diff behavior.
    // opts.pathspec(rel_str) limits the diff process strictly to the path of interest.
    let mut opts = DiffOptions::new();
    opts.pathspec(rel_str);

    // Collect every added/deleted line reported by git2. `diff_blobs` in git2 0.18
    // invokes callbacks directly rather than returning a `Diff` object. We keep
    // both the old-file and new-file coordinates so per-hunk staging can rebuild
    // the index without touching the working tree.
    let mut events: Vec<DiffEvent> = Vec::new();
    let mut old_pos = 0usize;
    let mut new_pos = 0usize;
    let mut line_cb = |_delta: git2::DiffDelta<'_>,
                       _hunk: Option<git2::DiffHunk<'_>>,
                       line: git2::DiffLine<'_>| {
        let kind = match line.origin_value() {
            DiffLineType::Addition => Some(HunkKind::Added),
            DiffLineType::Deletion => Some(HunkKind::Removed),
            // Context, headers, and other markers do not contribute to hunks, but
            // they still advance the old/new file positions.
            _ => None,
        };

        if let Some(kind) = kind {
            let old_index = old_pos;
            let new_index = new_pos;
            let (old_count, new_count) = match kind {
                HunkKind::Added => (0, 1),
                HunkKind::Removed => (1, 0),
                HunkKind::Modified => unreachable!(),
            };
            events.push(DiffEvent {
                kind,
                old_start: old_index,
                old_count,
                new_start: new_index,
                new_count,
            });
        }

        match line.origin_value() {
            DiffLineType::Addition => new_pos += 1,
            DiffLineType::Deletion => old_pos += 1,
            DiffLineType::Context => {
                old_pos += 1;
                new_pos += 1;
            }
            _ => {}
        }
        true
    };

    // repo.diff_blobs() performs a line-by-line diff between two git blobs.
    // It calls line_cb for each line of difference, passing diff options to restrict path/filtering.
    if let Err(error) = repo.diff_blobs(
        Some(&old_blob),
        Some(rel_str),
        Some(&new_blob),
        Some(rel_str),
        Some(&mut opts),
        None,
        None,
        None,
        Some(&mut line_cb),
    ) {
        eprintln!("git2: failed to diff blobs for {}: {error}", rel_str);
        return Vec::new();
    }

    group_events(events)
}

/// One changed line with its old/new file coordinates.
#[derive(Debug, Clone, Copy)]
struct DiffEvent {
    kind: HunkKind,
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
}

/// Group flat diff events into consecutive hunks.
///
/// A run that contains both additions and deletions is reported as `Modified`.
/// Runs of only additions or only deletions become `Added` or `Removed`.
fn group_events(events: Vec<DiffEvent>) -> Vec<DiffHunk> {
    if events.is_empty() {
        return Vec::new();
    }

    let mut hunks: Vec<DiffHunk> = Vec::new();
    for event in events {
        if let Some(hunk) = hunks.last_mut() {
            let old_touches = ranges_touch(
                hunk.old_line_start,
                hunk.old_line_count,
                event.old_start,
                event.old_count,
            );
            let new_touches = ranges_touch(
                hunk.new_line_start,
                hunk.new_line_count,
                event.new_start,
                event.new_count,
            );

            if old_touches && new_touches {
                merge_event(hunk, event);
                continue;
            }
        }

        hunks.push(DiffHunk {
            kind: event.kind,
            line_start: if event.kind == HunkKind::Removed {
                event.old_start
            } else {
                event.new_start
            },
            line_count: if event.kind == HunkKind::Removed {
                event.old_count
            } else {
                event.new_count
            },
            old_line_start: event.old_start,
            old_line_count: event.old_count,
            new_line_start: event.new_start,
            new_line_count: event.new_count,
        });
    }

    hunks
}

/// True when two 0-indexed ranges overlap or are directly adjacent.
fn ranges_touch(a_start: usize, a_count: usize, b_start: usize, b_count: usize) -> bool {
    let a_end = a_start + a_count;
    let b_end = b_start + b_count;
    b_start <= a_end && a_start <= b_end
}

/// Merge `event` into the final hunk in the accumulator.
fn merge_event(hunk: &mut DiffHunk, event: DiffEvent) {
    // Kind coalescing: any mix of additions/deletions becomes Modified.
    let kind = match (&hunk.kind, &event.kind) {
        (HunkKind::Added, HunkKind::Added) => HunkKind::Added,
        (HunkKind::Removed, HunkKind::Removed) => HunkKind::Removed,
        _ => HunkKind::Modified,
    };
    hunk.kind = kind;

    let old_start = hunk.old_line_start.min(event.old_start);
    let old_end = (hunk.old_line_start + hunk.old_line_count).max(event.old_start + event.old_count);
    let new_start = hunk.new_line_start.min(event.new_start);
    let new_end = (hunk.new_line_start + hunk.new_line_count).max(event.new_start + event.new_count);

    hunk.old_line_start = old_start;
    hunk.old_line_count = old_end - old_start;
    hunk.new_line_start = new_start;
    hunk.new_line_count = new_end - new_start;

    // Gutter coordinates: modified hunks span the new-file lines.
    hunk.line_start = hunk.new_line_start;
    hunk.line_count = hunk.new_line_count;
}

/// Stage only one hunk of `path` into the index.
///
/// The working-tree buffer (`text`) is used as the source of the hunk's new
/// lines. The index receives a copy of the HEAD content with just this hunk
/// applied, so changes in other hunks remain unstaged.
pub fn stage_hunk(
    repo: &Repository,
    path: &PathBuf,
    text: &str,
    hunk: &DiffHunk,
) -> Result<(), git2::Error> {
    // The index is already the "old" side for this hunk (it has any earlier
    // staged hunks applied), so staging is a splice of the selected hunk's new
    // lines into the existing index content.
    let current = index_content(repo, path).unwrap_or_default();
    let staged = splice(
        &current,
        hunk.old_line_start,
        hunk.old_line_count,
        text,
        hunk.new_line_start,
        hunk.new_line_count,
    );
    write_index_content(repo, path, &staged)
}

/// Unstage one hunk of `path`, reverting it in the index back to HEAD while
/// keeping the other staged hunks intact.
pub fn unstage_hunk(
    repo: &Repository,
    path: &PathBuf,
    _text: &str,
    hunk: &DiffHunk,
) -> Result<(), git2::Error> {
    let current = index_content(repo, path)
        .ok_or_else(|| git2::Error::from_str("path is not present in the index"))?;
    let old = old_content(repo, path).unwrap_or_default();
    let reverted = splice(
        &current,
        hunk.new_line_start,
        hunk.new_line_count,
        &old,
        hunk.old_line_start,
        hunk.old_line_count,
    );
    write_index_content(repo, path, &reverted)
}

/// Splice `source[source_start..source_start + source_count]` into
/// `base` at `base[base_start..base_start + base_count]`.
///
/// Newlines are preserved by splitting on `\n`; the trailing empty element
/// produced by a final newline is handled naturally by the join.
fn splice(
    base: &str,
    base_start: usize,
    base_count: usize,
    source: &str,
    source_start: usize,
    source_count: usize,
) -> String {
    let base_lines: Vec<&str> = base.split('\n').collect();
    let source_lines: Vec<&str> = source.split('\n').collect();

    let base_start = base_start.min(base_lines.len());
    let base_end = (base_start + base_count).min(base_lines.len());
    let source_start = source_start.min(source_lines.len());
    let source_end = (source_start + source_count).min(source_lines.len());

    let mut out = Vec::new();
    out.extend_from_slice(&base_lines[..base_start]);
    out.extend_from_slice(&source_lines[source_start..source_end]);
    out.extend_from_slice(&base_lines[base_end..]);
    out.join("\n")
}

/// Read the content of `path` at HEAD. Files not present at HEAD (e.g. an
/// index-only new file) return an empty string.
fn old_content(repo: &Repository, path: &PathBuf) -> Option<String> {
    let root = repo.workdir()?;
    let rel = path.strip_prefix(root).ok()?;
    let head = repo.head().ok()?;
    let tree = head.peel_to_tree().ok()?;
    match tree.get_path(rel) {
        Ok(entry) => repo
            .find_blob(entry.id())
            .ok()
            .map(|blob| String::from_utf8_lossy(blob.content()).into_owned()),
        Err(_) => Some(String::new()),
    }
}

/// Read the content currently stored in the index for `path`.
fn index_content(repo: &Repository, path: &PathBuf) -> Option<String> {
    let root = repo.workdir()?;
    let rel = path.strip_prefix(root).ok()?;
    let index = repo.index().ok()?;
    let entry = index.get_path(rel, 0)?;
    let blob = repo.find_blob(entry.id).ok()?;
    Some(String::from_utf8_lossy(blob.content()).into_owned())
}

/// Write `content` into the index for `path` without touching the working tree.
fn write_index_content(
    repo: &Repository,
    path: &PathBuf,
    content: &str,
) -> Result<(), git2::Error> {
    let Some(root) = repo.workdir() else {
        return Err(git2::Error::from_str("repository has no workdir"));
    };
    let rel = path.strip_prefix(root).unwrap_or(path);
    let rel_bytes = rel.as_os_str().as_encoded_bytes().to_vec();

    let mut index = repo.index()?;
    let mut entry = index.get_path(rel, 0).unwrap_or_else(|| git2::IndexEntry {
        ctime: git2::IndexTime::new(0, 0),
        mtime: git2::IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode: git2::FileMode::Blob as u32,
        uid: 0,
        gid: 0,
        file_size: 0,
        id: git2::Oid::zero(),
        flags: 0,
        flags_extended: 0,
        path: rel_bytes.clone(),
    });
    entry.path = rel_bytes;
    index.add_frombuffer(&entry, content.as_bytes())?;
    index.write()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_events_empty() {
        assert!(group_events(Vec::new()).is_empty());
    }

    #[test]
    fn group_events_added_run() {
        let events = vec![
            DiffEvent { kind: HunkKind::Added, old_start: 4, old_count: 0, new_start: 4, new_count: 1 },
            DiffEvent { kind: HunkKind::Added, old_start: 4, old_count: 0, new_start: 5, new_count: 1 },
            DiffEvent { kind: HunkKind::Added, old_start: 4, old_count: 0, new_start: 6, new_count: 1 },
        ];
        let hunks = group_events(events);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].kind, HunkKind::Added);
        assert_eq!(hunks[0].line_start, 4);
        assert_eq!(hunks[0].line_count, 3);
        assert_eq!(hunks[0].old_line_start, 4);
        assert_eq!(hunks[0].new_line_start, 4);
        assert_eq!(hunks[0].new_line_count, 3);
    }

    #[test]
    fn group_events_removed_run() {
        let events = vec![
            DiffEvent { kind: HunkKind::Removed, old_start: 1, old_count: 1, new_start: 1, new_count: 0 },
            DiffEvent { kind: HunkKind::Removed, old_start: 2, old_count: 1, new_start: 1, new_count: 0 },
        ];
        let hunks = group_events(events);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].kind, HunkKind::Removed);
        assert_eq!(hunks[0].line_start, 1);
        assert_eq!(hunks[0].line_count, 2);
        assert_eq!(hunks[0].old_line_count, 2);
        assert_eq!(hunks[0].new_line_count, 0);
    }

    #[test]
    fn group_events_mixed_becomes_modified() {
        let events = vec![
            DiffEvent { kind: HunkKind::Removed, old_start: 2, old_count: 1, new_start: 2, new_count: 0 },
            DiffEvent { kind: HunkKind::Added, old_start: 2, old_count: 0, new_start: 2, new_count: 1 },
            DiffEvent { kind: HunkKind::Added, old_start: 2, old_count: 0, new_start: 3, new_count: 1 },
        ];
        let hunks = group_events(events);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].kind, HunkKind::Modified);
        assert_eq!(hunks[0].old_line_start, 2);
        assert_eq!(hunks[0].old_line_count, 1);
        assert_eq!(hunks[0].new_line_start, 2);
        assert_eq!(hunks[0].new_line_count, 2);
        assert_eq!(hunks[0].line_start, 2);
        assert_eq!(hunks[0].line_count, 2);
    }

    #[test]
    fn group_events_splits_on_gap() {
        let events = vec![
            DiffEvent { kind: HunkKind::Added, old_start: 0, old_count: 0, new_start: 0, new_count: 1 },
            DiffEvent { kind: HunkKind::Added, old_start: 0, old_count: 0, new_start: 1, new_count: 1 },
            DiffEvent { kind: HunkKind::Added, old_start: 0, old_count: 0, new_start: 4, new_count: 1 },
            DiffEvent { kind: HunkKind::Added, old_start: 0, old_count: 0, new_start: 5, new_count: 1 },
        ];
        let hunks = group_events(events);
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
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
        drop(cfg);
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
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
        drop(cfg);
        let path = dir.join("new.txt");
        std::fs::write(&path, "new content\n").unwrap();

        let hunks = diff_file_against_head(&repo, &path, "new content\n");
        assert!(hunks.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn splice_replaces_middle_range() {
        let base = "one\ntwo\nthree\n";
        let source = "one\nTWO changed\nthree\nfour new\n";
        let result = splice(base, 1, 1, source, 1, 1);
        assert_eq!(result, "one\nTWO changed\nthree\n");
    }

    #[test]
    fn splice_inserts_and_deletes_without_touching_other_lines() {
        let base = "one\ntwo\nthree\n";
        let source = "one\ntwo\ninserted\nthree\n";
        let inserted = splice(base, 2, 0, source, 2, 1);
        assert_eq!(inserted, "one\ntwo\ninserted\nthree\n");

        let deleted = splice(base, 1, 1, source, 1, 0);
        assert_eq!(deleted, "one\nthree\n");
    }

    #[test]
    fn stage_hunk_writes_only_the_selected_hunk_to_the_index() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("blue_ide_git_stage_hunk_test_{unique}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let repo = git2::Repository::init(&dir).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
        drop(cfg);
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();

        let path = dir.join("file.txt");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        let working = "one\nTWO changed\nthree\nFOUR new\n";
        std::fs::write(&path, working).unwrap();
        let hunks = diff_file_against_head(&repo, &path, working);
        assert_eq!(hunks.len(), 2);

        // Stage only the first hunk (the line-2 replacement).
        stage_hunk(&repo, &path, working, &hunks[0]).unwrap();
        let staged = index_content(&repo, &path).unwrap();
        assert_eq!(staged, "one\nTWO changed\nthree\n");

        // The second hunk must still be unstaged.
        let after_stage = diff_file_against_head(&repo, &path, working);
        assert_eq!(after_stage.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unstage_hunk_reverts_one_staged_hunk() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("blue_ide_git_unstage_hunk_test_{unique}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let repo = git2::Repository::init(&dir).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
        drop(cfg);
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();

        let path = dir.join("file.txt");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();

        let working = "one\nTWO changed\nthree\nFOUR new\n";
        std::fs::write(&path, working).unwrap();
        let hunks = diff_file_against_head(&repo, &path, working);
        assert_eq!(hunks.len(), 2);

        // Stage both hunks, then unstage the first one only.
        for hunk in &hunks {
            stage_hunk(&repo, &path, working, hunk).unwrap();
        }
        assert_eq!(index_content(&repo, &path).unwrap(), working);

        unstage_hunk(&repo, &path, working, &hunks[0]).unwrap();
        let unstaged = index_content(&repo, &path).unwrap();
        assert_eq!(unstaged, "one\ntwo\nthree\nFOUR new\n");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
