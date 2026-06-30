//! Remote network operations: fetch, pull, and push.
//!
//! `git2::Repository` is not `Send`, so — mirroring the blame pattern in the app
//! layer — every network operation runs on a freshly discovered repository inside
//! a background thread. Progress is reported back to the UI through a
//! `crossbeam_channel` carrying [`NetworkProgress`] messages.

use std::path::PathBuf;

use crossbeam_channel::Sender;
use git2::{
    AutotagOption, CredentialType, Cred, FetchOptions, PushOptions, RemoteCallbacks, Repository,
};

/// Which network operation a progress message belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkOp {
    Fetch,
    Pull,
    Push,
}

impl NetworkOp {
    pub fn label(self) -> &'static str {
        match self {
            NetworkOp::Fetch => "Fetch",
            NetworkOp::Pull => "Pull",
            NetworkOp::Push => "Push",
        }
    }
}

/// A point-in-time stage of a running network operation.
#[derive(Debug, Clone)]
pub enum NetworkStage {
    /// Negotiating with the remote / opening the connection.
    Connecting,
    /// Receiving objects: `received` of `total` (total may be 0 if unknown).
    Transferring { received: usize, total: usize },
    /// Pushing objects to the remote.
    Pushing { pushed: usize, total: usize },
    /// Operation completed successfully with a human-readable summary.
    Done(String),
    /// Operation failed with an error message.
    Failed(String),
}

impl NetworkStage {
    /// Fractional progress in `0.0..=1.0`, or `None` when indeterminate.
    pub fn fraction(&self) -> Option<f32> {
        match self {
            NetworkStage::Transferring { received, total } if *total > 0 => {
                Some(*received as f32 / *total as f32)
            }
            NetworkStage::Pushing { pushed, total } if *total > 0 => {
                Some(*pushed as f32 / *total as f32)
            }
            NetworkStage::Done(_) => Some(1.0),
            _ => None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, NetworkStage::Done(_) | NetworkStage::Failed(_))
    }
}

/// A progress update for an in-flight network operation.
#[derive(Debug, Clone)]
pub struct NetworkProgress {
    pub op: NetworkOp,
    pub stage: NetworkStage,
}

/// Build remote callbacks wiring credential acquisition and transfer progress.
///
/// Credentials are resolved in order: the platform credential helper configured
/// in git config, then the default credential (for already-cached HTTPS), then
/// the ssh-agent identity for `git@`-style URLs.
pub(crate) fn make_callbacks<'cb>(
    op: NetworkOp,
    tx: Sender<NetworkProgress>,
) -> RemoteCallbacks<'cb> {
    let mut callbacks = RemoteCallbacks::new();

    callbacks.credentials(|url, username_from_url, allowed| {
        // Prefer SSH key auth from the agent when the transport allows it.
        if allowed.contains(CredentialType::SSH_KEY) {
            let user = username_from_url.unwrap_or("git");
            if let Ok(cred) = Cred::ssh_key_from_agent(user) {
                return Ok(cred);
            }
        }
        // Try the configured credential helper (e.g. Windows Credential Manager).
        if allowed.contains(CredentialType::USER_PASS_PLAINTEXT) {
            if let Ok(config) = git2::Config::open_default() {
                if let Ok(cred) = Cred::credential_helper(&config, url, username_from_url) {
                    return Ok(cred);
                }
            }
        }
        // Fall back to the default credential (cached HTTPS tokens).
        if allowed.contains(CredentialType::DEFAULT) {
            if let Ok(cred) = Cred::default() {
                return Ok(cred);
            }
        }
        Err(git2::Error::from_str(
            "no suitable authentication method available",
        ))
    });

    let progress_tx = tx.clone();
    callbacks.transfer_progress(move |stats| {
        let _ = progress_tx.send(NetworkProgress {
            op,
            stage: NetworkStage::Transferring {
                received: stats.received_objects(),
                total: stats.total_objects(),
            },
        });
        true
    });

    let push_tx = tx;
    callbacks.push_transfer_progress(move |current, total, _bytes| {
        let _ = push_tx.send(NetworkProgress {
            op: NetworkOp::Push,
            stage: NetworkStage::Pushing {
                pushed: current,
                total,
            },
        });
    });

    callbacks
}

/// Send a terminal `Done`/`Failed` result derived from a `Result`.
fn report_result(tx: &Sender<NetworkProgress>, op: NetworkOp, result: Result<String, git2::Error>) {
    let stage = match result {
        Ok(summary) => NetworkStage::Done(summary),
        Err(error) => NetworkStage::Failed(error.message().to_string()),
    };
    let _ = tx.send(NetworkProgress { op, stage });
}

/// Spawn a background fetch from `remote_name`. Progress flows through `tx`.
pub fn spawn_fetch(repo_path: PathBuf, remote_name: String, tx: Sender<NetworkProgress>) {
    std::thread::spawn(move || {
        let _ = tx.send(NetworkProgress {
            op: NetworkOp::Fetch,
            stage: NetworkStage::Connecting,
        });
        let result = fetch_blocking(&repo_path, &remote_name, &tx);
        report_result(&tx, NetworkOp::Fetch, result);
    });
}

