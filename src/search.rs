/// Search engine for Blue IDE – completely decoupled from egui rendering.
///
/// # Byte offsets vs. character indices
///
/// The `regex` crate operates on UTF-8 byte offsets.  Ropey stores text with
/// Unicode-scalar (char) indices.  Every public conversion that crosses the
/// boundary is documented at the call site.  We never place a cursor inside a
/// multi-byte code point; `TextBuffer::byte_index_to_position` already enforces
/// that invariant.
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use regex::{Regex, RegexBuilder};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Where the search is scoped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchScope {
    #[default]
    File,
    Project,
}

/// Whether the panel shows Find-only or Find+Replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchMode {
    #[default]
    Find,
    Replace,
}

/// All the options that define a search.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchQuery {
    pub query: String,
    pub replacement: String,
    pub use_regex: bool,
    pub case_sensitive: bool,
    pub scope: SearchScope,
    /// Optional glob pattern to include only matching files, e.g. `"*.rs"`.
    /// Empty string means "match all".
    pub include_glob: String,
    /// Optional glob pattern to exclude matching files, e.g. `"tests/**"`.
    /// Empty string means "exclude nothing".
    pub exclude_glob: String,
}

impl SearchQuery {
    /// Returns `true` if the query string is non-empty.
    pub fn is_non_empty(&self) -> bool {
        !self.query.is_empty()
    }

    /// Returns the include glob pattern if non-empty.
    pub fn include_pattern(&self) -> Option<&str> {
        if self.include_glob.is_empty() {
            None
        } else {
            Some(&self.include_glob)
        }
    }

    /// Returns the exclude glob pattern if non-empty.
    pub fn exclude_pattern(&self) -> Option<&str> {
        if self.exclude_glob.is_empty() {
            None
        } else {
            Some(&self.exclude_glob)
        }
    }
}

/// A single hit returned by the search engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    /// File the match lives in.
    pub path: PathBuf,
    /// UTF-8 byte range within the file's full text.
    pub byte_range: Range<usize>,
    /// Zero-based line number.
    pub line: usize,
    /// Zero-based column (byte offset from start of line).
    pub col: usize,
    /// A short preview of the matching line (the full logical line, trimmed).
    pub line_preview: String,
}

// ---------------------------------------------------------------------------
// Project-search worker message
// ---------------------------------------------------------------------------

/// Message sent from the background worker to the UI thread.
#[derive(Debug)]
pub struct ProjectSearchBatch {
    /// Generation this batch belongs to.  Stale batches (generation !=
    /// `SearchState::project_generation`) must be silently discarded.
    pub generation: u64,
    /// Accumulated matches so far (always the full result set, not a delta).
    pub matches: Vec<SearchMatch>,
    /// `true` iff the search has finished.
    pub done: bool,
    /// Files that could not be read.
    pub failures: Vec<(PathBuf, String)>,
}

// ---------------------------------------------------------------------------
// Compiled pattern – handles both literal and regex modes
// ---------------------------------------------------------------------------

/// A compiled, ready-to-use search pattern.
pub struct CompiledPattern {
    regex: Regex,
    /// Original query text (useful for zero-width match detection).
    query_len: usize,
}

impl CompiledPattern {
    /// Compile a pattern from a `SearchQuery`.  Returns an `Err` string when
    /// the regex is syntactically invalid.
    pub fn compile(query: &SearchQuery) -> Result<Self, String> {
        if query.query.is_empty() {
            // Callers must check `is_non_empty` first; guard here for safety.
            return Err("empty query".to_owned());
        }

        let pattern = if query.use_regex {
            query.query.clone()
        } else {
            // Escape the literal string so regex meta-characters are treated as
            // ordinary characters.
            regex::escape(&query.query)
        };

        let regex = RegexBuilder::new(&pattern)
            .case_insensitive(!query.case_sensitive)
            .build()
            .map_err(|error| error.to_string())?;

        Ok(Self {
            query_len: query.query.chars().count(),
            regex,
        })
    }

    /// Find all non-overlapping matches in `text`.
    ///
    /// Zero-width matches are advanced by one byte after each hit to avoid
    /// infinite loops.
    pub fn find_all(&self, text: &str) -> Vec<Range<usize>> {
        let mut results = Vec::new();
        let mut search_from = 0usize;
        while search_from <= text.len() {
            let Some(m) = self.regex.find(&text[search_from..]) else {
                break;
            };
            let abs_start = search_from + m.start();
            let abs_end = search_from + m.end();
            results.push(abs_start..abs_end);
            if abs_end == abs_start {
                // Zero-width match: advance by one byte (stay on char boundary).
                search_from = advance_one_char(text, abs_end);
            } else {
                search_from = abs_end;
            }
        }
        results
    }

