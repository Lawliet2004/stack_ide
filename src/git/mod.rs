//! Git integration facade for Blue IDE.
//!
//! `GitRepo` owns a `git2::Repository` and lives on the main thread. All public
//! methods are synchronous; expensive operations such as blame are spawned onto a
//! background thread by the app layer using only the repository root path, so the
//! `Repository` itself never crosses thread boundaries.

pub mod cherry_pick;
pub mod conflict;
pub mod diff;
pub mod log;
pub mod remote;
pub mod stash;
pub mod tag;
pub mod ui;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use git2::{BranchType, Repository, StatusOptions};

pub use conflict::{ConflictSides, Resolution};
pub use diff::{DiffHunk, FileDiff, HunkKind};
pub use log::CommitInfo;
pub use remote::{NetworkOp, NetworkProgress, NetworkStage};
pub use stash::StashEntry;
pub use tag::TagInfo;
pub use ui::{
    render_blame_gutter, render_branch_picker, render_conflict_resolver, render_diff_gutters,
    render_git_panel, render_log_viewer, render_network_progress, render_tag_manager,
    ConflictResolverOutcome, GitPanelAction, LogAction, TagManagerAction,
};

/// Working-tree status of a file, derived from `git2::Status`.
#[derive(Debug, Clone, PartialEq)]
pub enum FileStatus {
    Unmodified,
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

/// Per-line blame information for the inline blame gutter.
#[derive(Debug, Clone)]
pub struct BlameLine {
    pub line: usize,
    pub commit: String, // short SHA
    pub author: String,
    pub time: i64, // unix timestamp
}

/// In-memory git state refreshed on folder open and file save.
pub struct GitRepo {
    pub repo: Repository,
    pub root: PathBuf,
    pub branch: String,
    pub file_diffs: HashMap<PathBuf, Vec<DiffHunk>>,
    /// Hunks that are currently in the index (HEAD vs index).
    pub staged_diffs: HashMap<PathBuf, Vec<DiffHunk>>,
    /// Hunks that are currently unstaged (index vs working tree).
    pub unstaged_diffs: HashMap<PathBuf, Vec<DiffHunk>>,
    pub status_map: HashMap<PathBuf, FileStatus>,
    /// Paths that are currently staged in the index. Kept in sync by `refresh`.
    pub staged_paths: HashSet<PathBuf>,
    /// True when git state should be refreshed at the next safe moment.
    pub dirty: bool,
}

impl GitRepo {
    /// Discover a git repository containing `path` and initialize state.
    /// Returns `None` when no repository is found.
    pub fn open(path: &PathBuf) -> Option<Self> {
        // Repository::discover searches upward from the path until it finds a .git directory,
        // then opens and returns the Git Repository.
        let repo = Repository::discover(path).ok()?;
        // repo.workdir() returns the path to the working directory.
        let root = repo.workdir()?.to_path_buf();
        let branch = current_branch_name(&repo);
        Some(GitRepo {
            repo,
            root,
            branch,
            file_diffs: HashMap::new(),
            staged_diffs: HashMap::new(),
            unstaged_diffs: HashMap::new(),
            status_map: HashMap::new(),
            staged_paths: HashSet::new(),
            dirty: true,
        })
    }

