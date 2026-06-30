//! Editable text buffer: rope storage, cursor math, and LSP-facing coordinates.
//!
//! **Text replacement** (character- and byte-indexed):
//! - `replace_char_range` / `replace_byte_range` — single-span edits; invalid bounds
//!   return `Err` without mutating text or bumping `revision`.
//! - `apply_byte_replacements` — batch non-overlapping UTF-8 spans right-to-left with
//!   one `revision` increment; leaves the caret unchanged (callers reposition via
//!   `set_cursor_to_byte` when needed).
//! - `apply_lsp_text_edit` / `apply_completion_insertion` — single-span UTF-16 completion
//!   `textEdit` and plain-path insertion; literal text only (no snippet tab stops).
//!
//! **Cursor-safe edit operations**:
//! - `insert_str` / `delete_range` shift or clamp the caret when edits occur before,
//!   inside, or after the current position.
//! - `set_cursor_to_byte` and `byte_index_to_position` reject interior UTF-8 bytes and
//!   CRLF interior stops so the caret never lands inside a code point or `\r\n` pair.
//!
//! **Hover-related** (coordinates + stale guards only):
//! - **LSP position encoding** — map `CursorPosition` (Rust char index) to
//!   `LspPosition` (UTF-16 code units) via `position_lsp_position` /
//!   `cursor_lsp_position`; inbound UTF-16 columns decode via `decode_utf16_column`
//!   (`editor/position.rs`). Position unit tests: `cargo test --lib editor::position`;
//!   buffer wire integration: see **Buffer tests** (`cargo test --lib lsp_position`).
//! - **Stale-response snapshots** — monotonic `revision` and `lsp_version` captured
//!   by `app.rs` when sending hover; `HoverRequestSession::buffer_snapshot_matches`
//!   rejects responses after edits.
//! - **Document sync** — `needs_lsp_sync` / `mark_lsp_synced` track unsent `didChange`
//!   traffic before hover requests.
//!
//! Does not hit-test pointers, debounce hover, send LSP traffic, or render popups
//! (`widget.rs`, `app.rs`, `hover.rs`).
//!
//! # Buffer tests
//!
//! Part of crate-level **Regression tests** (`lib.rs`).
//!
//! Focused unit tests for `TextBuffer` edit semantics, coordinate conversion, and LSP
//! integration. Column math lives in `editor/position.rs`; buffer tests verify that
//! rope-backed line text is threaded correctly through encode/decode and UTF-16 edits.
//! `app.rs` and `widget.rs` call these APIs but do not reimplement buffer logic.
//!
//! | Area | Example tests | Run |
//! |------|---------------|-----|
//! | Empty / basic invariants | `empty_buffer_has_one_safe_editable_line` | `cargo test --lib editor::buffer` |
//! | Char ↔ byte coordinates | `utf8_conversions_*`, `byte_to_char_*` | `cargo test --lib byte_to_char` |
//! | Cursor-safe insert/delete | `insertion_and_deletion_*`, `insert_and_delete_adjust_cursor_*` | `cargo test --lib editor::buffer` |
//! | CRLF line endings | `crlf_is_one_logical_newline_for_editing`, `apply_byte_replacements_crlf` | `cargo test --lib crlf` |
//! | Char/byte range replacement | `replace_char_range_*`, `replace_byte_range_*` | `cargo test --lib replace_char_range` |
//! | Batch byte replacements | `apply_byte_replacements_*` | `cargo test --lib apply_byte_replacements` |
//! | Byte cursor placement | `set_cursor_to_byte_*` | `cargo test --lib set_cursor_to_byte` |
//! | LSP UTF-16 wire positions | `position_lsp_position_*`, `cursor_lsp_position_*`, `lsp_position_to_cursor_*`, `hover_request_position_*` | `cargo test --lib lsp_position` |
//! | Unicode/LSP position conversion (Always A18) | `unicode_lsp_position_conversion_is_correct` | `cargo test --lib unicode_lsp_position_conversion_is_correct` |
//! | Never: raw char columns on LSP wire | `use_raw_character_columns_as_lsp_utf16_columns` | `cargo test --lib use_raw_character_columns_as_lsp_utf16_columns` |
//! | LSP text edits | `apply_lsp_text_edit_*` | `cargo test --lib apply_lsp_text_edit` |
//! | Accept replaces identifier prefix | `accepting_a_completion_replaces_the_current_identifier_prefix` | `cargo test --lib accepting_a_completion_replaces` |
//! | App-layer prefix accept (Enter) | `accepted_completion_edits_the_correct_identifier_prefix` | `cargo test --lib accepted_completion_edits_the_correct_identifier_prefix` |
//! | Separators preserved on accept | `accepting_a_completion_does_not_remove_separators_or_punctuation` | `cargo test --lib does_not_remove_separators` |
//! | Unicode identifiers (no panic) | `unicode_identifiers_do_not_panic` | `cargo test --lib unicode_identifiers_do_not_panic` |
//! | Unicode + empty lines (no panic) | `keep_the_implementation_panic_free_for_unicode_and_empty_lines` | `cargo test --lib keep_the_implementation_panic_free_for_unicode_and_empty_lines` |
//! | Cursor after accept | `cursor_lands_after_inserted_text` | `cargo test --lib cursor_lands_after_inserted` |
//! | Revision / modified on accept | `revision_and_modified_state_update_correctly` | `cargo test --lib revision_and_modified_state` |
//! | State transitions + text edits | `add_tests_for_new_state_transitions_and_text_edits` | `cargo test --lib add_tests_for_new_state_transitions_and_text_edits` |
//! | Completion insertion | `apply_completion_insertion_*` | `cargo test --lib apply_completion_insertion` |
//! | Identifier prefix | `identifier_prefix_*` | `cargo test --lib identifier_prefix` |
//! | Revision / LSP sync | `lsp_sync_state_*`, `revision_and_lsp_version_*`, `syntax_layout_refresh_*` | `cargo test --lib lsp_sync` |
//! | File I/O | `file_round_trip_*`, `failed_load_*` | `cargo test --lib file_round_trip` |

use std::fmt;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Instant;

use egui::FontId;
use ropey::{iter::Lines, Rope, RopeSlice};

use super::folding::{FoldRange, FoldState};
use super::highlight::Highlighter;
pub use super::position::{
    clamp_char_column, decode_char_column, decode_utf16_column, encode_char_column, LspPosition,
};
use crate::lsp::types::LspTextEdit;
use crate::theme::{default_syntax_palette, SyntaxPalette};

/// Error produced by range operations on `TextBuffer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeError {
    pub message: String,
}

impl RangeError {
    fn new(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
        }
    }
}

impl fmt::Display for RangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RangeError {}

/// Editor caret / hit-test position in **Rust character indices** (Unicode scalars).
///
/// `col` counts `char` values on the line (`str::chars`), not UTF-16 code units and
/// not UTF-8 bytes. Encode to LSP wire coordinates via [`LspPosition`] only at the
/// app/LSP boundary (`position_lsp_position`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CursorPosition {
    /// 0-based line index.
    pub line: usize,
    /// 0-based column: Rust character index on the line.
    pub col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorAnchor {
    pub anchor: CursorPosition,
    pub head: CursorPosition,
    pub col_affinity: usize,
}

impl CursorAnchor {
    pub fn caret(position: CursorPosition) -> Self {
        Self {
            anchor: position,
            head: position,
            col_affinity: position.col,
        }
    }