    /// Apply capture-group expansion to `replacement` for a given match.
    ///
    /// Falls back to a literal replacement if capture expansion fails.
    pub fn replace_with(
        &self,
        text: &str,
        byte_range: Range<usize>,
        replacement: &str,
    ) -> Option<String> {
        let m = self.regex.find(&text[byte_range.start..])?;
        // Recheck that the match still starts at the expected position.
        if m.start() != 0 {
            return None;
        }
        // Use the Captures interface for expansion.
        let caps = self.regex.captures(&text[byte_range.start..])?;
        let mut expanded = String::new();
        caps.expand(replacement, &mut expanded);
        Some(expanded)
    }

    /// Returns `true` if the regex can match zero-length strings.
    pub fn can_be_zero_width(&self) -> bool {
        self.query_len == 0
    }
}

/// Advance `pos` to the next valid UTF-8 char boundary, staying within
/// `[pos, text.len()]`.
fn advance_one_char(text: &str, pos: usize) -> usize {
    if pos >= text.len() {
        return pos + 1; // signals "done" to callers
    }
    let mut next = pos + 1;
    while next < text.len() && !text.is_char_boundary(next) {
        next += 1;
    }
    next
}

// ---------------------------------------------------------------------------
// File-scope search
// ---------------------------------------------------------------------------

/// Maximum file size (in bytes) that the project walker will read.
/// Files larger than this are skipped.
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024; // 5 MiB

/// Search `text` (the full content of `path`) for `pattern`.
///
/// Returns `SearchMatch` values with byte ranges relative to the start of
/// `text`.  The results are in match-position order.
pub fn search_text(text: &str, path: &Path, pattern: &CompiledPattern) -> Vec<SearchMatch> {
    let byte_ranges = pattern.find_all(text);
    byte_ranges
        .into_iter()
        .map(|byte_range| {
            // Derive line / column by scanning the text once per match.
            // For large files with many matches this is O(matches * line_length)
            // but avoids building an auxiliary line-offset table.
            let (line, col, line_preview) = line_col_preview(text, byte_range.start);
            SearchMatch {
                path: path.to_path_buf(),
                byte_range,
                line,
                col,
                line_preview,
            }
        })
        .collect()
}

/// Derive (line, col, preview) for a byte offset within `text`.
///
/// `col` is the byte offset from the start of the line (consistent with how
/// the regex crate counts within a line slice).
fn line_col_preview(text: &str, byte_offset: usize) -> (usize, usize, String) {
    let before = &text[..byte_offset.min(text.len())];
    let line = before.bytes().filter(|&b| b == b'\n').count();
    let line_start = before.rfind('\n').map(|pos| pos + 1).unwrap_or(0);
    let col = byte_offset - line_start;

    // Extract the full logical line for the preview.
    let line_end = text[byte_offset..]
        .find('\n')
        .map(|pos| byte_offset + pos)
        .unwrap_or(text.len());
    // Trim trailing \r as well.
    let raw = &text[line_start..line_end];
    let preview = raw.trim_end_matches('\r').to_owned();

    (line, col, preview)
}

// ---------------------------------------------------------------------------
// Project search worker
// ---------------------------------------------------------------------------

/// Shared cancellation / generation counter.
pub type Generation = Arc<AtomicU64>;

