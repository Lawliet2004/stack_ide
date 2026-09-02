//! Vim modal editing — Zed-style embedded vim emulator.
//!
//! Implements Normal / Insert / Visual / Visual-Line modes with counts,
//! motions, operators, text objects, the unnamed register, `/` search with
//! `n`/`N`, and a `:` command line (`:w`, `:q`, `:wq`, `:x`, `:q!`, `:noh`).
//! Enabled app-wide via `editor.vim_mode` (default off, Ctrl+Alt+V); each
//! [`TextBuffer`](crate::editor::buffer::TextBuffer) owns its [`VimState`] so
//! the mode survives tab switches.
//!
//! The emulator is a pure state machine over the buffer: the editor widget
//! feeds it [`VimInput`] values and applies the returned [`VimResult`].
//! Unit tests drive it through real buffers (no egui required).

use crate::editor::buffer::{CursorAnchor, CursorPosition, TextBuffer};

/// Editor options vim needs for indenting and joins.
#[derive(Debug, Clone, Copy)]
pub struct VimOptions {
    pub tab_width: usize,
    pub insert_spaces: bool,
}

impl Default for VimOptions {
    fn default() -> Self {
        Self {
            tab_width: 4,
            insert_spaces: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimMode {
    Normal,
    Insert,
    /// Characterwise visual selection.
    Visual,
    /// Linewise visual selection.
    VisualLine,
    /// `:` command line.
    Command,
    /// `/` search prompt.
    Search,
}

impl VimMode {
    /// Status-bar label.
    pub const fn label(self) -> &'static str {
        match self {
            VimMode::Normal => "NORMAL",
            VimMode::Insert => "INSERT",
            VimMode::Visual => "VISUAL",
            VimMode::VisualLine => "V-LINE",
            VimMode::Command => "COMMAND",
            VimMode::Search => "SEARCH",
        }
    }
}

/// Commands typed on the `:` line that the widget cannot perform itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExCommand {
    /// `:w` — save the buffer.
    Write,
    /// `:q` / `:q!` — close the tab (app decides about unsaved prompts).
    Quit,
    /// `:wq` / `:x` — save then close.
    WriteQuit,
    /// `:noh[lsearch]` — clear search highlights.
    NoHighlight,
}

/// Keys that arrive as `egui::Event::Key` (non-printable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedKey {
    Escape,
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    /// Ctrl+R — redo.
    CtrlR,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimInput {
    /// A printable character (`egui::Event::Text`).
    Char(char),
    /// A non-printable key (`egui::Event::Key`).
    Key(NamedKey),
}

/// Two-key/awaiting states while in Normal mode or operator-pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    /// `d`/`c`/`y`/`>`/`<` awaiting a motion, count, doubling, or `i`/`a` object.
    Operator(PendingOperator),
    /// `f`/`F`/`t`/`T` awaiting its target char (`op` set → operator motion).
    Find {
        op: Option<PendingOperator>,
        kind: char,
    },
    /// `i`/`a` object prefix awaiting the object char.
    Object {
        op: PendingOperator,
        around: bool,
    },
    /// `r` awaiting the replacement char.
    Replace,
    /// `g` pressed: `gg`, `gu`, `gU`.
    G,
    /// `z` pressed (viewport ops; accepted as no-ops here).
    Z,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingOperator {
    Delete,
    Change,
    Yank,
    Indent,
    Outdent,
    Lowercase,
    Uppercase,
}

impl PendingOperator {
    fn doubling_char(self) -> char {
        match self {
            PendingOperator::Delete => 'd',
            PendingOperator::Change => 'c',
            PendingOperator::Yank => 'y',
            PendingOperator::Indent => '>',
            PendingOperator::Outdent => '<',
            PendingOperator::Lowercase => 'u',
            PendingOperator::Uppercase => 'U',
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct VimState {
    pub mode: VimMode,
    /// Unnamed register (yank/delete payload).
    register: String,
    /// True when the register holds whole lines.
    register_linewise: bool,
    /// Digits accumulated for the next operator/motion.
    count: String,
    /// Awaited second/third key of a multi-key command.
    pending: Option<Pending>,
    /// Last `f`/`t` target for `;` and `,` — (char, kind).
    last_find: Option<(char, char)>,
    /// Last `/` pattern (mirrored into the app's search for highlights).
    pub last_search: Option<String>,
    /// Current `:` or `/` line content (prompt char included).
    cmdline: String,
}

impl VimState {
    pub fn register(&self) -> &str {
        &self.register
    }

    /// Current `:`/`/` line, including the prompt character.
    pub fn cmdline(&self) -> &str {
        &self.cmdline
    }

    /// Feed one input; returns the outcome. In Insert mode everything except
    /// Escape flows through to the ordinary editor handler (`consumed=false`).
    pub fn process(
        &mut self,
        buffer: &mut TextBuffer,
        input: VimInput,
        options: VimOptions,
    ) -> VimResult {
        let mut result = VimResult::default();
        match self.mode {
            VimMode::Insert => self.process_insert(input, &mut result),
            VimMode::Normal => self.process_normal(buffer, input, options, &mut result),
            VimMode::Visual | VimMode::VisualLine => {
                self.process_visual(buffer, input, options, &mut result)
            }
            VimMode::Command | VimMode::Search => {
                self.process_cmdline(buffer, input, &mut result)
            }
        }
        result
    }

    // ─── Insert mode ────────────────────────────────────────────────────────

    fn process_insert(&mut self, input: VimInput, result: &mut VimResult) {
        match input {
            VimInput::Key(NamedKey::Escape) => {
                self.mode = VimMode::Normal;
                result.consumed = true;
            }
            _ => result.consumed = false,
        }
    }

    // ─── Command line (`:` / `/`) ───────────────────────────────────────────

    fn process_cmdline(
        &mut self,
        buffer: &mut TextBuffer,
        input: VimInput,
        result: &mut VimResult,
    ) {
        match input {
            VimInput::Key(NamedKey::Escape) => {
                self.cmdline.clear();
                self.mode = VimMode::Normal;
            }
            VimInput::Key(NamedKey::Enter) => {
                let prompt = self.cmdline.chars().next().unwrap_or(':');
                let line = self.cmdline[1..].to_owned();
                self.cmdline.clear();
                self.mode = VimMode::Normal;
                if prompt == ':' {
                    self.execute_ex(&line, result);
                } else {
                    self.execute_search(buffer, &line, result);
                }
            }
            VimInput::Key(NamedKey::Backspace) => {
                self.cmdline.pop();
                if self.cmdline.is_empty() {
                    self.mode = VimMode::Normal;
                }
            }
            VimInput::Char(c) => self.cmdline.push(c),
            VimInput::Key(_) => {}
        }
        result.consumed = true;
    }

    fn execute_ex(&mut self, line: &str, result: &mut VimResult) {
        match line.trim() {
            "w" | "write" => result.ex = Some(ExCommand::Write),
            "q" | "quit" | "q!" | "quit!" => result.ex = Some(ExCommand::Quit),
            "wq" | "wq!" | "x" | "exit" => result.ex = Some(ExCommand::WriteQuit),
            "noh" | "nohl" | "nohlsearch" | "nohls" => {
                result.ex = Some(ExCommand::NoHighlight)
            }
            _ => {}
        }
    }

    fn execute_search(&mut self, buffer: &mut TextBuffer, line: &str, result: &mut VimResult) {
        let pattern = line.trim().to_owned();
        if pattern.is_empty() {
            if let Some(last) = self.last_search.clone() {
                self.jump_to_next_match(buffer, &last, true);
            }
            return;
        }
        self.last_search = Some(pattern.clone());
        result.search = Some(pattern.clone());
        self.jump_to_next_match(buffer, &pattern, true);
    }

    // ─── Normal mode ────────────────────────────────────────────────────────

    fn process_normal(
        &mut self,
        buffer: &mut TextBuffer,
        input: VimInput,
        options: VimOptions,
        result: &mut VimResult,
    ) {
        if let VimInput::Key(key) = input {
            self.process_named_key(buffer, key, result);
            return;
        }
        let ch = match input {
            VimInput::Char(c) => c,
            VimInput::Key(_) => unreachable!(),
        };

        if let Some(pending) = self.pending {
            self.process_pending(buffer, pending, ch, options);
            return;
        }

        // Count accumulation: 1-9 starts a count, 0-9 continues; bare 0 is a
        // motion.
        if ch.is_ascii_digit() && (ch != '0' || !self.count.is_empty()) {
            self.count.push(ch);
            return;
        }

        let count = self.take_count();

        match ch {
            'i' => self.enter_insert(buffer, InsertAt::BeforeCursor),
            'I' => self.enter_insert(buffer, InsertAt::FirstNonBlank),
            'a' => self.enter_insert(buffer, InsertAt::AfterCursor),
            'A' => self.enter_insert(buffer, InsertAt::EndOfLine),
            'o' => self.open_line(buffer, true, options),
            'O' => self.open_line(buffer, false, options),
            'v' => self.enter_visual(buffer, false),
            'V' => self.enter_visual(buffer, true),
            ':' => {
                self.mode = VimMode::Command;
                self.cmdline = ":".to_owned();
            }
            '/' | '?' => {
                self.mode = VimMode::Search;
                self.cmdline = "/".to_owned();
            }
            'h' => self.move_horizontal(buffer, -(count.max(1) as isize)),
            'l' => self.move_horizontal(buffer, count.max(1) as isize),
            'j' => self.move_vertical(buffer, count.max(1) as isize),
            'k' => self.move_vertical(buffer, -(count.max(1) as isize)),
            '0' => self.set_col(buffer, 0),
            '^' => self.first_non_blank(buffer),
            '$' => self.end_of_line(buffer),
            'g' => self.pending = Some(Pending::G),
            'G' => self.goto_line(buffer, if count > 0 { Some(count) } else { None }),
            'w' => self.word_forward(buffer, count, false),
            'W' => self.word_forward(buffer, count, true),
            'b' => self.word_back(buffer, count, false),
            'B' => self.word_back(buffer, count, true),
            'e' => self.word_end(buffer, count, false),
            'E' => self.word_end(buffer, count, true),
            '{' => self.paragraph_move(buffer, -(count.max(1) as isize)),
            '}' => self.paragraph_move(buffer, count.max(1) as isize),
            'n' => {
                if let Some(pattern) = self.last_search.clone() {
                    self.jump_to_next_match(buffer, &pattern, true);
                }
            }
            'N' => {
                if let Some(pattern) = self.last_search.clone() {
                    self.jump_to_next_match(buffer, &pattern, false);
                }
            }
            'f' | 'F' | 't' | 'T' => {
                self.pending = Some(Pending::Find { op: None, kind: ch })
            }
            ';' => {
                if let Some((target, kind)) = self.last_find {
                    self.do_find(buffer, target, kind, count);
                }
            }
            ',' => {
                if let Some((target, kind)) = self.last_find {
                    let inverted = match kind {
                        'f' => 'F',
                        'F' => 'f',
                        't' => 'T',
                        _ => 't',
                    };
                    self.do_find(buffer, target, inverted, count);
                }
            }
            '%' => self.match_bracket(buffer),
            'x' => self.delete_chars(buffer, count, false),
            'X' => self.delete_chars(buffer, count, true),
            'd' => self.pending = Some(Pending::Operator(PendingOperator::Delete)),
            'c' => self.pending = Some(Pending::Operator(PendingOperator::Change)),
            'y' => self.pending = Some(Pending::Operator(PendingOperator::Yank)),
            '>' => self.pending = Some(Pending::Operator(PendingOperator::Indent)),
            '<' => self.pending = Some(Pending::Operator(PendingOperator::Outdent)),
            'D' => self.change_to_eol(buffer, count, false),
            'C' => self.change_to_eol(buffer, count, true),
            's' => {
                self.delete_chars(buffer, count.max(1), false);
                self.enter_insert(buffer, InsertAt::BeforeCursor);
            }
            'S' => {
                let line = buffer.cursor().line;
                self.yank_lines(buffer, line, line + count.max(1) - 1);
                self.change_lines(buffer, line, line + count.max(1) - 1);
            }
            'p' => self.put(buffer, count, true),
            'P' => self.put(buffer, count, false),
            'r' => self.pending = Some(Pending::Replace),
            'J' => self.join_below(buffer, count),
            '~' => self.toggle_case_char(buffer, count),
            'u' => {
                buffer.undo();
                self.clamp_cursor(buffer);
            }
            'z' => self.pending = Some(Pending::Z),
            _ => {}
        }
    }

    fn process_pending(
        &mut self,
        buffer: &mut TextBuffer,
        pending: Pending,
        ch: char,
        options: VimOptions,
    ) {
        // Counts keep accumulating while pending.
        if ch.is_ascii_digit() && (ch != '0' || !self.count.is_empty()) {
            match pending {
                Pending::Operator(_) | Pending::Find { op: Some(_), .. } => {
                    self.count.push(ch);
                    return;
                }
                _ => {}
            }
        }

        match pending {
            Pending::G => match ch {
                'g' => {
                    let count = self.take_count();
                    self.goto_line(buffer, if count > 0 { Some(count) } else { None });
                    self.pending = None;
                }
                'u' => {
                    self.pending =
                        Some(Pending::Operator(PendingOperator::Lowercase));
                }
                'U' => {
                    self.pending =
                        Some(Pending::Operator(PendingOperator::Uppercase));
                }
                _ => self.cancel_pending(),
            },
            Pending::Z => {
                // zz / zt / zb — viewport centering is applied by the widget's
                // scroll logic; accept and clear.
                self.pending = None;
            }
            Pending::Replace => {
                let count = self.take_count();
                self.replace_pending(buffer, ch, count);
                self.pending = None;
            }
            Pending::Find { op, kind } => {
                let count = self.take_count();
                match op {
                    Some(op) => {
                        // Operator + find motion (e.g. `df,`): anchor first.
                        let anchor = buffer.cursor();
                        if self.do_find(buffer, ch, kind, count) {
                            let to = buffer.cursor();
                            self.pending = None;
                            self.finish_motion_operator(buffer, op, anchor, to, true, options);
                        } else {
                            self.pending = None;
                        }
                    }
                    None => {
                        self.do_find(buffer, ch, kind, count);
                        self.pending = None;
                    }
                }
            }
            Pending::Operator(op) => {
                if ch == op.doubling_char() || (op == PendingOperator::Indent && ch == '>')
                    || (op == PendingOperator::Outdent && ch == '<')
                {
                    let count = self.take_count();
                    let line = buffer.cursor().line;
                    let last = (line + count.max(1) - 1).min(buffer.len_lines().saturating_sub(1));
                    self.pending = None;
                    self.finish_linewise_operator(buffer, op, line, last, options);
                    return;
                }
                match ch {
                    'i' => self.pending = Some(Pending::Object { op, around: false }),
                    'a' => self.pending = Some(Pending::Object { op, around: true }),
                    'f' | 'F' | 't' | 'T' => {
                        self.pending = Some(Pending::Find { op: Some(op), kind: ch })
                    }
                    'g' => {
                        // gg as a motion.
                        self.pending = Some(Pending::G);
                    }
                    _ => {
                        self.process_pending_motion(buffer, pending, op, ch, options);
                    }
                }
            }
            Pending::Object { op, around } => {
                let count = self.take_count();
                if let Some(mut object) = text_object(buffer, &buffer.cursor(), ch, around) {
                    for _ in 1..count.max(1) {
                        match expand_word_object(buffer, object, around) {
                            Some(next) => object = next,
                            None => break,
                        }
                    }
                    self.pending = None;
                    self.finish_range_operator(buffer, op, object, options);
                } else {
                    self.cancel_pending();
                }
            }
        }
    }

    fn pending_anchor(&self, buffer: &TextBuffer) -> CursorPosition {
        buffer.cursor()
    }

    fn process_pending_motion(
        &mut self,
        buffer: &mut TextBuffer,
        pending: Pending,
        op: PendingOperator,
        ch: char,
        options: VimOptions,
    ) {
        let _ = pending;
        let count = self.take_count().max(1);
        let cursor = buffer.cursor();
        match ch {
            'h' => {
                let target = CursorPosition {
                    line: cursor.line,
                    col: cursor.col.saturating_sub(count),
                };
                self.pending = None;
                self.finish_motion_operator(buffer, op, cursor, target, false, options);
            }
            'l' => {
                let line_len = buffer.line_content_len(cursor.line).unwrap_or(0);
                let target = CursorPosition {
                    line: cursor.line,
                    col: (cursor.col + count).min(line_len.saturating_sub(1)),
                };
                self.pending = None;
                self.finish_motion_operator(buffer, op, cursor, target, true, options);
            }
            'j' | 'k' => {
                let line = if ch == 'j' {
                    (cursor.line + count).min(buffer.len_lines().saturating_sub(1))
                } else {
                    cursor.line.saturating_sub(count)
                };
                self.pending = None;
                self.finish_linewise_operator(buffer, op, cursor.line.min(line), line.max(cursor.line), options);
            }
            '0' => {
                let target = CursorPosition { line: cursor.line, col: 0 };
                self.pending = None;
                self.finish_motion_operator(buffer, op, cursor, target, false, options);
            }
            '^' => {
                let target = CursorPosition {
                    line: cursor.line,
                    col: first_non_blank_col(buffer, cursor.line),
                };
                self.pending = None;
                self.finish_motion_operator(buffer, op, cursor, target, false, options);
            }
            '$' => {
                let len = buffer.line_content_len(cursor.line).unwrap_or(0);
                let target = CursorPosition {
                    line: cursor.line,
                    col: len.saturating_sub(1),
                };
                self.pending = None;
                self.finish_motion_operator(buffer, op, cursor, target, true, options);
            }
            'w' | 'W' => {
                let big = ch == 'W';
                let target = motion_repeat(buffer, &cursor, count, big);
                self.pending = None;
                self.finish_motion_operator(buffer, op, cursor, target, false, options);
            }
            'e' | 'E' => {
                let big = ch == 'E';
                let target = motion_repeat_end(buffer, &cursor, count, big);
                self.pending = None;
                self.finish_motion_operator(buffer, op, cursor, target, true, options);
            }
            'b' | 'B' => {
                let big = ch == 'B';
                let target = motion_back_repeat(buffer, &cursor, count, big);
                self.pending = None;
                self.finish_motion_operator(buffer, op, cursor, target, false, options);
            }
            'G' => {
                let target_line = buffer.len_lines().saturating_sub(1);
                self.pending = None;
                self.finish_linewise_operator(buffer, op, cursor.line.min(target_line), target_line, options);
            }
            _ => self.cancel_pending(),
        }
    }

    fn cancel_pending(&mut self) {
        self.pending = None;
        self.count.clear();
    }

    fn process_named_key(
        &mut self,
        buffer: &mut TextBuffer,
        key: NamedKey,
        result: &mut VimResult,
    ) {
        result.consumed = true;
        match key {
            NamedKey::Escape => self.cancel_pending(),
            NamedKey::Left => self.move_horizontal(buffer, -1),
            NamedKey::Right => self.move_horizontal(buffer, 1),
            NamedKey::Up => self.move_vertical(buffer, -1),
            NamedKey::Down => self.move_vertical(buffer, 1),
            NamedKey::Home => self.set_col(buffer, 0),
            NamedKey::End => self.end_of_line(buffer),
            NamedKey::PageUp => self.move_vertical(buffer, -15),
            NamedKey::PageDown => self.move_vertical(buffer, 15),
            NamedKey::Backspace => self.move_horizontal(buffer, -1),
            NamedKey::Delete => {
                let count = self.take_count();
                self.delete_chars(buffer, count, false);
            }
            NamedKey::CtrlR => {
                buffer.redo();
                self.clamp_cursor(buffer);
            }
        }
    }

    // ─── Visual mode ────────────────────────────────────────────────────────

    fn process_visual(
        &mut self,
        buffer: &mut TextBuffer,
        input: VimInput,
        options: VimOptions,
        result: &mut VimResult,
    ) {
        result.consumed = true;
        let linewise = self.mode == VimMode::VisualLine;
        let ch = match input {
            VimInput::Char(c) => c,
            VimInput::Key(key) => {
                match key {
                    NamedKey::Escape => self.exit_visual(buffer),
                    NamedKey::Left => self.visual_move(buffer, -1, 0),
                    NamedKey::Right => self.visual_move(buffer, 1, 0),
                    NamedKey::Up => self.visual_move(buffer, 0, -1),
                    NamedKey::Down => self.visual_move(buffer, 0, 1),
                    NamedKey::Home => self.visual_move_to_col(buffer, 0),
                    NamedKey::End => {
                        let line = buffer.primary_cursor().head.line;
                        let len = buffer.line_content_len(line).unwrap_or(0);
                        self.visual_move_to_col(buffer, len.saturating_sub(1));
                    }
                    NamedKey::Backspace => self.visual_move(buffer, -1, 0),
                    _ => {}
                }
                return;
            }
        };

        if ch.is_ascii_digit() && (ch != '0' || !self.count.is_empty()) {
            self.count.push(ch);
            return;
        }
        let count = self.take_count();

        match ch {
            'v' if !linewise => self.exit_visual(buffer),
            'V' => {
                if linewise {
                    self.exit_visual(buffer);
                } else {
                    self.mode = VimMode::VisualLine;
                }
            }
            'h' => self.visual_move(buffer, -(count.max(1) as isize), 0),
            'l' => self.visual_move(buffer, count.max(1) as isize, 0),
            'j' => self.visual_move(buffer, 0, count.max(1) as isize),
            'k' => self.visual_move(buffer, 0, -(count.max(1) as isize)),
            '0' => self.visual_move_to_col(buffer, 0),
            '^' => {
                let head = buffer.primary_cursor().head;
                let col = first_non_blank_col(buffer, head.line);
                self.visual_move_to_col(buffer, col);
            }
            '$' => {
                let head = buffer.primary_cursor().head;
                let len = buffer.line_content_len(head.line).unwrap_or(0);
                self.visual_move_to_col(buffer, len.saturating_sub(1));
            }
            'w' => {
                for _ in 0..count.max(1) {
                    let head = buffer.primary_cursor().head;
                    let next = next_word_position(buffer, &head, false);
                    if next == head {
                        break;
                    }
                    self.visual_set_head(buffer, next);
                }
            }
            'b' => {
                for _ in 0..count.max(1) {
                    let head = buffer.primary_cursor().head;
                    let prev = prev_word_position(buffer, &head, false);
                    if prev == head {
                        break;
                    }
                    self.visual_set_head(buffer, prev);
                }
            }
            'e' => {
                for _ in 0..count.max(1) {
                    let head = buffer.primary_cursor().head;
                    let end = word_end_position(buffer, &head, false);
                    if end == head {
                        break;
                    }
                    self.visual_set_head(buffer, end);
                }
            }
            'G' => {
                let last = buffer.len_lines().saturating_sub(1);
                let line = if count > 0 { count - 1 } else { last };
                let col = first_non_blank_col(buffer, line);
                self.visual_set_head(buffer, CursorPosition { line, col });
            }
            'o' => self.swap_visual_ends(buffer),
            'd' | 'x' | 'X' => {
                if linewise {
                    let (first, last) = self.visual_line_span(buffer);
                    let (text, _) = self.delete_lines(buffer, first, last);
                    self.register = text;
                    self.register_linewise = true;
                    self.mode = VimMode::Normal;
                    let line = first.min(buffer.len_lines().saturating_sub(1));
                    self.set_cursor(
                        buffer,
                        CursorPosition {
                            line,
                            col: first_non_blank_col(buffer, line),
                        },
                    );
                } else {
                    let (start, _) = self.visual_char_range(buffer);
                    let range = self.visual_char_range(buffer);
                    let (text, _) = self.delete_range(buffer, range);
                    self.register = text;
                    self.register_linewise = false;
                    self.mode = VimMode::Normal;
                    self.set_cursor(buffer, start);
                }
            }
            'y' => {
                if linewise {
                    let (first, last) = self.visual_line_span(buffer);
                    self.yank_lines(buffer, first, last);
                    self.mode = VimMode::Normal;
                    self.set_cursor(buffer, CursorPosition { line: first, col: 0 });
                } else {
                    let (start, _) = self.visual_char_range(buffer);
                    let range = self.visual_char_range(buffer);
                    let (text, _) = self.extract_range(buffer, range);
                    self.register = text;
                    self.register_linewise = false;
                    self.mode = VimMode::Normal;
                    self.set_cursor(buffer, start);
                }
            }
            'c' | 's' => {
                if linewise {
                    let (first, last) = self.visual_line_span(buffer);
                    self.yank_lines(buffer, first, last);
                    self.change_lines(buffer, first, last);
                } else {
                    let range = self.visual_char_range(buffer);
                    let (text, _) = self.delete_range(buffer, range);
                    self.register = text;
                    self.register_linewise = false;
                    self.mode = VimMode::Insert;
                }
            }
            'p' => {
                // Swap the selection with the register contents.
                if linewise {
                    let (first, last) = self.visual_line_span(buffer);
                    let (deleted, _) = self.delete_lines(buffer, first, last);
                    let incoming = (self.register.clone(), self.register_linewise);
                    self.register = deleted;
                    self.register_linewise = true;
                    self.put(buffer, 1, false);
                    self.register = incoming.0;
                    self.register_linewise = incoming.1;
                } else {
                    let range = self.visual_char_range(buffer);
                    let (deleted, _) = self.delete_range(buffer, range);
                    let incoming = (self.register.clone(), self.register_linewise);
                    self.register = deleted;
                    self.register_linewise = false;
                    self.put(buffer, 1, true);
                    self.register = incoming.0;
                    self.register_linewise = incoming.1;
                }
                self.mode = VimMode::Normal;
                self.clamp_cursor(buffer);
            }
            '~' => self.transform_visual(buffer, CaseOp::Toggle),
            'u' => self.transform_visual(buffer, CaseOp::Lower),
            'U' => self.transform_visual(buffer, CaseOp::Upper),
            'J' => self.join_below(buffer, count),
            '>' => {
                let (first, last) = self.visual_line_span(buffer);
                self.indent_lines(buffer, first, last, true, options);
                self.mode = VimMode::Normal;
                self.set_cursor(
                    buffer,
                    CursorPosition {
                        line: first,
                        col: 0,
                    },
                );
            }
            '<' => {
                let (first, last) = self.visual_line_span(buffer);
                self.indent_lines(buffer, first, last, false, options);
                self.mode = VimMode::Normal;
                self.set_cursor(
                    buffer,
                    CursorPosition {
                        line: first,
                        col: 0,
                    },
                );
            }
            ':' => {
                self.mode = VimMode::Command;
                self.cmdline = ":".to_owned();
            }
            _ => {}
        }
    }

    // ─── Pending helpers shared with normal mode ────────────────────────────

    fn take_count(&mut self) -> usize {
        let value: usize = if self.count.is_empty() {
            0
        } else {
            self.count.parse().unwrap_or(1)
        };
        self.count.clear();
        value
    }

    fn set_cursor(&self, buffer: &mut TextBuffer, position: CursorPosition) {
        let clamped = clamp_position(buffer, position);
        buffer.cursors = vec![CursorAnchor::caret(clamped)];
        buffer.primary = 0;
        buffer.column_selection = None;
    }

    fn clamp_cursor(&self, buffer: &mut TextBuffer) {
        let position = buffer.cursor();
        self.set_cursor(buffer, position);
    }

    fn move_horizontal(&self, buffer: &mut TextBuffer, delta: isize) {
        let mut position = buffer.cursor();
        let line_len = buffer.line_content_len(position.line).unwrap_or(0);
        let max_col = line_len.saturating_sub(1) as isize;
        let new_col = (position.col as isize + delta).clamp(0, max_col.max(0));
        position.col = new_col as usize;
        self.set_cursor(buffer, position);
    }

    fn move_vertical(&self, buffer: &mut TextBuffer, delta: isize) {
        let mut position = buffer.cursor();
        let lines = buffer.len_lines() as isize;
        let new_line = (position.line as isize + delta).clamp(0, lines.saturating_sub(1));
        if new_line == position.line as isize {
            return;
        }
        position.line = new_line as usize;
        let line_len = buffer.line_content_len(position.line).unwrap_or(0);
        position.col = position.col.min(line_len.saturating_sub(1));
        self.set_cursor(buffer, position);
    }

    fn set_col(&self, buffer: &mut TextBuffer, col: usize) {
        let mut position = buffer.cursor();
        position.col = col;
        self.set_cursor(buffer, position);
    }

    fn first_non_blank(&self, buffer: &mut TextBuffer) {
        let line = buffer.cursor().line;
        self.set_cursor(
            buffer,
            CursorPosition {
                line,
                col: first_non_blank_col(buffer, line),
            },
        );
    }

    fn end_of_line(&self, buffer: &mut TextBuffer) {
        let line = buffer.cursor().line;
        let len = buffer.line_content_len(line).unwrap_or(0);
        self.set_cursor(
            buffer,
            CursorPosition {
                line,
                col: len.saturating_sub(1),
            },
        );
    }

    fn goto_line(&self, buffer: &mut TextBuffer, line: Option<usize>) {
        let target = line
            .map(|n| n.saturating_sub(1))
            .unwrap_or_else(|| buffer.len_lines().saturating_sub(1));
        let line = target.min(buffer.len_lines().saturating_sub(1));
        self.set_cursor(
            buffer,
            CursorPosition {
                line,
                col: first_non_blank_col(buffer, line),
            },
        );
    }

    fn word_forward(&self, buffer: &mut TextBuffer, count: usize, big: bool) {
        let mut position = buffer.cursor();
        for _ in 0..count.max(1) {
            position = next_word_position(buffer, &position, big);
        }
        self.set_cursor(buffer, position);
    }

    fn word_back(&self, buffer: &mut TextBuffer, count: usize, big: bool) {
        let mut position = buffer.cursor();
        for _ in 0..count.max(1) {
            position = prev_word_position(buffer, &position, big);
        }
        self.set_cursor(buffer, position);
    }

    fn word_end(&self, buffer: &mut TextBuffer, count: usize, big: bool) {
        let mut position = buffer.cursor();
        for _ in 0..count.max(1) {
            position = word_end_position(buffer, &position, big);
        }
        self.set_cursor(buffer, position);
    }

    fn paragraph_move(&self, buffer: &mut TextBuffer, delta: isize) {
        let last = buffer.len_lines() as isize - 1;
        let mut line = buffer.cursor().line as isize;
        let step = delta.signum();
        let mut blank = line_is_blank(buffer, line.max(0) as usize);
        while (line > 0 || step > 0) && (line < last || step < 0) && step != 0 {
            let next = line + step;
            if next < 0 || next > last {
                break;
            }
            line = next;
            let now_blank = line_is_blank(buffer, line as usize);
            // A paragraph boundary is a blank line adjacent to a non-blank.
            if now_blank && !blank && step > 0 {
                break;
            }
            if !now_blank && blank && step < 0 {
                break;
            }
            blank = now_blank;
        }
        let line = line.clamp(0, last).max(0) as usize;
        self.set_cursor(
            buffer,
            CursorPosition {
                line,
                col: first_non_blank_col(buffer, line),
            },
        );
    }

    fn do_find(&mut self, buffer: &mut TextBuffer, target: char, kind: char, count: usize) -> bool {
        let position = buffer.cursor();
        let Some(text) = buffer.line_text(position.line) else {
            return false;
        };
        let chars: Vec<char> = text.chars().collect();
        let mut found: Option<usize> = None;
        let mut remaining = count.max(1);
        match kind {
            'f' | 't' => {
                let mut index = position.col + 1;
                while index < chars.len() {
                    if chars[index] == target {
                        remaining = remaining.saturating_sub(1);
                        if remaining == 0 {
                            found = Some(if kind == 'f' {
                                index
                            } else {
                                index.saturating_sub(1)
                            });
                            break;
                        }
                    }
                    index += 1;
                }
            }
            _ => {
                let mut index = position.col as isize - 1;
                while index >= 0 {
                    let index_us = index as usize;
                    if chars[index_us] == target {
                        remaining = remaining.saturating_sub(1);
                        if remaining == 0 {
                            found = Some(if kind == 'F' {
                                index_us
                            } else {
                                (index_us + 1).min(chars.len().saturating_sub(1))
                            });
                            break;
                        }
                    }
                    index -= 1;
                }
            }
        }
        match found {
            Some(col) => {
                self.last_find = Some((target, kind));
                self.set_cursor(buffer, CursorPosition { line: position.line, col });
                true
            }
            None => false,
        }
    }

    fn match_bracket(&self, buffer: &mut TextBuffer) {
        let position = buffer.cursor();
        let Some(text) = buffer.line_text(position.line) else {
            return;
        };
        let chars: Vec<char> = text.chars().collect();
        if position.col >= chars.len() {
            return;
        }
        let open = chars[position.col];
        let matching = match open {
            '(' => ')',
            '[' => ']',
            '{' => '}',
            ')' => '(',
            ']' => '[',
            '}' => '{',
            _ => return,
        };
        let opening = matches!(open, '(' | '[' | '{');
        let mut depth = 0i32;
        if opening {
            let mut line = position.line;
            let mut col = position.col;
            loop {
                let Some(line_text) = buffer.line_text(line) else { return };
                let line_chars: Vec<char> = line_text.chars().collect();
                while col < line_chars.len() {
                    let c = line_chars[col];
                    if c == open {
                        depth += 1;
                    } else if c == matching {
                        depth -= 1;
                        if depth == 0 {
                            self.set_cursor(buffer, CursorPosition { line, col });
                            return;
                        }
                    }
                    col += 1;
                }
                line += 1;
                col = 0;
                if line >= buffer.len_lines() {
                    return;
                }
            }
        } else {
            let mut line = position.line as isize;
            let mut col = position.col as isize;
            while line >= 0 {
                let Some(line_text) = buffer.line_text(line as usize) else { return };
                let line_chars: Vec<char> = line_text.chars().collect();
                while col >= 0 && (col as usize) < line_chars.len() {
                    let c = line_chars[col as usize];
                    if c == open {
                        depth += 1;
                    } else if c == matching {
                        depth -= 1;
                        if depth == 0 {
                            self.set_cursor(
                                buffer,
                                CursorPosition {
                                    line: line as usize,
                                    col: col as usize,
                                },
                            );
                            return;
                        }
                    }
                    col -= 1;
                }
                line -= 1;
                col = buffer
                    .line_content_len(line.max(0) as usize)
                    .map(|len| len as isize - 1)
                    .unwrap_or(-1);
            }
        }
    }

    fn enter_insert(&self, buffer: &mut TextBuffer, at: InsertAt) {
        let position = buffer.cursor();
        let target = match at {
            InsertAt::BeforeCursor => position,
            InsertAt::AfterCursor => {
                let len = buffer.line_content_len(position.line).unwrap_or(0);
                CursorPosition {
                    line: position.line,
                    col: (position.col + 1).min(len),
                }
            }
            InsertAt::FirstNonBlank => CursorPosition {
                line: position.line,
                col: first_non_blank_col(buffer, position.line),
            },
            InsertAt::EndOfLine => {
                let len = buffer.line_content_len(position.line).unwrap_or(0);
                CursorPosition {
                    line: position.line,
                    col: len,
                }
            }
        };
        self.set_cursor(buffer, target);
        self.mode = VimMode::Insert;
    }

    fn open_line(&mut self, buffer: &mut TextBuffer, below: bool, options: VimOptions) {
        let line = buffer.cursor().line;
        let indent = line_indent(buffer, line, options);
        let newline = if buffer.text().contains("\r\n") { "\r\n" } else { "\n" };
        let line_start = char_offset(buffer, CursorPosition { line, col: 0 });
        let insert_at = if below {
            line_start + buffer.line_content_len(line).unwrap_or(0) + line_break_len(buffer, line)
        } else {
            line_start
        };
        // Clamp for the last line (no trailing break): append at buffer end.
        let insert_at = insert_at.min(buffer.len_chars());
        let text = if below {
            format!("{newline}{indent}")
        } else {
            format!("{indent}{newline}")
        };
        buffer.begin_edit();
        let ok = buffer.replace_char_range(insert_at, insert_at, &text).is_ok();
        buffer.commit_edit();
        if ok {
            let new_line = if below { line + 1 } else { line };
            self.set_cursor(
                buffer,
                CursorPosition {
                    line: new_line,
                    col: indent.chars().count(),
                },
            );
            self.mode = VimMode::Insert;
        }
    }

    fn enter_visual(&mut self, buffer: &mut TextBuffer, linewise: bool) {
        self.mode = if linewise {
            VimMode::VisualLine
        } else {
            VimMode::Visual
        };
        let position = buffer.cursor();
        buffer.cursors = vec![CursorAnchor {
            anchor: position,
            head: position,
            col_affinity: position.col,
        }];
        buffer.primary = 0;
    }

    fn exit_visual(&mut self, buffer: &mut TextBuffer) {
        self.mode = VimMode::Normal;
        let head = buffer.primary_cursor().head;
        self.set_cursor(buffer, head);
    }

    fn visual_move(&self, buffer: &mut TextBuffer, dx: isize, dy: isize) {
        let head = buffer.primary_cursor().head;
        let mut target = head;
        if dx != 0 {
            let line_len = buffer.line_content_len(head.line).unwrap_or(0);
            let max_col = line_len.saturating_sub(1) as isize;
            target.col = ((head.col as isize + dx).clamp(0, max_col.max(0))) as usize;
        }
        if dy != 0 {
            let lines = buffer.len_lines() as isize;
            let new_line = (head.line as isize + dy).clamp(0, lines.saturating_sub(1));
            target.line = new_line as usize;
            let line_len = buffer.line_content_len(target.line).unwrap_or(0);
            target.col = target.col.min(line_len.saturating_sub(1));
        }
        self.visual_set_head(buffer, target);
    }

    fn visual_move_to_col(&self, buffer: &mut TextBuffer, col: usize) {
        let head = buffer.primary_cursor().head;
        self.visual_set_head(buffer, CursorPosition { line: head.line, col });
    }

    fn visual_set_head(&self, buffer: &mut TextBuffer, head: CursorPosition) {
        let head = clamp_position(buffer, head);
        let anchor = buffer.primary_cursor().anchor;
        buffer.cursors = vec![CursorAnchor {
            anchor,
            head,
            col_affinity: head.col,
        }];
        buffer.primary = 0;
    }

    fn swap_visual_ends(&self, buffer: &mut TextBuffer) {
        let current = buffer.primary_cursor();
        let (anchor, head) = (current.head, current.anchor);
        buffer.cursors = vec![CursorAnchor {
            anchor,
            head,
            col_affinity: head.col,
        }];
        buffer.primary = 0;
    }

    /// First..last line covered by the visual selection.
    fn visual_line_span(&self, buffer: &TextBuffer) -> (usize, usize) {
        let (anchor, head) = buffer.primary_cursor().normalize();
        (anchor.line.min(head.line), anchor.line.max(head.line))
    }

    /// Normalized (start, end) characterwise selection (both inclusive).
    fn visual_char_range(&self, buffer: &TextBuffer) -> (CursorPosition, CursorPosition) {
        buffer.primary_cursor().normalize()
    }

    fn extract_range(
        &self,
        buffer: &mut TextBuffer,
        range: (CursorPosition, CursorPosition),
    ) -> (String, bool) {
        let (start, end) = range;
        let Some(start_off) = buffer.position_to_char_index(start) else {
            return (String::new(), false);
        };
        let Some(end_off) = buffer.position_to_char_index(end) else {
            return (String::new(), false);
        };
        let end_off = (end_off + 1).min(buffer.len_chars());
        if end_off <= start_off {
            return (String::new(), false);
        }
        (buffer.text()[start_off..end_off].to_owned(), false)
    }

    /// Delete an inclusive (start..=end) character range; returns the text.
    fn delete_range(
        &mut self,
        buffer: &mut TextBuffer,
        range: (CursorPosition, CursorPosition),
    ) -> (String, bool) {
        let (start, end) = range;
        let Some(start_off) = buffer.position_to_char_index(start) else {
            return (String::new(), false);
        };
        let Some(end_off) = buffer.position_to_char_index(end) else {
            return (String::new(), false);
        };
        let end_off = (end_off + 1).min(buffer.len_chars());
        let mut deleted = String::new();
        if end_off > start_off {
            buffer.begin_edit();
            deleted = buffer.text()[start_off..end_off].to_owned();
            let _ = buffer.replace_char_range(start_off, end_off, "");
            buffer.commit_edit();
        }
        (deleted, false)
    }

    fn transform_visual(&self, buffer: &mut TextBuffer, op: CaseOp) {
        let (start, end) = self.visual_char_range(buffer);
        let Some(start_off) = buffer.position_to_char_index(start) else { return };
        let Some(end_off) = buffer.position_to_char_index(end) else { return };
        let end_off = (end_off + 1).min(buffer.len_chars());
        if end_off <= start_off {
            return;
        }
        let text = buffer.text()[start_off..end_off].to_owned();
        let transformed: String = match op {
            CaseOp::Toggle => text
                .chars()
                .map(|c| {
                    if c.is_uppercase() {
                        c.to_lowercase().next().unwrap_or(c)
                    } else {
                        c.to_uppercase().next().unwrap_or(c)
                    }
                })
                .collect(),
            CaseOp::Lower => text.to_lowercase(),
            CaseOp::Upper => text.to_uppercase(),
        };
        buffer.begin_edit();
        let _ = buffer.replace_char_range(start_off, end_off, &transformed);
        buffer.commit_edit();
        let head = buffer.cursor();
        self.set_cursor(buffer, head);
        self.mode = VimMode::Normal;
    }

    fn indent_lines(
        &self,
        buffer: &mut TextBuffer,
        first: usize,
        last: usize,
        indent: bool,
        options: VimOptions,
    ) {
        let last = last.min(buffer.len_lines().saturating_sub(1));
        let unit = if options.insert_spaces {
            " ".repeat(options.tab_width.max(1))
        } else {
            "\t".to_owned()
        };
        for line in (first..=last).rev() {
            let start = char_offset(buffer, CursorPosition { line, col: 0 });
            if indent {
                buffer.begin_edit();
                let _ = buffer.replace_char_range(start, start, &unit);
                buffer.commit_edit();
            } else {
                let text = buffer.line_text(line).unwrap_or_default();
                let strip_len = if text.starts_with('\t') {
                    1
                } else {
                    text.chars()
                        .take_while(|c| *c == ' ')
                        .count()
                        .min(options.tab_width.max(1))
                };
                if strip_len > 0 {
                    buffer.begin_edit();
                    let _ = buffer.replace_char_range(start, start + strip_len, "");
                    buffer.commit_edit();
                }
            }
        }
    }

    fn finish_linewise_operator(
        &mut self,
        buffer: &mut TextBuffer,
        op: PendingOperator,
        first: usize,
        last: usize,
        options: VimOptions,
    ) {
        let last = last.min(buffer.len_lines().saturating_sub(1));
        match op {
            PendingOperator::Yank => {
                self.yank_lines(buffer, first, last);
                self.set_cursor(buffer, CursorPosition { line: first, col: 0 });
            }
            PendingOperator::Delete => {
                let (text, _) = self.delete_lines(buffer, first, last);
                self.register = text;
                self.register_linewise = true;
                let line = first.min(buffer.len_lines().saturating_sub(1));
                self.set_cursor(
                    buffer,
                    CursorPosition {
                        line,
                        col: first_non_blank_col(buffer, line),
                    },
                );
            }
            PendingOperator::Change => {
                self.yank_lines(buffer, first, last);
                self.change_lines(buffer, first, last);
            }
            PendingOperator::Indent | PendingOperator::Outdent => {
                self.indent_lines(buffer, first, last, op == PendingOperator::Indent, options);
                let line = first;
                self.set_cursor(
                    buffer,
                    CursorPosition {
                        line,
                        col: first_non_blank_col(buffer, line),
                    },
                );
            }
            PendingOperator::Lowercase | PendingOperator::Uppercase => {
                self.transform_lines(buffer, first, last, op == PendingOperator::Uppercase);
                self.set_cursor(buffer, CursorPosition { line: first, col: 0 });
            }
        }
    }

    fn finish_motion_operator(
        &mut self,
        buffer: &mut TextBuffer,
        op: PendingOperator,
        from: CursorPosition,
        to: CursorPosition,
        inclusive: bool,
        options: VimOptions,
    ) {
        let (start, end) = if (to.line, to.col) < (from.line, from.col) {
            (to, from)
        } else {
            (from, to)
        };
        let Some(start_off) = buffer.position_to_char_index(start) else {
            self.cancel_pending();
            return;
        };
        let Some(mut end_off) = buffer.position_to_char_index(end) else {
            self.cancel_pending();
            return;
        };
        if inclusive {
            let line_len = buffer.line_content_len(end.line).unwrap_or(0);
            if end.col < line_len {
                end_off += 1;
            }
        }
        match op {
            PendingOperator::Yank => {
                if end_off > start_off {
                    self.register = buffer.text()[start_off..end_off].to_owned();
                    self.register_linewise = false;
                }
                self.set_cursor(buffer, start);
            }
            PendingOperator::Delete => {
                let mut deleted = String::new();
                buffer.begin_edit();
                if end_off > start_off {
                    deleted = buffer.text()[start_off..end_off].to_owned();
                    let _ = buffer.replace_char_range(start_off, end_off, "");
                }
                buffer.commit_edit();
                self.register = deleted;
                self.register_linewise = false;
                self.set_cursor(buffer, clamp_position(buffer, start));
            }
            PendingOperator::Change => {
                let mut deleted = String::new();
                buffer.begin_edit();
                if end_off > start_off {
                    deleted = buffer.text()[start_off..end_off].to_owned();
                    let _ = buffer.replace_char_range(start_off, end_off, "");
                }
                buffer.commit_edit();
                self.register = deleted;
                self.register_linewise = false;
                self.set_cursor(buffer, clamp_position(buffer, start));
                self.mode = VimMode::Insert;
            }
            PendingOperator::Indent | PendingOperator::Outdent => {
                self.indent_lines(buffer, start.line, end.line, op == PendingOperator::Indent, options);
                self.set_cursor(
                    buffer,
                    CursorPosition {
                        line: start.line,
                        col: first_non_blank_col(buffer, start.line),
                    },
                );
            }
            PendingOperator::Lowercase | PendingOperator::Uppercase => {
                if end_off > start_off {
                    let text = buffer.text()[start_off..end_off].to_owned();
                    let transformed = if op == PendingOperator::Uppercase {
                        text.to_uppercase()
                    } else {
                        text.to_lowercase()
                    };
                    buffer.begin_edit();
                    let _ = buffer.replace_char_range(start_off, end_off, &transformed);
                    buffer.commit_edit();
                }
                self.set_cursor(buffer, start);
            }
        }
    }

    fn finish_range_operator(
        &mut self,
        buffer: &mut TextBuffer,
        op: PendingOperator,
        object: (CursorPosition, CursorPosition),
        options: VimOptions,
    ) {
        // Text-object endpoints are inclusive.
        self.finish_motion_operator(buffer, op, object.0, object.1, true, options);
    }

    fn yank_lines(&mut self, buffer: &mut TextBuffer, first: usize, last: usize) {
        let last = last.min(buffer.len_lines().saturating_sub(1));
        let mut text = String::new();
        for line in first..=last {
            if let Some(line_text) = buffer.line_text(line) {
                text.push_str(&line_text);
                text.push('\n');
            }
        }
        self.register = text;
        self.register_linewise = true;
    }

    /// Delete whole lines `first..=last` including their line breaks.
    fn delete_lines(&mut self, buffer: &mut TextBuffer, first: usize, last: usize) -> (String, bool) {
        let last = last.min(buffer.len_lines().saturating_sub(1));
        let start = char_offset(buffer, CursorPosition { line: first, col: 0 });
        let end = if last + 1 < buffer.len_lines() {
            char_offset(buffer, CursorPosition { line: last + 1, col: 0 })
        } else {
            buffer.len_chars()
        };
        let mut deleted = String::new();
        if end > start {
            buffer.begin_edit();
            deleted = buffer.text()[start..end].to_owned();
            let _ = buffer.replace_char_range(start, end, "");
            buffer.commit_edit();
        }
        (deleted, true)
    }

    /// Blank the content of lines `first..=last`, leave one empty line, and
    /// enter insert mode (linewise `cc` / `S`).
    fn change_lines(&mut self, buffer: &mut TextBuffer, first: usize, last: usize) {
        let last = last.min(buffer.len_lines().saturating_sub(1));
        let start = char_offset(buffer, CursorPosition { line: first, col: 0 });
        let end = if first == last {
            start + buffer.line_content_len(first).unwrap_or(0)
        } else if last + 1 < buffer.len_lines() {
            char_offset(buffer, CursorPosition { line: last + 1, col: 0 })
        } else {
            buffer.len_chars()
        };
        buffer.begin_edit();
        if end > start {
            let _ = buffer.replace_char_range(start, end, "");
        }
        buffer.commit_edit();
        self.set_cursor(buffer, CursorPosition { line: first, col: 0 });
        self.mode = VimMode::Insert;
    }

    fn transform_lines(&self, buffer: &mut TextBuffer, first: usize, last: usize, upper: bool) {
        for line in first..=last.min(buffer.len_lines().saturating_sub(1)) {
            let Some(text) = buffer.line_text(line) else { continue };
            let transformed = if upper {
                text.to_uppercase()
            } else {
                text.to_lowercase()
            };
            if transformed != text {
                let start = char_offset(buffer, CursorPosition { line, col: 0 });
                buffer.begin_edit();
                let _ = buffer.replace_char_range(start, start + text.chars().count(), &transformed);
                buffer.commit_edit();
            }
        }
    }

    fn delete_chars(&mut self, buffer: &mut TextBuffer, count: usize, backward: bool) {
        let position = buffer.cursor();
        let line_len = buffer.line_content_len(position.line).unwrap_or(0);
        let start = if backward {
            position.col.saturating_sub(count.max(1))
        } else {
            position.col
        };
        let end = if backward {
            position.col
        } else {
            (position.col + count.max(1)).min(line_len)
        };
        if end <= start {
            return;
        }
        let start_off = char_offset(buffer, CursorPosition { line: position.line, col: start });
        let end_off = start_off + (end - start);
        let mut deleted = String::new();
        buffer.begin_edit();
        if end_off > start_off && end_off <= buffer.len_chars() {
            deleted = buffer.text()[start_off..end_off].to_owned();
            let _ = buffer.replace_char_range(start_off, end_off, "");
        }
        buffer.commit_edit();
        self.register = deleted;
        self.register_linewise = false;
        self.set_cursor(buffer, CursorPosition { line: position.line, col: start });
    }

    fn change_to_eol(&mut self, buffer: &mut TextBuffer, count: usize, enter_insert: bool) {
        let position = buffer.cursor();
        let line_len = buffer.line_content_len(position.line).unwrap_or(0);
        let end_col = (position.col + count.max(1)).min(line_len);
        let start_off = char_offset(buffer, position);
        let end_off = start_off + (end_col - position.col);
        let mut deleted = String::new();
        buffer.begin_edit();
        if end_off > start_off {
            deleted = buffer.text()[start_off..end_off].to_owned();
            let _ = buffer.replace_char_range(start_off, end_off, "");
        }
        buffer.commit_edit();
        self.register = deleted;
        self.register_linewise = false;
        if enter_insert {
            self.mode = VimMode::Insert;
        } else {
            self.clamp_cursor(buffer);
        }
    }

    fn put(&mut self, buffer: &mut TextBuffer, count: usize, after: bool) {
        if self.register.is_empty() {
            return;
        }
        // Cap the repeat so a fat-fingered `99999p` cannot exhaust memory.
        let repetitions = count.max(1).min(4_096);
        let mut payload = self.register.repeat(repetitions);
        let linewise = self.register_linewise;
        if linewise && !payload.ends_with('\n') {
            payload.push('\n');
        }
        let position = buffer.cursor();
        let insert_at = if linewise {
            let line = if after { position.line + 1 } else { position.line };
            if line >= buffer.len_lines() {
                buffer.len_chars()
            } else {
                char_offset(buffer, CursorPosition { line, col: 0 })
            }
        } else {
            let line_len = buffer.line_content_len(position.line).unwrap_or(0);
            let col = if after {
                (position.col + 1).min(line_len)
            } else {
                position.col
            };
            char_offset(buffer, CursorPosition { line: position.line, col })
        };
        buffer.begin_edit();
        let inserted = buffer.replace_char_range(insert_at, insert_at, &payload).is_ok();
        buffer.commit_edit();
        if !inserted {
            return;
        }
        if linewise {
            let lines_added = payload.matches('\n').count();
            let target_line = if after {
                position.line + lines_added
            } else {
                position.line
            };
            let target_line = target_line.min(buffer.len_lines().saturating_sub(1));
            self.set_cursor(
                buffer,
                CursorPosition {
                    line: target_line,
                    col: first_non_blank_col(buffer, target_line),
                },
            );
        } else {
            // Cursor lands on the last inserted character.
            let chars_added = payload.chars().count();
            self.set_cursor(
                buffer,
                CursorPosition {
                    line: position.line,
                    col: position.col + chars_added,
                },
            );
        }
    }

    fn replace_pending(&mut self, buffer: &mut TextBuffer, replacement: char, count: usize) {
        let position = buffer.cursor();
        let line_len = buffer.line_content_len(position.line).unwrap_or(0);
        let span = count.max(1).min(line_len.saturating_sub(position.col));
        if span == 0 {
            return;
        }
        let start_off = char_offset(buffer, position);
        let text: String = std::iter::repeat(replacement).take(span).collect();
        buffer.begin_edit();
        let _ = buffer.replace_char_range(start_off, start_off + span, &text);
        buffer.commit_edit();
        self.set_cursor(
            buffer,
            CursorPosition {
                line: position.line,
                col: position.col + span - 1,
            },
        );
    }

    fn join_below(&mut self, buffer: &mut TextBuffer, count: usize) {
        let joins = if count > 1 { count - 1 } else { 1 };
        for _ in 0..joins {
            let line = buffer.cursor().line;
            if line + 1 >= buffer.len_lines() {
                break;
            }
            let line_len = buffer.line_content_len(line).unwrap_or(0);
            let current_text = buffer.line_text(line).unwrap_or_default();
            let next_text = buffer.line_text(line + 1).unwrap_or_default();
            let next_trim = next_text.trim_start();
            let leading = next_text.chars().count() - next_trim.chars().count();
            let next_start = char_offset(buffer, CursorPosition {
                line: line + 1,
                col: 0,
            });
            // Replace [end of current line .. first non-blank of next] with a
            // single space (or nothing for closing brackets / empty next).
            let joiner = if next_trim.is_empty() || next_trim.starts_with(')') || next_trim.starts_with(']') || next_trim.starts_with('}') {
                ""
            } else if current_text.ends_with(' ') || current_text.ends_with('\t') {
                ""
            } else {
                " "
            };
            let delete_end = next_start + leading;
            let line_len_off = char_offset(buffer, CursorPosition { line, col: line_len });
            buffer.begin_edit();
            let _ = buffer.replace_char_range(line_len_off, delete_end, joiner);
            buffer.commit_edit();
            self.set_cursor(
                buffer,
                CursorPosition {
                    line,
                    col: line_len + joiner.chars().count().saturating_sub(1),
                },
            );
        }
    }

    fn toggle_case_char(&mut self, buffer: &mut TextBuffer, count: usize) {
        for _ in 0..count.max(1) {
            let position = buffer.cursor();
            let line_len = buffer.line_content_len(position.line).unwrap_or(0);
            if position.col >= line_len {
                break;
            }
            let start_off = char_offset(buffer, position);
            let c = buffer.text()[start_off..].chars().next().unwrap_or(' ');
            let replacement = if c.is_uppercase() {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                c.to_uppercase().next().unwrap_or(c)
            };
            buffer.begin_edit();
            let _ = buffer.replace_char_range(start_off, start_off + c.len_utf8().max(1), &replacement.to_string());
            buffer.commit_edit();
            self.move_horizontal(buffer, 1);
        }
    }

    fn jump_to_next_match(&mut self, buffer: &mut TextBuffer, pattern: &str, forward: bool) {
        // ASCII-only case folding keeps 1:1 char alignment with the raw text,
        // so folded offsets stay valid `char_index_to_position` indices.
        let needle: Vec<char> = pattern.chars().map(|c| c.to_ascii_lowercase()).collect();
        if needle.is_empty() {
            return;
        }
        let haystack: Vec<char> = buffer
            .text()
            .chars()
            .map(|c| c.to_ascii_lowercase())
            .collect();
        let length = haystack.len();
        if length == 0 {
            return;
        }
        // Case-folded haystack has the same length as the raw text only for
        // 1:1 folds; for safety compute positions in the folded vector and map
        // by index (approximation acceptable for code identifiers).
        let start = char_offset(buffer, buffer.cursor()).min(length.saturating_sub(1));
        let contains_at = |offset: usize| -> bool {
            if offset + needle.len() > length {
                return false;
            }
            haystack[offset..offset + needle.len()] == needle[..]
        };
        let mut offset = if forward {
            (start + 1) % length
        } else {
            (start + length - 1) % length
        };
        for _ in 0..length {
            if contains_at(offset) {
                if let Some(position) = buffer.char_index_to_position(offset) {
                    self.set_cursor(buffer, position);
                }
                return;
            }
            offset = if forward {
                (offset + 1) % length
            } else {
                (offset + length - 1) % length
            };
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum InsertAt {
    BeforeCursor,
    AfterCursor,
    FirstNonBlank,
    EndOfLine,
}

#[derive(Debug, Clone, Copy)]
enum CaseOp {
    Toggle,
    Lower,
    Upper,
}

// ─── Free helpers ────────────────────────────────────────────────────────────

fn clamp_position(buffer: &TextBuffer, position: CursorPosition) -> CursorPosition {
    let line = position.line.min(buffer.len_lines().saturating_sub(1));
    let line_len = buffer.line_content_len(line).unwrap_or(0);
    CursorPosition {
        line,
        col: position.col.min(line_len),
    }
}

fn char_offset(buffer: &TextBuffer, position: CursorPosition) -> usize {
    buffer.position_to_char_index(position).unwrap_or(0)
}

fn first_non_blank_col(buffer: &TextBuffer, line: usize) -> usize {
    let Some(text) = buffer.line_text(line) else { return 0 };
    let content_len = text.chars().count();
    if content_len == 0 {
        return 0;
    }
    text.chars()
        .take_while(|c| c.is_whitespace())
        .count()
        .min(content_len - 1)
}

fn line_is_blank(buffer: &TextBuffer, line: usize) -> bool {
    buffer
        .line_text(line)
        .map(|text| text.trim().is_empty())
        .unwrap_or(true)
}

fn line_indent(buffer: &TextBuffer, line: usize, options: VimOptions) -> String {
    let Some(text) = buffer.line_text(line) else { return String::new() };
    let indent: String = text
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    if !indent.is_empty() {
        return indent;
    }
    if options.insert_spaces {
        " ".repeat(options.tab_width.max(1))
    } else {
        "\t".to_owned()
    }
}

/// Length of the line break terminating `line` (0 or 1 for LF, 2 for CRLF).
fn line_break_len(buffer: &TextBuffer, line: usize) -> usize {
    if line + 1 >= buffer.len_lines() {
        return 0;
    }
    let this_start = buffer.position_to_char_index(CursorPosition { line, col: 0 }).unwrap_or(0);
    let next_start = buffer
        .position_to_char_index(CursorPosition {
            line: line + 1,
            col: 0,
        })
        .unwrap_or(this_start);
    next_start - this_start - buffer.line_content_len(line).unwrap_or(0)
}

fn is_word_char(c: char, big: bool) -> bool {
    if big {
        !c.is_whitespace()
    } else {
        c.is_alphanumeric() || c == '_'
    }
}

fn next_word_position(buffer: &TextBuffer, from: &CursorPosition, big: bool) -> CursorPosition {
    let chars: Vec<char> = buffer.text().chars().collect();
    let total = chars.len();
    let start_offset = char_offset(buffer, *from).min(total);
    let mut offset = start_offset;
    // 1. Skip the current word/punctuation run.
    if let Some(current) = chars.get(offset) {
        let run_is_word = is_word_char(*current, big);
        while offset < total
            && chars
                .get(offset)
                .map(|c| is_word_char(*c, big) == run_is_word && !c.is_whitespace())
                .unwrap_or(false)
        {
            offset += 1;
        }
    }
    // 2. Skip whitespace.
    while offset < total
        && chars
            .get(offset)
            .map(|c| c.is_whitespace())
            .unwrap_or(false)
    {
        offset += 1;
    }
    let _ = start_offset;
    buffer
        .char_index_to_position(offset.min(total))
        .unwrap_or(*from)
}

fn prev_word_position(buffer: &TextBuffer, from: &CursorPosition, big: bool) -> CursorPosition {
    let chars: Vec<char> = buffer.text().chars().collect();
    let mut offset = char_offset(buffer, *from).min(chars.len());
    offset = offset.saturating_sub(1);
    // Skip whitespace backwards.
    while offset > 0
        && chars
            .get(offset)
            .map(|c| c.is_whitespace())
            .unwrap_or(false)
    {
        offset -= 1;
    }
    // Skip the word run backwards, landing on its first char.
    while offset > 0
        && chars
            .get(offset.saturating_sub(1))
            .map(|c| is_word_char(*c, big))
            .unwrap_or(false)
        && chars.get(offset).map(|c| is_word_char(*c, big)).unwrap_or(false)
    {
        offset -= 1;
    }
    buffer
        .char_index_to_position(offset)
        .unwrap_or(*from)
}

fn word_end_position(buffer: &TextBuffer, from: &CursorPosition, big: bool) -> CursorPosition {
    let chars: Vec<char> = buffer.text().chars().collect();
    let mut offset = char_offset(buffer, *from).min(chars.len());
    offset += 1;
    // Skip whitespace forward.
    while offset < chars.len()
        && chars
            .get(offset)
            .map(|c| c.is_whitespace())
            .unwrap_or(false)
    {
        offset += 1;
    }
    // Advance to the last char of the word run.
    while offset + 1 < chars.len()
        && chars
            .get(offset + 1)
            .map(|c| is_word_char(*c, big))
            .unwrap_or(false)
        && chars.get(offset).map(|c| is_word_char(*c, big)).unwrap_or(false)
    {
        offset += 1;
    }
    buffer
        .char_index_to_position(offset.min(chars.len().saturating_sub(1).max(0)))
        .or_else(|| buffer.char_index_to_position(offset))
        .unwrap_or(*from)
}

fn motion_repeat(
    buffer: &TextBuffer,
    from: &CursorPosition,
    count: usize,
    big: bool,
) -> CursorPosition {
    let mut position = *from;
    for _ in 0..count.max(1) {
        position = next_word_position(buffer, &position, big);
    }
    position
}

fn motion_repeat_end(
    buffer: &TextBuffer,
    from: &CursorPosition,
    count: usize,
    big: bool,
) -> CursorPosition {
    let mut position = *from;
    for _ in 0..count.max(1) {
        position = word_end_position(buffer, &position, big);
    }
    position
}

fn motion_back_repeat(
    buffer: &TextBuffer,
    from: &CursorPosition,
    count: usize,
    big: bool,
) -> CursorPosition {
    let mut position = *from;
    for _ in 0..count.max(1) {
        position = prev_word_position(buffer, &position, big);
    }
    position
}

/// Resolve a text object (`iw`, `aw`, `i"`, `a(`, …) at the cursor.
/// Returns an inclusive (start, end) pair.
fn text_object(
    buffer: &TextBuffer,
    cursor: &CursorPosition,
    object: char,
    around: bool,
) -> Option<(CursorPosition, CursorPosition)> {
    match object {
        'w' | 'W' => word_object(buffer, cursor, object == 'W', around),
        '"' | '\'' | '`' => quote_object(buffer, cursor, object, around),
        '(' | ')' | 'b' => bracket_object(buffer, cursor, '(', ')', around),
        '{' | '}' | 'B' => bracket_object(buffer, cursor, '{', '}', around),
        '[' | ']' => bracket_object(buffer, cursor, '[', ']', around),
        _ => None,
    }
}

fn expand_word_object(
    buffer: &TextBuffer,
    current: (CursorPosition, CursorPosition),
    around: bool,
) -> Option<(CursorPosition, CursorPosition)> {
    let after = next_word_position(buffer, &current.1, false);
    if after == current.1 {
        return None;
    }
    let end = if around {
        // Extend over trailing horizontal whitespace.
        let chars: Vec<char> = buffer.text().chars().collect();
        let mut offset = char_offset(buffer, after);
        let line_len = buffer.line_content_len(after.line).unwrap_or(0);
        let line_end = char_offset(buffer, CursorPosition {
            line: after.line,
            col: line_len,
        });
        while offset < line_end
            && chars
                .get(offset)
                .map(|c| c.is_whitespace() && *c != '\n')
                .unwrap_or(false)
        {
            offset += 1;
        }
        buffer.char_index_to_position(offset.saturating_sub(1)).unwrap_or(after)
    } else {
        after
    };
    Some((current.0, end))
}

fn word_object(
    buffer: &TextBuffer,
    cursor: &CursorPosition,
    big: bool,
    around: bool,
) -> Option<(CursorPosition, CursorPosition)> {
    let chars: Vec<char> = buffer.text().chars().collect();
    let offset = char_offset(buffer, *cursor);
    if offset >= chars.len() {
        return None;
    }
    let in_word = is_word_char(chars[offset], big);
    if !in_word {
        // On punctuation/whitespace: operate on the following word.
        let next = next_word_position(buffer, cursor, big);
        let end = word_end_position(buffer, &next, big);
        return Some((next, end));
    }
    let mut start = offset;
    while start > 0 && chars.get(start - 1).map(|c| is_word_char(*c, big)).unwrap_or(false) {
        start -= 1;
    }
    let mut end = offset;
    while end + 1 < chars.len()
        && chars.get(end + 1).map(|c| is_word_char(*c, big)).unwrap_or(false)
    {
        end += 1;
    }
    if around {
        while end + 1 < chars.len()
            && chars[end + 1].is_whitespace()
            && chars[end + 1] != '\n'
        {
            end += 1;
        }
        if chars.get(end + 1).map(|c| *c == '\n').unwrap_or(false) && start > 0 {
            let mut new_start = start;
            while new_start > 0
                && chars[new_start - 1].is_whitespace()
                && chars[new_start - 1] != '\n'
            {
                new_start -= 1;
            }
            start = new_start;
        }
    }
    Some((
        buffer.char_index_to_position(start)?,
        buffer.char_index_to_position(end)?,
    ))
}

fn quote_object(
    buffer: &TextBuffer,
    cursor: &CursorPosition,
    quote: char,
    around: bool,
) -> Option<(CursorPosition, CursorPosition)> {
    let line_text = buffer.line_text(cursor.line)?;
    let chars: Vec<char> = line_text.chars().collect();
    let col = cursor.col.min(chars.len().saturating_sub(1));
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    let mut open: Option<usize> = None;
    for (index, c) in chars.iter().enumerate() {
        if *c == quote {
            match open {
                Some(o) => {
                    pairs.push((o, index));
                    open = None;
                }
                None => open = Some(index),
            }
        }
    }
    let &(open, close) = pairs
        .iter()
        .rev()
        .find(|(open, close)| col >= *open && col <= *close + 1)
        .or_else(|| pairs.last())?;
    if around {
        let mut close_end = close;
        while close_end + 1 < chars.len()
            && chars[close_end + 1].is_whitespace()
            && chars[close_end + 1] != '\n'
        {
            close_end += 1;
        }
        Some((
            CursorPosition { line: cursor.line, col: open },
            CursorPosition { line: cursor.line, col: close_end },
        ))
    } else {
        if close <= open + 1 {
            // Empty quotes: select nothing meaningful; fall back to a point.
            return Some((
                CursorPosition { line: cursor.line, col: open + 1 },
                CursorPosition { line: cursor.line, col: open + 1 },
            ));
        }
        Some((
            CursorPosition { line: cursor.line, col: open + 1 },
            CursorPosition { line: cursor.line, col: close - 1 },
        ))
    }
}

fn bracket_object(
    buffer: &TextBuffer,
    cursor: &CursorPosition,
    open: char,
    close: char,
    around: bool,
) -> Option<(CursorPosition, CursorPosition)> {
    let chars: Vec<char> = buffer.text().chars().collect();
    let offset = char_offset(buffer, *cursor).min(chars.len());
    // Search backwards for the innermost unmatched open bracket.
    let mut start: Option<usize> = None;
    let mut back_depth = 0i32;
    let mut back = offset;
    while back > 0 {
        let c = chars[back - 1];
        if c == close {
            back_depth += 1;
        } else if c == open {
            if back_depth == 0 {
                start = Some(back - 1);
                break;
            }
            back_depth -= 1;
        }
        back -= 1;
    }
    let start = start?;
    // Forward to the matching close.
    let mut end: Option<usize> = None;
    let mut forward_depth = 0i32;
    for (index, c) in chars.iter().enumerate().skip(start + 1) {
        if *c == open {
            forward_depth += 1;
        } else if *c == close {
            if forward_depth == 0 {
                end = Some(index);
                break;
            }
            forward_depth -= 1;
        }
    }
    let end = end?;
    if around {
        Some((
            buffer.char_index_to_position(start)?,
            buffer.char_index_to_position(end)?,
        ))
    } else {
        if end <= start + 1 {
            return Some((
                buffer.char_index_to_position(start + 1)?,
                buffer.char_index_to_position(start + 1)?,
            ));
        }
        Some((
            buffer.char_index_to_position(start + 1)?,
            buffer.char_index_to_position(end - 1)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(text: &str) -> TextBuffer {
        TextBuffer::from_text(text)
    }

    fn state() -> VimState {
        VimState::default()
    }

    fn options() -> VimOptions {
        VimOptions {
            tab_width: 4,
            insert_spaces: true,
        }
    }

    fn feed(buffer: &mut TextBuffer, state: &mut VimState, input: VimInput) -> VimResult {
        state.process(buffer, input, options())
    }

    fn chars(buffer: &mut TextBuffer, state: &mut VimState, text: &str) -> VimResult {
        let mut result = VimResult::default();
        for c in text.chars() {
            result = feed(buffer, state, VimInput::Char(c));
        }
        result
    }

    fn cursor(buffer: &TextBuffer) -> (usize, usize) {
        let c = buffer.cursor();
        (c.line, c.col)
    }

    #[test]
    fn insert_mode_lets_normal_typing_through() {
        let mut b = buffer("hello\n");
        let mut v = state();
        chars(&mut b, &mut v, "i");
        assert_eq!(v.mode, VimMode::Insert);
        let result = feed(&mut b, &mut v, VimInput::Char('X'));
        assert!(!result.consumed, "insert-mode chars must flow to the editor");
    }

    #[test]
    fn escape_returns_to_normal_mode() {
        let mut b = buffer("hi\n");
        let mut v = state();
        chars(&mut b, &mut v, "iX");
        feed(&mut b, &mut v, VimInput::Key(NamedKey::Escape));
        assert_eq!(v.mode, VimMode::Normal);
    }

    #[test]
    fn basic_motions_move_the_cursor() {
        let mut b = buffer("one two three\nsecond line\n");
        let mut v = state();
        v.process(&mut b, VimInput::Char('w'), options());
        assert_eq!(cursor(&b), (0, 4));
        v.process(&mut b, VimInput::Char('w'), options());
        assert_eq!(cursor(&b), (0, 8));
        v.process(&mut b, VimInput::Char('b'), options());
        assert_eq!(cursor(&b), (0, 4));
        v.process(&mut b, VimInput::Char('e'), options());
        assert_eq!(cursor(&b), (0, 6));
        v.process(&mut b, VimInput::Char('j'), options());
        assert_eq!(cursor(&b), (1, 6));
        v.process(&mut b, VimInput::Char('k'), options());
        assert_eq!(cursor(&b), (0, 6));
        v.process(&mut b, VimInput::Key(NamedKey::Down), options());
        assert_eq!(cursor(&b), (1, 6));
        v.process(&mut b, VimInput::Char('0'), options());
        assert_eq!(cursor(&b), (1, 0));
        v.process(&mut b, VimInput::Char('$'), options());
        assert_eq!(cursor(&b), (1, 10));
    }

    #[test]
    fn counts_repeat_motions() {
        let mut b = buffer("a b c d e f\n");
        let mut v = state();
        chars(&mut b, &mut v, "3w");
        assert_eq!(cursor(&b), (0, 6));
    }

    #[test]
    fn dd_deletes_a_line_into_the_register() {
        let mut b = buffer("one\ntwo\nthree\n");
        let mut v = state();
        chars(&mut b, &mut v, "jdd");
        assert_eq!(b.text(), "one\nthree\n");
        assert_eq!(v.register(), "two\n");
        assert_eq!(cursor(&b), (1, 0));
    }

    #[test]
    fn dw_deletes_to_next_word() {
        let mut b = buffer("one two three\n");
        let mut v = state();
        chars(&mut b, &mut v, "dw");
        assert_eq!(b.text(), "two three\n");
    }

    #[test]
    fn d2d_deletes_two_lines() {
        let mut b = buffer("a\nb\nc\nd\n");
        let mut v = state();
        chars(&mut b, &mut v, "2dd");
        assert_eq!(b.text(), "c\nd\n");
    }

    #[test]
    fn yy_yanks_and_p_pastes_linewise() {
        let mut b = buffer("alpha\nbeta\n");
        let mut v = state();
        chars(&mut b, &mut v, "yyjp");
        assert_eq!(b.text(), "alpha\nbeta\nalpha\n");
    }

    #[test]
    fn x_deletes_char_under_cursor_and_p_pastes_charwise() {
        let mut b = buffer("abc\n");
        let mut v = state();
        chars(&mut b, &mut v, "x");
        assert_eq!(b.text(), "bc\n");
        chars(&mut b, &mut v, "p");
        assert_eq!(b.text(), "bac\n");
    }

    #[test]
    fn o_and_O_open_lines_with_insert_mode() {
        let mut b = buffer("one\ntwo\n");
        let mut v = state();
        chars(&mut b, &mut v, "o");
        assert_eq!(v.mode, VimMode::Insert);
        assert_eq!(b.text(), "one\n\ntwo\n");
        feed(&mut b, &mut v, VimInput::Key(NamedKey::Escape));
        // Cursor is on the new empty line (line 1); open above → line 1 again.
        chars(&mut b, &mut v, "O");
        assert_eq!(b.text(), "one\n\n\ntwo\n");
    }

    #[test]
    fn cc_changes_the_line() {
        let mut b = buffer("keep\nchange me\n");
        let mut v = state();
        chars(&mut b, &mut v, "jccX");
        assert_eq!(b.text(), "keep\nX\n");
        assert_eq!(v.mode, VimMode::Insert);
    }

    #[test]
    fn ciw_changes_inner_word() {
        let mut b = buffer("foo bar baz\n");
        let mut v = state();
        v.process(&mut b, VimInput::Char('w'), options());
        v.process(&mut b, VimInput::Char('c'), options());
        v.process(&mut b, VimInput::Char('i'), options());
        v.process(&mut b, VimInput::Char('w'), options());
        assert_eq!(v.mode, VimMode::Insert);
        chars(&mut b, &mut v, "qux");
        assert_eq!(b.text(), "foo qux baz\n");
    }

    #[test]
    fn diw_deletes_inner_word() {
        let mut b = buffer("foo bar baz\n");
        let mut v = state();
        v.process(&mut b, VimInput::Char('w'), options());
        v.process(&mut b, VimInput::Char('d'), options());
        v.process(&mut b, VimInput::Char('i'), options());
        v.process(&mut b, VimInput::Char('w'), options());
        assert_eq!(b.text(), "foo  baz\n");
    }

    #[test]
    fn daw_deletes_a_word_with_trailing_space() {
        let mut b = buffer("foo bar baz\n");
        let mut v = state();
        v.process(&mut b, VimInput::Char('d'), options());
        v.process(&mut b, VimInput::Char('a'), options());
        v.process(&mut b, VimInput::Char('w'), options());
        assert_eq!(b.text(), "foo baz\n");
    }

    #[test]
    fn visual_line_delete_removes_lines() {
        let mut b = buffer("a\nb\nc\n");
        let mut v = state();
        chars(&mut b, &mut v, "Vjd");
        assert_eq!(b.text(), "c\n");
    }

    #[test]
    fn visual_charwise_yank_then_paste() {
        let mut b = buffer("hello world\n");
        let mut v = state();
        // v ll y → yank "hel"; then P at start.
        chars(&mut b, &mut v, "vlly");
        assert_eq!(v.register(), "hel");
        chars(&mut b, &mut v, "P");
        assert_eq!(b.text(), "helhello world\n");
    }

    #[test]
    fn search_and_n_next() {
        let mut b = buffer("foo bar foo baz\n");
        let mut v = state();
        chars(&mut b, &mut v, "/foo");
        assert_eq!(v.mode, VimMode::Search);
        let result = feed(&mut b, &mut v, VimInput::Key(NamedKey::Enter));
        assert_eq!(result.search.as_deref(), Some("foo"));
        assert_eq!(cursor(&b), (0, 0));
        v.process(&mut b, VimInput::Char('n'), options());
        assert_eq!(cursor(&b), (0, 8));
        v.process(&mut b, VimInput::Char('N'), options());
        assert_eq!(cursor(&b), (0, 0));
    }

    #[test]
    fn ex_write_quit_commands_are_reported() {
        let mut b = buffer("x\n");
        let mut v = state();
        chars(&mut b, &mut v, ":wq");
        let result = feed(&mut b, &mut v, VimInput::Key(NamedKey::Enter));
        assert_eq!(result.ex, Some(ExCommand::WriteQuit));
        assert_eq!(v.mode, VimMode::Normal);
        let _ = b;
    }

    #[test]
    fn undo_redo_route_through_the_buffer() {
        let mut b = buffer("one\n");
        let mut v = state();
        chars(&mut b, &mut v, "x");
        assert_eq!(b.text(), "ne\n");
        v.process(&mut b, VimInput::Char('u'), options());
        assert_eq!(b.text(), "one\n");
        v.process(&mut b, VimInput::Key(NamedKey::CtrlR), options());
        assert_eq!(b.text(), "ne\n");
    }

    #[test]
    fn gg_and_G_jump_lines() {
        let mut b = buffer("l1\nl2\nl3\n");
        let mut v = state();
        chars(&mut b, &mut v, "G");
        assert_eq!(cursor(&b).0, 2);
        chars(&mut b, &mut v, "gg");
        assert_eq!(cursor(&b).0, 0);
        chars(&mut b, &mut v, "2G");
        assert_eq!(cursor(&b).0, 1);
    }

    #[test]
    fn indent_and_outdent_lines() {
        let mut b = buffer("a\nb\n");
        let mut v = state();
        chars(&mut b, &mut v, ">>");
        assert_eq!(b.text(), "    a\nb\n");
        chars(&mut b, &mut v, "<<");
        assert_eq!(b.text(), "a\nb\n");
    }

    #[test]
    fn r_replaces_chars() {
        let mut b = buffer("aaa\n");
        let mut v = state();
        chars(&mut b, &mut v, "rz");
        assert_eq!(b.text(), "zaa\n");
    }

    #[test]
    fn tilde_toggles_case() {
        let mut b = buffer("aBc\n");
        let mut v = state();
        chars(&mut b, &mut v, "~");
        assert_eq!(b.text(), "ABc\n");
    }

    #[test]
    fn join_lines_adds_a_space() {
        let mut b = buffer("foo\nbar\n");
        let mut v = state();
        chars(&mut b, &mut v, "J");
        assert_eq!(b.text(), "foo bar\n");
    }

    #[test]
    fn f_find_and_semicolon_repeats() {
        let mut b = buffer("one two one\n");
        let mut v = state();
        chars(&mut b, &mut v, "fo");
        assert_eq!(cursor(&b), (0, 8));
        v.process(&mut b, VimInput::Char(';'), options());
        assert_eq!(cursor(&b), (0, 10));
    }

    #[test]
    fn gu_lowercases_a_word() {
        let mut b = buffer("ABC def\n");
        let mut v = state();
        chars(&mut b, &mut v, "guu");
        assert_eq!(b.text(), "abc def\n");
    }

    #[test]
    fn d_e_deletes_to_word_end() {
        let mut b = buffer("one two\n");
        let mut v = state();
        chars(&mut b, &mut v, "de");
        assert_eq!(b.text(), " two\n");
    }

    #[test]
    fn visual_o_swaps_ends() {
        let mut b = buffer("abcdef\n");
        let mut v = state();
        chars(&mut b, &mut v, "vlllo");
        let (anchor, head) = b.primary_cursor().normalize();
        assert_eq!((anchor.line, anchor.col), (0, 0));
        assert_eq!((head.line, head.col), (0, 3));
    }

    #[test]
    fn registers_survive_mode_switches() {
        let mut b = buffer("keep\nnext\n");
        let mut v = state();
        chars(&mut b, &mut v, "yy");
        assert_eq!(v.register(), "keep\n");
        feed(&mut b, &mut v, VimInput::Key(NamedKey::Escape));
        assert_eq!(v.register(), "keep\n");
    }
}
