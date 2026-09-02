//! Cherry-pick a commit onto the current HEAD.

use git2::{Oid, Repository};

/// Cherry-pick the commit identified by `oid` onto HEAD.
///
/// On a clean apply this writes a new commit reusing the original author and
/// message, then advances HEAD. If the cherry-pick produces conflicts, the
/// working tree/index are left in the conflicted state (so the conflict resolver
/// can take over) and an error is returned.
pub fn cherry_pick(repo: &Repository, oid: Oid) -> Result<Oid, git2::Error> {
    let commit = repo.find_commit(oid)?;

    // Apply the commit's changes into the index/working tree.
    repo.cherrypick(&commit, None)?;

    let mut index = repo.index()?;
    if index.has_conflicts() {
        return Err(git2::Error::from_str(
            "cherry-pick produced conflicts; resolve them and commit",
        ));
    }

    // Build the resulting tree and create a commit on top of HEAD.
    let tree_oid = index.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;
    let head_commit = repo.head()?.peel_to_commit()?;
    let committer = repo.signature()?;
    let author = commit.author();
    let message = commit.message().unwrap_or("cherry-picked commit");

    let new_oid = repo.commit(
        Some("HEAD"),
        &author,
        &committer,
        message,
        &tree,
        &[&head_commit],
    )?;

    // Clear the CHERRY_PICK_HEAD / MERGE state.
    repo.cleanup_state()?;
    Ok(new_oid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cherry_pick_applies_commit_from_another_branch() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("blue_ide_git_cp_{unique}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let repo = Repository::init(&dir).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
        drop(cfg);
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();

        // Base commit on main.
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("a.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let base = repo
            .commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
            .unwrap();

        // Create a feature branch with a new file.
        let base_commit = repo.find_commit(base).unwrap();
        repo.branch("feature", &base_commit, false).unwrap();
        repo.set_head("refs/heads/feature").unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
            .unwrap();
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("b.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let feature_commit = repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "add b",
                &tree,
                &[&base_commit],
            )
            .unwrap();

        // Back to main and cherry-pick the feature commit.
        repo.set_head("refs/heads/master")
            .or_else(|_| repo.set_head("refs/heads/main"))
            .ok();
        // Some git2 builds default the branch to "master"; ensure HEAD points at base.
        repo.set_head_detached(base).unwrap();
        repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))
            .unwrap();

        let new_oid = cherry_pick(&repo, feature_commit).unwrap();
        assert!(!new_oid.is_zero());
        assert!(dir.join("b.txt").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