/// Spawn a background thread that walks `root`, reads text files, and sends
/// results back through `tx` in batches.
///
/// # Arguments
/// * `root` – the workspace root to walk.
/// * `query` – the compiled query.  Passed by value because the thread owns
///   it.
/// * `open_files` – a snapshot of `(path, text)` for any buffer currently open
///   in the editor.  The thread will use these in place of the on-disk content
///   so that unsaved edits are included.
/// * `generation` – a shared counter; if the value at the time the thread
///   started changes before the thread finishes, all subsequent results are
///   discarded.
/// * `start_generation` – the value at spawn time.
/// * `tx` – channel for sending `ProjectSearchBatch` to the UI.
/// * `ctx` – egui context used to request repaints when results arrive.
#[allow(clippy::too_many_arguments)]
pub fn spawn_project_search(
    root: PathBuf,
    _query: Arc<SearchQuery>,
    pattern: Arc<CompiledPattern>,
    open_files: Vec<(PathBuf, String)>,
    generation: Generation,
    start_generation: u64,
    tx: Sender<ProjectSearchBatch>,
    ctx: egui::Context,
) {
    std::thread::spawn(move || {
        let mut all_matches: Vec<SearchMatch> = Vec::new();
        let mut failures: Vec<(PathBuf, String)> = Vec::new();

        // Build a set of paths covered by open buffers so we don't also
        // search the same file from disk.
        let open_paths: std::collections::HashSet<PathBuf> =
            open_files.iter().map(|(p, _)| p.clone()).collect();

        // Search open buffers first.
        for (path, text) in &open_files {
            if generation.load(Ordering::Relaxed) != start_generation {
                return; // cancelled
            }
            let mut hits = search_text(text, path, &pattern);
            all_matches.append(&mut hits);
        }

        // Walk the project directory.
        let walker = ignore::WalkBuilder::new(&root)
            .follow_links(false)
            .hidden(false) // respect .gitignore but visit hidden files that aren't ignored
            .build();

        // Pre-compile glob patterns (if provided) to avoid recompiling per entry.
        let include_pattern = _query
            .include_pattern()
            .and_then(|pat| glob::Pattern::new(pat).ok());
        let exclude_pattern = _query
            .exclude_pattern()
            .and_then(|pat| glob::Pattern::new(pat).ok());

        for result in walker {
            if generation.load(Ordering::Relaxed) != start_generation {
                return; // cancelled
            }

            let entry = match result {
                Ok(entry) => entry,
                Err(err) => {
                    failures.push((root.clone(), err.to_string()));
                    continue;
                }
            };

            let path = entry.path().to_path_buf();

            // Skip directories and already-covered open buffers.
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                continue;
            }
            if open_paths.contains(&path) {
                continue;
            }

            // Apply include/exclude glob filters against the file name and full path.
            if let Some(ref inc) = include_pattern {
                // Try matching against file name first, then full path string.
                let name_matches = path
                    .file_name()
                    .map(|n| inc.matches(n.to_string_lossy().as_ref()))
                    .unwrap_or(false);
                let path_matches = inc.matches_path(&path);
                if !name_matches && !path_matches {
                    continue;
                }
            }
            if let Some(ref exc) = exclude_pattern {
                let name_matches = path
                    .file_name()
                    .map(|n| exc.matches(n.to_string_lossy().as_ref()))
                    .unwrap_or(false);
                let path_matches = exc.matches_path(&path);
                if name_matches || path_matches {
                    continue;
                }
            }

            // Apply file-size guard.
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(err) => {
                    failures.push((path.clone(), err.to_string()));
                    continue;
                }
            };
            if metadata.len() > MAX_FILE_BYTES {
                continue;
            }

            // Read and validate UTF-8.
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(err) => {
                    // Silently skip binary / non-UTF-8 files.
                    if err.kind() != std::io::ErrorKind::InvalidData {
                        failures.push((path.clone(), err.to_string()));
                    }
                    continue;
                }
            };

            let mut hits = search_text(&text, &path, &pattern);
            if !hits.is_empty() {
                all_matches.append(&mut hits);
                // Send an incremental update so the UI stays responsive.
                let batch = ProjectSearchBatch {
                    generation: start_generation,
                    matches: all_matches.clone(),
                    done: false,
                    failures: failures.clone(),
                };
                let _ = tx.send(batch);
                ctx.request_repaint();
            }
        }

        // Final batch.
        // Sort: by path then by byte position for determinism.
        all_matches.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then_with(|| a.byte_range.start.cmp(&b.byte_range.start))
        });

        let batch = ProjectSearchBatch {
            generation: start_generation,
            matches: all_matches,
            done: true,
            failures,
        };
        let _ = tx.send(batch);
        ctx.request_repaint();
    });
}

// ---------------------------------------------------------------------------
// SearchState – lives inside BlueIdeApp
// ---------------------------------------------------------------------------

/// Encapsulates all mutable state for the Find/Replace panel.
pub struct SearchState {
    // Panel visibility and mode
    pub visible: bool,
    pub mode: SearchMode,

    // Current query as the user typed it
    pub query: SearchQuery,

    // Compiled pattern + its error string (None if no compile error)
    compiled: Option<CompiledPattern>,
    pub compile_error: Option<String>,