    pub fn normalize(self) -> (CursorPosition, CursorPosition) {
        if (self.anchor.line, self.anchor.col) <= (self.head.line, self.head.col) {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RopeOp {
    Insert {
        char_offset: usize,
        text: String,
    },
    Delete {
        char_offset: usize,
        length: usize,
        deleted: String,
    },
}

#[derive(Debug, Clone)]
pub struct EditRecord {
    pub operations: Vec<RopeOp>,
    pub cursor_before: Vec<CursorAnchor>,
    pub cursor_after: Vec<CursorAnchor>,
    pub timestamp: Instant,
}

#[derive(Debug, Default)]
pub struct UndoStack {
    pub past: Vec<EditRecord>,
    pub future: Vec<EditRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSelection {
    pub anchor_line: usize,
    pub anchor_col: usize,
    pub head_line: usize,
    pub head_col: usize,
    pub cursors: Vec<CursorAnchor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseTransform {
    Upper,
    Lower,
    Title,
    Camel,
    Snake,
    Pascal,
    Kebab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BracketColor {
    pub char_offset: usize,
    pub depth: usize,
    pub color: egui::Color32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    Crlf,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum TextEncoding {
    #[default]
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferError {
    index: usize,
    len: usize,
}

impl BufferError {
    fn new(index: usize, len: usize) -> Self {
        Self { index, len }
    }
}

impl fmt::Display for BufferError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "character index {} is outside the buffer length {}",
            self.index, self.len
        )
    }
}

impl std::error::Error for BufferError {}

#[derive(Debug, Clone)]
pub struct HoverState {
    pub content: String,
    pub rendered: egui::text::LayoutJob,
    pub anchor_pos: egui::Pos2,
    pub request_id: u64,
}

#[derive(Debug, Clone)]
pub struct SignatureState {
    pub signatures: Vec<crate::lsp::types::SignatureInfo>,
    pub active_signature: usize,
    pub active_parameter: usize,
    pub anchor_line: usize,
}

#[derive(Debug)]
pub struct TextBuffer {
    rope: Rope,
    cursor: CursorPosition,
    pub cursors: Vec<CursorAnchor>,
    pub primary: usize,
    pub column_selection: Option<ColumnSelection>,
    pub selection_history: Vec<(usize, usize)>,
    pub occurrence_limit_reached: bool,
    pub bracket_match: Option<(usize, usize, usize, usize)>,
    pub bracket_colors: Vec<BracketColor>,
    bracket_colorization_enabled: bool,
    bracket_matching_enabled: bool,
    pub sticky_lines: Vec<usize>,
    pub last_scroll_y: f32,
    pub hover_state: Option<HoverState>,
    pub inlay_hints: Vec<crate::lsp::types::LspInlayHint>,
    pub inlay_hints_dirty: bool,
    pub inlay_hint_range: Option<(u32, u32)>,
    pub signature_state: Option<SignatureState>,
    pub code_lenses: Vec<crate::lsp::types::CodeLensItem>,
    pub code_lens_dirty: bool,
    pub semantic_tokens: Vec<crate::lsp::types::SemanticToken>,
    pub semantic_tokens_dirty: bool,
    pub undo_stack: UndoStack,
    recording: Option<EditRecord>,
    path: Option<PathBuf>,
    pub dirty: bool,
    pub modified: bool,
    /// Monotonic edit counter used to reject stale LSP hover responses.
    revision: u64,
    /// LSP `textDocument` version; paired with `revision` in hover request sessions.
    pub lsp_version: i32,
    lsp_dirty: bool,
    line_ending: LineEnding,
    encoding: TextEncoding,
    highlighter: Highlighter,
    pub fold_state: FoldState,
    cached_layout: Option<egui::text::LayoutJob>,
    cached_syntax_palette: Option<SyntaxPalette>,
    // ─── Large file mode ─────────────────────────────────────────────────────
    /// True when file exceeds size/line-count thresholds.
    pub large_file_mode: bool,
    /// Set by "Enable anyway" — re-enables features without changing config.
    /// Not persisted across IDE restarts.
    pub large_file_override: bool,
    /// Cached file size in bytes (updated on open/save).
    pub file_size_bytes: u64,
    /// How long the last tree-sitter parse took.
    pub last_parse_duration: Option<std::time::Duration>,
    /// How long the last highlight build took.
    pub last_highlight_duration: Option<std::time::Duration>,
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self::from_text("")
    }
}

impl TextBuffer {
    pub fn from_text(text: &str) -> Self {
        let rope = Rope::from_str(text);
        let line_ending = detect_line_ending(&rope);
        Self {
            rope,
            cursor: CursorPosition::default(),
            cursors: vec![CursorAnchor::caret(CursorPosition::default())],
            primary: 0,
            column_selection: None,
            selection_history: Vec::new(),
            occurrence_limit_reached: false,
            bracket_match: None,
            bracket_colors: Vec::new(),
            bracket_colorization_enabled: true,
            bracket_matching_enabled: true,
            sticky_lines: Vec::new(),
            last_scroll_y: 0.0,
            hover_state: None,
            inlay_hints: Vec::new(),
            inlay_hints_dirty: true,
            inlay_hint_range: None,
            signature_state: None,
            code_lenses: Vec::new(),
            code_lens_dirty: true,
            semantic_tokens: Vec::new(),
            semantic_tokens_dirty: true,
            undo_stack: UndoStack::default(),
            recording: None,
            path: None,
            dirty: false,
            modified: false,
            revision: 0,
            lsp_version: 0,
            lsp_dirty: false,
            line_ending,
            encoding: TextEncoding::Utf8,
            highlighter: Highlighter::new(),
            fold_state: FoldState::default(),
            cached_layout: None,
            cached_syntax_palette: None,
            large_file_mode: false,
            large_file_override: false,
            file_size_bytes: 0,
            last_parse_duration: None,
            last_highlight_duration: None,
        }
    }

    pub fn from_text_and_language(text: &str, language: crate::language::LanguageId) -> Self {
        let mut buffer = Self::from_text(text);
        buffer.set_language(language);
        buffer
    }

    pub fn invalidate_layout(&mut self) {
        self.cached_layout = None;
    }

    pub fn has_code_lens(&self, line: usize) -> bool {
        self.code_lenses
            .iter()
            .any(|lens| lens.line == line && !lens.entries.is_empty())
    }

    pub fn language(&self) -> crate::language::LanguageId {
        self.highlighter.language()
    }

    pub fn set_language(&mut self, language: crate::language::LanguageId) {
        if self.highlighter.language() != language {
            self.highlighter.set_language(language);
            self.cached_layout = None;
            self.dirty = true;
        }
    }

    pub fn set_bracket_features(&mut self, colorization: bool, matching: bool) {
        if self.bracket_colorization_enabled != colorization {
            self.bracket_colorization_enabled = colorization;
            self.cached_layout = None;
        }
        self.bracket_matching_enabled = matching;
        if !matching {
            self.bracket_match = None;
        }
    }

    pub fn cursor(&self) -> CursorPosition {
        self.cursor
    }

    /// Cursor position encoded for LSP requests.
    pub fn cursor_lsp_position(&self) -> LspPosition {
        self.position_lsp_position(self.cursor)
    }

    /// Encode a widget-reported character-index position for LSP requests.
    pub fn position_lsp_position(&self, position: CursorPosition) -> LspPosition {
        let line_text = self.line_text(position.line).unwrap_or_default();
        encode_char_column(&line_text, position.line, position.col)
    }

    /// Decode an LSP position to a [`CursorPosition`] in this buffer.
    pub fn lsp_position_to_cursor(&self, position: LspPosition) -> CursorPosition {
        let line = usize::try_from(position.line)
            .unwrap_or(0)
            .min(self.len_lines().saturating_sub(1));
        let line_text = self.line_text(line).unwrap_or_default();
        CursorPosition {
            line,
            col: decode_char_column(&line_text, position),
        }
    }

    pub fn set_cursor(&mut self, position: CursorPosition) {
        self.reveal_line(position.line);
        let line = position.line.min(self.len_lines().saturating_sub(1));
        let line_text = self.line_text(line).unwrap_or_default();
        let col = clamp_char_column(&line_text, position.col);
        self.cursor = CursorPosition { line, col };
        self.cursors = vec![CursorAnchor::caret(self.cursor)];
        self.primary = 0;
        self.column_selection = None;
        self.update_bracket_match();
    }

    pub fn primary_cursor(&self) -> &CursorAnchor {
        &self.cursors[self.primary.min(self.cursors.len() - 1)]
    }

    pub fn primary_cursor_mut(&mut self) -> &mut CursorAnchor {
        let index = self.primary.min(self.cursors.len() - 1);
        &mut self.cursors[index]
    }

    pub fn add_cursor(&mut self, line: usize, col: usize) {
        let position = self.clamped_position(CursorPosition { line, col });
        if self
            .cursors
            .iter()
            .any(|cursor| cursor.anchor == position && cursor.head == position)
        {
            return;
        }
        self.cursors.push(CursorAnchor::caret(position));
        self.sort_cursors();
    }

    pub fn add_cursor_vertical(&mut self, down: bool) {
        let existing = self.cursors.clone();
        for cursor in existing {
            let line = if down {
                cursor.head.line.saturating_add(1)
            } else {
                cursor.head.line.saturating_sub(1)
            };
            if line < self.len_lines() && (down || cursor.head.line > 0) {
                self.add_cursor(line, cursor.col_affinity);
            }
        }
    }

    pub fn remove_cursor(&mut self, index: usize) {
        if self.cursors.len() <= 1 || index >= self.cursors.len() {
            return;
        }
        self.cursors.remove(index);
        self.primary = self.primary.min(self.cursors.len() - 1);
        self.sync_legacy_cursor();
    }

    pub fn collapse_to_primary(&mut self) {
        let primary = *self.primary_cursor();
        self.cursors = vec![CursorAnchor::caret(primary.head)];
        self.primary = 0;
        self.column_selection = None;
        self.sync_legacy_cursor();
    }

    pub fn all_selections(&self) -> Vec<(usize, usize)> {
        self.cursors
            .iter()
            .filter_map(|cursor| {
                let (start, end) = cursor.normalize();
                Some((
                    self.position_to_char_index(start)?,
                    self.position_to_char_index(end)?,
                ))
            })
            .collect()
    }

    pub fn merge_overlapping_cursors(&mut self) {
        self.sort_cursors();
        let mut merged: Vec<CursorAnchor> = Vec::with_capacity(self.cursors.len());
        for cursor in self.cursors.drain(..) {
            if let Some(last) = merged.last_mut() {
                let (last_start, last_end) = last.normalize();
                let (start, end) = cursor.normalize();
                if (start.line, start.col) <= (last_end.line, last_end.col) {
                    last.anchor = last_start;
                    last.head = if (end.line, end.col) > (last_end.line, last_end.col) {
                        end
                    } else {
                        last_end
                    };
                    last.col_affinity = last.head.col;
                    continue;
                }
            }
            merged.push(cursor);
        }
        self.cursors = merged;
        self.primary = self.primary.min(self.cursors.len() - 1);
        self.sync_legacy_cursor();
    }

    pub fn begin_edit(&mut self) {
        if self.recording.is_none() {
            self.recording = Some(EditRecord {
                operations: Vec::new(),
                cursor_before: self.cursors.clone(),
                cursor_after: Vec::new(),
                timestamp: Instant::now(),
            });
        }
    }

    pub fn commit_edit(&mut self) {
        let Some(mut record) = self.recording.take() else {
            return;
        };
        if record.operations.is_empty() {
            return;
        }
        record.cursor_after = self.cursors.clone();
        self.undo_stack.future.clear();
        let single_char_insert = |edit: &EditRecord| {
            !edit.operations.is_empty()
                && edit.operations.iter().all(
                    |operation| matches!(operation, RopeOp::Insert { text, .. } if text.chars().count() == 1),
                )
        };
        if single_char_insert(&record) {
            if let Some(previous) = self.undo_stack.past.last_mut() {
                if single_char_insert(previous)
                    && record.timestamp.duration_since(previous.timestamp)
                        <= std::time::Duration::from_millis(500)
                {
                    previous.operations.extend(record.operations);
                    previous.cursor_after = record.cursor_after;
                    previous.timestamp = record.timestamp;
                    return;
                }
            }
        }
        self.undo_stack.past.push(record);
        if self.undo_stack.past.len() > 1000 {
            self.undo_stack.past.remove(0);
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some(record) = self.undo_stack.past.pop() else {
            return false;
        };
        for operation in record.operations.iter().rev() {
            match operation {
                RopeOp::Insert { char_offset, text } => self
                    .rope
                    .remove(*char_offset..*char_offset + text.chars().count()),
                RopeOp::Delete {
                    char_offset,
                    deleted,
                    ..
                } => self.rope.insert(*char_offset, deleted),
            }
        }
        self.cursors = record.cursor_before.clone();
        self.primary = self.primary.min(self.cursors.len() - 1);
        self.sync_legacy_cursor();
        self.undo_stack.future.push(record);
        self.note_text_changed();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(record) = self.undo_stack.future.pop() else {
            return false;
        };
        for operation in &record.operations {
            match operation {
                RopeOp::Insert { char_offset, text } => self.rope.insert(*char_offset, text),
                RopeOp::Delete {
                    char_offset,
                    length,
                    ..
                } => self.rope.remove(*char_offset..*char_offset + *length),
            }
        }
        self.cursors = record.cursor_after.clone();
        self.primary = self.primary.min(self.cursors.len() - 1);
        self.sync_legacy_cursor();
        self.undo_stack.past.push(record);
        self.note_text_changed();
        true
    }

    fn clamped_position(&self, position: CursorPosition) -> CursorPosition {
        let line = position.line.min(self.len_lines().saturating_sub(1));
        CursorPosition {
            line,
            col: position.col.min(self.line_content_len(line).unwrap_or(0)),
        }
    }

    fn sort_cursors(&mut self) {
        let primary_cursor = self.cursors.get(self.primary).copied();
        self.cursors.sort_by_key(|cursor| {
            let (start, end) = cursor.normalize();
            (start.line, start.col, end.line, end.col)
        });
        self.primary = primary_cursor
            .and_then(|value| self.cursors.iter().position(|cursor| *cursor == value))
            .unwrap_or(0);
    }

    fn sync_legacy_cursor(&mut self) {
        self.cursor = self.primary_cursor().head;
    }

    pub fn smart_home(&mut self, extend: bool) {
        let positions: Vec<(usize, usize)> = self
            .cursors
            .iter()
            .map(|cursor| {
                let line = cursor.head.line;
                let text = self.line_text(line).unwrap_or_default();
                let first = text
                    .chars()
                    .take_while(|ch| *ch == ' ' || *ch == '\t')
                    .count();
                let target =
                    if text.chars().all(|ch| ch == ' ' || ch == '\t') || cursor.head.col == first {
                        0
                    } else {
                        first
                    };
                (line, target)
            })
            .collect();
        for (cursor, (line, col)) in self.cursors.iter_mut().zip(positions) {
            cursor.head = CursorPosition { line, col };
            cursor.col_affinity = col;
            if !extend {
                cursor.anchor = cursor.head;
            }
        }
        self.column_selection = None;
        self.sync_legacy_cursor();
        self.update_bracket_match();
    }

    pub fn move_cursors_horizontal(&mut self, right: bool, extend: bool) {
        let next: Vec<CursorPosition> = self
            .cursors
            .iter()
            .map(|cursor| {
                let mut position = cursor.head;
                let line_len = self.line_content_len(position.line).unwrap_or(0);
                if right {
                    if position.col < line_len {
                        position.col += 1;
                    } else if let Some(line) = self.next_visible_line(position.line) {
                        position = CursorPosition { line, col: 0 };
                    }
                } else if position.col > 0 {
                    position.col -= 1;
                } else if let Some(line) = self.previous_visible_line(position.line) {
                    position = CursorPosition {
                        line,
                        col: self.line_content_len(line).unwrap_or(0),
                    };
                }
                position
            })
            .collect();
        for (cursor, head) in self.cursors.iter_mut().zip(next) {
            cursor.head = head;
            cursor.col_affinity = head.col;
            if !extend {
                cursor.anchor = head;
            }
        }
        self.column_selection = None;
        self.sync_legacy_cursor();
        self.update_bracket_match();
    }

    pub fn move_cursors_vertical(&mut self, down: bool, extend: bool) {
        let next: Vec<CursorPosition> = self
            .cursors
            .iter()
            .map(|cursor| {
                let line = if down {
                    self.next_visible_line(cursor.head.line)
                } else {
                    self.previous_visible_line(cursor.head.line)
                }
                .unwrap_or(cursor.head.line);
                CursorPosition {
                    line,
                    col: cursor
                        .col_affinity
                        .min(self.line_content_len(line).unwrap_or(0)),
                }
            })
            .collect();
        for (cursor, head) in self.cursors.iter_mut().zip(next) {
            cursor.head = head;
            if !extend {
                cursor.anchor = head;
            }
        }
        self.column_selection = None;
        self.sync_legacy_cursor();
        self.update_bracket_match();
    }

    pub fn insert_at_cursors(&mut self, text: &str) -> Result<(), RangeError> {
        if text.is_empty() {
            return Ok(());
        }
        self.begin_edit();
        self.sort_cursors();
        let cursors = self.cursors.clone();
        let mut new_offsets = Vec::with_capacity(cursors.len());
        for cursor in cursors.iter().rev() {
            let (start, end) = cursor.normalize();
            let start = self
                .position_to_char_index(start)
                .ok_or_else(|| RangeError::new("invalid selection start"))?;
            let end = self
                .position_to_char_index(end)
                .ok_or_else(|| RangeError::new("invalid selection end"))?;
            self.replace_char_range(start, end, text)?;
            new_offsets.push(start + text.chars().count());
        }
        new_offsets.reverse();
        self.cursors = new_offsets
            .into_iter()
            .filter_map(|offset| self.char_index_to_position(offset))
            .map(CursorAnchor::caret)
            .collect();
        if self.cursors.is_empty() {
            self.cursors
                .push(CursorAnchor::caret(CursorPosition::default()));
        }
        self.primary = self.primary.min(self.cursors.len() - 1);
        self.merge_overlapping_cursors();
        self.column_selection = None;
        self.commit_edit();
        self.update_bracket_match();
        Ok(())
    }

    pub fn delete_at_all_cursors(&mut self, backward: bool) -> Result<bool, RangeError> {
        self.sort_cursors();
        let cursors = self.cursors.clone();
        let mut ranges = Vec::new();
        for cursor in cursors {
            let (start, end) = cursor.normalize();
            let mut start = self.position_to_char_index(start).unwrap_or(0);
            let mut end = self.position_to_char_index(end).unwrap_or(start);
            if start == end {
                if backward {
                    if start == 0 {
                        continue;
                    }
                    start -= 1;
                    if self.rope.get_char(start) == Some('\n')
                        && start > 0
                        && self.rope.get_char(start - 1) == Some('\r')
                    {
                        start -= 1;
                    }
                } else {
                    if end >= self.len_chars() {
                        continue;
                    }
                    end += 1;
                    if self.rope.get_char(end - 1) == Some('\r')
                        && self.rope.get_char(end) == Some('\n')
                    {
                        end += 1;
                    }
                }
            }
            ranges.push(start..end);
        }
        if ranges.is_empty() {
            return Ok(false);
        }
        ranges.sort_by_key(|range| std::cmp::Reverse(range.start));
        self.begin_edit();
        let mut positions = Vec::new();
        for range in ranges {
            positions.push(range.start);
            self.replace_char_range(range.start, range.end, "")?;
        }
        positions.sort_unstable();
        self.cursors = positions
            .into_iter()
            .filter_map(|offset| self.char_index_to_position(offset))
            .map(CursorAnchor::caret)
            .collect();
        self.merge_overlapping_cursors();
        self.column_selection = None;
        self.commit_edit();
        Ok(true)
    }

    pub fn select_all_occurrences(&mut self) -> usize {
        self.occurrence_limit_reached = false;
        let original = self.primary_cursor().head;
        let pattern_range = self.primary_selection_or_word();
        let Some((pattern, _)) = pattern_range.and_then(|(start, end)| {
            self.char_range_to_string(start..end)
                .map(|text| (text, (start, end)))
        }) else {
            return 0;
        };
        if pattern.is_empty() {
            return 0;
        }
        let source = self.rope.to_string();
        let mut cursors = Vec::new();
        for (index, (byte, _)) in source.match_indices(&pattern).enumerate() {
            if index >= 500 {
                self.occurrence_limit_reached = true;
                break;
            }
            let start = self.rope.byte_to_char(byte);
            let end = start + pattern.chars().count();
            if let (Some(anchor), Some(head)) = (
                self.char_index_to_position(start),
                self.char_index_to_position(end),
            ) {
                cursors.push(CursorAnchor {
                    anchor,
                    head,
                    col_affinity: head.col,
                });
            }
        }
        if cursors.is_empty() {
            return 0;
        }
        let original_offset = self.position_to_char_index(original).unwrap_or(0);
        self.primary = cursors
            .iter()
            .enumerate()
            .min_by_key(|(_, cursor)| {
                self.position_to_char_index(cursor.head)
                    .unwrap_or(0)
                    .abs_diff(original_offset)
            })
            .map(|(index, _)| index)
            .unwrap_or(0);
        self.cursors = cursors;
        self.sync_legacy_cursor();
        self.cursors.len()
    }

    pub fn select_next_occurrence(&mut self) -> bool {
        if self.primary_cursor().anchor == self.primary_cursor().head {
            if let Some((start, end)) = self.primary_selection_or_word() {
                if let (Some(anchor), Some(head)) = (
                    self.char_index_to_position(start),
                    self.char_index_to_position(end),
                ) {
                    let primary = self.primary;
                    self.cursors[primary] = CursorAnchor {
                        anchor,
                        head,
                        col_affinity: head.col,
                    };
                    self.sync_legacy_cursor();
                    return true;
                }
            }
            return false;
        }
        let (start, end) = self.primary_selection_or_word().unwrap_or((0, 0));
        let pattern = self.char_range_to_string(start..end).unwrap_or_default();
        if pattern.is_empty() {
            return false;
        }
        let text = self.rope.to_string();
        let start_byte = self.rope.char_to_byte(end);
        let candidates = text[start_byte..]
            .match_indices(&pattern)
            .map(|(byte, _)| start_byte + byte)
            .chain(
                text[..start_byte]
                    .match_indices(&pattern)
                    .map(|(byte, _)| byte),
            );
        for byte in candidates {
            let char_start = self.rope.byte_to_char(byte);
            let char_end = char_start + pattern.chars().count();
            if self
                .all_selections()
                .iter()
                .any(|selection| *selection == (char_start, char_end))
            {
                continue;
            }
            if let (Some(anchor), Some(head)) = (
                self.char_index_to_position(char_start),
                self.char_index_to_position(char_end),
            ) {
                self.cursors.push(CursorAnchor {
                    anchor,
                    head,
                    col_affinity: head.col,
                });
                self.sort_cursors();
                return true;
            }
        }
        false
    }

    pub fn set_column_selection(&mut self, anchor: CursorPosition, head: CursorPosition) {
        let first_line = anchor.line.min(head.line);
        let last_line = anchor
            .line
            .max(head.line)
            .min(self.len_lines().saturating_sub(1));
        let first_col = anchor.col.min(head.col);
        let last_col = anchor.col.max(head.col);
        let cursors: Vec<CursorAnchor> = (first_line..=last_line)
            .map(|line| {
                let line_len = self.line_content_len(line).unwrap_or(0);
                let start = CursorPosition {
                    line,
                    col: first_col.min(line_len),
                };
                let end = CursorPosition {
                    line,
                    col: last_col.min(line_len),
                };
                CursorAnchor {
                    anchor: start,
                    head: end,
                    col_affinity: last_col,
                }
            })
            .collect();
        self.column_selection = Some(ColumnSelection {
            anchor_line: anchor.line,
            anchor_col: anchor.col,
            head_line: head.line,
            head_col: head.col,
            cursors: cursors.clone(),
        });
        self.cursors = cursors;
        self.primary = 0;
        self.sync_legacy_cursor();
    }

    pub fn finish_column_selection(&mut self) {
        self.column_selection = None;
    }

    pub fn update_sticky_lines(&mut self, first_visible_line: usize, max_lines: usize) {
        let mut lines = Vec::new();
        if let Some(tree) = self.highlighter.tree() {
            let mut stack = vec![tree.root_node()];
            while let Some(node) = stack.pop() {
                let range = node.range();
                if range.start_point.row < first_visible_line
                    && range.end_point.row > first_visible_line
                {
                    if matches!(
                        node.kind(),
                        "function_item" | "impl_item" | "struct_item" | "enum_item" | "mod_item"
                    ) {
                        lines.push(range.start_point.row);
                    }
                    let mut cursor = node.walk();
                    stack.extend(node.children(&mut cursor));
                }
            }
        }
        lines.sort_unstable();
        lines.dedup();
        if lines.len() > max_lines {
            lines = lines.split_off(lines.len() - max_lines);
        }
        self.sticky_lines = lines;
    }

    pub fn move_selected_lines(&mut self, down: bool) -> Result<bool, RangeError> {
        let mut unique = Vec::<CursorAnchor>::new();
        for cursor in self.cursors.clone() {
            if !unique
                .iter()
                .any(|existing| existing.head.line == cursor.head.line)
            {
                unique.push(cursor);
            }
        }
        self.cursors = unique;
        self.primary = self.primary.min(self.cursors.len().saturating_sub(1));
        let mut groups: Vec<(usize, usize)> = self
            .cursors
            .iter()
            .map(|cursor| {
                let (start, end) = cursor.normalize();
                (start.line, end.line)
            })
            .collect();
        groups.sort_unstable();
        let mut merged = Vec::<(usize, usize)>::new();
        for (start, end) in groups {
            if let Some(last) = merged.last_mut() {
                if start <= last.1.saturating_add(1) {
                    last.1 = last.1.max(end);
                    continue;
                }
            }
            merged.push((start, end));
        }
        let physical_last = self.len_lines().saturating_sub(1);
        let last_content_line = if physical_last > 0
            && self.line_text(physical_last).unwrap_or_default().is_empty()
            && self.rope.to_string().ends_with('\n')
        {
            physical_last - 1
        } else {
            physical_last
        };
        let movable: Vec<(usize, usize)> = merged
            .into_iter()
            .filter(|(start, end)| {
                if down {
                    *end < last_content_line
                } else {
                    *start > 0
                }
            })
            .collect();
        if movable.is_empty() {
            return Ok(false);
        }
        let mut lines: Vec<String> = (0..self.len_lines())
            .map(|line| self.line_text(line).unwrap_or_default())
            .collect();
        if down {
            for (start, end) in movable.iter().rev().copied() {
                let next = lines.remove(end + 1);
                lines.insert(start, next);
            }
        } else {
            for (start, end) in movable.iter().copied() {
                let previous = lines.remove(start - 1);
                lines.insert(end, previous);
            }
        }
        let separator = match self.line_ending {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
        };
        let replacement = lines.join(separator);
        let original_len = self.len_chars();
        let old_cursors = self.cursors.clone();
        self.begin_edit();
        self.replace_char_range(0, original_len, &replacement)?;
        self.cursors = old_cursors
            .into_iter()
            .map(|mut cursor| {
                for (start, end) in &movable {
                    if cursor.head.line >= *start && cursor.head.line <= *end {
                        let delta = if down { 1isize } else { -1 };
                        cursor.anchor.line = cursor.anchor.line.saturating_add_signed(delta);
                        cursor.head.line = cursor.head.line.saturating_add_signed(delta);
                        break;
                    }
                }
                cursor.anchor = self.clamped_position(cursor.anchor);
                cursor.head = self.clamped_position(cursor.head);
                cursor
            })
            .collect();
        self.sync_legacy_cursor();
        self.commit_edit();
        Ok(true)
    }

    pub fn expand_ast_selection(&mut self) {
        if self.dirty {
            let head = self.primary_cursor().head;
            if let Some(range) = self.word_char_range_at(head) {
                if let (Some(anchor), Some(head)) = (
                    self.char_index_to_position(range.start),
                    self.char_index_to_position(range.end),
                ) {
                    self.collapse_to_primary();
                    self.cursors[0] = CursorAnchor {
                        anchor,
                        head,
                        col_affinity: head.col,
                    };
                    self.sync_legacy_cursor();
                }
            }
            return;
        }
        let current = self.primary_selection_or_word().unwrap_or_else(|| {
            let offset = self
                .position_to_char_index(self.primary_cursor().head)
                .unwrap_or(0);
            (offset, offset)
        });
        self.selection_history.push(current);
        let current_bytes = self.rope.char_to_byte(current.0)..self.rope.char_to_byte(current.1);
        let mut best = None;
        if let Some(tree) = self.highlighter.tree() {
            let mut stack = vec![tree.root_node()];
            while let Some(node) = stack.pop() {
                let range = node.byte_range();
                if range.start <= current_bytes.start
                    && range.end >= current_bytes.end
                    && range != current_bytes
                {
                    if best.is_none_or(|old: tree_sitter::Node<'_>| {
                        range.len() < old.byte_range().len()
                    }) {
                        best = Some(node);
                    }
                    let mut cursor = node.walk();
                    stack.extend(node.children(&mut cursor));
                }
            }
        }
        let range = best
            .map(|node| node.byte_range())
            .unwrap_or(0..self.rope.len_bytes());
        if let (Some(anchor), Some(head)) = (
            self.byte_index_to_position(range.start),
            self.byte_index_to_position(range.end),
        ) {
            self.collapse_to_primary();
            self.cursors[0] = CursorAnchor {
                anchor,
                head,
                col_affinity: head.col,
            };
            self.sync_legacy_cursor();
        }
    }

    pub fn shrink_ast_selection(&mut self) {
        if let Some((start, end)) = self.selection_history.pop() {
            if let (Some(anchor), Some(head)) = (
                self.char_index_to_position(start),
                self.char_index_to_position(end),
            ) {
                self.cursors = vec![CursorAnchor {
                    anchor,
                    head,
                    col_affinity: head.col,
                }];
            }
        } else {
            let start = self.primary_cursor().normalize().0;
            self.cursors = vec![CursorAnchor::caret(start)];
        }
        self.primary = 0;
        self.sync_legacy_cursor();
    }

    pub fn join_lines(&mut self) -> Result<bool, RangeError> {
        let mut groups: Vec<(usize, usize)> = self
            .cursors
            .iter()
            .filter_map(|cursor| {
                let (start, end) = cursor.normalize();
                let last = if start == end {
                    start.line.checked_add(1)?
                } else {
                    end.line
                };
                (last < self.len_lines() && last > start.line).then_some((start.line, last))
            })
            .collect();
        groups.sort_unstable();
        let mut merged = Vec::<(usize, usize)>::new();
        for (start, end) in groups {
            if let Some(last) = merged.last_mut() {
                if start <= last.1 {
                    last.1 = last.1.max(end);
                    continue;
                }
            }
            merged.push((start, end));
        }
        let mut edits = Vec::<(usize, usize, String, usize)>::new();
        for (first, last) in merged {
            let range_start = self
                .position_to_char_index(CursorPosition {
                    line: first,
                    col: 0,
                })
                .unwrap_or(0);
            let range_end = if last + 1 < self.len_lines() {
                self.position_to_char_index(CursorPosition {
                    line: last + 1,
                    col: 0,
                })
                .unwrap_or(self.len_chars())
            } else {
                self.len_chars()
            };
            let mut joined = self.line_text(first).unwrap_or_default();
            let junction = joined.chars().count();
            for line in first + 1..=last {
                let next = self.line_text(line).unwrap_or_default();
                let trimmed = next.trim_start_matches([' ', '\t']);
                if !joined.is_empty()
                    && !joined.ends_with(char::is_whitespace)
                    && !trimmed.is_empty()
                {
                    joined.push(' ');
                }
                joined.push_str(trimmed);
            }
            if last + 1 < self.len_lines() {
                joined.push_str(match self.line_ending {
                    LineEnding::Lf => "\n",
                    LineEnding::Crlf => "\r\n",
                });
            }
            edits.push((range_start, range_end, joined, junction));
        }
        if edits.is_empty() {
            return Ok(false);
        }
        self.begin_edit();
        for (start, end, replacement, _) in edits.iter().rev() {
            self.replace_char_range(*start, *end, replacement)?;
        }
        let mut delta = 0isize;
        let mut cursors = Vec::new();
        for (start, end, replacement, junction) in &edits {
            let target = (*start as isize + delta + *junction as isize).max(0) as usize;
            if let Some(position) = self.char_index_to_position(target.min(self.len_chars())) {
                cursors.push(CursorAnchor::caret(position));
            }
            delta += replacement.chars().count() as isize - (*end - *start) as isize;
        }
        if !cursors.is_empty() {
            self.cursors = cursors;
            self.primary = self.primary.min(self.cursors.len() - 1);
            self.sync_legacy_cursor();
        }
        self.commit_edit();
        Ok(true)
    }

    pub fn duplicate_selection_or_line(&mut self) -> Result<(), RangeError> {
        self.merge_overlapping_cursors();
        let mut ranges = Vec::<(usize, usize, String, usize)>::new();
        for cursor in self.cursors.clone() {
            let (start, end) = cursor.normalize();
            let (range_start, range_end) = if start == end {
                let start_offset = self
                    .position_to_char_index(CursorPosition {
                        line: start.line,
                        col: 0,
                    })
                    .unwrap_or(0);
                let end_offset = if start.line + 1 < self.len_lines() {
                    self.position_to_char_index(CursorPosition {
                        line: start.line + 1,
                        col: 0,
                    })
                    .unwrap_or(self.len_chars())
                } else {
                    self.len_chars()
                };
                (start_offset, end_offset)
            } else {
                (
                    self.position_to_char_index(start).unwrap_or(0),
                    self.position_to_char_index(end).unwrap_or(0),
                )
            };
            if ranges.iter().any(|(existing_start, existing_end, _, _)| {
                *existing_start == range_start && *existing_end == range_end
            }) {
                continue;
            }
            let mut text = self
                .char_range_to_string(range_start..range_end)
                .unwrap_or_default();
            let mut target_in_insert = if start == end {
                start.col
            } else {
                text.chars().count()
            };
            if start == end && start.line + 1 == self.len_lines() && !text.ends_with('\n') {
                let separator = match self.line_ending {
                    LineEnding::Lf => "\n",
                    LineEnding::Crlf => "\r\n",
                };
                text = format!("{separator}{text}");
                target_in_insert += separator.chars().count();
            }
            ranges.push((range_start, range_end, text, target_in_insert));
        }
        ranges.sort_by_key(|range| range.0);
        self.begin_edit();
        for (_, end, text, _) in ranges.iter().rev() {
            self.replace_char_range(*end, *end, text)?;
        }
        let mut inserted_before = 0usize;
        let mut cursors = Vec::new();
        for (_, end, text, target_in_insert) in &ranges {
            let inserted = text.chars().count();
            let target = end + inserted_before + target_in_insert;
            if let Some(position) = self.char_index_to_position(target.min(self.len_chars())) {
                cursors.push(CursorAnchor::caret(position));
            }
            inserted_before += inserted;
        }
        if !cursors.is_empty() {
            self.cursors = cursors;
            self.primary = self.primary.min(self.cursors.len() - 1);
            self.sync_legacy_cursor();
        }
        self.commit_edit();
        Ok(())
    }

    pub fn sort_selected_lines(&mut self, descending: bool) -> Result<(), RangeError> {
        let (start, end) = self.primary_cursor().normalize();
        let first = if start == end { 0 } else { start.line };
        let mut last = if start == end {
            self.len_lines().saturating_sub(1)
        } else {
            end.line
        };
        if last > first
            && self.line_text(last).unwrap_or_default().is_empty()
            && self.rope.to_string().ends_with('\n')
        {
            last -= 1;
        }
        let mut lines: Vec<String> = (first..=last)
            .map(|line| self.line_text(line).unwrap_or_default())
            .collect();
        lines.sort();
        if descending {
            lines.reverse();
        }
        let separator = match self.line_ending {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
        };
        let replacement = lines.join(separator);
        let range_start = self
            .position_to_char_index(CursorPosition {
                line: first,
                col: 0,
            })
            .unwrap_or(0);
        let range_end = if last + 1 < self.len_lines() {
            self.position_to_char_index(CursorPosition {
                line: last + 1,
                col: 0,
            })
            .unwrap_or(self.len_chars())
        } else {
            self.len_chars()
        };
        let had_ending = self
            .char_range_to_string(range_start..range_end)
            .is_some_and(|text| text.ends_with('\n'));
        let replacement = if had_ending {
            format!("{replacement}{separator}")
        } else {
            replacement
        };
        self.begin_edit();
        self.replace_char_range(range_start, range_end, &replacement)?;
        let anchor = CursorPosition {
            line: first,
            col: 0,
        };
        let head = CursorPosition {
            line: last.min(self.len_lines().saturating_sub(1)),
            col: self
                .line_content_len(last.min(self.len_lines().saturating_sub(1)))
                .unwrap_or(0),
        };
        self.cursors = vec![CursorAnchor {
            anchor,
            head,
            col_affinity: head.col,
        }];
        self.primary = 0;
        self.sync_legacy_cursor();
        self.commit_edit();
        Ok(())
    }

    pub fn transform_selections(&mut self, transform: CaseTransform) -> Result<(), RangeError> {
        self.merge_overlapping_cursors();
        let mut edits = Vec::<(usize, usize, String)>::new();
        for cursor in self.cursors.clone() {
            let (start, end) = cursor.normalize();
            let range = if start == end {
                self.word_char_range_at(start).unwrap_or_else(|| {
                    let offset = self.position_to_char_index(start).unwrap_or(0);
                    offset..offset
                })
            } else {
                self.position_to_char_index(start).unwrap_or(0)
                    ..self.position_to_char_index(end).unwrap_or(0)
            };
            let source = self.char_range_to_string(range.clone()).unwrap_or_default();
            edits.push((range.start, range.end, transform_text(&source, transform)));
        }
        edits.sort_by_key(|edit| edit.0);
        self.begin_edit();
        for (start, end, replacement) in edits.iter().rev() {
            self.replace_char_range(*start, *end, replacement)?;
        }
        let mut delta = 0isize;
        let mut cursors = Vec::new();
        for (start, end, replacement) in &edits {
            let final_start = (*start as isize + delta).max(0) as usize;
            let final_end = final_start + replacement.chars().count();
            if let (Some(anchor), Some(head)) = (
                self.char_index_to_position(final_start.min(self.len_chars())),
                self.char_index_to_position(final_end.min(self.len_chars())),
            ) {
                cursors.push(CursorAnchor {
                    anchor,
                    head,
                    col_affinity: head.col,
                });
            }
            delta += replacement.chars().count() as isize - (*end - *start) as isize;
        }
        if !cursors.is_empty() {
            self.cursors = cursors;
            self.primary = self.primary.min(self.cursors.len() - 1);
            self.sync_legacy_cursor();
        }
        self.commit_edit();
        Ok(())
    }

    fn primary_selection_or_word(&self) -> Option<(usize, usize)> {
        let (start, end) = self.primary_cursor().normalize();
        if start != end {
            return Some((
                self.position_to_char_index(start)?,
                self.position_to_char_index(end)?,
            ));
        }
        let range = self.word_char_range_at(start)?;
        Some((range.start, range.end))
    }

    fn word_char_range_at(&self, position: CursorPosition) -> Option<Range<usize>> {
        let line = self.line_text(position.line)?;
        let chars: Vec<char> = line.chars().collect();
        let mut start = position.col.min(chars.len());
        let mut end = start;
        while start > 0 && is_identifier_prefix_char(chars[start - 1]) {
            start -= 1;
        }
        while end < chars.len() && is_identifier_prefix_char(chars[end]) {
            end += 1;
        }
        let base = self.rope.line_to_char(position.line);
        Some(base + start..base + end)
    }

    fn recompute_bracket_colors(&mut self) {
        const COLORS: [egui::Color32; 6] = [
            egui::Color32::from_rgb(255, 215, 0),
            egui::Color32::from_rgb(180, 80, 255),
            egui::Color32::from_rgb(0, 200, 120),
            egui::Color32::from_rgb(80, 160, 255),
            egui::Color32::from_rgb(255, 100, 80),
            egui::Color32::from_rgb(0, 210, 210),
        ];
        let error = egui::Color32::from_rgb(255, 70, 70);
        let mut stack: Vec<(char, usize)> = Vec::new();
        let mut colors = Vec::<BracketColor>::new();
        for (offset, ch) in self.rope.chars().enumerate() {
            match ch {
                '{' | '(' | '[' => {
                    let depth = stack.len();
                    stack.push((ch, colors.len()));
                    colors.push(BracketColor {
                        char_offset: offset,
                        depth,
                        color: COLORS[depth % 6],
                    });
                }
                '}' | ')' | ']' => {
                    let expected = match ch {
                        '}' => '{',
                        ')' => '(',
                        _ => '[',
                    };
                    if stack.last().is_some_and(|entry| entry.0 == expected) {
                        let (_, open_index) = stack.pop().unwrap();
                        let depth = stack.len();
                        let color = COLORS[depth % 6];
                        colors[open_index].color = color;
                        colors.push(BracketColor {
                            char_offset: offset,
                            depth,
                            color,
                        });
                    } else {
                        colors.push(BracketColor {
                            char_offset: offset,
                            depth: stack.len(),
                            color: error,
                        });
                    }
                }
                _ => {}
            }
        }
        for (_, index) in stack {
            colors[index].color = error;
        }
        self.bracket_colors = colors;
    }

    pub fn update_bracket_match(&mut self) {
        if !self.bracket_matching_enabled {
            self.bracket_match = None;
            return;
        }
        self.bracket_match = None;
        let caret = self
            .position_to_char_index(self.primary_cursor().head)
            .unwrap_or(0);
        let candidate = [caret, caret.saturating_sub(1)].into_iter().find(|offset| {
            self.rope
                .get_char(*offset)
                .is_some_and(|ch| "{}()[]".contains(ch))
        });
        let Some(offset) = candidate else {
            return;
        };
        let ch = self.rope.char(offset);
        let (open, close, forward) = match ch {
            '{' => ('{', '}', true),
            '(' => ('(', ')', true),
            '[' => ('[', ']', true),
            '}' => ('{', '}', false),
            ')' => ('(', ')', false),
            ']' => ('[', ']', false),
            _ => return,
        };
        let mut depth = 0usize;
        let matched = if forward {
            (offset..self.len_chars().min(offset + 10_001)).find(|index| {
                let value = self.rope.char(*index);
                if value == open {
                    depth += 1;
                } else if value == close {
                    depth = depth.saturating_sub(1);
                }
                value == close && depth == 0
            })
        } else {
            (offset.saturating_sub(10_000)..=offset)
                .rev()
                .find(|index| {
                    let value = self.rope.char(*index);
                    if value == close {
                        depth += 1;
                    } else if value == open {
                        depth = depth.saturating_sub(1);
                    }
                    value == open && depth == 0
                })
        };
        if let Some(other) = matched {
            let (a, b) = if forward {
                (offset, other)
            } else {
                (other, offset)
            };
            if let (Some(a), Some(b)) = (
                self.char_index_to_position(a),
                self.char_index_to_position(b),
            ) {
                self.bracket_match = Some((a.line, a.col, b.line, b.col));
            }
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn is_modified(&self) -> bool {
        self.modified
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn needs_lsp_sync(&self) -> bool {
        self.lsp_dirty
    }

    pub fn mark_lsp_synced(&mut self) {
        self.lsp_dirty = false;
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    pub fn line_content_len(&self, line: usize) -> Option<usize> {
        let slice = self.rope.get_line(line)?;
        Some(slice.len_chars() - line_break_len(slice))
    }

    pub fn line_slice(&self, line: usize) -> Option<RopeSlice<'_>> {
        let slice = self.rope.get_line(line)?;
        let content_len = slice.len_chars() - line_break_len(slice);
        Some(slice.slice(..content_len))
    }

    pub fn line_text(&self, line: usize) -> Option<String> {
        Some(self.line_slice(line)?.to_string())
    }

    // Cache LayoutJob: re-parse only if dirty or cache missing
    pub fn get_layout(&mut self, font_id: FontId) -> egui::text::LayoutJob {
        self.get_layout_with_palette(font_id, default_syntax_palette())
    }

    pub fn get_layout_with_palette(
        &mut self,
        font_id: FontId,
        palette: SyntaxPalette,
    ) -> egui::text::LayoutJob {
        // In large file mode (unless overridden), skip syntax highlighting — plain text only.
        if self.features_suppressed() {
            use egui::text::LayoutJob;
            let src = self.rope.to_string();
            let mut job = LayoutJob::default();
            job.append(
                &src,
                0.0,
                egui::TextFormat {
                    font_id: font_id.clone(),
                    color: egui::Color32::from_rgb(200, 200, 200),
                    ..Default::default()
                },
            );
            return job;
        }
        if self.dirty || self.cached_layout.is_none() || self.cached_syntax_palette != Some(palette)
        {
            let src = self.rope.to_string();
            let highlighted = apply_semantic_tokens(
                self.highlighter.highlight(&src, font_id, palette),
                &self.rope,
                &self.semantic_tokens,
            );
            let job = if self.bracket_colorization_enabled {
                self.recompute_bracket_colors();
                apply_bracket_colors(highlighted, &src, &self.bracket_colors)
            } else {
                self.bracket_colors.clear();
                highlighted
            };
            self.cached_layout = Some(job);
            self.cached_syntax_palette = Some(palette);
            self.dirty = false;
            self.refresh_folds();
        }
        self.cached_layout.clone().unwrap_or_default()
    }

    pub fn refresh_folds(&mut self) {
        self.fold_state
            .refresh_from_tree(self.highlighter.tree(), &self.rope);
        self.ensure_cursor_visible();
    }

    pub fn toggle_fold(&mut self, line: usize) -> bool {
        let Some(range) = self.fold_state.fold_starting_at(line).cloned() else {
            return false;
        };
        if self.fold_state.collapsed.contains(&range.start_line) {
            self.fold_state.collapsed.remove(&range.start_line);
        } else {
            self.fold_state.collapsed.insert(range.start_line);
            self.ensure_cursor_visible();
        }
        true
    }

    pub fn collapse_fold_at(&mut self, line: usize) -> bool {
        let Some(range) = self.fold_state.fold_starting_at(line).cloned() else {
            return false;
        };
        self.fold_state.collapsed.insert(range.start_line);
        self.ensure_cursor_visible();
        true
    }

    pub fn expand_fold_at(&mut self, line: usize) -> bool {
        let Some(range) = self.fold_state.fold_starting_at(line).cloned() else {
            return false;
        };
        self.fold_state.collapsed.remove(&range.start_line)
    }

    pub fn collapse_all_folds(&mut self) {
        self.fold_state.collapsed = self
            .fold_state
            .available_folds
            .iter()
            .map(|range| range.start_line)
            .collect();
        self.ensure_cursor_visible();
    }

    pub fn expand_all_folds(&mut self) {
        self.fold_state.collapsed.clear();
    }

    pub fn is_line_visible(&self, line: usize) -> bool {
        self.fold_state.is_line_visible(line)
    }

    pub fn visible_line_count(&self) -> usize {
        (0..self.len_lines())
            .filter(|line| self.is_line_visible(*line))
            .count()
    }

    pub fn fold_containing(&self, line: usize) -> Option<&FoldRange> {
        self.fold_state.collapsed_containing(line)
    }

    pub fn fold_starting_at(&self, line: usize) -> Option<&FoldRange> {
        self.fold_state.fold_starting_at(line)
    }

    pub fn ensure_cursor_visible(&mut self) {
        if let Some(fold) = self.fold_state.collapsed_containing(self.cursor.line) {
            self.cursor.line = fold.start_line;
            self.cursor.col = self
                .cursor
                .col
                .min(self.line_content_len(self.cursor.line).unwrap_or(0));
        }
    }

    pub fn reveal_line(&mut self, line: usize) {
        while let Some(start_line) = self
            .fold_state
            .collapsed_containing(line)
            .map(|fold| fold.start_line)
        {
            self.fold_state.collapsed.remove(&start_line);
        }
    }

    pub fn lines_iter(&self) -> Lines<'_> {
        self.rope.lines()
    }

    pub(crate) fn char_range_to_string(&self, range: Range<usize>) -> Option<String> {
        if range.start > range.end || range.end > self.len_chars() {
            return None;
        }
        Some(self.rope.slice(range).to_string())
    }

    pub fn position_to_char_index(&self, position: CursorPosition) -> Option<usize> {
        let content_len = self.line_content_len(position.line)?;
        if position.col > content_len {
            return None;
        }

        // Ropey addresses edits with a single global character index. A line/column
        // cursor becomes that index by adding its Unicode-scalar column to the
        // line's global start character.
        Some(self.rope.line_to_char(position.line) + position.col)
    }

    /// Global character range of the identifier prefix ending at `position`.
    ///
    /// Defined conservatively as the contiguous run of identifier-prefix
    /// characters (ASCII letters, ASCII digits, `_`, and Unicode alphanumeric
    /// characters where practical) immediately before the caret on the current
    /// line. Stops at punctuation (`.`, `::`, `(`, `!`, etc.) and whitespace;
    /// those characters are never part of the replace range. An empty range
    /// means insert at the caret on acceptance.
    pub fn identifier_prefix_char_range_at(
        &self,
        position: CursorPosition,
    ) -> Option<Range<usize>> {
        let char_end = self.position_to_char_index(position)?;
        let line = self.line_slice(position.line)?;
        let mut col = position.col;
        while col > 0 {
            let ch = line.char(col - 1);
            if !is_identifier_prefix_char(ch) {
                break;
            }
            col -= 1;
        }
        let char_start = self.position_to_char_index(CursorPosition {
            line: position.line,
            col,
        })?;
        Some(char_start..char_end)
    }

    pub fn char_index_to_position(&self, char_index: usize) -> Option<CursorPosition> {
        if char_index > self.len_chars() {
            return None;
        }

        let line = self.rope.char_to_line(char_index);
        let col = char_index - self.rope.line_to_char(line);
        // There is no visible cursor stop between '\r' and '\n'. Rejecting that
        // interior position keeps CRLF a single logical newline.
        (col <= self.line_content_len(line)?).then_some(CursorPosition { line, col })
    }

    pub fn position_to_byte_index(&self, position: CursorPosition) -> Option<usize> {
        let char_index = self.position_to_char_index(position)?;
        Some(self.rope.char_to_byte(char_index))
    }

    pub fn byte_index_to_position(&self, byte_index: usize) -> Option<CursorPosition> {
        if byte_index > self.rope.len_bytes() {
            return None;
        }

        // Ropey's byte_to_char deliberately rounds an interior UTF-8 byte down to
        // its containing scalar. Requiring the round trip to match makes this API
        // strict: callers cannot accidentally create a cursor inside a code point.
        let char_index = self.rope.byte_to_char(byte_index);
        if self.rope.char_to_byte(char_index) != byte_index {
            return None;
        }
        self.char_index_to_position(char_index)
    }

    pub fn insert_char(&mut self, char_index: usize, ch: char) -> Result<(), BufferError> {
        if char_index > self.len_chars() {
            return Err(BufferError::new(char_index, self.len_chars()));
        }
        self.insert_str(char_index, &ch.to_string())
    }

    pub fn delete_char(&mut self, char_index: usize) -> Result<char, BufferError> {
        let ch = self
            .rope
            .get_char(char_index)
            .ok_or_else(|| BufferError::new(char_index, self.len_chars()))?;
        self.delete_range(char_index..char_index + 1)?;
        Ok(ch)
    }

    pub fn insert_at_cursor(&mut self, text: &str) -> Result<(), BufferError> {
        let owns_recording = self.recording.is_none();
        if owns_recording {
            self.begin_edit();
        }
        let char_index = self.cursor_char_index();
        let inserted_chars = text.chars().count();
        self.insert_str(char_index, text)?;
        self.set_cursor_from_char_index(char_index + inserted_chars);
        if owns_recording {
            self.commit_edit();
        }
        Ok(())
    }

    pub fn insert_newline(&mut self) -> Result<(), BufferError> {
        let newline = match self.line_ending {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
        };
        self.insert_at_cursor(newline)
    }

    pub fn delete_backward(&mut self) -> Result<bool, BufferError> {
        let char_index = self.cursor_char_index();
        if self.cursor.col > 0 {
            self.delete_range(char_index - 1..char_index)?;
            self.set_cursor_from_char_index(char_index - 1);
            return Ok(true);
        }
        if self.cursor.line == 0 {
            return Ok(false);
        }

        let previous_line = self.cursor.line - 1;
        let previous_end = self.rope.line_to_char(previous_line)
            + self.line_content_len(previous_line).unwrap_or(0);
        self.delete_range(previous_end..char_index)?;
        self.set_cursor_from_char_index(previous_end);
        Ok(true)
    }

    pub fn delete_forward(&mut self) -> Result<bool, BufferError> {
        let char_index = self.cursor_char_index();
        let line_len = self.line_content_len(self.cursor.line).unwrap_or(0);
        if self.cursor.col < line_len {
            self.delete_range(char_index..char_index + 1)?;
            return Ok(true);
        }
        if self.cursor.line + 1 >= self.len_lines() {
            return Ok(false);
        }

        let next_line_start = self.rope.line_to_char(self.cursor.line + 1);
        self.delete_range(char_index..next_line_start)?;
        Ok(true)
    }

    pub fn move_left(&mut self) {
        if self.cursor.col > 0 {
            self.cursor.col -= 1;
        } else if self.cursor.line > 0 {
            self.cursor.line = self.previous_visible_line(self.cursor.line).unwrap_or(0);
            self.cursor.col = self.line_content_len(self.cursor.line).unwrap_or(0);
        }
    }

    pub fn move_right(&mut self) {
        let line_len = self.line_content_len(self.cursor.line).unwrap_or(0);
        if self.cursor.col < line_len {
            self.cursor.col += 1;
        } else if let Some(next_line) = self.next_visible_line(self.cursor.line) {
            self.cursor.line = next_line;
            self.cursor.col = 0;
        }
    }

    pub fn move_vertical(&mut self, line_delta: isize, preferred_col: usize) {
        let line = if line_delta < 0 {
            self.previous_visible_line(self.cursor.line)
                .unwrap_or(self.cursor.line)
        } else if line_delta > 0 {
            self.next_visible_line(self.cursor.line)
                .unwrap_or(self.cursor.line)
        } else {
            self.cursor.line
        };
        let col = preferred_col.min(self.line_content_len(line).unwrap_or(0));
        self.cursor = CursorPosition { line, col };
    }

    fn previous_visible_line(&self, line: usize) -> Option<usize> {
        (0..line)
            .rev()
            .find(|candidate| self.is_line_visible(*candidate))
    }

    fn next_visible_line(&self, line: usize) -> Option<usize> {
        (line + 1..self.len_lines()).find(|candidate| self.is_line_visible(*candidate))
    }

    pub fn load_from_file(&mut self, path: &Path) -> io::Result<()> {
        let bytes = std::fs::read(path)?;
        let (text, encoding) = decode_text(&bytes)?;
        let rope = Rope::from_str(&text);
        let line_ending = detect_line_ending(&rope);

        // Assign only after the whole read succeeds, so a failed open cannot
        // destroy the document already being edited.
        self.rope = rope;
        self.cursor = CursorPosition::default();
        self.path = Some(path.to_path_buf());
        self.dirty = false;
        self.modified = false;
        self.cached_layout = None;
        self.revision = self.revision.wrapping_add(1);
        self.lsp_version = 0;
        self.lsp_dirty = false;
        self.line_ending = line_ending;
        self.encoding = encoding;
        self.fold_state = FoldState::default();

        let lang = crate::language::LanguageId::from_path(path);
        self.set_language(lang);

        // Cache file size and reset large-file-mode override.
        self.file_size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(bytes.len() as u64);
        self.large_file_override = false;
        // large_file_mode is set by the caller after inspecting config thresholds.

        Ok(())
    }

    pub fn save_to_file(&mut self, path: &Path) -> io::Result<()> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);
        write_encoded(&mut writer, &self.rope.to_string(), self.encoding)?;
        writer.flush()?;

        self.path = Some(path.to_path_buf());
        self.modified = false;

        let lang = crate::language::LanguageId::from_path(path);
        self.set_language(lang);

        self.inlay_hints_dirty = true;
        self.code_lens_dirty = true;
        self.semantic_tokens_dirty = true;

        // Update cached file size after save.
        self.file_size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

        Ok(())
    }

    /// Check whether this buffer should enter large file mode based on config thresholds.
    /// Returns `true` if the mode changed (so caller can trigger banner update).
    pub fn check_large_file_mode(
        &mut self,
        warn_kb: u64,
        mode_kb: u64,
        warn_lines: usize,
        mode_lines: usize,
    ) -> bool {
        let size_triggers_mode = self.file_size_bytes >= mode_kb * 1024;
        let line_triggers_mode = self.rope.len_lines() >= mode_lines;
        let should_be_large = (size_triggers_mode || line_triggers_mode) && !self.large_file_override;

        // Warning zone: log performance data for expensive ops.
        let _in_warn_zone = self.file_size_bytes >= warn_kb * 1024
            || self.rope.len_lines() >= warn_lines;

        let changed = self.large_file_mode != should_be_large;
        if changed {
            self.large_file_mode = should_be_large;
            if should_be_large {
                // Disable expensive features.
                self.cached_layout = None;
                self.dirty = false; // prevent pending re-highlight
            } else {
                // Re-enable: trigger re-highlight.
                self.dirty = true;
                self.inlay_hints_dirty = true;
                self.code_lens_dirty = true;
                self.semantic_tokens_dirty = true;
            }
        }
        changed
    }

    /// Returns whether expensive rendering operations (syntax highlight, minimap, etc.)
    /// are currently suppressed.
    pub fn features_suppressed(&self) -> bool {
        self.large_file_mode && !self.large_file_override
    }

    fn cursor_char_index(&self) -> usize {
        self.position_to_char_index(self.cursor)
            .expect("TextBuffer always stores a valid cursor")
    }

    fn insert_str(&mut self, char_index: usize, text: &str) -> Result<(), BufferError> {
        if char_index > self.len_chars() {
            return Err(BufferError::new(char_index, self.len_chars()));
        }
        if text.is_empty() {
            return Ok(());
        }
        let owns_recording = self.recording.is_none();
        if owns_recording {
            self.begin_edit();
        }

        let cursor_index = self.cursor_char_index();
        let inserted_chars = text.chars().count();
        self.rope.insert(char_index, text);
        if let Some(record) = self.recording.as_mut() {
            record.operations.push(RopeOp::Insert {
                char_offset: char_index,
                text: text.to_owned(),
            });
        }
        self.note_text_changed();
        let adjusted_cursor = if char_index <= cursor_index {
            cursor_index + inserted_chars
        } else {
            cursor_index
        };
        self.set_cursor_from_char_index(adjusted_cursor);
        if owns_recording {
            self.commit_edit();
        }
        Ok(())
    }

    fn delete_range(&mut self, range: Range<usize>) -> Result<(), BufferError> {
        if range.start > range.end || range.end > self.len_chars() {
            return Err(BufferError::new(range.end, self.len_chars()));
        }
        if range.is_empty() {
            return Ok(());
        }
        let owns_recording = self.recording.is_none();
        if owns_recording {
            self.begin_edit();
        }

        let cursor_index = self.cursor_char_index();
        let deleted = self.rope.slice(range.clone()).to_string();
        let removed_chars = range.end - range.start;
        self.rope.remove(range.clone());
        if let Some(record) = self.recording.as_mut() {
            record.operations.push(RopeOp::Delete {
                char_offset: range.start,
                length: removed_chars,
                deleted,
            });
        }
        self.note_text_changed();
        let adjusted_cursor = if cursor_index <= range.start {
            cursor_index
        } else if cursor_index >= range.end {
            cursor_index - removed_chars
        } else {
            range.start
        };
        self.set_cursor_from_char_index(adjusted_cursor);
        if owns_recording {
            self.commit_edit();
        }
        Ok(())
    }

    fn note_text_changed(&mut self) {
        self.dirty = true;
        self.modified = true;
        self.revision = self.revision.wrapping_add(1);
        self.lsp_version = self.lsp_version.saturating_add(1);
        self.lsp_dirty = true;
    }

    // -----------------------------------------------------------------------
    // Search-facing public APIs
    // -----------------------------------------------------------------------

    /// Return the entire buffer as a `String`.  Used by the search engine to
    /// obtain text without cloning the rope into a `Vec<u8>` first.
    pub fn to_full_string(&self) -> String {
        self.rope.to_string()
    }

    /// Convert a UTF-8 byte offset (as returned by the `regex` crate) into a
    /// Ropey character (Unicode scalar) index.
    ///
    /// Returns `None` when:
    /// - `byte_index > self.rope.len_bytes()`
    /// - `byte_index` is not on a UTF-8 char boundary (i.e. it points inside a
    ///   multi-byte code point).
    ///
    /// The round-trip check mirrors `byte_index_to_position` to enforce the
    /// same strictness across both APIs.
    pub fn byte_to_char_index(&self, byte_index: usize) -> Option<usize> {
        if byte_index > self.rope.len_bytes() {
            return None;
        }
        let char_index = self.rope.byte_to_char(byte_index);
        // Round-trip: confirm the byte_index sits on a char boundary.
        if self.rope.char_to_byte(char_index) != byte_index {
            return None;
        }
        Some(char_index)
    }

    /// Replace the text in the character range `char_start..char_end` with
    /// `new_text`, then mark the buffer modified and increment the revision.
    ///
    /// Returns `Err` when:
    /// - `char_start > char_end`
    /// - `char_end > self.len_chars()`
    ///
    /// Apply one completion `textEdit` span using UTF-16 columns from the server.
    ///
    /// `new_text` is inserted literally; snippet placeholders are not expanded.
    pub fn apply_lsp_text_edit(&mut self, edit: &LspTextEdit) -> Result<(), RangeError> {
        let start_line = usize::try_from(edit.line_start)
            .map_err(|_| RangeError::new("text edit start line out of range"))?;
        let end_line = usize::try_from(edit.line_end)
            .map_err(|_| RangeError::new("text edit end line out of range"))?;
        let start_line_text = self.line_text(start_line).unwrap_or_default();
        let end_line_text = if end_line == start_line {
            start_line_text.clone()
        } else {
            self.line_text(end_line).unwrap_or_default()
        };
        let start = CursorPosition {
            line: start_line,
            col: decode_utf16_column(&start_line_text, edit.col_start),
        };
        let end = CursorPosition {
            line: end_line,
            col: decode_utf16_column(&end_line_text, edit.col_end),
        };
        let char_start = self
            .position_to_char_index(start)
            .ok_or_else(|| RangeError::new("text edit start position is invalid"))?;
        let char_end = self
            .position_to_char_index(end)
            .ok_or_else(|| RangeError::new("text edit end position is invalid"))?;
        self.replace_char_range(char_start, char_end, &edit.new_text)
    }

    /// Apply an accepted completion item (literal text only).
    ///
    /// When `text_edit` is present, one UTF-16 range replacement is applied and `insert_text`
    /// is ignored. Otherwise `insert_text` (from `completion_acceptance_insert_text`) replaces
    /// the snapshotted identifier prefix, or inserts at the caret when the prefix range is empty.
    /// Snippet tab stops are not expanded on either path.
    /// The caret ends immediately after the inserted text. Buffer revision is incremented
    /// exactly once. `lsp_dirty` is set so the app can send a normal `didChange` on sync.
    pub fn apply_completion_insertion(
        &mut self,
        text_edit: Option<&LspTextEdit>,
        prefix_char_range: Option<Range<usize>>,
        insert_text: &str,
    ) -> Result<(), RangeError> {
        if let Some(edit) = text_edit {
            return self.apply_lsp_text_edit(edit);
        }
        let range = prefix_char_range.unwrap_or_else(|| {
            let end = self.cursor_char_index();
            end..end
        });
        if range.is_empty() {
            self.insert_at_cursor(insert_text)
                .map_err(|error| RangeError::new(error.to_string()))
        } else {
            self.replace_char_range(range.start, range.end, insert_text)
        }
    }

    pub fn replace_char_range(
        &mut self,
        char_start: usize,
        char_end: usize,
        new_text: &str,
    ) -> Result<(), RangeError> {
        if char_start > char_end {
            return Err(RangeError::new(format!(
                "replace_char_range: start {char_start} > end {char_end}"
            )));
        }
        if char_end > self.len_chars() {
            return Err(RangeError::new(format!(
                "replace_char_range: end {char_end} > buffer length {}",
                self.len_chars()
            )));
        }
        let owns_recording = self.recording.is_none();
        if owns_recording {
            self.begin_edit();
        }
        // Remove then insert – both operations must succeed atomically from the
        // user's perspective.  Because Ropey never panics for valid indices, we
        // chain them without intermediate error checks.
        let deleted = if char_start < char_end {
            self.rope.slice(char_start..char_end).to_string()
        } else {
            String::new()
        };
        if char_start < char_end {
            self.rope.remove(char_start..char_end);
            if let Some(record) = self.recording.as_mut() {
                record.operations.push(RopeOp::Delete {
                    char_offset: char_start,
                    length: char_end - char_start,
                    deleted,
                });
            }
        }
        if !new_text.is_empty() {
            self.rope.insert(char_start, new_text);
            if let Some(record) = self.recording.as_mut() {
                record.operations.push(RopeOp::Insert {
                    char_offset: char_start,
                    text: new_text.to_owned(),
                });
            }
        }
        self.note_text_changed();
        // Move cursor to the end of the inserted text.
        self.set_cursor_from_char_index(char_start + new_text.chars().count());
        if owns_recording {
            self.commit_edit();
        }
        Ok(())
    }

    /// Replace a UTF-8 byte range with `new_text`.
    ///
    /// Converts the byte offsets to char indices (via `byte_to_char_index`)
    /// then calls `replace_char_range`.  Returns `Err` when either byte
    /// offset is not on a UTF-8 char boundary or is out of range.
    pub fn replace_byte_range(
        &mut self,
        byte_range: Range<usize>,
        new_text: &str,
    ) -> Result<(), RangeError> {
        let char_start = self.byte_to_char_index(byte_range.start).ok_or_else(|| {
            RangeError::new(format!(
                "byte offset {} is not a valid char boundary or is out of range",
                byte_range.start
            ))
        })?;
        let char_end = self.byte_to_char_index(byte_range.end).ok_or_else(|| {
            RangeError::new(format!(
                "byte offset {} is not a valid char boundary or is out of range",
                byte_range.end
            ))
        })?;
        self.replace_char_range(char_start, char_end, new_text)
    }

    /// Apply multiple `(byte_range, replacement)` pairs to the buffer in a
    /// single logical operation.
    ///
    /// Ranges must be non-overlapping.  Processing is done **right-to-left**
    /// so that earlier byte offsets remain valid after each substitution.
    ///
    /// Only one revision increment is performed regardless of how many ranges
    /// are replaced (the buffer is only marked dirty once).
    ///
    /// Returns `Err` when any range is invalid or overlapping.
    pub fn apply_byte_replacements(
        &mut self,
        mut pairs: Vec<(Range<usize>, String)>,
    ) -> Result<usize, RangeError> {
        if pairs.is_empty() {
            return Ok(0);
        }

        // Validate all ranges up-front before mutating anything.
        let byte_len = self.rope.len_bytes();
        for (range, _) in &pairs {
            if range.start > range.end {
                return Err(RangeError::new(format!(
                    "range {}..{} is inverted",
                    range.start, range.end
                )));
            }
            if range.end > byte_len {
                return Err(RangeError::new(format!(
                    "range {}..{} exceeds buffer byte length {byte_len}",
                    range.start, range.end
                )));
            }
            // Boundary check.
            self.byte_to_char_index(range.start).ok_or_else(|| {
                RangeError::new(format!("byte {} is not a UTF-8 char boundary", range.start))
            })?;
            self.byte_to_char_index(range.end).ok_or_else(|| {
                RangeError::new(format!("byte {} is not a UTF-8 char boundary", range.end))
            })?;
        }

        // Check overlap (sort by start, then verify end < next start).
        let mut sorted = pairs.iter().map(|(r, _)| r.clone()).collect::<Vec<_>>();
        sorted.sort_by_key(|r| r.start);
        for w in sorted.windows(2) {
            if w[0].end > w[1].start {
                return Err(RangeError::new(format!(
                    "ranges {}..{} and {}..{} overlap",
                    w[0].start, w[0].end, w[1].start, w[1].end
                )));
            }
        }

        // Apply right-to-left.
        pairs.sort_by_key(|b| std::cmp::Reverse(b.0.start));
        let count = pairs.len();
        let owns_recording = self.recording.is_none();
        if owns_recording {
            self.begin_edit();
        }
        for (range, new_text) in pairs {
            let char_start = self.rope.byte_to_char(range.start);
            let char_end = self.rope.byte_to_char(range.end);
            if char_start < char_end {
                let deleted = self.rope.slice(char_start..char_end).to_string();
                self.rope.remove(char_start..char_end);
                if let Some(record) = self.recording.as_mut() {
                    record.operations.push(RopeOp::Delete {
                        char_offset: char_start,
                        length: char_end - char_start,
                        deleted,
                    });
                }
            }
            if !new_text.is_empty() {
                self.rope.insert(char_start, &new_text);
                if let Some(record) = self.recording.as_mut() {
                    record.operations.push(RopeOp::Insert {
                        char_offset: char_start,
                        text: new_text,
                    });
                }
            }
        }
        // Mark changed exactly once.
        self.note_text_changed();
        if owns_recording {
            self.commit_edit();
        }
        // Leave cursor where it was; callers that need to reposition do so
        // via set_cursor_to_byte.
        Ok(count)
    }

    /// Move the cursor to the character position corresponding to a UTF-8 byte
    /// offset.  Returns `Err` when the offset is not on a char boundary or is
    /// out of range.
    pub fn set_cursor_to_byte(&mut self, byte_offset: usize) -> Result<(), RangeError> {
        let position = self.byte_index_to_position(byte_offset).ok_or_else(|| {
            RangeError::new(format!(
                "byte offset {byte_offset} is not a valid char boundary or is out of range"
            ))
        })?;
        self.cursor = position;
        Ok(())
    }

    fn set_cursor_from_char_index(&mut self, char_index: usize) {
        if let Some(position) = self.char_index_to_position(char_index.min(self.len_chars())) {
            self.cursor = position;
            if self.cursors.len() == 1 {
                self.cursors[0] = CursorAnchor::caret(position);
                self.primary = 0;
            }
            return;
        }

        // This is reachable only when a low-level edit targets the middle of a
        // CRLF pair. Snap to the visible end of that line instead of panicking.
        let line = self.rope.char_to_line(char_index.min(self.len_chars()));
        self.cursor = CursorPosition {
            line,
            col: self.line_content_len(line).unwrap_or(0),
        };
        if self.cursors.len() == 1 {
            self.cursors[0] = CursorAnchor::caret(self.cursor);
            self.primary = 0;
        }
    }
}

fn apply_semantic_tokens(
    mut job: egui::text::LayoutJob,
    rope: &Rope,
    tokens: &[crate::lsp::types::SemanticToken],
) -> egui::text::LayoutJob {
    for token in tokens {
        let line = token.line as usize;
        let Some(slice) = rope.get_line(line) else {
            continue;
        };
        let content_len = slice.len_chars() - line_break_len(slice);
        let line_text = slice.slice(..content_len).to_string();
        let start_col = decode_utf16_column(&line_text, token.col);
        let end_col = decode_utf16_column(&line_text, token.col.saturating_add(token.length));
        let char_start = rope.line_to_char(line) + start_col;
        let char_end = rope.line_to_char(line) + end_col;
        let byte_start = rope.char_to_byte(char_start.min(rope.len_chars()));
        let byte_end = rope.char_to_byte(char_end.min(rope.len_chars()));
        if byte_start >= byte_end {
            continue;
        }
        let Some(index) = job.sections.iter().position(|section| {
            section.byte_range.start <= byte_start && section.byte_range.end >= byte_end
        }) else {
            continue;
        };
        let section = job.sections.remove(index);
        let mut replacement = Vec::new();
        if section.byte_range.start < byte_start {
            let mut before = section.clone();
            before.byte_range.end = byte_start;
            replacement.push(before);
        }
        let mut semantic = section.clone();
        semantic.byte_range = byte_start..byte_end;
        semantic.format.color = token.color;
        semantic.format.italics = token.italic;
        if token.underline {
            semantic.format.underline = egui::Stroke::new(1.0, token.color);
        }
        replacement.push(semantic);
        if byte_end < section.byte_range.end {
            let mut after = section;
            after.byte_range.start = byte_end;
            replacement.push(after);
        }
        job.sections.splice(index..index, replacement);
    }
    job
}

fn apply_bracket_colors(
    mut job: egui::text::LayoutJob,
    source: &str,
    colors: &[BracketColor],
) -> egui::text::LayoutJob {
    let char_bytes: Vec<(usize, char)> = source.char_indices().collect();
    for bracket in colors {
        let Some((byte_start, ch)) = char_bytes.get(bracket.char_offset).copied() else {
            continue;
        };
        let byte_end = byte_start + ch.len_utf8();
        let Some(index) = job.sections.iter().position(|section| {
            section.byte_range.start <= byte_start && section.byte_range.end >= byte_end
        }) else {
            continue;
        };
        let section = job.sections.remove(index);
        let mut replacement = Vec::new();
        if section.byte_range.start < byte_start {
            let mut before = section.clone();
            before.byte_range.end = byte_start;
            replacement.push(before);
        }
        let mut colored = section.clone();
        colored.byte_range = byte_start..byte_end;
        colored.format.color = bracket.color;
        replacement.push(colored);
        if byte_end < section.byte_range.end {
            let mut after = section;
            after.byte_range.start = byte_end;
            replacement.push(after);
        }
        job.sections.splice(index..index, replacement);
    }
    job
}

fn transform_text(text: &str, transform: CaseTransform) -> String {
    if transform == CaseTransform::Upper {
        return text.to_uppercase();
    }
    if transform == CaseTransform::Lower {
        return text.to_lowercase();
    }
    if transform == CaseTransform::Title {
        return text
            .split_whitespace()
            .map(|word| {
                let mut chars = word.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" ");
    }
    let mut words = Vec::<String>::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_whitespace() || ch == '_' || ch == '-' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        } else if ch.is_uppercase() && current.chars().last().is_some_and(char::is_lowercase) {
            words.push(std::mem::take(&mut current));
            current.push(ch);
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    let mut lower: Vec<String> = words.into_iter().map(|word| word.to_lowercase()).collect();
    match transform {
        CaseTransform::Snake => lower.join("_"),
        CaseTransform::Kebab => lower.join("-"),
        CaseTransform::Camel | CaseTransform::Pascal => {
            for (index, word) in lower.iter_mut().enumerate() {
                if index > 0 || transform == CaseTransform::Pascal {
                    if let Some(first) = word.chars().next() {
                        word.replace_range(..first.len_utf8(), &first.to_uppercase().to_string());
                    }
                }
            }
            lower.concat()
        }
        _ => text.to_owned(),
    }
}

pub(crate) fn is_identifier_prefix_char(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

/// Public re-export for use in `editor/completion.rs`.
pub fn is_identifier_prefix_char_pub(ch: char) -> bool {
    is_identifier_prefix_char(ch)
}

fn decode_text(bytes: &[u8]) -> io::Result<(String, TextEncoding)> {
    if let Some(payload) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(payload, u16::from_le_bytes).map(|text| (text, TextEncoding::Utf16Le));
    }
    if let Some(payload) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(payload, u16::from_be_bytes).map(|text| (text, TextEncoding::Utf16Be));
    }

    let (payload, encoding) = match bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        Some(payload) => (payload, TextEncoding::Utf8Bom),
        None => (bytes, TextEncoding::Utf8),
    };
    String::from_utf8(payload.to_vec())
        .map(|text| (text, encoding))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn decode_utf16(bytes: &[u8], decode_unit: fn([u8; 2]) -> u16) -> io::Result<String> {
    if bytes.len() % 2 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "UTF-16 text contains an incomplete code unit",
        ));
    }

    let units = bytes
        .chunks_exact(2)
        .map(|chunk| decode_unit([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    String::from_utf16(&units).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn write_encoded(writer: &mut impl Write, text: &str, encoding: TextEncoding) -> io::Result<()> {
    match encoding {
        TextEncoding::Utf8 => writer.write_all(text.as_bytes()),
        TextEncoding::Utf8Bom => {
            writer.write_all(&[0xEF, 0xBB, 0xBF])?;
            writer.write_all(text.as_bytes())
        }
        TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
            let (bom, encode_unit): (&[u8], fn(u16) -> [u8; 2]) = match encoding {
                TextEncoding::Utf16Le => (&[0xFF, 0xFE], u16::to_le_bytes),
                TextEncoding::Utf16Be => (&[0xFE, 0xFF], u16::to_be_bytes),
                _ => unreachable!(),
            };
            writer.write_all(bom)?;
            for unit in text.encode_utf16() {
                writer.write_all(&encode_unit(unit))?;
            }
            Ok(())
        }
    }
}

fn line_break_len(line: RopeSlice<'_>) -> usize {
    let len = line.len_chars();
    if len >= 2 && line.char(len - 2) == '\r' && line.char(len - 1) == '\n' {
        return 2;
    }
    if len == 0 {
        return 0;
    }

    matches!(
        line.char(len - 1),
        '\n' | '\r' | '\u{000B}' | '\u{000C}' | '\u{0085}' | '\u{2028}' | '\u{2029}'
    ) as usize
}

fn detect_line_ending(rope: &Rope) -> LineEnding {
    for line in rope.lines() {
        let len = line.len_chars();
        if len >= 2 && line.char(len - 2) == '\r' && line.char(len - 1) == '\n' {
            return LineEnding::Crlf;
        }
        if line_break_len(line) > 0 {
            return LineEnding::Lf;
        }
    }
    LineEnding::Lf
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    // Buffer encoding regressions — see module docs (Buffer tests).

    #[test]
    fn empty_buffer_has_one_safe_editable_line() {
        let mut buffer = TextBuffer::default();

        assert_eq!(buffer.len_chars(), 0);
        assert_eq!(buffer.len_lines(), 1);
        assert_eq!(buffer.cursor(), CursorPosition::default());
        assert_eq!(buffer.line_content_len(0), Some(0));
        assert_eq!(buffer.line_text(0).as_deref(), Some(""));
        assert_eq!(
            buffer.position_to_char_index(CursorPosition::default()),
            Some(0)
        );
        assert_eq!(
            buffer.byte_index_to_position(0),
            Some(CursorPosition::default())
        );

        buffer.move_left();
        buffer.move_right();
        assert!(!buffer.delete_backward().unwrap());
        assert!(!buffer.delete_forward().unwrap());
        assert_eq!(buffer.cursor(), CursorPosition::default());
    }

    #[test]
    fn utf8_conversions_only_accept_scalar_boundaries() {
        let buffer = TextBuffer::from_text("aé🙂\n中");

        assert_eq!(
            buffer.position_to_char_index(CursorPosition { line: 0, col: 3 }),
            Some(3)
        );
        assert_eq!(
            buffer.position_to_byte_index(CursorPosition { line: 0, col: 3 }),
            Some(7)
        );
        assert_eq!(
            buffer.byte_index_to_position(7),
            Some(CursorPosition { line: 0, col: 3 })
        );
        assert_eq!(buffer.byte_index_to_position(2), None);
        assert_eq!(
            buffer.char_index_to_position(4),
            Some(CursorPosition { line: 1, col: 0 })
        );
        assert_eq!(
            buffer.position_to_char_index(CursorPosition { line: 0, col: 4 }),
            None
        );
    }

    #[test]
    fn insertion_and_deletion_use_global_character_indices() {
        let mut buffer = TextBuffer::from_text("ab🙂");

        buffer.insert_char(2, 'é').unwrap();
        assert_eq!(buffer.line_text(0).as_deref(), Some("abé🙂"));
        assert_eq!(buffer.delete_char(3).unwrap(), '🙂');
        assert_eq!(buffer.line_text(0).as_deref(), Some("abé"));
        assert!(buffer.insert_char(99, 'x').is_err());
        assert!(buffer.delete_char(99).is_err());
    }

    #[test]
    fn crlf_is_one_logical_newline_for_editing() {
        let mut buffer = TextBuffer::from_text("one\r\ntwo");
        assert_eq!(buffer.line_ending(), LineEnding::Crlf);
        assert_eq!(buffer.line_content_len(0), Some(3));
        assert_eq!(buffer.char_index_to_position(4), None);

        buffer.set_cursor(CursorPosition { line: 1, col: 0 });
        assert!(buffer.delete_backward().unwrap());
        assert_eq!(buffer.line_text(0).as_deref(), Some("onetwo"));
        assert_eq!(buffer.cursor(), CursorPosition { line: 0, col: 3 });

        buffer.set_cursor(CursorPosition { line: 0, col: 3 });
        buffer.insert_newline().unwrap();
        assert_eq!(
            buffer
                .lines_iter()
                .map(|line| line.to_string())
                .collect::<String>(),
            "one\r\ntwo"
        );
    }

    #[test]
    fn vertical_movement_preserves_the_requested_column() {
        let mut buffer = TextBuffer::from_text("abcdef\nx\n123456");
        buffer.set_cursor(CursorPosition { line: 0, col: 5 });

        buffer.move_vertical(1, 5);
        assert_eq!(buffer.cursor(), CursorPosition { line: 1, col: 1 });
        buffer.move_vertical(1, 5);
        assert_eq!(buffer.cursor(), CursorPosition { line: 2, col: 5 });
        buffer.move_left();
        buffer.move_right();
        assert_eq!(buffer.cursor(), CursorPosition { line: 2, col: 5 });
    }

    #[test]
    fn file_round_trip_updates_path_and_dirty_state() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("blue_ide_buffer_{unique}.rs"));
        let mut buffer = TextBuffer::from_text("fn main() { println!(\"héllo\"); }\n");
        buffer.insert_at_cursor("// ").unwrap();
        assert!(buffer.is_dirty());

        buffer.save_to_file(&path).unwrap();
        assert_eq!(buffer.path(), Some(path.as_path()));
        assert!(buffer.is_dirty());
        assert!(!buffer.is_modified());
        let _ = buffer.get_layout(FontId::monospace(14.0));
        assert!(!buffer.is_dirty());

        let mut loaded = TextBuffer::default();
        loaded.load_from_file(&path).unwrap();
        assert_eq!(
            loaded
                .lines_iter()
                .map(|line| line.to_string())
                .collect::<String>(),
            fs::read_to_string(&path).unwrap()
        );
        assert_eq!(loaded.path(), Some(path.as_path()));
        assert!(!loaded.is_dirty());

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn loads_utf16_little_endian_text_with_a_bom() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("blue_ide_utf16_le_{unique}.txt"));
        let text = "cargo check\r\nhello 世界\r\n";
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        fs::write(&path, bytes).unwrap();

        let mut buffer = TextBuffer::default();
        buffer.load_from_file(&path).unwrap();

        assert_eq!(buffer.text(), text);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn loads_utf16_big_endian_and_utf8_bom_without_exposing_the_bom() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let utf16_path = std::env::temp_dir().join(format!("blue_ide_utf16_be_{unique}.txt"));
        let utf8_path = std::env::temp_dir().join(format!("blue_ide_utf8_bom_{unique}.txt"));
        let text = "hello 🌍\n";

        let mut utf16_bytes = vec![0xFE, 0xFF];
        utf16_bytes.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
        fs::write(&utf16_path, utf16_bytes).unwrap();
        fs::write(
            &utf8_path,
            [vec![0xEF, 0xBB, 0xBF], text.as_bytes().to_vec()].concat(),
        )
        .unwrap();

        let mut utf16_buffer = TextBuffer::default();
        utf16_buffer.load_from_file(&utf16_path).unwrap();
        let mut utf8_buffer = TextBuffer::default();
        utf8_buffer.load_from_file(&utf8_path).unwrap();

        assert_eq!(utf16_buffer.text(), text);
        assert_eq!(utf8_buffer.text(), text);
        fs::remove_file(utf16_path).unwrap();
        fs::remove_file(utf8_path).unwrap();
    }

    #[test]
    fn save_preserves_the_loaded_text_encoding_and_bom() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let cases = [
            (TextEncoding::Utf8, "utf8"),
            (TextEncoding::Utf8Bom, "utf8_bom"),
            (TextEncoding::Utf16Le, "utf16_le"),
            (TextEncoding::Utf16Be, "utf16_be"),
        ];

        for (encoding, label) in cases {
            let path = std::env::temp_dir().join(format!("blue_ide_{label}_{unique}.txt"));
            let original = "hello 世界\n";
            let mut bytes = Vec::new();
            write_encoded(&mut bytes, original, encoding).unwrap();
            fs::write(&path, bytes).unwrap();

            let mut buffer = TextBuffer::default();
            buffer.load_from_file(&path).unwrap();
            buffer.insert_at_cursor("X").unwrap();
            buffer.save_to_file(&path).unwrap();

            let mut expected = Vec::new();
            write_encoded(&mut expected, &format!("X{original}"), encoding).unwrap();
            assert_eq!(fs::read(&path).unwrap(), expected, "encoding case: {label}");
            fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn malformed_encoded_text_fails_without_replacing_the_buffer() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("blue_ide_invalid_encoding_{unique}.txt"));
        let mut buffer = TextBuffer::from_text("existing text");

        for invalid_bytes in [
            vec![0xFF, 0xFE, 0x41],
            vec![0xFF, 0xFE, 0x00, 0xD8],
            vec![0xFF],
        ] {
            fs::write(&path, invalid_bytes).unwrap();
            let error = buffer.load_from_file(&path).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(buffer.text(), "existing text");
            assert_eq!(buffer.path(), None);
        }

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn accepting_a_completion_replaces_the_current_identifier_prefix() {
        let mut buffer = TextBuffer::from_text("fn ma() {}\n");
        buffer.set_cursor(CursorPosition { line: 0, col: 5 });
        let prefix = buffer
            .identifier_prefix_char_range_at(buffer.cursor())
            .expect("prefix at caret");
        assert_eq!(prefix, 3..5, "only the partial identifier is replaced");

        buffer
            .apply_completion_insertion(None, Some(prefix), "main")
            .unwrap();
        assert_eq!(buffer.text(), "fn main() {}\n");
        assert_eq!(buffer.cursor(), CursorPosition { line: 0, col: 7 });

        let mut qualified = TextBuffer::from_text("self.par\n");
        qualified.set_cursor(CursorPosition { line: 0, col: 8 });
        let prefix = qualified
            .identifier_prefix_char_range_at(qualified.cursor())
            .expect("suffix prefix");
        assert_eq!(prefix, 5..8);
        qualified
            .apply_completion_insertion(None, Some(prefix), "partial")
            .unwrap();
        assert_eq!(qualified.text(), "self.partial\n");
        assert_eq!(qualified.cursor(), CursorPosition { line: 0, col: 12 });

        let mut unicode = TextBuffer::from_text("let caf\n");
        unicode.set_cursor(CursorPosition { line: 0, col: 7 });
        let prefix = unicode
            .identifier_prefix_char_range_at(unicode.cursor())
            .expect("unicode prefix");
        assert_eq!(prefix, 4..7);
        unicode
            .apply_completion_insertion(None, Some(prefix), "café")
            .unwrap();
        assert_eq!(unicode.text(), "let café\n");
        assert_eq!(unicode.cursor(), CursorPosition { line: 0, col: 8 });

        let mut snapshotted = TextBuffer::from_text("fn ma() {}\n");
        snapshotted.set_cursor(CursorPosition { line: 0, col: 5 });
        let frozen_prefix = 3..5;
        snapshotted
            .apply_completion_insertion(None, Some(frozen_prefix), "main")
            .unwrap();
        assert_eq!(snapshotted.text(), "fn main() {}\n");
    }

    #[test]
    fn accepting_a_completion_does_not_remove_separators_or_punctuation() {
        fn accept_label(buffer: &mut TextBuffer, cursor: CursorPosition, label: &str) {
            buffer.set_cursor(cursor);
            let prefix = buffer
                .identifier_prefix_char_range_at(cursor)
                .expect("prefix range");
            buffer
                .apply_completion_insertion(None, Some(prefix), label)
                .unwrap();
        }

        let mut dot = TextBuffer::from_text("self.ba\n");
        let dot_cursor = CursorPosition { line: 0, col: 7 };
        let prefix = dot
            .identifier_prefix_char_range_at(dot_cursor)
            .expect("dot prefix");
        assert_eq!(prefix, 5..7, "`.` must stay outside the replace range");
        accept_label(&mut dot, dot_cursor, "bar");
        assert_eq!(dot.text(), "self.bar\n");

        let mut path_sep = TextBuffer::from_text("foo::ba\n");
        let path_cursor = CursorPosition { line: 0, col: 7 };
        let prefix = path_sep
            .identifier_prefix_char_range_at(path_cursor)
            .expect("path prefix");
        assert_eq!(prefix, 5..7, "`::` must stay outside the replace range");
        accept_label(&mut path_sep, path_cursor, "bar");
        assert_eq!(path_sep.text(), "foo::bar\n");

        let mut space = TextBuffer::from_text("let ma\n");
        let space_cursor = CursorPosition { line: 0, col: 6 };
        let prefix = space
            .identifier_prefix_char_range_at(space_cursor)
            .expect("space prefix");
        assert_eq!(
            prefix,
            4..6,
            "whitespace must stay outside the replace range"
        );
        accept_label(&mut space, space_cursor, "main");
        assert_eq!(space.text(), "let main\n");

        let mut paren = TextBuffer::from_text("main(\n");
        let paren_cursor = CursorPosition { line: 0, col: 5 };
        let prefix = paren
            .identifier_prefix_char_range_at(paren_cursor)
            .expect("paren prefix");
        assert_eq!(prefix, 5..5, "empty prefix inserts before `(`");
        accept_label(&mut paren, paren_cursor, "arg");
        assert_eq!(paren.text(), "main(arg\n");

        let mut braces = TextBuffer::from_text("fn ma() {}\n");
        let braces_cursor = CursorPosition { line: 0, col: 5 };
        let prefix = braces
            .identifier_prefix_char_range_at(braces_cursor)
            .expect("braces prefix");
        assert_eq!(prefix, 3..5);
        accept_label(&mut braces, braces_cursor, "main");
        assert_eq!(braces.text(), "fn main() {}\n");
    }

    #[test]
    fn keep_the_implementation_panic_free_for_unicode_and_empty_lines() {
        use crate::editor::position::{char_column_to_utf16, decode_utf16_column};
        use crate::lsp::types::LspTextEdit;

        fn exercise_cursor_ops(buffer: &mut TextBuffer, line: usize, cols: &[usize]) {
            for &col in cols {
                buffer.set_cursor(CursorPosition { line, col });
                let cursor = buffer.cursor();
                let lsp = buffer.position_lsp_position(cursor);
                let _ = buffer.lsp_position_to_cursor(lsp);
                let _ = buffer.position_to_char_index(cursor);
                let _ = buffer.identifier_prefix_char_range_at(cursor);
                buffer.move_left();
                buffer.move_right();
                buffer.move_vertical(-1, cursor.col);
                buffer.move_vertical(1, cursor.col);
            }
        }

        let mut empty = TextBuffer::default();
        exercise_cursor_ops(&mut empty, 0, &[0, 1, 99]);
        let _ = empty.insert_at_cursor("");
        let _ = empty.insert_at_cursor("x");
        assert_eq!(empty.line_text(0).as_deref(), Some("x"));

        let mut multi = TextBuffer::from_text("alpha\n\nβ🙂\n");
        exercise_cursor_ops(&mut multi, 1, &[0, 5, 99]);
        let empty_line_text = multi.line_text(1).unwrap();
        assert_eq!(empty_line_text, "");
        assert_eq!(encode_char_column(&empty_line_text, 1, 99).utf16_col, 0);
        assert_eq!(
            decode_char_column(&empty_line_text, LspPosition::new(1, 99)),
            0
        );

        let unicode_line = "let 🙂na = 1;";
        let unicode_cols: Vec<usize> = (0..=unicode_line.chars().count() + 5).collect();
        exercise_cursor_ops(
            &mut TextBuffer::from_text(&format!("{unicode_line}\n")),
            0,
            &unicode_cols,
        );

        let mut unicode = TextBuffer::from_text(&format!("{unicode_line}\n"));
        unicode.set_cursor(CursorPosition { line: 0, col: 4 });
        let prefix = unicode
            .identifier_prefix_char_range_at(unicode.cursor())
            .expect("prefix on unicode line");
        unicode
            .apply_completion_insertion(None, Some(prefix), "identity")
            .expect("completion on unicode line");

        let mut empty_line_edit = TextBuffer::from_text("\n");
        empty_line_edit
            .apply_lsp_text_edit(&LspTextEdit {
                line_start: 0,
                col_start: 0,
                line_end: 0,
                col_end: 0,
                new_text: "typed".to_owned(),
            })
            .expect("lsp edit on empty line");

        let mut emoji_edit = TextBuffer::from_text("a🙂z\n");
        emoji_edit
            .apply_lsp_text_edit(&LspTextEdit {
                line_start: 0,
                col_start: 3,
                line_end: 0,
                col_end: 4,
                new_text: "X".to_owned(),
            })
            .expect("lsp edit across emoji utf-16 boundary");

        for (line_text, cols) in [
            ("", [0usize, 5, 99].as_slice()),
            ("a🙂z", &[0, 1, 2, 3, 99][..]),
            ("Rust 🙂 code", &[0, 5, 6, 7, 99][..]),
        ] {
            for &col in cols {
                let lsp = encode_char_column(line_text, 0, col);
                let _ = decode_char_column(line_text, lsp);
                let _ = char_column_to_utf16(line_text, col);
                let _ = decode_utf16_column(line_text, lsp.utf16_col);
            }
        }
    }

    #[test]
    fn unicode_identifiers_do_not_panic() {
        fn accept(buffer: &mut TextBuffer, col: usize, insert: &str) {
            let cursor = CursorPosition { line: 0, col };
            buffer.set_cursor(cursor);
            let prefix = buffer
                .identifier_prefix_char_range_at(buffer.cursor())
                .expect("unicode prefix range");
            buffer
                .apply_completion_insertion(None, Some(prefix), insert)
                .expect("unicode completion apply");
        }

        let mut accented = TextBuffer::from_text("let caf\n");
        accept(&mut accented, 7, "café");
        assert_eq!(accented.text(), "let café\n");

        let mut cjk = TextBuffer::from_text("fn 中\n");
        accept(&mut cjk, 4, "中文");
        assert_eq!(cjk.text(), "fn 中文\n");

        let mut cyrillic = TextBuffer::from_text("let перем\n");
        accept(&mut cyrillic, 9, "переменная");
        assert_eq!(cyrillic.text(), "let переменная\n");

        let mut greek = TextBuffer::from_text("foo_β\n");
        accept(&mut greek, 6, "foo_βeta");
        assert_eq!(greek.text(), "foo_βeta\n");

        let mut emoji_boundary = TextBuffer::from_text("id🙂na\n");
        accept(&mut emoji_boundary, 2, "identity");
        assert_eq!(emoji_boundary.text(), "identity🙂na\n");

        let line = "变量.te";
        let mut qualified_cjk = TextBuffer::from_text(&format!("{line}\n"));
        accept(&mut qualified_cjk, line.chars().count(), "test");
        assert_eq!(qualified_cjk.text(), "变量.test\n");

        for ch in "café中文перемβ".chars() {
            assert!(
                is_identifier_prefix_char(ch),
                "test characters must be treated as identifier prefix chars"
            );
        }

        let mut clamped = TextBuffer::from_text("αβγ\n");
        clamped.set_cursor(CursorPosition { line: 0, col: 100 });
        let clamped_col = clamped.cursor().col;
        assert!(clamped
            .identifier_prefix_char_range_at(clamped.cursor())
            .is_some());
        accept(&mut clamped, clamped_col, "αβγδ");
        assert_eq!(clamped.text(), "αβγδ\n");
    }

    #[test]
    fn cursor_lands_after_inserted_text() {
        fn accept_and_assert_cursor(
            buffer: &mut TextBuffer,
            cursor: CursorPosition,
            insert: &str,
            expected: CursorPosition,
        ) {
            buffer.set_cursor(cursor);
            let prefix = buffer
                .identifier_prefix_char_range_at(cursor)
                .expect("prefix");
            let prefix_start = prefix.start;
            buffer
                .apply_completion_insertion(None, Some(prefix), insert)
                .unwrap();
            assert_eq!(
                buffer.cursor(),
                expected,
                "caret should land immediately after `{insert}`"
            );
            assert_ne!(
                buffer.cursor().col,
                prefix_start,
                "caret must not remain at the prefix start"
            );
        }

        let mut replace = TextBuffer::from_text("fn ma() {}\n");
        accept_and_assert_cursor(
            &mut replace,
            CursorPosition { line: 0, col: 5 },
            "main",
            CursorPosition { line: 0, col: 7 },
        );

        let mut insert = TextBuffer::from_text("fn () {}\n");
        accept_and_assert_cursor(
            &mut insert,
            CursorPosition { line: 0, col: 3 },
            "main",
            CursorPosition { line: 0, col: 7 },
        );

        let mut unicode = TextBuffer::from_text("let caf\n");
        accept_and_assert_cursor(
            &mut unicode,
            CursorPosition { line: 0, col: 7 },
            "café",
            CursorPosition { line: 0, col: 8 },
        );

        let mut emoji = TextBuffer::from_text("fn ma() {}\n");
        accept_and_assert_cursor(
            &mut emoji,
            CursorPosition { line: 0, col: 5 },
            "main🙂",
            CursorPosition { line: 0, col: 8 },
        );

        let mut paren = TextBuffer::from_text("main(\n");
        accept_and_assert_cursor(
            &mut paren,
            CursorPosition { line: 0, col: 5 },
            "arg",
            CursorPosition { line: 0, col: 8 },
        );

        let mut text_edit = TextBuffer::from_text("fn ma() {}\n");
        text_edit.set_cursor(CursorPosition { line: 0, col: 5 });
        let prefix = text_edit
            .identifier_prefix_char_range_at(text_edit.cursor())
            .expect("prefix");
        text_edit
            .apply_completion_insertion(
                Some(&LspTextEdit {
                    line_start: 0,
                    col_start: 3,
                    line_end: 0,
                    col_end: 5,
                    new_text: "main".to_owned(),
                }),
                Some(prefix),
                "ignored",
            )
            .unwrap();
        assert_eq!(text_edit.cursor(), CursorPosition { line: 0, col: 7 });
    }

    #[test]
    fn revision_and_modified_state_update_correctly() {
        fn accept_plain(buffer: &mut TextBuffer) {
            buffer.set_cursor(CursorPosition { line: 0, col: 5 });
            let prefix = buffer
                .identifier_prefix_char_range_at(buffer.cursor())
                .expect("prefix");
            buffer
                .apply_completion_insertion(None, Some(prefix), "main")
                .unwrap();
        }

        let mut plain = TextBuffer::from_text("fn ma() {}\n");
        assert!(!plain.is_modified());
        assert!(!plain.is_dirty());
        assert!(!plain.needs_lsp_sync());
        assert_eq!(plain.revision(), 0);
        assert_eq!(plain.lsp_version, 0);

        accept_plain(&mut plain);
        assert!(plain.is_modified());
        assert!(plain.is_dirty());
        assert!(plain.needs_lsp_sync());
        assert_eq!(plain.revision(), 1);
        assert_eq!(plain.lsp_version, 1);

        plain.mark_lsp_synced();
        assert!(!plain.needs_lsp_sync());
        assert!(
            plain.is_modified(),
            "mark_lsp_synced must not clear the unsaved modified flag"
        );

        let revision_before_second = plain.revision();
        let version_before_second = plain.lsp_version;
        plain.set_cursor(CursorPosition { line: 0, col: 7 });
        let prefix = plain
            .identifier_prefix_char_range_at(plain.cursor())
            .expect("second prefix");
        plain
            .apply_completion_insertion(None, Some(prefix), "mainx")
            .unwrap();
        assert_eq!(plain.revision(), revision_before_second + 1);
        assert_eq!(plain.lsp_version, version_before_second + 1);
        assert!(plain.needs_lsp_sync());

        let mut text_edit = TextBuffer::from_text("fn ma() {}\n");
        text_edit.mark_lsp_synced();
        let revision_before = text_edit.revision();
        let version_before = text_edit.lsp_version;
        text_edit.set_cursor(CursorPosition { line: 0, col: 5 });
        let prefix = text_edit
            .identifier_prefix_char_range_at(text_edit.cursor())
            .expect("prefix");
        text_edit
            .apply_completion_insertion(
                Some(&LspTextEdit {
                    line_start: 0,
                    col_start: 3,
                    line_end: 0,
                    col_end: 5,
                    new_text: "main".to_owned(),
                }),
                Some(prefix),
                "ignored",
            )
            .unwrap();
        assert!(text_edit.is_modified());
        assert!(text_edit.is_dirty());
        assert!(text_edit.needs_lsp_sync());
        assert_eq!(text_edit.revision(), revision_before + 1);
        assert_eq!(text_edit.lsp_version, version_before + 1);

        let mut failed = TextBuffer::from_text("fn ma() {}\n");
        let revision_before = failed.revision();
        failed.set_cursor(CursorPosition { line: 0, col: 5 });
        let prefix = failed
            .identifier_prefix_char_range_at(failed.cursor())
            .expect("prefix");
        let err = failed.apply_completion_insertion(
            Some(&LspTextEdit {
                line_start: 99,
                col_start: 0,
                line_end: 99,
                col_end: 0,
                new_text: "broken".to_owned(),
            }),
            Some(prefix),
            "ignored",
        );
        assert!(err.is_err());
        assert_eq!(failed.revision(), revision_before);
        assert!(!failed.is_modified());
        assert!(!failed.needs_lsp_sync());
        assert_eq!(failed.text(), "fn ma() {}\n");
    }

    #[test]
    fn apply_completion_insertion_replaces_identifier_prefix() {
        let mut buffer = TextBuffer::from_text("fn ma() {}\n");
        buffer.set_cursor(CursorPosition { line: 0, col: 5 });
        let prefix = buffer
            .identifier_prefix_char_range_at(buffer.cursor())
            .expect("prefix");
        buffer
            .apply_completion_insertion(None, Some(prefix), "main")
            .unwrap();
        assert_eq!(buffer.text(), "fn main() {}\n");
        assert_eq!(buffer.cursor(), CursorPosition { line: 0, col: 7 });
    }

    #[test]
    fn apply_completion_insertion_inserts_at_cursor_when_prefix_is_empty() {
        let mut buffer = TextBuffer::from_text("fn () {}\n");
        buffer.set_cursor(CursorPosition { line: 0, col: 3 });
        let prefix = buffer
            .identifier_prefix_char_range_at(buffer.cursor())
            .expect("prefix");
        buffer
            .apply_completion_insertion(None, Some(prefix), "main")
            .unwrap();
        assert_eq!(buffer.text(), "fn main() {}\n");
        assert_eq!(buffer.cursor(), CursorPosition { line: 0, col: 7 });
    }

    #[test]
    fn apply_completion_insertion_marks_buffer_for_lsp_sync() {
        let mut buffer = TextBuffer::from_text("fn ma() {}\n");
        buffer.set_cursor(CursorPosition { line: 0, col: 5 });
        buffer.mark_lsp_synced();
        let prefix = buffer
            .identifier_prefix_char_range_at(buffer.cursor())
            .expect("prefix");

        buffer
            .apply_completion_insertion(None, Some(prefix), "main")
            .unwrap();

        assert!(buffer.is_modified());
        assert!(buffer.is_dirty());
        assert!(buffer.needs_lsp_sync());
        assert_eq!(buffer.lsp_version, 1);

        buffer.mark_lsp_synced();
        assert!(!buffer.needs_lsp_sync());
    }

    #[test]
    fn apply_completion_insertion_increments_revision_once_like_normal_edit() {
        let mut buffer = TextBuffer::from_text("fn ma() {}\n");
        buffer.set_cursor(CursorPosition { line: 0, col: 5 });
        let prefix = buffer
            .identifier_prefix_char_range_at(buffer.cursor())
            .expect("prefix");
        let revision_before = buffer.revision();

        buffer
            .apply_completion_insertion(None, Some(prefix), "main")
            .unwrap();

        assert_eq!(buffer.revision(), revision_before + 1);

        let mut typing = TextBuffer::from_text("fn ma() {}\n");
        typing.set_cursor(CursorPosition { line: 0, col: 5 });
        let typing_revision_before = typing.revision();
        typing.insert_at_cursor("x").unwrap();
        assert_eq!(typing.revision(), typing_revision_before + 1);
    }

    #[test]
    fn apply_completion_insertion_prefers_text_edit() {
        let mut buffer = TextBuffer::from_text("fn main() {}\n");
        buffer.set_cursor(CursorPosition { line: 0, col: 8 });
        let prefix = buffer
            .identifier_prefix_char_range_at(buffer.cursor())
            .expect("prefix");
        buffer
            .apply_completion_insertion(
                Some(&LspTextEdit {
                    line_start: 0,
                    col_start: 3,
                    line_end: 0,
                    col_end: 7,
                    new_text: "test".to_owned(),
                }),
                Some(prefix),
                "ignored",
            )
            .unwrap();
        assert_eq!(buffer.text(), "fn test() {}\n");
        assert_eq!(buffer.cursor(), CursorPosition { line: 0, col: 7 });
    }

    #[test]
    fn apply_lsp_text_edit_replaces_utf16_range() {
        let mut buffer = TextBuffer::from_text("fn main() {\n    pr\n}\n");
        let edit = LspTextEdit {
            line_start: 1,
            col_start: 4,
            line_end: 1,
            col_end: 6,
            new_text: "println!()".to_owned(),
        };
        buffer.apply_lsp_text_edit(&edit).unwrap();
        assert!(buffer.text().contains("println!()"));
        assert!(!buffer.text().contains("pr\n"));
    }

    #[test]
    fn apply_lsp_text_edit_inserts_snippet_markers_verbatim() {
        let mut buffer = TextBuffer::from_text("x");
        buffer.set_cursor(CursorPosition { line: 0, col: 1 });
        let snippet_body = "for ${1:i} in ${2:iter} {}";
        buffer
            .apply_lsp_text_edit(&LspTextEdit {
                line_start: 0,
                col_start: 0,
                col_end: 1,
                line_end: 0,
                new_text: snippet_body.to_owned(),
            })
            .unwrap();
        assert_eq!(buffer.text(), snippet_body);
    }

    #[test]
    fn position_lsp_position_uses_utf16_columns() {
        let buffer = TextBuffer::from_text("a🙂z");
        let first = CursorPosition { line: 0, col: 1 };
        assert_eq!(buffer.position_lsp_position(first), LspPosition::new(0, 1));
        let second = CursorPosition { line: 0, col: 2 };
        assert_eq!(buffer.position_lsp_position(second), LspPosition::new(0, 3));
        assert_eq!(
            buffer.cursor_lsp_position(),
            buffer.position_lsp_position(buffer.cursor())
        );
    }

    #[test]
    fn hover_request_position_maps_char_index_to_utf16_coordinates() {
        let buffer = TextBuffer::from_text("fn main() {}\n");
        let char_index = CursorPosition { line: 0, col: 4 };
        assert_eq!(
            buffer.position_lsp_position(char_index),
            LspPosition::new(0, 4),
            "ASCII hover targets should map 1:1 to UTF-16 columns"
        );

        let unicode = TextBuffer::from_text("a🙂z");
        let over_emoji = CursorPosition { line: 0, col: 2 };
        assert_eq!(
            unicode.position_lsp_position(over_emoji),
            LspPosition::new(0, 3),
            "hover UTF-16 column must follow LSP rules for supplementary characters"
        );
    }

    #[test]
    fn revision_and_lsp_version_advance_together_for_hover_snapshots() {
        let mut buffer = TextBuffer::from_text("fn main() {}\n");
        let revision_before = buffer.revision();
        let version_before = buffer.lsp_version;

        buffer.insert_at_cursor("x").unwrap();

        assert_eq!(buffer.revision(), revision_before + 1);
        assert_eq!(buffer.lsp_version, version_before + 1);
        assert!(buffer.needs_lsp_sync());
    }

    #[test]
    fn lsp_position_to_cursor_decodes_utf16_columns() {
        let mut buffer = TextBuffer::from_text("a🙂z\n");
        assert_eq!(
            buffer.lsp_position_to_cursor(LspPosition::new(0, 3)),
            CursorPosition { line: 0, col: 2 }
        );
        buffer.set_cursor(buffer.lsp_position_to_cursor(LspPosition::new(0, 1)));
        assert_eq!(buffer.cursor(), CursorPosition { line: 0, col: 1 });
    }

    #[test]
    fn cursor_lsp_position_uses_utf16_columns() {
        let mut buffer = TextBuffer::from_text("a🙂z");
        buffer.set_cursor(CursorPosition { line: 0, col: 1 });
        assert_eq!(buffer.cursor_lsp_position(), LspPosition::new(0, 1));
        buffer.set_cursor(CursorPosition { line: 0, col: 2 });
        assert_eq!(buffer.cursor_lsp_position(), LspPosition::new(0, 3));
        buffer.set_cursor(CursorPosition { line: 0, col: 3 });
        assert_eq!(buffer.cursor_lsp_position(), LspPosition::new(0, 4));
    }

    #[test]
    fn lsp_sync_state_changes_only_for_real_text_mutations() {
        let mut buffer = TextBuffer::from_text("fn main() {}");
        assert_eq!(buffer.lsp_version, 0);
        assert!(!buffer.needs_lsp_sync());

        buffer.insert_at_cursor("").unwrap();
        assert_eq!(buffer.lsp_version, 0);
        assert!(!buffer.needs_lsp_sync());

        buffer.insert_at_cursor("// ").unwrap();
        assert_eq!(buffer.lsp_version, 1);
        assert!(buffer.needs_lsp_sync());
        assert_eq!(buffer.text(), "// fn main() {}");

        buffer.mark_lsp_synced();
        assert!(!buffer.needs_lsp_sync());
    }

    #[test]
    fn syntax_layout_refresh_does_not_clear_lsp_sync_state() {
        let mut buffer = TextBuffer::from_text("let x = 1;");
        buffer.insert_at_cursor("pub ").unwrap();

        let _ = buffer.get_layout(FontId::monospace(14.0));

        assert!(!buffer.is_dirty());
        assert!(buffer.needs_lsp_sync());
    }

    #[test]
    fn layout_refresh_does_not_clear_the_unsaved_changes_state() {
        let mut buffer = TextBuffer::from_text("fn main() {}\n");
        buffer.insert_at_cursor("// edited\n").unwrap();
        assert!(buffer.is_modified());

        let _ = buffer.get_layout(FontId::monospace(14.0));

        assert!(buffer.is_modified());
    }

    #[test]
    fn failed_load_preserves_existing_document() {
        let mut buffer = TextBuffer::from_text("keep me");
        let before_revision = buffer.revision();
        let missing = std::env::temp_dir().join("blue_ide_definitely_missing_file.rs");
        let _ = fs::remove_file(&missing);

        assert!(buffer.load_from_file(&missing).is_err());
        assert_eq!(buffer.line_text(0).as_deref(), Some("keep me"));
        assert_eq!(buffer.revision(), before_revision);
    }

    #[test]
    fn ten_thousand_lines_are_safe_to_query_and_edit() {
        let text = (0..10_000)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let mut buffer = TextBuffer::from_text(&text);
        assert_eq!(buffer.len_lines(), 10_001);
        buffer.set_cursor(CursorPosition {
            line: 9_999,
            col: 9,
        });
        buffer.insert_at_cursor("!").unwrap();
        assert_eq!(buffer.line_text(9_999).as_deref(), Some("line 9999!"));
    }

    #[test]
    fn identifier_prefix_char_range_scans_backward_on_line() {
        let mut buffer = TextBuffer::from_text("self.foo");
        buffer.set_cursor(CursorPosition { line: 0, col: 7 });
        assert_eq!(
            buffer.identifier_prefix_char_range_at(buffer.cursor()),
            Some(5..7)
        );
    }

    #[test]
    fn identifier_prefix_char_range_stops_at_non_identifier() {
        let mut buffer = TextBuffer::from_text("fn main");
        buffer.set_cursor(CursorPosition { line: 0, col: 3 });
        assert_eq!(
            buffer.identifier_prefix_char_range_at(buffer.cursor()),
            Some(3..3)
        );
    }

    #[test]
    fn identifier_prefix_is_contiguous_and_stops_at_path_separators() {
        let mut buffer = TextBuffer::from_text("foo::bar");
        buffer.set_cursor(CursorPosition {
            line: 0,
            col: "foo::bar".chars().count(),
        });
        assert_eq!(
            buffer.identifier_prefix_char_range_at(buffer.cursor()),
            Some(5..8)
        );
    }

    #[test]
    fn identifier_prefix_stops_before_macro_bang() {
        let mut buffer = TextBuffer::from_text("print!");
        buffer.set_cursor(CursorPosition { line: 0, col: 5 });
        assert_eq!(
            buffer.identifier_prefix_char_range_at(buffer.cursor()),
            Some(0..5)
        );
    }

    #[test]
    fn identifier_prefix_includes_ascii_letters() {
        let mut buffer = TextBuffer::from_text("partial");
        buffer.set_cursor(CursorPosition {
            line: 0,
            col: "partial".chars().count(),
        });
        assert_eq!(
            buffer.identifier_prefix_char_range_at(buffer.cursor()),
            Some(0..7)
        );
    }

    #[test]
    fn identifier_prefix_includes_ascii_digits() {
        let mut buffer = TextBuffer::from_text("foo123");
        buffer.set_cursor(CursorPosition {
            line: 0,
            col: "foo123".chars().count(),
        });
        assert_eq!(
            buffer.identifier_prefix_char_range_at(buffer.cursor()),
            Some(0..6)
        );
    }

    #[test]
    fn identifier_prefix_includes_underscore() {
        let mut buffer = TextBuffer::from_text("foo_bar");
        buffer.set_cursor(CursorPosition {
            line: 0,
            col: "foo_bar".chars().count(),
        });
        assert_eq!(
            buffer.identifier_prefix_char_range_at(buffer.cursor()),
            Some(0..7)
        );
    }

    #[test]
    fn identifier_prefix_includes_unicode_alphanumeric_characters() {
        let text = "café";
        let mut buffer = TextBuffer::from_text(text);
        buffer.set_cursor(CursorPosition {
            line: 0,
            col: text.chars().count(),
        });
        assert_eq!(
            buffer.identifier_prefix_char_range_at(buffer.cursor()),
            Some(0..4)
        );
    }

    #[test]
    fn identifier_prefix_stops_at_non_alphanumeric_boundaries() {
        let mut buffer = TextBuffer::from_text("id🙂name");
        buffer.set_cursor(CursorPosition {
            line: 0,
            col: "id".chars().count(),
        });
        assert_eq!(
            buffer.identifier_prefix_char_range_at(buffer.cursor()),
            Some(0..2)
        );
    }

    #[test]
    fn identifier_prefix_excludes_punctuation_and_whitespace() {
        let mut buffer = TextBuffer::from_text("self.partial");
        buffer.set_cursor(CursorPosition {
            line: 0,
            col: "self.partial".chars().count(),
        });
        assert_eq!(
            buffer.identifier_prefix_char_range_at(buffer.cursor()),
            Some(5..12)
        );

        buffer = TextBuffer::from_text("foo::partial");
        buffer.set_cursor(CursorPosition {
            line: 0,
            col: "foo::partial".chars().count(),
        });
        assert_eq!(
            buffer.identifier_prefix_char_range_at(buffer.cursor()),
            Some(5..12)
        );

        buffer = TextBuffer::from_text("main(");
        buffer.set_cursor(CursorPosition {
            line: 0,
            col: "main(".chars().count(),
        });
        assert_eq!(
            buffer.identifier_prefix_char_range_at(buffer.cursor()),
            Some(5..5)
        );

        buffer = TextBuffer::from_text("let partial");
        buffer.set_cursor(CursorPosition {
            line: 0,
            col: "let partial".chars().count(),
        });
        assert_eq!(
            buffer.identifier_prefix_char_range_at(buffer.cursor()),
            Some(4..11)
        );
    }

    // -----------------------------------------------------------------------
    // Tests for search-facing APIs (byte offsets, range replacement)
    // -----------------------------------------------------------------------

    #[test]
    fn byte_to_char_ascii_is_identity() {
        let buffer = TextBuffer::from_text("hello");
        assert_eq!(buffer.byte_to_char_index(0), Some(0));
        assert_eq!(buffer.byte_to_char_index(3), Some(3));
        assert_eq!(buffer.byte_to_char_index(5), Some(5));
    }

    #[test]
    fn byte_to_char_multibyte_unicode() {
        // "aé🙂" = 'a'(1B) + 'é'(2B) + '🙂'(4B) = 7 bytes
        let buffer = TextBuffer::from_text("aé🙂");
        assert_eq!(buffer.byte_to_char_index(0), Some(0)); // 'a'
        assert_eq!(buffer.byte_to_char_index(1), Some(1)); // start of 'é'
        assert_eq!(buffer.byte_to_char_index(2), None); // inside 'é'
        assert_eq!(buffer.byte_to_char_index(3), Some(2)); // start of '🙂'
        assert_eq!(buffer.byte_to_char_index(4), None); // inside '🙂'
        assert_eq!(buffer.byte_to_char_index(7), Some(3)); // end of string
    }

    #[test]
    fn byte_to_char_cjk() {
        // '中' is 3 bytes
        let buffer = TextBuffer::from_text("中文");
        assert_eq!(buffer.byte_to_char_index(0), Some(0));
        assert_eq!(buffer.byte_to_char_index(1), None); // inside '中'
        assert_eq!(buffer.byte_to_char_index(3), Some(1)); // '文'
        assert_eq!(buffer.byte_to_char_index(6), Some(2)); // past end
    }

    #[test]
    fn byte_to_char_crlf() {
        let buffer = TextBuffer::from_text("one\r\ntwo");
        assert_eq!(buffer.byte_to_char_index(3), Some(3)); // '\r'
        assert_eq!(buffer.byte_to_char_index(4), Some(4)); // '\n'
        assert_eq!(buffer.byte_to_char_index(5), Some(5)); // 't'
    }

    #[test]
    fn replace_char_range_single() {
        let mut buffer = TextBuffer::from_text("hello world");
        let rev_before = buffer.revision();
        buffer.replace_char_range(6, 11, "rust").unwrap();
        assert_eq!(buffer.to_full_string(), "hello rust");
        assert!(buffer.is_modified());
        assert_eq!(buffer.revision(), rev_before + 1);
    }

    #[test]
    fn replace_char_range_unicode() {
        // Replace 'é' (char index 1) with "ee"
        let mut buffer = TextBuffer::from_text("aéb");
        buffer.replace_char_range(1, 2, "ee").unwrap();
        assert_eq!(buffer.to_full_string(), "aeeb");
    }

    #[test]
    fn replace_char_range_invalid_bounds() {
        let mut buffer = TextBuffer::from_text("hello");
        let rev_before = buffer.revision();
        assert!(buffer.replace_char_range(3, 2, "x").is_err());
        assert!(buffer.replace_char_range(0, 99, "x").is_err());
        assert_eq!(buffer.to_full_string(), "hello");
        assert_eq!(buffer.revision(), rev_before);
    }

    #[test]
    fn replace_char_range_places_cursor_after_inserted_text() {
        let mut buffer = TextBuffer::from_text("hello world");
        buffer.set_cursor(CursorPosition { line: 0, col: 0 });
        buffer.replace_char_range(6, 11, "rust").unwrap();
        assert_eq!(buffer.cursor(), CursorPosition { line: 0, col: 10 });
    }

    #[test]
    fn insert_and_delete_adjust_cursor_relative_to_edit_site() {
        let mut buffer = TextBuffer::from_text("hello world");
        buffer.set_cursor(CursorPosition { line: 0, col: 5 });

        buffer.insert_char(2, 'X').unwrap();
        assert_eq!(buffer.cursor(), CursorPosition { line: 0, col: 6 });

        buffer.set_cursor(CursorPosition { line: 0, col: 2 });
        buffer.delete_char(6).unwrap();
        assert_eq!(buffer.cursor(), CursorPosition { line: 0, col: 2 });

        buffer.set_cursor(CursorPosition { line: 0, col: 3 });
        buffer.delete_char(3).unwrap();
        assert_eq!(buffer.cursor(), CursorPosition { line: 0, col: 3 });
    }

    #[test]
    fn apply_byte_replacements_preserves_cursor_when_edits_are_distant() {
        let mut buffer = TextBuffer::from_text("aaa bbb ccc");
        buffer.set_cursor(CursorPosition { line: 0, col: 11 });
        let cursor_before = buffer.cursor();
        let pairs = vec![
            (0..3, "AAA".to_owned()),
            (4..7, "BBB".to_owned()),
            (8..11, "CCC".to_owned()),
        ];
        buffer.apply_byte_replacements(pairs).unwrap();
        assert_eq!(buffer.to_full_string(), "AAA BBB CCC");
        assert_eq!(buffer.cursor(), cursor_before);
    }

    #[test]
    fn replace_byte_range_ascii() {
        let mut buffer = TextBuffer::from_text("foo bar");
        buffer.replace_byte_range(4..7, "baz").unwrap();
        assert_eq!(buffer.to_full_string(), "foo baz");
    }

    #[test]
    fn replace_byte_range_emoji() {
        let text = "hello 🙂 world";
        let emoji_start = text.find('🙂').unwrap();
        let emoji_end = emoji_start + '🙂'.len_utf8();
        let mut buffer = TextBuffer::from_text(text);
        buffer
            .replace_byte_range(emoji_start..emoji_end, ":)")
            .unwrap();
        assert_eq!(buffer.to_full_string(), "hello :) world");
    }

    #[test]
    fn replace_byte_range_interior_boundary_returns_err() {
        let text = "aé"; // 'é' = bytes 1..3
        let mut buffer = TextBuffer::from_text(text);
        assert!(buffer.replace_byte_range(1..2, "x").is_err());
    }

    #[test]
    fn apply_byte_replacements_multiple() {
        let mut buffer = TextBuffer::from_text("aaa bbb ccc");
        let pairs = vec![
            (0..3, "AAA".to_owned()),
            (4..7, "BBB".to_owned()),
            (8..11, "CCC".to_owned()),
        ];
        let rev_before = buffer.revision();
        let count = buffer.apply_byte_replacements(pairs).unwrap();
        assert_eq!(count, 3);
        assert_eq!(buffer.to_full_string(), "AAA BBB CCC");
        assert!(buffer.is_modified());
        // Exactly one revision bump for the whole batch.
        assert_eq!(buffer.revision(), rev_before + 1);
    }

    #[test]
    fn apply_byte_replacements_crlf() {
        let mut buffer = TextBuffer::from_text("one\r\ntwo");
        let pairs = vec![(0..3, "ONE".to_owned()), (5..8, "TWO".to_owned())];
        buffer.apply_byte_replacements(pairs).unwrap();
        assert_eq!(buffer.to_full_string(), "ONE\r\nTWO");
    }

    #[test]
    fn apply_byte_replacements_overlapping_returns_err_and_no_mutation() {
        let mut buffer = TextBuffer::from_text("hello");
        let rev_before = buffer.revision();
        let pairs = vec![(0..3, "A".to_owned()), (2..5, "B".to_owned())];
        assert!(buffer.apply_byte_replacements(pairs).is_err());
        assert_eq!(buffer.to_full_string(), "hello");
        assert_eq!(buffer.revision(), rev_before);
    }

    #[test]
    fn apply_byte_replacements_out_of_bounds_returns_err() {
        let mut buffer = TextBuffer::from_text("hi");
        assert!(buffer
            .apply_byte_replacements(vec![(0..99, "x".to_owned())])
            .is_err());
    }

    #[test]
    fn apply_byte_replacements_interior_utf8_returns_err() {
        let text = "café"; // 'é' at bytes 3..5
        let mut buffer = TextBuffer::from_text(text);
        assert!(buffer
            .apply_byte_replacements(vec![(3..4, "e".to_owned())])
            .is_err());
    }

    #[test]
    fn replace_all_increments_revision_only_once() {
        let mut buffer = TextBuffer::from_text("x x x");
        let rev_before = buffer.revision();
        let pairs = vec![
            (0..1, "y".to_owned()),
            (2..3, "y".to_owned()),
            (4..5, "y".to_owned()),
        ];
        buffer.apply_byte_replacements(pairs).unwrap();
        assert_eq!(buffer.revision(), rev_before + 1);
        assert_eq!(buffer.to_full_string(), "y y y");
    }

    #[test]
    fn set_cursor_to_byte_positions_correctly() {
        let mut buffer = TextBuffer::from_text("hello\nworld");
        buffer.set_cursor_to_byte(6).unwrap(); // 'w'
        assert_eq!(buffer.cursor(), CursorPosition { line: 1, col: 0 });
    }

    #[test]
    fn set_cursor_to_byte_interior_utf8_returns_err() {
        let mut buffer = TextBuffer::from_text("aé");
        // byte 2 is inside 'é'
        assert!(buffer.set_cursor_to_byte(2).is_err());
        // cursor unchanged
        assert_eq!(buffer.cursor(), CursorPosition::default());
    }

    #[test]
    fn set_cursor_to_byte_rejects_crlf_interior_byte() {
        let mut buffer = TextBuffer::from_text("one\r\ntwo");
        assert!(buffer.set_cursor_to_byte(4).is_err());
        assert_eq!(buffer.cursor(), CursorPosition::default());
    }

    #[test]
    fn multi_cursor_insert_is_reverse_order_and_undo_restores_all_cursors() {
        let mut buffer = TextBuffer::from_text("one\ntwo");
        buffer.add_cursor(1, 3);
        let before = buffer.cursors.clone();
        buffer.insert_at_cursors("!").unwrap();
        assert_eq!(buffer.text(), "!one\ntwo!");
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "one\ntwo");
        assert_eq!(buffer.cursors, before);
        assert!(buffer.redo());
        assert_eq!(buffer.text(), "!one\ntwo!");
    }