    /// Recompute branch name, working-tree statuses, staged-file set, and diffs
    /// for every open buffer. Called after folder open or file save.
    pub fn refresh(&mut self, open_buffers: &[(PathBuf, &str)]) {
        // 1. Refresh branch name in case it changed outside the IDE.
        self.branch = current_branch_name(&self.repo);

        // 2. Refresh file statuses via `repo.statuses()`.
        let mut opts = StatusOptions::new();
        opts.include_untracked(true);
        // repo.statuses(opts) gathers status flags (modified, untracked, deleted, etc.)
        // for all files in the working directory and staging area.
        if let Ok(statuses) = self.repo.statuses(Some(&mut opts)) {
            self.status_map.clear();
            self.staged_paths.clear();
            for entry in statuses.iter() {
                let Some(path) = entry.path() else { continue };
                let abs = self.root.join(path);
                let status = entry.status();
                self.status_map.insert(abs.clone(), map_status(status));
                if is_index_status(status) {
                    self.staged_paths.insert(abs);
                }
            }
        }

        // 3. Refresh diffs for open buffers against HEAD, the index (staged),
        // and the working tree (unstaged). The staged/unstaged split is what
        // drives per-hunk staging in the git panel.
        self.file_diffs.clear();
        self.staged_diffs.clear();
        self.unstaged_diffs.clear();
        for (path, text) in open_buffers {
            let hunks = diff::diff_file_against_head(&self.repo, path, text);
            self.file_diffs.insert(path.clone(), hunks);

            let staged = diff::diff_index_against_head(&self.repo, path);
            self.staged_diffs.insert(path.clone(), staged);

            let unstaged = diff::diff_file_against_index(&self.repo, path, text);
            self.unstaged_diffs.insert(path.clone(), unstaged);
        }

        self.dirty = false;
    }

    /// Stage a file by adding it to the index.
    pub fn stage_file(&self, path: &PathBuf) -> Result<(), git2::Error> {
        // repo.index() retrieves the index (staging area) representation for modification.
        let mut index = self.repo.index()?;
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        // index.add_path(rel) registers the changes of the file at the path into the index.
        index.add_path(rel)?;
        // index.write() saves the updated index state back to the disk's .git/index file.
        index.write()?;
        Ok(())
    }

    /// Stage one hunk of `path` using the current working-tree content.
    pub fn stage_hunk(
        &self,
        path: &PathBuf,
        text: &str,
        hunk: &DiffHunk,
    ) -> Result<(), git2::Error> {
        diff::stage_hunk(&self.repo, path, text, hunk)
    }

    /// Unstage one hunk of `path`, reverting it in the index to HEAD.
    pub fn unstage_hunk(
        &self,
        path: &PathBuf,
        text: &str,
        hunk: &DiffHunk,
    ) -> Result<(), git2::Error> {
        diff::unstage_hunk(&self.repo, path, text, hunk)
    }

    /// Unstage a file by resetting the index entry to the HEAD tree.
    pub fn unstage_file(&self, path: &PathBuf) -> Result<(), git2::Error> {
        // repo.head() gets the reference HEAD points to; peel_to_commit() dereferences it to a Commit.
        let head = self.repo.head()?.peel_to_commit()?;
        // repo.reset_default(...) resets specific paths in the index/staging area to match the specified commit (HEAD).
        self.repo.reset_default(Some(head.as_object()), [path])?;
        Ok(())
    }

    /// Create a commit from the current index and advance HEAD.
    pub fn commit(&self, message: &str) -> Result<(), git2::Error> {
        // Access the staging index.
        let mut index = self.repo.index()?;
        // index.write_tree() writes the current state of the index as a tree object into the Git repository database
        // and returns the OID of that Tree.
        let tree_oid = index.write_tree()?;
        // repo.find_tree(tree_oid) retrieves the parsed Tree object from the OID.
        let tree = self.repo.find_tree(tree_oid)?;
        // repo.signature() obtains the default committer/author identity from git config (user.name and user.email).
        let sig = self.repo.signature()?;
        // repo.head() finds the HEAD reference, and peel_to_commit() finds the parent commit of the commit we are about to create.
        let parent = self.repo.head()?.peel_to_commit().ok();
        let parents: Vec<&git2::Commit> = parent.as_ref().map(|c| vec![c]).unwrap_or_default();
        // repo.commit(...) creates a new commit object, writing it to the DB, and moves HEAD to point to this new commit.
        self.repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)?;
        Ok(())
    }

    /// Amend the current HEAD commit with the staged index and a new message.
    pub fn amend(&self, message: &str) -> Result<(), git2::Error> {
        let mut index = self.repo.index()?;
        let tree_oid = index.write_tree()?;
        let tree = self.repo.find_tree(tree_oid)?;
        let sig = self.repo.signature()?;
        let head = self.repo.head()?.peel_to_commit()?;
        head.amend(
            Some("HEAD"),
            Some(&sig),
            Some(&sig),
            None,
            Some(message),
            Some(&tree),
        )
        .map(|_| ())
    }

