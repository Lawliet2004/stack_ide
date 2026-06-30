//! Merge-conflict inspection and resolution.
//!
//! Provides the data needed by a three-pane (base / ours / theirs) conflict
//! resolver: the list of conflicted paths, the three blob contents for a path,
//! and a helper to write a chosen resolution and stage it.

use std::path::PathBuf;

use git2::Repository;

/// The three sides of a conflict for a single path.
#[derive(Debug, Clone, Default)]
pub struct ConflictSides {
    /// Common ancestor content (stage 1), if present.
    pub base: Option<String>,
    /// Our side / current branch content (stage 2), if present.
    pub ours: Option<String>,
    /// Their side / incoming content (stage 3), if present.
    pub theirs: Option<String>,
}

/// Which side the user chose when resolving a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Ours,
    Theirs,
    Base,
}

/// List the repository-relative paths that currently have conflicts.
pub fn conflicted_paths(repo: &Repository) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let Ok(index) = repo.index() else {
        return paths;
    };
    if !index.has_conflicts() {
        return paths;
    }
    if let Ok(conflicts) = index.conflicts() {
        for conflict in conflicts.flatten() {
            // Prefer "our" entry for the path, falling back to ancestor/their.
            let entry = conflict
                .our
                .or(conflict.their)
                .or(conflict.ancestor);
            if let Some(entry) = entry {
                if let Ok(path) = std::str::from_utf8(&entry.path) {
                    paths.push(PathBuf::from(path));
                }
            }
        }
    }
    paths
}

/// Read the base/ours/theirs blob contents for a conflicted `rel_path`.
pub fn conflict_sides(repo: &Repository, rel_path: &str) -> ConflictSides {
    let mut sides = ConflictSides::default();
    let Ok(index) = repo.index() else {
        return sides;
    };
    let Ok(conflicts) = index.conflicts() else {
        return sides;
    };

    for conflict in conflicts.flatten() {
        let matches = |entry: &Option<git2::IndexEntry>| {
            entry
                .as_ref()
                .and_then(|e| std::str::from_utf8(&e.path).ok())
                .map(|p| p == rel_path)
                .unwrap_or(false)
        };
        if matches(&conflict.ancestor) || matches(&conflict.our) || matches(&conflict.their) {
            sides.base = conflict
                .ancestor
                .and_then(|e| blob_text(repo, e.id));
            sides.ours = conflict.our.and_then(|e| blob_text(repo, e.id));
            sides.theirs = conflict.their.and_then(|e| blob_text(repo, e.id));
            break;
        }
    }
    sides
}

/// Write `content` to `rel_path` on disk, remove its conflict markers from the
/// index, and stage the resolved file.
pub fn resolve_with_content(
    repo: &Repository,
    rel_path: &str,
    content: &str,
) -> Result<(), git2::Error> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| git2::Error::from_str("bare repository has no working directory"))?;
    let abs = workdir.join(rel_path);
    std::fs::write(&abs, content)
        .map_err(|e| git2::Error::from_str(&format!("failed to write {}: {e}", abs.display())))?;

    let mut index = repo.index()?;
    let path = std::path::Path::new(rel_path);
    // Removing conflict entries then re-adding the path stages the resolution.
    index.remove_path(path)?;
    index.add_path(path)?;
    index.write()?;
    Ok(())
}

/// Resolve a conflict by choosing one whole side.
pub fn resolve_with_side(
    repo: &Repository,
    rel_path: &str,
    resolution: Resolution,
) -> Result<(), git2::Error> {
    let sides = conflict_sides(repo, rel_path);
    let content = match resolution {
        Resolution::Ours => sides.ours,
        Resolution::Theirs => sides.theirs,
        Resolution::Base => sides.base,
    }
    .unwrap_or_default();
    resolve_with_content(repo, rel_path, &content)
}

/// Read a blob's contents as UTF-8 text, if it decodes cleanly.
fn blob_text(repo: &Repository, oid: git2::Oid) -> Option<String> {
    let blob = repo.find_blob(oid).ok()?;
    String::from_utf8(blob.content().to_vec()).ok()
}