    #[test]
    fn smart_home_toggles_indentation_for_each_cursor() {
        let mut buffer = TextBuffer::from_text("    one\n  two");
        buffer.set_cursor(CursorPosition { line: 0, col: 7 });
        buffer.add_cursor(1, 2);
        buffer.smart_home(false);
        assert_eq!(buffer.cursors[0].head.col, 4);
        assert_eq!(buffer.cursors[1].head.col, 0);

        let mut whitespace = TextBuffer::from_text("    ");
        whitespace.set_cursor(CursorPosition { line: 0, col: 2 });
        whitespace.smart_home(false);
        assert_eq!(whitespace.cursor().col, 0);
    }

    #[test]
    fn case_transforms_split_camel_and_separator_boundaries() {
        let mut buffer = TextBuffer::from_text("myFunctionName");
        buffer.cursors[0] = CursorAnchor {
            anchor: CursorPosition { line: 0, col: 0 },
            head: CursorPosition { line: 0, col: 14 },
            col_affinity: 14,
        };
        buffer.transform_selections(CaseTransform::Snake).unwrap();
        assert_eq!(buffer.text(), "my_function_name");
    }

    #[test]
    fn column_selection_generates_real_clamped_cursors() {
        let mut buffer = TextBuffer::from_text("abcdef\nxy\n12345");
        buffer.set_column_selection(
            CursorPosition { line: 0, col: 2 },
            CursorPosition { line: 2, col: 5 },
        );
        assert_eq!(buffer.cursors.len(), 3);
        assert_eq!(buffer.cursors[1].normalize().0.col, 2);
        assert_eq!(buffer.cursors[1].normalize().1.col, 2);
        buffer.finish_column_selection();
        assert!(buffer.column_selection.is_none());
        assert_eq!(buffer.cursors.len(), 3);
    }

