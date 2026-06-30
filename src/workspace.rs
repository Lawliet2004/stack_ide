//! Portable workspace identity, trust, background jobs, and session persistence.
//!
//! These types deliberately do not depend on UI state. The application can migrate
//! individual subsystems from the legacy single-root model without duplicating root
//! ownership or trust decisions.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RootId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DocumentUri {
    Local(PathBuf),
    Ssh { profile: String, path: String },
}

impl DocumentUri {
    pub fn local(path: impl Into<PathBuf>) -> Self {
        Self::Local(path.into())
    }

    pub fn ssh(profile: impl Into<String>, path: impl Into<String>) -> Result<Self, String> {
        let profile = profile.into();
        let path = path.into().replace('\\', "/");
        if profile.trim().is_empty() || !path.starts_with('/') || path.contains('\0') {
            return Err("SSH document URIs require a profile and an absolute path".into());
        }
        Ok(Self::Ssh { profile, path })
    }
}

impl fmt::Display for DocumentUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(path) => write!(f, "file://{}", path.to_string_lossy().replace('\\', "/")),
            Self::Ssh { profile, path } => write!(f, "ssh://{profile}{path}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRoot {
    pub id: RootId,
    pub path: PathBuf,
    pub canonical_path: PathBuf,
    pub label: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    roots: Vec<WorkspaceRoot>,
    next_root_id: u64,
}

impl Workspace {
    pub fn roots(&self) -> &[WorkspaceRoot] {
        &self.roots
    }

    pub fn add_root(&mut self, path: impl Into<PathBuf>) -> io::Result<RootId> {
        let path = path.into();
        let canonical_path = path.canonicalize()?;
        if let Some(root) = self
            .roots
            .iter()
            .find(|root| root.canonical_path == canonical_path)
        {
            return Ok(root.id);
        }
        self.next_root_id = self.next_root_id.max(1);
        let id = RootId(self.next_root_id);
        self.next_root_id += 1;
        let label = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("workspace")
            .to_owned();
        self.roots.push(WorkspaceRoot {
            id,
            path,
            canonical_path,
            label,
        });
        Ok(id)
    }

    pub fn remove_root(&mut self, id: RootId) -> bool {
        let old_len = self.roots.len();
        self.roots.retain(|root| root.id != id);
        old_len != self.roots.len()
    }

    /// Resolves nested roots deterministically: the deepest canonical root owns a file.
    pub fn owner_of(&self, path: &Path) -> Option<&WorkspaceRoot> {
        let canonical = canonicalize_existing_or_parent(path)?;
        self.roots
            .iter()
            .filter(|root| canonical.starts_with(&root.canonical_path))
            .max_by_key(|root| root.canonical_path.components().count())
    }

    pub fn root(&self, id: RootId) -> Option<&WorkspaceRoot> {
        self.roots.iter().find(|root| root.id == id)
    }
}

