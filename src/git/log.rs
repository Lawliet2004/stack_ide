//! Commit-log retrieval via a revwalk over HEAD.

use git2::{Repository, Sort};

/// A single entry in the commit log.
#[derive(Debug, Clone)]
pub struct CommitInfo {
    /// Full commit hash.
    pub oid: String,
    /// Abbreviated 7-character hash for display.
    pub short_oid: String,
    pub author: String,
    pub email: String,
    /// Commit time as a unix timestamp (seconds).
    pub time: i64,
    /// First line of the commit message.
    pub summary: String,
    /// Full commit message body.
    pub message: String,
}

/// Walk history from HEAD and return up to `limit` commits, newest first.
///
/// Returns an empty vector for an empty repository or on any git2 failure.
pub fn commit_log(repo: &Repository, limit: usize) -> Vec<CommitInfo> {
    let mut commits = Vec::new();

    // revwalk yields commit OIDs reachable from the pushed starting point.
    let mut revwalk = match repo.revwalk() {
        Ok(rw) => rw,
        Err(_) => return commits,
    };
    // Topological + time ordering with newest commits first.
    let _ = revwalk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME);
    if revwalk.push_head().is_err() {
        // No HEAD yet (empty repo).
        return commits;
    }

    for oid in revwalk.flatten().take(limit) {
        let Ok(commit) = repo.find_commit(oid) else {
            continue;
        };
        let author = commit.author();
        let message = commit.message().unwrap_or("").to_string();
        commits.push(CommitInfo {
            oid: oid.to_string(),
            short_oid: format!("{:.7}", oid),
            author: author.name().unwrap_or("?").to_string(),
            email: author.email().unwrap_or("").to_string(),
            time: commit.time().seconds(),
            summary: commit.summary().unwrap_or("").to_string(),
            message,
        });
    }

    commits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo_with_commits(n: usize) -> (tempdir_like::TempDir, Repository) {
        let dir = tempdir_like::TempDir::new("blue_ide_git_log");
        let repo = Repository::init(dir.path()).unwrap();
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let mut parent_oid = None;
        for i in 0..n {
            let path = dir.path().join(format!("f{i}.txt"));
            std::fs::write(&path, format!("content {i}")).unwrap();
            let mut index = repo.index().unwrap();
            index
                .add_path(std::path::Path::new(&format!("f{i}.txt")))
                .unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let parents: Vec<git2::Commit> = parent_oid
                .and_then(|oid| repo.find_commit(oid).ok())
                .into_iter()
                .collect();
            let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
            let oid = repo
                .commit(
                    Some("HEAD"),
                    &sig,
                    &sig,
                    &format!("commit {i}"),
                    &tree,
                    &parent_refs,
                )
                .unwrap();
            parent_oid = Some(oid);
        }
        (dir, repo)
    }

    #[test]
    fn empty_repo_returns_no_commits() {
        let dir = tempdir_like::TempDir::new("blue_ide_git_log_empty");
        let repo = Repository::init(dir.path()).unwrap();
        assert!(commit_log(&repo, 10).is_empty());
    }

    #[test]
    fn returns_commits_newest_first_with_limit() {
        let (_dir, repo) = temp_repo_with_commits(3);
        let log = commit_log(&repo, 2);
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].summary, "commit 2");
        assert_eq!(log[1].summary, "commit 1");
        assert_eq!(log[0].short_oid.len(), 7);
    }

    /// Minimal self-contained temp-dir helper to avoid a dev-dependency.
    mod tempdir_like {
        use std::path::{Path, PathBuf};

        pub struct TempDir(PathBuf);

        impl TempDir {
            pub fn new(prefix: &str) -> Self {
                use std::time::{SystemTime, UNIX_EPOCH};
                let unique = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let dir = std::env::temp_dir().join(format!("{prefix}_{unique}"));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).unwrap();
                TempDir(dir)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