    #[test]
    fn bracket_matching_respects_type_and_colorization_marks_mismatch() {
        let mut buffer = TextBuffer::from_text("{[x]}");
        buffer.set_cursor(CursorPosition { line: 0, col: 0 });
        buffer.update_bracket_match();
        assert_eq!(buffer.bracket_match, Some((0, 0, 0, 4)));
        let _ = buffer.get_layout(FontId::monospace(14.0));
        assert_eq!(buffer.bracket_colors.len(), 4);

        let mut mismatch = TextBuffer::from_text("{]");
        let _ = mismatch.get_layout(FontId::monospace(14.0));
        assert!(mismatch
            .bracket_colors
            .iter()
            .all(|bracket| bracket.color == egui::Color32::from_rgb(255, 70, 70)));
    }

    #[test]
    fn join_lines_applies_to_independent_multi_cursor_groups() {
        let mut buffer = TextBuffer::from_text("a\n  b\nc\n  d\nend");
        buffer.set_cursor(CursorPosition { line: 0, col: 1 });
        buffer.add_cursor(2, 1);
        assert!(buffer.join_lines().unwrap());
        assert_eq!(buffer.text(), "a b\nc d\nend");
        assert_eq!(buffer.cursors.len(), 2);
    }

    #[test]
    fn duplicate_line_deduplicates_cursors_sharing_the_same_line() {
        let mut buffer = TextBuffer::from_text("alpha\nbeta\n");
        buffer.set_cursor(CursorPosition { line: 0, col: 1 });
        buffer.add_cursor(0, 4);
        buffer.duplicate_selection_or_line().unwrap();
        assert_eq!(buffer.text(), "alpha\nalpha\nbeta\n");
        assert_eq!(buffer.cursors.len(), 1);
        assert_eq!(buffer.cursor(), CursorPosition { line: 1, col: 1 });
    }

