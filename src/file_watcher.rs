//! Simple polling-based file watcher for live-reload in image and markdown panes.
//!
//! No inotify/FSEvents — polls `metadata().modified()` every `poll_interval`.
//! The poll happens in the main update loop guarded by a time comparison,
//! so it never stalls frame rendering (< 1ms per file for `fs::metadata`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

/// A simple polling file watcher.
///
/// Register paths with `watch(path)`, unregister with `unwatch(path)`.
/// Call `poll()` each frame — it returns paths whose modification time changed.
pub struct FileWatcher {
    watched: HashMap<PathBuf, SystemTime>,
    poll_interval: Duration,
    last_poll: Instant,
}

impl Default for FileWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl FileWatcher {
    pub fn new() -> Self {
        Self {
            watched: HashMap::new(),
            poll_interval: Duration::from_secs(2),
            last_poll: Instant::now(),
        }
    }

    /// Start watching a file. No-op if already watched.
    pub fn watch(&mut self, path: PathBuf) {
        let mtime = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        self.watched.entry(path).or_insert(mtime);
    }

    /// Stop watching a file.
    pub fn unwatch(&mut self, path: &PathBuf) {
        self.watched.remove(path);
    }

    /// Poll for changed files. Returns at most once every `poll_interval`.
    /// Returns the list of paths whose modification time changed since last poll.
    pub fn poll(&mut self) -> Vec<PathBuf> {
        if self.last_poll.elapsed() < self.poll_interval {
            return Vec::new();
        }
        self.last_poll = Instant::now();

        let mut changed = Vec::new();
        for (path, stored_mtime) in &mut self.watched {
            let current_mtime = std::fs::metadata(path)
                .and_then(|m| m.modified())
                .unwrap_or(*stored_mtime);
            if current_mtime != *stored_mtime {
                *stored_mtime = current_mtime;
                changed.push(path.clone());
            }
        }
        changed
    }

    /// True if any paths are being watched.
    pub fn is_watching_any(&self) -> bool {
        !self.watched.is_empty()
    }
}