/// Spawn a background pull (fetch + fast-forward/merge) from `remote_name`.
pub fn spawn_pull(repo_path: PathBuf, remote_name: String, tx: Sender<NetworkProgress>) {
    std::thread::spawn(move || {
        let _ = tx.send(NetworkProgress {
            op: NetworkOp::Pull,
            stage: NetworkStage::Connecting,
        });
        let result = pull_blocking(&repo_path, &remote_name, &tx);
        report_result(&tx, NetworkOp::Pull, result);
    });
}

/// Spawn a background push of the current branch to `remote_name`.
pub fn spawn_push(repo_path: PathBuf, remote_name: String, tx: Sender<NetworkProgress>) {
    std::thread::spawn(move || {
        let _ = tx.send(NetworkProgress {
            op: NetworkOp::Push,
            stage: NetworkStage::Connecting,
        });
        let result = push_blocking(&repo_path, &remote_name, &tx);
        report_result(&tx, NetworkOp::Push, result);
    });
}

/// Fetch all refs from `remote_name` into the local repository.
fn fetch_blocking(
    repo_path: &PathBuf,
    remote_name: &str,
    tx: &Sender<NetworkProgress>,
) -> Result<String, git2::Error> {
    let repo = Repository::discover(repo_path)?;
    let mut remote = repo.find_remote(remote_name)?;

    let mut fetch_opts = FetchOptions::new();
    fetch_opts.remote_callbacks(make_callbacks(NetworkOp::Fetch, tx.clone()));
    fetch_opts.download_tags(AutotagOption::All);

    // An empty refspec list tells git2 to use the remote's configured refspecs.
    let refspecs: [&str; 0] = [];
    remote.fetch(&refspecs, Some(&mut fetch_opts), None)?;

    let stats = remote.stats();
    Ok(format!(
        "Fetched {} object(s) from {}",
        stats.received_objects(),
        remote_name
    ))
}

/// Fetch then integrate the upstream of the current branch via fast-forward,
/// or a normal merge commit when histories have diverged.
fn pull_blocking(
    repo_path: &PathBuf,
    remote_name: &str,
    tx: &Sender<NetworkProgress>,
) -> Result<String, git2::Error> {
    let repo = Repository::discover(repo_path)?;

    // 1. Fetch from the remote.
    {
        let mut remote = repo.find_remote(remote_name)?;
        let mut fetch_opts = FetchOptions::new();
        fetch_opts.remote_callbacks(make_callbacks(NetworkOp::Pull, tx.clone()));
        fetch_opts.download_tags(AutotagOption::All);
        let refspecs: [&str; 0] = [];
        remote.fetch(&refspecs, Some(&mut fetch_opts), None)?;
    }

    // 2. Determine the upstream commit to merge.
    let head = repo.head()?;
    let branch_name = head
        .shorthand()
        .ok_or_else(|| git2::Error::from_str("HEAD is not on a branch"))?
        .to_string();
    let fetch_head = repo.find_reference("FETCH_HEAD")?;
    let fetch_commit = repo.reference_to_annotated_commit(&fetch_head)?;

    // 3. Analyze how to integrate.
    let (analysis, _) = repo.merge_analysis(&[&fetch_commit])?;

    if analysis.is_up_to_date() {
        return Ok(format!("Already up to date with {}", remote_name));
    }

    if analysis.is_fast_forward() {
        // Move the branch ref forward and check out the new tree.
        let refname = format!("refs/heads/{}", branch_name);
        let mut reference = repo.find_reference(&refname)?;
        reference.set_target(fetch_commit.id(), "pull: fast-forward")?;
        repo.set_head(&refname)?;
        repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))?;
        return Ok(format!("Fast-forwarded {} to {}", branch_name, remote_name));
    }

    // Normal merge: produce a merge commit when there are no conflicts.
    repo.merge(&[&fetch_commit], None, None)?;
    let mut index = repo.index()?;
    if index.has_conflicts() {
        return Err(git2::Error::from_str(
            "pull produced conflicts; resolve them and commit",
        ));
    }
    let tree_oid = index.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;
    let sig = repo.signature()?;
    let local_commit = repo.head()?.peel_to_commit()?;
    let remote_commit = repo.find_commit(fetch_commit.id())?;
    let message = format!("Merge remote-tracking branch from {}", remote_name);
    repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        &message,
        &tree,
        &[&local_commit, &remote_commit],
    )?;
    repo.cleanup_state()?;
    Ok(format!("Merged {} into {}", remote_name, branch_name))
}

/// Push the current branch to `remote_name`.
fn push_blocking(
    repo_path: &PathBuf,
    remote_name: &str,
    tx: &Sender<NetworkProgress>,
) -> Result<String, git2::Error> {
    let repo = Repository::discover(repo_path)?;
    let head = repo.head()?;
    let branch_name = head
        .shorthand()
        .ok_or_else(|| git2::Error::from_str("HEAD is not on a branch"))?
        .to_string();

    let mut remote = repo.find_remote(remote_name)?;
    let mut push_opts = PushOptions::new();
    push_opts.remote_callbacks(make_callbacks(NetworkOp::Push, tx.clone()));

    let refspec = format!("refs/heads/{0}:refs/heads/{0}", branch_name);
    remote.push(&[refspec.as_str()], Some(&mut push_opts))?;

    Ok(format!("Pushed {} to {}", branch_name, remote_name))
}

/// List configured remote names (e.g. `["origin"]`).
pub fn remote_names(repo: &Repository) -> Vec<String> {
    repo.remotes()
        .map(|remotes| remotes.iter().flatten().map(String::from).collect())
        .unwrap_or_default()
}