fn canonicalize_existing_or_parent(path: &Path) -> Option<PathBuf> {
    if let Ok(path) = path.canonicalize() {
        return Some(path);
    }
    let mut ancestor = path;
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        suffix.push(ancestor.file_name()?.to_owned());
        ancestor = ancestor.parent()?;
    }
    let mut result = ancestor.canonicalize().ok()?;
    for part in suffix.iter().rev() {
        result.push(part);
    }
    Some(result)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustState {
    Untrusted,
    Trusted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutableCapability {
    Lsp,
    Plugin,
    Terminal,
    Debugger,
    Profiler,
    Command,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TrustFile {
    version: u32,
    roots: BTreeMap<String, TrustState>,
}

pub struct TrustStore {
    path: PathBuf,
    data: TrustFile,
}

impl TrustStore {
    pub fn load(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let data = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => TrustFile::default(),
            Err(error) => return Err(error),
        };
        Ok(Self { path, data })
    }

    pub fn state(&self, root: &WorkspaceRoot) -> TrustState {
        self.data
            .roots
            .get(&trust_key(root))
            .copied()
            .unwrap_or(TrustState::Untrusted)
    }

    pub fn permits(&self, root: &WorkspaceRoot, _capability: ExecutableCapability) -> bool {
        self.state(root) == TrustState::Trusted
    }

    pub fn set(&mut self, root: &WorkspaceRoot, state: TrustState) -> io::Result<()> {
        self.data.version = 1;
        self.data.roots.insert(trust_key(root), state);
        write_json_atomic(&self.path, &self.data)
    }
}

fn trust_key(root: &WorkspaceRoot) -> String {
    root.canonical_path.to_string_lossy().to_lowercase()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackgroundJobStatus {
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackgroundJob {
    pub id: u64,
    pub root_id: RootId,
    pub description: String,
    pub progress: Option<f32>,
    pub status: BackgroundJobStatus,
    pub error: Option<JobError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobError {
    pub code: String,
    pub message: String,
    pub details: Option<String>,
}

#[derive(Debug, Default)]
pub struct BackgroundJobs {
    next_id: u64,
    jobs: HashMap<u64, BackgroundJob>,
}

impl BackgroundJobs {
    pub fn start(&mut self, root_id: RootId, description: impl Into<String>) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.jobs.insert(
            id,
            BackgroundJob {
                id,
                root_id,
                description: description.into(),
                progress: None,
                status: BackgroundJobStatus::Running,
                error: None,
            },
        );
        id
    }

    pub fn get(&self, id: u64) -> Option<&BackgroundJob> {
        self.jobs.get(&id)
    }

    pub fn set_progress(&mut self, id: u64, progress: f32) -> bool {
        let Some(job) = self.jobs.get_mut(&id) else {
            return false;
        };
        if job.status != BackgroundJobStatus::Running {
            return false;
        }
        job.progress = Some(progress.clamp(0.0, 1.0));
        true
    }

    pub fn request_cancel(&mut self, id: u64) -> bool {
        let Some(job) = self.jobs.get_mut(&id) else {
            return false;
        };
        if job.status != BackgroundJobStatus::Running {
            return false;
        }
        job.status = BackgroundJobStatus::Cancelling;
        true
    }

    pub fn finish(&mut self, id: u64, result: Result<(), JobError>) -> bool {
        let Some(job) = self.jobs.get_mut(&id) else {
            return false;
        };
        match result {
            Ok(()) => {
                job.progress = Some(1.0);
                job.status = if job.status == BackgroundJobStatus::Cancelling {
                    BackgroundJobStatus::Cancelled
                } else {
                    BackgroundJobStatus::Completed
                };
            }
            Err(error) => {
                job.status = BackgroundJobStatus::Failed;
                job.error = Some(error);
            }
        }
        true
    }
}

pub const SESSION_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    pub version: u32,
    pub roots: Vec<PathBuf>,
    pub tabs: Vec<DocumentUri>,
    pub active: Option<DocumentUri>,
    pub bottom_panel_height: f32,
    pub terminal_names: Vec<String>,
    pub recovery: BTreeMap<String, String>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            version: SESSION_VERSION,
            roots: Vec::new(),
            tabs: Vec::new(),
            active: None,
            bottom_panel_height: 280.0,
            terminal_names: Vec::new(),
            recovery: BTreeMap::new(),
        }
    }
}

impl SessionState {
    pub fn load(path: &Path) -> Self {
        let Ok(bytes) = fs::read(path) else {
            return Self::default();
        };
        let Ok(mut state) = serde_json::from_slice::<Self>(&bytes) else {
            return Self::default();
        };
        if state.version != SESSION_VERSION {
            return Self::default();
        }
        state.roots.retain(|path| path.is_dir());
        state.tabs.retain(|uri| match uri {
            DocumentUri::Local(path) => path.is_file(),
            DocumentUri::Ssh { .. } => true,
        });
        if state
            .active
            .as_ref()
            .is_some_and(|active| !state.tabs.contains(active))
        {
            state.active = state.tabs.first().cloned();
        }
        state.bottom_panel_height = state.bottom_panel_height.clamp(80.0, 1200.0);
        state
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_json_atomic(path, self)
    }
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = path.with_extension(format!("tmp-{stamp}"));
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    fs::write(&temp, bytes)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(temp, path)
}

#[derive(Debug, Clone, Default)]
pub struct ExcludeMatcher {
    pub patterns: Vec<glob::Pattern>,
}