    /// List all local branch names.
    pub fn branches(&self) -> Vec<String> {
        // repo.branches() returns an iterator over the branches in the repository (here filtered to Local branches).
        self.repo
            .branches(Some(BranchType::Local))
            .map(|branches| {
                branches
                    .flatten()
                    .filter_map(|(branch, _)| branch.name().ok().flatten().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Check out a local branch and update HEAD.
    pub fn checkout_branch(&self, name: &str) -> Result<(), git2::Error> {
        // repo.revparse_single() parses a git revision string (e.g. a branch reference path) and finds the corresponding git Object.
        let obj = self.repo.revparse_single(&format!("refs/heads/{}", name))?;
        // repo.checkout_tree() updates files in the working directory to match the target commit/tree.
        self.repo.checkout_tree(&obj, None)?;
        // repo.set_head() updates the HEAD reference file to point to the newly checked out branch.
        self.repo.set_head(&format!("refs/heads/{}", name))?;
        Ok(())
    }

    /// Compute blame information for every line of `path`. This reads the file
    /// from disk and may be slow for large histories; the app layer runs it on a
    /// background thread using a freshly discovered repository.
    pub fn blame_file(&self, path: &PathBuf) -> Option<Vec<BlameLine>> {
        let rel = path.strip_prefix(&self.root).ok()?;
        // repo.blame_file() computes the line-by-line blame for the file, returning which commit/author last modified each line.
        let blame = self.repo.blame_file(rel, None).ok()?;
        let text = std::fs::read_to_string(path).ok()?;

        text.lines()
            .enumerate()
            .map(|(i, _)| {
                // blame.get_line(1-based-line-number) gets the blame hunk containing that line.
                let hunk = blame.get_line(i + 1)?;
                let sig = hunk.final_signature();
                Some(BlameLine {
                    line: i,
                    commit: format!("{:.7}", hunk.final_commit_id()),
                    author: sig.name().unwrap_or("?").to_string(),
                    time: sig.when().seconds(),
                })
            })
            .flatten()
            .collect::<Vec<_>>()
            .into()
    }

    /// True when `path` is represented in the index (i.e. staged).
    pub fn is_staged(&self, path: &Path) -> bool {
        self.staged_paths.contains(path)
    }

    /// Configured remote names (e.g. `["origin"]`).
    pub fn remote_names(&self) -> Vec<String> {
        remote::remote_names(&self.repo)
    }

    /// The first remote name, defaulting to "origin" when none are configured.
    pub fn default_remote(&self) -> String {
        self.remote_names()
            .into_iter()
            .next()
            .unwrap_or_else(|| "origin".to_string())
    }

    /// Recent commit history (newest first), limited to `limit` entries.
    pub fn commit_log(&self, limit: usize) -> Vec<CommitInfo> {
        log::commit_log(&self.repo, limit)
    }

    /// Current stash stack (most recent first).
    pub fn stash_list(&mut self) -> Vec<StashEntry> {
        stash::stash_list(&mut self.repo)
    }

    /// Save the working tree to a new stash.
    pub fn stash_save(&mut self, message: &str, include_untracked: bool) -> Result<(), git2::Error> {
        stash::stash_save(&mut self.repo, message, include_untracked).map(|_| ())
    }

    /// Apply (and keep) the stash at `index`.
    pub fn stash_apply(&mut self, index: usize) -> Result<(), git2::Error> {
        stash::stash_apply(&mut self.repo, index)
    }

    /// Apply and remove the stash at `index`.
    pub fn stash_pop(&mut self, index: usize) -> Result<(), git2::Error> {
        stash::stash_pop(&mut self.repo, index)
    }

    /// Drop the stash at `index` without applying it.
    pub fn stash_drop(&mut self, index: usize) -> Result<(), git2::Error> {
        stash::stash_drop(&mut self.repo, index)
    }

    /// Cherry-pick a commit (by full or abbreviated SHA) onto HEAD.
    pub fn cherry_pick(&self, rev: &str) -> Result<(), git2::Error> {
        let oid = self.repo.revparse_single(rev)?.id();
        cherry_pick::cherry_pick(&self.repo, oid).map(|_| ())
    }

    /// Repository-relative paths that currently have merge conflicts.
    pub fn conflicted_paths(&self) -> Vec<PathBuf> {
        conflict::conflicted_paths(&self.repo)
    }

    /// Base/ours/theirs content for a conflicted relative path.
    pub fn conflict_sides(&self, rel_path: &str) -> ConflictSides {
        conflict::conflict_sides(&self.repo, rel_path)
    }

    /// Resolve a conflict by writing explicit content and staging it.
    pub fn resolve_conflict_with_content(
        &self,
        rel_path: &str,
        content: &str,
    ) -> Result<(), git2::Error> {
        conflict::resolve_with_content(&self.repo, rel_path, content)
    }

    /// Resolve a conflict by taking one whole side.
    pub fn resolve_conflict_with_side(
        &self,
        rel_path: &str,
        resolution: Resolution,
    ) -> Result<(), git2::Error> {
        conflict::resolve_with_side(&self.repo, rel_path, resolution)
    }

    /// All tags, sorted by name.
    pub fn tags(&self) -> Vec<TagInfo> {
        tag::tags(&self.repo)
    }

    /// Create a tag at HEAD (annotated when `message` is non-empty).
    pub fn create_tag(&self, name: &str, message: &str) -> Result<(), git2::Error> {
        tag::create_tag(&self.repo, name, message).map(|_| ())
    }

    /// Delete a tag by name.
    pub fn delete_tag(&self, name: &str) -> Result<(), git2::Error> {
        tag::delete_tag(&self.repo, name)
    }
}

fn current_branch_name(repo: &Repository) -> String {
    // repo.head() gets the reference HEAD points to.
    // head.shorthand() returns the branch name without the refs/heads/ prefix (e.g., "main" instead of "refs/heads/main").
    repo.head()
        .ok()
        .and_then(|head| head.shorthand().map(String::from))
        .unwrap_or_else(|| "HEAD".into())
}

fn map_status(s: git2::Status) -> FileStatus {
    if s.is_conflicted() {
        FileStatus::Conflicted
    } else if s.is_wt_modified() || s.is_index_modified() {
        FileStatus::Modified
    } else if s.is_wt_new() {
        FileStatus::Untracked
    } else if s.is_index_new() {
        FileStatus::Added
    } else if s.is_wt_deleted() || s.is_index_deleted() {
        FileStatus::Deleted
    } else if s.is_index_renamed() || s.is_wt_renamed() {
        FileStatus::Renamed
    } else {
        FileStatus::Unmodified
    }
}

/// True for statuses that have an index entry (staged changes).
fn is_index_status(s: git2::Status) -> bool {
    s.is_index_new() || s.is_index_modified() || s.is_index_deleted() || s.is_index_renamed()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo(name: &str) -> (std::path::PathBuf, GitRepo) {
        let dir = std::env::temp_dir().join(format!(
            "blue_ide_git_mod_{name}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = Repository::init(&dir).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
        drop(cfg);
        let git = GitRepo::open(&dir).unwrap();
        (dir, git)
    }

    fn commit_file(repo: &Repository, dir: &Path, name: &str, content: &str, message: &str) {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(name)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[])
            .unwrap();
    }

    #[test]
    fn amend_replaces_head_message_and_content() {
        let (dir, git) = temp_repo("amend");
        commit_file(&git.repo, &dir, "a.txt", "one\n", "initial");
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        git.stage_file(&dir.join("a.txt")).unwrap();
        git.amend("amended").unwrap();

        let head = git.repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.message().map(str::trim), Some("amended"));
        let tree = head.tree().unwrap();
        let entry = tree.get_path(std::path::Path::new("a.txt")).unwrap();
        let blob = git.repo.find_blob(entry.id()).unwrap();
        assert_eq!(blob.content(), b"one\ntwo\n");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