    #[test]
    fn move_lines_groups_adjacent_cursors_and_preserves_trailing_newline() {
        let mut buffer = TextBuffer::from_text("zero\none\ntwo\nthree\n");
        buffer.set_cursor(CursorPosition { line: 1, col: 0 });
        buffer.add_cursor(2, 0);
        assert!(buffer.move_selected_lines(false).unwrap());
        assert_eq!(buffer.text(), "one\ntwo\nzero\nthree\n");
        assert_eq!(
            buffer
                .cursors
                .iter()
                .map(|cursor| cursor.head.line)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn undo_coalesces_a_typing_burst_and_restores_cursor_snapshot() {
        let mut buffer = TextBuffer::from_text("");
        buffer.insert_at_cursor("a").unwrap();
        buffer.insert_at_cursor("b").unwrap();
        buffer.insert_at_cursor("c").unwrap();
        assert_eq!(buffer.undo_stack.past.len(), 1);
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "");
        assert_eq!(buffer.cursor(), CursorPosition::default());
    }

    #[test]
    fn bracket_layout_override_runs_after_semantic_tokens() {
        let mut buffer = TextBuffer::from_text("(x)");
        buffer.semantic_tokens = vec![
            crate::lsp::types::SemanticToken {
                line: 0,
                col: 0,
                length: 1,
                token_type: "variable".into(),
                modifiers: crate::lsp::types::SemanticModifiers::default(),
                color: egui::Color32::RED,
                italic: false,
                underline: false,
            },
            crate::lsp::types::SemanticToken {
                line: 0,
                col: 1,
                length: 1,
                token_type: "variable".into(),
                modifiers: crate::lsp::types::SemanticModifiers::default(),
                color: egui::Color32::RED,
                italic: false,
                underline: false,
            },
        ];
        buffer.invalidate_layout();
        let layout = buffer.get_layout(FontId::monospace(14.0));
        let bracket = layout
            .sections
            .iter()
            .find(|section| section.byte_range == (0..1))
            .unwrap();
        let identifier = layout
            .sections
            .iter()
            .find(|section| section.byte_range == (1..2))
            .unwrap();
        assert_eq!(bracket.format.color, egui::Color32::from_rgb(255, 215, 0));
        assert_eq!(identifier.format.color, egui::Color32::RED);
    }
}