impl ExcludeMatcher {
    pub fn load_for_root(root_path: &Path) -> Self {
        let exclude_file = root_path.join(".blue").join("exclude");
        let mut patterns = Vec::new();
        if let Ok(content) = fs::read_to_string(exclude_file) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                if let Ok(pat) = glob::Pattern::new(trimmed) {
                    patterns.push(pat);
                }
            }
        }
        Self { patterns }
    }

    pub fn is_excluded(&self, path: &Path, root_path: &Path) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            for pattern in &self.patterns {
                if pattern.matches(name) {
                    return true;
                }
            }
        }
        if let Ok(rel) = path.strip_prefix(root_path) {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            for pattern in &self.patterns {
                if pattern.matches(&rel_str) {
                    return true;
                }
                for component in rel.components() {
                    if let Some(comp_str) = component.as_os_str().to_str() {
                        if pattern.matches(comp_str) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("blue-ide-{name}-{unique}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn deepest_canonical_root_owns_nested_files() {
        let base = temp_dir("roots");
        let nested = base.join("crates").join("app");
        fs::create_dir_all(&nested).unwrap();
        let mut workspace = Workspace::default();
        let outer = workspace.add_root(&base).unwrap();
        let inner = workspace.add_root(&nested).unwrap();
        assert_eq!(
            workspace.owner_of(&nested.join("src/new.rs")).unwrap().id,
            inner
        );
        assert_eq!(
            workspace.owner_of(&base.join("README.md")).unwrap().id,
            outer
        );
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn trust_defaults_to_deny_and_persists_outside_workspace() {
        let root_path = temp_dir("trust-root");
        let config = temp_dir("trust-config").join("trust.json");
        let mut workspace = Workspace::default();
        let id = workspace.add_root(&root_path).unwrap();
        let root = workspace.root(id).unwrap();
        let mut trust = TrustStore::load(&config).unwrap();
        assert!(!trust.permits(root, ExecutableCapability::Terminal));
        trust.set(root, TrustState::Trusted).unwrap();
        assert!(TrustStore::load(&config)
            .unwrap()
            .permits(root, ExecutableCapability::Lsp));
        fs::remove_dir_all(root_path).unwrap();
        fs::remove_dir_all(config.parent().unwrap()).unwrap();
    }

    #[test]
    fn jobs_clamp_progress_and_have_terminal_states() {
        let mut jobs = BackgroundJobs::default();
        let id = jobs.start(RootId(2), "fetch");
        assert!(jobs.set_progress(id, 4.0));
        assert_eq!(jobs.get(id).unwrap().progress, Some(1.0));
        assert!(jobs.request_cancel(id));
        assert!(jobs.finish(id, Ok(())));
        assert_eq!(jobs.get(id).unwrap().status, BackgroundJobStatus::Cancelled);
        assert!(!jobs.set_progress(id, 0.5));
    }

    #[test]
    fn corrupt_session_recovers_and_missing_local_tabs_are_skipped() {
        let dir = temp_dir("session");
        let path = dir.join("session.json");
        fs::write(&path, b"not-json").unwrap();
        assert_eq!(SessionState::load(&path), SessionState::default());
        let present = dir.join("present.rs");
        fs::write(&present, "fn main() {}").unwrap();
        let mut state = SessionState::default();
        state.tabs = vec![
            DocumentUri::local(dir.join("missing.rs")),
            DocumentUri::local(&present),
        ];
        state.active = state.tabs.first().cloned();
        state.save(&path).unwrap();
        let loaded = SessionState::load(&path);
        assert_eq!(loaded.tabs, vec![DocumentUri::local(&present)]);
        assert_eq!(loaded.active, Some(DocumentUri::local(&present)));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ssh_document_identity_is_distinct_from_local_paths() {
        let remote = DocumentUri::ssh("prod", "/srv/app/main.rs").unwrap();
        assert_eq!(remote.to_string(), "ssh://prod/srv/app/main.rs");
        assert_ne!(remote, DocumentUri::local("/srv/app/main.rs"));
        assert!(DocumentUri::ssh("", "/file").is_err());
        assert!(DocumentUri::ssh("prod", "relative").is_err());
    }

    #[test]
    fn exclude_matcher_matches_simple_and_relative_patterns() {
        let root = temp_dir("exclude-test");
        let blue = root.join(".blue");
        fs::create_dir_all(&blue).unwrap();
        fs::write(blue.join("exclude"), b"target\n*.o\nsrc/ignored.rs\n# comment\n").unwrap();

        let matcher = ExcludeMatcher::load_for_root(&root);
        assert!(matcher.is_excluded(&root.join("target"), &root));
        assert!(matcher.is_excluded(&root.join("target/debug/foo"), &root));
        assert!(matcher.is_excluded(&root.join("main.o"), &root));
        assert!(matcher.is_excluded(&root.join("src/ignored.rs"), &root));
        assert!(!matcher.is_excluded(&root.join("src/main.rs"), &root));

        fs::remove_dir_all(root).unwrap();
    }
}