    // File-scope result cache
    pub file_matches: Vec<SearchMatch>,
    /// The query options + buffer revision at which `file_matches` was computed.
    cache_key: Option<FileCacheKey>,

    // Active match index (into `file_matches` for file scope, or
    // `project_matches` for project scope).
    pub active_index: Option<usize>,

    // Project-scope state
    pub project_matches: Vec<SearchMatch>,
    pub project_done: bool,
    pub project_failures: Vec<(PathBuf, String)>,
    project_generation: Generation,
    current_generation: u64,
    project_rx: Option<Receiver<ProjectSearchBatch>>,

    // Focus requests for egui
    /// When set the panel should request focus on the query TextEdit next frame.
    pub want_query_focus: bool,
    /// When set the panel should select all text in the query TextEdit next frame.
    pub want_query_select_all: bool,

    // Replace-all report (shown after a Replace All finishes)
    pub last_replace_report: Option<ReplaceReport>,

    /// Paths whose match groups are collapsed in the project-results list.
    pub collapsed_files: std::collections::HashSet<PathBuf>,

    /// When `Some`, the UI should show a Replace All confirmation dialog.
    /// The tuple is `(match_count, file_count)` computed at confirmation time.
    pub pending_replace_confirm: Option<(usize, usize)>,
}

#[derive(Debug, Clone)]
pub struct ReplaceReport {
    pub replaced: usize,
    pub files_affected: usize,
    pub failures: Vec<(PathBuf, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileCacheKey {
    query: String,
    use_regex: bool,
    case_sensitive: bool,
    scope: SearchScope,
    include_glob: String,
    exclude_glob: String,
    buffer_revision: u64,
    buffer_path: Option<PathBuf>,
}

impl SearchState {
    pub fn new() -> Self {
        Self {
            visible: false,
            mode: SearchMode::Find,
            query: SearchQuery::default(),
            compiled: None,
            compile_error: None,
            file_matches: Vec::new(),
            cache_key: None,
            active_index: None,
            project_matches: Vec::new(),
            project_done: false,
            project_failures: Vec::new(),
            project_generation: Arc::new(AtomicU64::new(0)),
            current_generation: 0,
            project_rx: None,
            want_query_focus: false,
            want_query_select_all: false,
            last_replace_report: None,
            collapsed_files: std::collections::HashSet::new(),
            pending_replace_confirm: None,
        }
    }

    // -----------------------------------------------------------------------
    // File-group collapse helpers
    // -----------------------------------------------------------------------

    /// Toggle the collapsed state of a file group in project results.
    pub fn toggle_file_collapsed(&mut self, path: &PathBuf) {
        if self.collapsed_files.contains(path) {
            self.collapsed_files.remove(path);
        } else {
            self.collapsed_files.insert(path.clone());
        }
    }

    /// Returns `true` when the given file group is collapsed.
    pub fn is_file_collapsed(&self, path: &PathBuf) -> bool {
        self.collapsed_files.contains(path)
    }

    // -----------------------------------------------------------------------
    // Replace All confirmation helpers
    // -----------------------------------------------------------------------

    /// Request a Replace All confirmation dialog by computing match/file counts.
    /// The UI should show the dialog when `pending_replace_confirm` is `Some`.
    pub fn request_replace_confirm(&mut self) {
        let match_count = match self.query.scope {
            SearchScope::File => self.file_matches.len(),
            SearchScope::Project => self.project_matches.len(),
        };
        // Count distinct files.
        let file_count = match self.query.scope {
            SearchScope::File => {
                if match_count > 0 {
                    1
                } else {
                    0
                }
            }
            SearchScope::Project => {
                let mut seen = std::collections::HashSet::new();
                for m in &self.project_matches {
                    seen.insert(&m.path);
                }
                seen.len()
            }
        };
        if match_count > 0 {
            self.pending_replace_confirm = Some((match_count, file_count));
        }
    }

    /// Discard a pending Replace All confirmation (user pressed Cancel).
    pub fn cancel_replace_confirm(&mut self) {
        self.pending_replace_confirm = None;
    }

    // -----------------------------------------------------------------------
    // Panel open / close helpers
    // -----------------------------------------------------------------------

    /// Open the panel in `Find` mode or re-focus it if already open.
    pub fn open_find(&mut self) {
        if self.visible {
            self.want_query_focus = true;
            self.want_query_select_all = true;
        } else {
            self.visible = true;
            self.mode = SearchMode::Find;
            self.want_query_focus = true;
            self.want_query_select_all = true;
        }
    }

    /// Open the panel in `Replace` mode.  Preserves the current query.
    pub fn open_replace(&mut self) {
        if self.visible && self.mode == SearchMode::Replace {
            self.want_query_focus = true;
            self.want_query_select_all = true;
        } else {
            self.visible = true;
            self.mode = SearchMode::Replace;
            self.want_query_focus = true;
        }
    }

    /// Close the panel.
    pub fn close(&mut self) {
        self.visible = false;
    }

    // -----------------------------------------------------------------------
    // Pattern recompilation
    // -----------------------------------------------------------------------

    /// (Re-)compile the pattern from the current query.  Must be called
    /// whenever `query` changes.
    pub fn recompile(&mut self) {
        if !self.query.is_non_empty() {
            self.compiled = None;
            self.compile_error = None;
            return;
        }
        match CompiledPattern::compile(&self.query) {
            Ok(pat) => {
                self.compiled = Some(pat);
                self.compile_error = None;
            }
            Err(err) => {
                self.compiled = None;
                self.compile_error = Some(err);
            }
        }
    }

    /// Returns the compiled pattern, or `None` if the query is empty or invalid.
    pub fn compiled_pattern(&self) -> Option<&CompiledPattern> {
        self.compiled.as_ref()
    }

    // -----------------------------------------------------------------------
    // File-scope search
    // -----------------------------------------------------------------------

    /// Recompute file-scope matches if the cache key has changed.
    ///
    /// `text` is the full content of the active buffer (as a `&str`).
    /// `buffer_path` and `buffer_revision` are used to detect staleness.
    pub fn refresh_file_matches(
        &mut self,
        text: &str,
        buffer_path: Option<&Path>,
        buffer_revision: u64,
    ) {
        let key = FileCacheKey {
            query: self.query.query.clone(),
            use_regex: self.query.use_regex,
            case_sensitive: self.query.case_sensitive,
            scope: self.query.scope,
            include_glob: self.query.include_glob.clone(),
            exclude_glob: self.query.exclude_glob.clone(),
            buffer_revision,
            buffer_path: buffer_path.map(PathBuf::from),
        };
        if self.cache_key.as_ref() == Some(&key) {
            return; // still valid
        }
        self.cache_key = Some(key);
        self.file_matches.clear();
        self.active_index = None;

        let Some(pattern) = &self.compiled else {
            return;
        };
        let path = buffer_path.unwrap_or_else(|| Path::new("<unsaved>"));
        self.file_matches = search_text(text, path, pattern);
    }

    /// Set the active match index closest to `cursor_byte` (e.g. after
    /// opening the panel while the cursor is somewhere in the file).
    pub fn set_active_near_byte(&mut self, cursor_byte: usize) {
        if self.file_matches.is_empty() {
            self.active_index = None;
            return;
        }
        // Choose the first match whose start >= cursor; wrap to 0 if none.
        let idx = self
            .file_matches
            .iter()
            .position(|m| m.byte_range.start >= cursor_byte)
            .unwrap_or(0);
        self.active_index = Some(idx);
    }

    /// Move to the next match (wrapping).
    pub fn next_match(&mut self) {
        let count = match self.query.scope {
            SearchScope::File => self.file_matches.len(),
            SearchScope::Project => self.project_matches.len(),
        };
        if count == 0 {
            self.active_index = None;
            return;
        }
        self.active_index = Some(match self.active_index {
            None => 0,
            Some(i) => (i + 1) % count,
        });
    }

    /// Move to the previous match (wrapping).
    pub fn prev_match(&mut self) {
        let count = match self.query.scope {
            SearchScope::File => self.file_matches.len(),
            SearchScope::Project => self.project_matches.len(),
        };
        if count == 0 {
            self.active_index = None;
            return;
        }
        self.active_index = Some(match self.active_index {
            None | Some(0) => count - 1,
            Some(i) => i - 1,
        });
    }

    /// Return the currently active `SearchMatch` for file scope.
    pub fn active_file_match(&self) -> Option<&SearchMatch> {
        let idx = self.active_index?;
        self.file_matches.get(idx)
    }

    /// Return the currently active `SearchMatch` for project scope.
    pub fn active_project_match(&self) -> Option<&SearchMatch> {
        let idx = self.active_index?;
        self.project_matches.get(idx)
    }

    /// Invalidate the file-scope cache (e.g. when the active tab changes).
    pub fn invalidate_file_cache(&mut self) {
        self.cache_key = None;
        self.file_matches.clear();
        self.active_index = None;
    }

    // -----------------------------------------------------------------------
    // Project-scope search
    // -----------------------------------------------------------------------

    /// Start a new project-wide search, cancelling any in-progress one.
    pub fn start_project_search(
        &mut self,
        root: PathBuf,
        open_files: Vec<(PathBuf, String)>,
        ctx: egui::Context,
    ) {
        let Some(_pattern) = &self.compiled else {
            self.project_matches.clear();
            self.project_done = true;
            return;
        };

        // Bump generation to invalidate any running worker.
        self.current_generation = self.project_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.project_matches.clear();
        self.project_done = false;
        self.project_failures.clear();
        self.active_index = None;

        let (tx, rx) = crossbeam_channel::unbounded();
        self.project_rx = Some(rx);

        spawn_project_search(
            root,
            Arc::new(self.query.clone()),
            Arc::new(CompiledPattern::compile(&self.query).expect("already compiled")),
            open_files,
            Arc::clone(&self.project_generation),
            self.current_generation,
            tx,
            ctx,
        );
    }

    /// Poll the project-search channel and integrate any waiting results.
    /// Call once per frame.
    pub fn poll_project_results(&mut self) {
        let Some(rx) = &self.project_rx else {
            return;
        };
        // Drain all pending messages.
        loop {
            match rx.try_recv() {
                Ok(batch) => {
                    if batch.generation != self.current_generation {
                        continue; // stale
                    }
                    self.project_matches = batch.matches;
                    self.project_failures = batch.failures;
                    if batch.done {
                        self.project_done = true;
                        self.project_rx = None;
                        break;
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.project_done = true;
                    self.project_rx = None;
                    break;
                }
            }
        }
    }

    /// Return whether a project search is currently running.
    pub fn project_searching(&self) -> bool {
        self.project_rx.is_some()
    }
}

impl Default for SearchState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Replacement helpers (pure, no UI)
// ---------------------------------------------------------------------------

/// Compute replacement text for a single match.
///
/// Returns `None` when regex capture-expansion fails (the match is stale).
pub fn compute_replacement(
    text: &str,
    byte_range: Range<usize>,
    pattern: &CompiledPattern,
    replacement: &str,
    use_regex: bool,
) -> Option<String> {
    if use_regex {
        pattern.replace_with(text, byte_range, replacement)
    } else {
        Some(replacement.to_owned())
    }
}

/// Given a list of `(byte_range, replacement_text)` pairs, apply all of them
/// to `original` from **right to left** so that earlier byte offsets remain
/// stable.
///
/// Returns `Err` if:
/// - any range end exceeds `original.len()`
/// - ranges overlap each other
/// - any byte offset is not on a UTF-8 char boundary
pub fn apply_replacements(
    original: &str,
    mut replacements: Vec<(Range<usize>, String)>,
) -> Result<String, String> {
    // Sort right-to-left so offsets stay stable as we apply each replacement.
    replacements.sort_by_key(|b| std::cmp::Reverse(b.0.start));

    // Validate: check boundaries and overlaps.
    let mut last_start = original.len() + 1;
    for (range, _) in &replacements {
        if range.end > original.len() {
            return Err(format!(
                "byte range {}..{} exceeds text length {}",
                range.start,
                range.end,
                original.len()
            ));
        }
        if !original.is_char_boundary(range.start) || !original.is_char_boundary(range.end) {
            return Err(format!(
                "byte range {}..{} is not on a UTF-8 char boundary",
                range.start, range.end
            ));
        }
        if range.end > last_start {
            return Err(format!(
                "overlapping ranges: {}..{} overlaps previous range ending at {}",
                range.start, range.end, last_start
            ));
        }
        last_start = range.start;
    }

    let mut result = original.to_owned();
    for (range, replacement) in replacements {
        result.replace_range(range, &replacement);
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn query(q: &str, use_regex: bool, case_sensitive: bool) -> SearchQuery {
        SearchQuery {
            query: q.to_owned(),
            replacement: String::new(),
            use_regex,
            case_sensitive,
            scope: SearchScope::File,
            include_glob: String::new(),
            exclude_glob: String::new(),
        }
    }

    fn compile(q: &SearchQuery) -> CompiledPattern {
        CompiledPattern::compile(q).expect("should compile")
    }

    // --- CompiledPattern ---

    #[test]
    fn literal_search_finds_all_occurrences() {
        let q = query("foo", false, true);
        let pat = compile(&q);
        let text = "foo bar foo baz foo";
        let ranges = pat.find_all(text);
        assert_eq!(ranges.len(), 3);
        assert_eq!(&text[ranges[0].clone()], "foo");
    }

    #[test]
    fn literal_search_is_case_sensitive_by_default() {
        let q = query("Foo", false, true);
        let pat = compile(&q);
        assert_eq!(pat.find_all("foo Foo FOO").len(), 1);
    }

    #[test]
    fn case_insensitive_literal_search() {
        let q = query("foo", false, false);
        let pat = compile(&q);
        assert_eq!(pat.find_all("foo Foo FOO").len(), 3);
    }

    #[test]
    fn regex_search_finds_pattern() {
        let q = query(r"\d+", true, true);
        let pat = compile(&q);
        let text = "abc 123 def 456";
        let ranges = pat.find_all(text);
        assert_eq!(ranges.len(), 2);
        assert_eq!(&text[ranges[0].clone()], "123");
        assert_eq!(&text[ranges[1].clone()], "456");
    }

    #[test]
    fn invalid_regex_returns_error() {
        let q = query(r"[unclosed", true, true);
        assert!(CompiledPattern::compile(&q).is_err());
    }

    #[test]
    fn zero_width_match_does_not_loop() {
        let q = query(r"\b", true, true);
        let pat = compile(&q);
        // Should terminate and return a bounded number of results.
        let ranges = pat.find_all("hello world");
        // "hello" and "world" each have 2 word boundaries → 4 total.
        assert!(ranges.len() <= 10);
    }

    #[test]
    fn empty_query_is_rejected() {
        let q = query("", false, true);
        assert!(CompiledPattern::compile(&q).is_err());
    }

    #[test]
    fn regex_capture_expansion() {
        let q = SearchQuery {
            query: r"(\w+)\s(\w+)".to_owned(),
            replacement: "$2 $1".to_owned(),
            use_regex: true,
            case_sensitive: true,
            scope: SearchScope::File,
            include_glob: String::new(),
            exclude_glob: String::new(),
        };
        let pat = compile(&q);
        let text = "hello world";
        let ranges = pat.find_all(text);
        assert_eq!(ranges.len(), 1);
        let replaced = pat
            .replace_with(text, ranges[0].clone(), &q.replacement)
            .unwrap();
        assert_eq!(replaced, "world hello");
    }

    #[test]
    fn literal_meta_characters_are_escaped() {
        let q = query("a.b", false, true);
        let pat = compile(&q);
        // Should only match literal "a.b", not "axb".
        let text = "a.b axb";
        let ranges = pat.find_all(text);
        assert_eq!(ranges.len(), 1);
        assert_eq!(&text[ranges[0].clone()], "a.b");
    }

    // --- search_text ---

    #[test]
    fn search_text_returns_line_and_column() {
        let q = query("bar", false, true);
        let pat = compile(&q);
        let text = "foo\nbar\nbaz";
        let matches = search_text(text, Path::new("f.rs"), &pat);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 1);
        assert_eq!(matches[0].col, 0);
        assert_eq!(matches[0].line_preview, "bar");
    }

    #[test]
    fn search_text_no_results_for_empty_pattern_match() {
        let q = query("xyz", false, true);
        let pat = compile(&q);
        let matches = search_text("abc def", Path::new("f.rs"), &pat);
        assert!(matches.is_empty());
    }

    #[test]
    fn search_text_unicode_matches() {
        let q = query("café", false, true);
        let pat = compile(&q);
        let text = "hello café world";
        let matches = search_text(text, Path::new("f.rs"), &pat);
        assert_eq!(matches.len(), 1);
        assert_eq!(&text[matches[0].byte_range.clone()], "café");
    }

    #[test]
    fn search_text_emoji() {
        let q = query("🙂", false, true);
        let pat = compile(&q);
        let text = "hello 🙂 world";
        let matches = search_text(text, Path::new("f.rs"), &pat);
        assert_eq!(matches.len(), 1);
        assert_eq!(&text[matches[0].byte_range.clone()], "🙂");
    }

    #[test]
    fn search_text_crlf_file() {
        let q = query("bar", false, true);
        let pat = compile(&q);
        let text = "foo\r\nbar\r\nbaz";
        let matches = search_text(text, Path::new("f.rs"), &pat);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 1);
    }

    #[test]
    fn search_text_multiline_regex() {
        let q = query(r"foo\nbar", true, true);
        let pat = compile(&q);
        let text = "foo\nbar";
        let matches = search_text(text, Path::new("f.rs"), &pat);
        assert_eq!(matches.len(), 1);
    }

    // --- apply_replacements ---

    #[test]
    fn single_replacement_works() {
        let result = apply_replacements("hello world", vec![(6..11, "rust".to_owned())]).unwrap();
        assert_eq!(result, "hello rust");
    }

    #[test]
    fn multiple_replacements_applied_right_to_left() {
        let original = "aaa bbb ccc";
        let replacements = vec![
            (0..3, "AAA".to_owned()),
            (4..7, "BBB".to_owned()),
            (8..11, "CCC".to_owned()),
        ];
        let result = apply_replacements(original, replacements).unwrap();
        assert_eq!(result, "AAA BBB CCC");
    }

    #[test]
    fn overlapping_ranges_return_error() {
        let result = apply_replacements(
            "hello",
            vec![(0..3, "A".to_owned()), (2..5, "B".to_owned())],
        );
        assert!(result.is_err());
    }

    #[test]
    fn out_of_bounds_range_returns_error() {
        let result = apply_replacements("hi", vec![(0..99, "x".to_owned())]);
        assert!(result.is_err());
    }

    #[test]
    fn interior_utf8_range_returns_error() {
        let text = "café"; // 'é' is 2 bytes: 0xC3 0xA9 at bytes 3..5
                           // byte 4 is inside 'é' – not a char boundary
        let result = apply_replacements(text, vec![(3..4, "e".to_owned())]);
        assert!(result.is_err());
    }

    // --- SearchState ---

    #[test]
    fn open_find_sets_visible_and_mode() {
        let mut state = SearchState::new();
        state.open_find();
        assert!(state.visible);
        assert_eq!(state.mode, SearchMode::Find);
        assert!(state.want_query_focus);
    }

    #[test]
    fn open_replace_expands_existing_find_panel() {
        let mut state = SearchState::new();
        state.query.query = "foo".to_owned();
        state.open_find();
        state.open_replace();
        assert_eq!(state.mode, SearchMode::Replace);
        assert_eq!(state.query.query, "foo"); // query preserved
    }

    #[test]
    fn close_hides_panel() {
        let mut state = SearchState::new();
        state.open_find();
        state.close();
        assert!(!state.visible);
    }

    #[test]
    fn next_match_wraps() {
        let mut state = SearchState::new();
        state.query.query = "x".to_owned();
        state.query.case_sensitive = true;
        state.recompile();
        state.refresh_file_matches("x y x", None, 0);
        assert_eq!(state.file_matches.len(), 2);
        state.active_index = Some(1);
        state.next_match();
        assert_eq!(state.active_index, Some(0));
    }

    #[test]
    fn prev_match_wraps() {
        let mut state = SearchState::new();
        state.query.query = "x".to_owned();
        state.query.case_sensitive = true;
        state.recompile();
        state.refresh_file_matches("x y x", None, 0);
        state.active_index = Some(0);
        state.prev_match();
        assert_eq!(state.active_index, Some(1));
    }

    #[test]
    fn empty_query_gives_no_matches() {
        let mut state = SearchState::new();
        state.recompile();
        state.refresh_file_matches("hello", None, 0);
        assert!(state.file_matches.is_empty());
    }

    #[test]
    fn file_cache_is_reused_on_same_revision() {
        let mut state = SearchState::new();
        state.query.query = "hello".to_owned();
        state.query.case_sensitive = true;
        state.recompile();
        state.refresh_file_matches("hello world", None, 42);
        let first_len = state.file_matches.len();
        // Call again with same inputs – must not clear the match list.
        state.refresh_file_matches("hello world", None, 42);
        assert_eq!(state.file_matches.len(), first_len);
    }

    #[test]
    fn file_cache_is_invalidated_on_revision_change() {
        let mut state = SearchState::new();
        state.query.query = "hello".to_owned();
        state.query.case_sensitive = true;
        state.recompile();
        state.refresh_file_matches("hello world", None, 1);
        assert_eq!(state.file_matches.len(), 1);
        state.refresh_file_matches("hello hello world", None, 2);
        assert_eq!(state.file_matches.len(), 2);
    }
}
