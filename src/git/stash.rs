//! Stash management: list, save, apply, pop, and drop.
//!
//! These operations mutate the repository and therefore take `&mut Repository`
//! (git2 requires a mutable borrow for stash APIs).

use git2::{Repository, StashFlags};

/// One entry in the stash stack.
#[derive(Debug, Clone)]
pub struct StashEntry {
    /// Stack index (0 is the most recent stash).
    pub index: usize,
    /// Stash message, e.g. "WIP on main: abc123 summary".
    pub message: String,
    /// Abbreviated stash commit hash.
    pub short_oid: String,
}

/// List the current stash stack, most recent first.
pub fn stash_list(repo: &mut Repository) -> Vec<StashEntry> {
    let mut entries = Vec::new();
    // stash_foreach iterates the reflog of refs/stash, newest entry first.
    let _ = repo.stash_foreach(|index, message, oid| {
        entries.push(StashEntry {
            index,
            message: message.to_string(),
            short_oid: format!("{:.7}", oid),
        });
        true
    });
    entries
}

/// Save the working tree and index into a new stash.
///
/// `include_untracked` also stashes untracked files. Returns the new stash OID.
pub fn stash_save(
    repo: &mut Repository,
    message: &str,
    include_untracked: bool,
) -> Result<git2::Oid, git2::Error> {
    let sig = repo.signature()?;
    let mut flags = StashFlags::DEFAULT;
    if include_untracked {
        flags |= StashFlags::INCLUDE_UNTRACKED;
    }
    let msg = if message.is_empty() {
        None
    } else {
        Some(message)
    };
    repo.stash_save(&sig, msg.unwrap_or("WIP"), Some(flags))
}

/// Apply the stash at `index` without removing it from the stack.
pub fn stash_apply(repo: &mut Repository, index: usize) -> Result<(), git2::Error> {
    repo.stash_apply(index, None)
}

/// Apply the stash at `index` and remove it from the stack on success.
pub fn stash_pop(repo: &mut Repository, index: usize) -> Result<(), git2::Error> {
    repo.stash_pop(index, None)
}

/// Drop (delete) the stash at `index` without applying it.
pub fn stash_drop(repo: &mut Repository, index: usize) -> Result<(), git2::Error> {
    repo.stash_drop(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo() -> (PathBufGuard, Repository) {
        use std::time::{SystemTime, UNIX_EPOCH};
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("blue_ide_git_stash_{unique}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = Repository::init(&dir).unwrap();
        let cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
        drop(cfg);
        repo.config().unwrap().set_bool("core.autocrlf", false).unwrap();
        // Initial commit so a stash has a base.
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        std::fs::write(dir.join("file.txt"), "base\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        drop(tree);
        drop(index);
        (PathBufGuard(dir), repo)
    }

    struct PathBufGuard(std::path::PathBuf);
    impl Drop for PathBufGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn save_list_and_pop_roundtrip() {
        let (guard, mut repo) = temp_repo();
        // Modify the file so there is something to stash.
        std::fs::write(guard.0.join("file.txt"), "modified\n").unwrap();

        let oid = stash_save(&mut repo, "test stash", false).unwrap();
        assert!(!oid.is_zero());

        let list = stash_list(&mut repo);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].index, 0);

        // Working tree should be clean again after stashing.
        let content = std::fs::read_to_string(guard.0.join("file.txt")).unwrap();
        assert_eq!(content, "base\n");

        stash_pop(&mut repo, 0).unwrap();
        let content = std::fs::read_to_string(guard.0.join("file.txt")).unwrap();
        assert_eq!(content, "modified\n");
        assert!(stash_list(&mut repo).is_empty());
    }
}
