//! Tag management: list, create (lightweight or annotated), delete, and push.

use std::path::PathBuf;

use crossbeam_channel::Sender;
use git2::{PushOptions, Repository};

use crate::git::remote::{make_callbacks, NetworkOp, NetworkProgress, NetworkStage};

/// A single tag.
#[derive(Debug, Clone)]
pub struct TagInfo {
    pub name: String,
    /// Abbreviated hash of the tagged object.
    pub short_oid: String,
    /// Annotation message for annotated tags; empty for lightweight tags.
    pub message: String,
}

/// List all tags, sorted by name.
pub fn tags(repo: &Repository) -> Vec<TagInfo> {
    let mut out = Vec::new();
    let Ok(names) = repo.tag_names(None) else {
        return out;
    };
    for name in names.iter().flatten() {
        let refname = format!("refs/tags/{name}");
        let (short_oid, message) = match repo.revparse_single(&refname) {
            Ok(obj) => {
                let short = format!("{:.7}", obj.id());
                // Annotated tags resolve to a Tag object carrying a message.
                let msg = obj
                    .as_tag()
                    .and_then(|t| t.message())
                    .unwrap_or("")
                    .to_string();
                (short, msg)
            }
            Err(_) => (String::new(), String::new()),
        };
        out.push(TagInfo {
            name: name.to_string(),
            short_oid,
            message,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Create a tag at HEAD. When `message` is non-empty an annotated tag is created,
/// otherwise a lightweight tag.
pub fn create_tag(repo: &Repository, name: &str, message: &str) -> Result<git2::Oid, git2::Error> {
    let head = repo.head()?.peel_to_commit()?;
    let target = head.as_object();
    if message.is_empty() {
        repo.tag_lightweight(name, target, false)
    } else {
        let sig = repo.signature()?;
        repo.tag(name, target, &sig, message, false)
    }
}

/// Delete the tag named `name`.
pub fn delete_tag(repo: &Repository, name: &str) -> Result<(), git2::Error> {
    repo.tag_delete(name)
}

/// Spawn a background push of a single tag to `remote_name`.
pub fn spawn_push_tag(
    repo_path: PathBuf,
    remote_name: String,
    tag_name: String,
    tx: Sender<NetworkProgress>,
) {
    std::thread::spawn(move || {
        let _ = tx.send(NetworkProgress {
            op: NetworkOp::Push,
            stage: NetworkStage::Connecting,
        });
        let result = (|| -> Result<String, git2::Error> {
            let repo = Repository::discover(&repo_path)?;
            let mut remote = repo.find_remote(&remote_name)?;
            let mut push_opts = PushOptions::new();
            push_opts.remote_callbacks(make_callbacks(NetworkOp::Push, tx.clone()));
            let refspec = format!("refs/tags/{0}:refs/tags/{0}", tag_name);
            remote.push(&[refspec.as_str()], Some(&mut push_opts))?;
            Ok(format!("Pushed tag {} to {}", tag_name, remote_name))
        })();
        let stage = match result {
            Ok(summary) => NetworkStage::Done(summary),
            Err(error) => NetworkStage::Failed(error.message().to_string()),
        };
        let _ = tx.send(NetworkProgress {
            op: NetworkOp::Push,
            stage,
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_list_and_delete_tags() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("blue_ide_git_tag_{unique}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let repo = Repository::init(&dir).unwrap();
        let cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
        drop(cfg);
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        std::fs::write(dir.join("f.txt"), "x\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("f.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        create_tag(&repo, "v1.0", "release one").unwrap();
        create_tag(&repo, "v0.9", "").unwrap();

        let list = tags(&repo);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "v0.9");
        assert_eq!(list[1].name, "v1.0");
        assert_eq!(list[1].message, "release one");

        delete_tag(&repo, "v0.9").unwrap();
        assert_eq!(tags(&repo).len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
