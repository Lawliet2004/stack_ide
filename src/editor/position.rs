//! Coordinate spaces for the editor interaction model.
//!
//! | Space | Type | Column unit |
//! |-------|------|-------------|
//! | Editor caret / hit-test | [`CursorPosition`](super::buffer::CursorPosition) | Rust `char` index on the line |
//! | LSP wire (JSON-RPC) | [`LspPosition`] | UTF-16 code unit offset on the line |
//! | Search highlights | `byte_range` in `search.rs` | UTF-8 byte offset in the file |
//!
//! LSP positions always use UTF-16 code units. Editor cursor columns always use Rust
//! character indices (`str::chars` enumeration). To encode a char index, sum
//! [`char::len_utf16`](char::len_utf16) for the characters preceding the column via
//! [`char_column_to_utf16`]. Decode inbound LSP UTF-16 columns with
//! [`decode_utf16_column`] (diagnostic squiggles, text edits). Use
//! [`encode_char_column`] / [`decode_char_column`] for full positions;
//! [`lsp_utf16_range_char_span_on_line`] maps multiline LSP ranges to per-line char spans.
//! `lsp/transport.rs` does not reinterpret columns.
//!
//! # Examples
//!
//! | Line text | Rust `char` col | UTF-16 col | Note |
//! |-----------|-----------------|------------|------|
//! | `"az"` | 1 | 1 | ASCII is 1:1 |
//! | `"az"` | 2 | 2 | ASCII end of a two-character line |
//! | `"a🙂"` | 2 | 3 | Supplementary second character widens UTF-16 |
//! | `"abc"` | 2 | 2 | ASCII is 1:1 |
//! | `"fn main"` | 3 | 3 | ASCII is 1:1 |
//! | `"文字列"` | 2 | 2 | CJK is multibyte UTF-8 but one UTF-16 code unit per char |
//! | `"a🙂z"` | 2 | 3 | Supplementary `🙂` spans two UTF-16 code units |
//! | `"hello"` | 5 | 5 | End of line (ASCII) |
//! | `"a🙂z"` | 3 | 4 | End of line (char vs UTF-16 diverge) |
//! | `"a🙂z"` | 99 | 4 | Past end clamps to line length |
//! | `""` | 0 | 0 | Empty line end |
//! | `""` | 5 | 0 | Past end on empty line |
//!
//! Decode snaps interior UTF-16 offsets to the owning character's start:
//!
//! | Line text | UTF-16 col | Rust `char` col |
//! |-----------|------------|-----------------|
//! | `"a🙂z"` | 2 | 1 | Middle of `🙂` → index 1 |
//! | `"a🙂z"` | 3 | 2 | After `🙂` |
//!
//! # Position tests
//!
//! Part of crate-level **Regression tests** (`lib.rs`).
//!
//! Focused unit tests for UTF-16 ↔ Rust char-index conversion. Downstream modules
//! (`editor/buffer.rs`, `app.rs`, `problems_panel.rs`, `editor/widget.rs`) call these
//! helpers for LSP requests, diagnostic squiggles, and problems navigation; they do not
//! reimplement column math. `lsp/transport.rs` forwards encoded columns verbatim.
//!
//! | Area | Example tests | Run |
//! |------|---------------|-----|
//! | Documented examples | `documented_conversion_examples_match_module_tables` | `cargo test --lib editor::position` |
//! | Unicode/LSP position conversion (Always A18) | `unicode_lsp_position_conversion_is_correct` | `cargo test --lib unicode_lsp_position_conversion_is_correct` |
//! | ASCII encode (char → UTF-16) | `character_to_utf16_conversion_for_ascii` | `cargo test --lib character_to_utf16_conversion_for_ascii` |
//! | Emoji / surrogate pair | `conversion_with_emoji_surrogate_pair`, `supplementary_characters_*` | `cargo test --lib conversion_with_emoji_surrogate_pair` |
//! | BMP multibyte Unicode | `conversion_with_other_multibyte_unicode_characters` | `cargo test --lib conversion_with_other_multibyte_unicode` |
//! | Unicode + empty lines (no panic) | `keep_the_implementation_panic_free_for_unicode_and_empty_lines` | `cargo test --lib keep_the_implementation_panic_free_for_unicode_and_empty_lines` |
//! | End-of-line / clamped | `end_of_line_and_clamped_positions` | `cargo test --lib end_of_line_and_clamped` |
//! | Encode (char → UTF-16) | `char_column_to_utf16_*`, `encode_*` | `cargo test --lib editor::position` |
//! | Decode (UTF-16 → char) | `decode_utf16_column_*`, `decode_char_column` via round-trip tests | same |
//! | Clamp / non-panicking `u32` | `clamp_helpers_*`, `encode_and_decode_clamp_*`, `sum_utf16_len_saturates_*`, `char_column_to_utf16_accepts_unclamped_columns_without_panicking` | same |
//! | Diagnostic multiline ranges | `lsp_utf16_range_char_span_*` | same |
//! | Buffer ↔ LSP wire | `position_lsp_position_*`, `cursor_lsp_position_*`, `lsp_position_to_cursor_*`, `hover_request_position_*` | `cargo test --lib lsp_position` |
//! | App outbound requests | `unicode_earlier_on_the_same_line_does_not_offset_the_hover_request`, `unicode_earlier_on_the_same_line_does_not_offset_the_completion_request` | `cargo test --lib unicode_earlier_on_the_same_line_does_not_offset_the_hover`, `cargo test --lib unicode_earlier_on_the_same_line_does_not_offset_the_completion` |
//! | Never: raw char columns on LSP wire | `use_raw_character_columns_as_lsp_utf16_columns` | `cargo test --lib use_raw_character_columns_as_lsp_utf16_columns` |
//! | Problems navigation | `utf16_navigation_*` | `cargo test --lib utf16_navigation` |

/// 0-based line and UTF-16 code unit column (LSP `Position` encoding).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct LspPosition {
    pub line: u32,
    /// UTF-16 code unit offset from the start of the line (not a Rust `char` index).
    pub utf16_col: u32,
}

impl LspPosition {
    pub const fn new(line: u32, utf16_col: u32) -> Self {
        Self { line, utf16_col }
    }
}

/// Rust `char` count on one line (excludes the line break).
pub fn line_char_count(line_text: &str) -> usize {
    line_text.chars().count()
}

/// UTF-16 code unit length of one line (excludes the line break).
///
/// Returns [`u32`] using saturating addition so pathological line lengths never panic.
pub fn line_utf16_len(line_text: &str) -> u32 {
    sum_utf16_len(line_text.chars())
}

/// Clamp a Rust character index to `0..=line_char_count(line_text)`.
pub fn clamp_char_column(line_text: &str, char_col: usize) -> usize {
    char_col.min(line_char_count(line_text))
}

/// Clamp a UTF-16 column to `0..=line_utf16_len(line_text)`.
pub fn clamp_utf16_column(line_text: &str, utf16_col: u32) -> u32 {
    utf16_col.min(line_utf16_len(line_text))
}

/// UTF-16 code unit offset at a Rust character-index column on one line.
///
/// Sums [`char::len_utf16`](char::len_utf16) for the characters preceding the column
/// (none when `char_col` is 0). For ASCII and other BMP code points (including CJK and
/// accented Latin) each character is one UTF-16 code unit, so the column equals the char
/// index. Supplementary characters (emoji) widen UTF-16 only. `char_col` is clamped to
/// the line's character count; the running total uses saturating addition so the result is
/// always a plain [`u32`] without panicking.
///
/// # Examples
///
/// ```
/// use blue_ide::editor::position::char_column_to_utf16;
///
/// assert_eq!(char_column_to_utf16("az", 1), 1);
/// assert_eq!(char_column_to_utf16("az", 2), 2);
/// assert_eq!(char_column_to_utf16("a🙂", 2), 3);
/// assert_eq!(char_column_to_utf16("abc", 2), 2);
/// assert_eq!(char_column_to_utf16("fn main", 3), 3);
/// assert_eq!(char_column_to_utf16("a🙂z", 2), 3);
/// assert_eq!(char_column_to_utf16("a🙂z", 99), 4);
/// ```
pub fn char_column_to_utf16(line_text: &str, char_col: usize) -> u32 {
    let char_col = clamp_char_column(line_text, char_col);
    sum_utf16_len(line_text.chars().take(char_col))
}

/// Encode a line and Rust character-index column to an LSP position.
///
/// `line_text` must be the content of `line` without its line break. `char_col` is
/// clamped to the line's character count, then encoded via [`char_column_to_utf16`].
///
/// # Examples
///
/// ```
/// use blue_ide::editor::position::{encode_char_column, LspPosition};
///
/// assert_eq!(encode_char_column("az", 0, 1), LspPosition::new(0, 1));
/// assert_eq!(encode_char_column("az", 0, 2), LspPosition::new(0, 2));
/// assert_eq!(encode_char_column("a🙂", 0, 2), LspPosition::new(0, 3));
/// assert_eq!(encode_char_column("abc", 0, 2), LspPosition::new(0, 2));
/// assert_eq!(encode_char_column("fn main", 0, 3), LspPosition::new(0, 3));
/// assert_eq!(encode_char_column("a🙂z", 0, 2), LspPosition::new(0, 3));
/// assert_eq!(encode_char_column("a🙂z", 0, 99), LspPosition::new(0, 4));
/// ```
pub fn encode_char_column(line_text: &str, line: usize, char_col: usize) -> LspPosition {
    let char_col = clamp_char_column(line_text, char_col);
    LspPosition {
        line: u32::try_from(line).unwrap_or(u32::MAX),
        utf16_col: char_column_to_utf16(line_text, char_col),
    }
}

/// Decode a UTF-16 column on one line to a Rust character index.
///
/// Reverse of [`char_column_to_utf16`]. The column is clamped to the line length first.
/// Offsets that fall inside a supplementary character (the low surrogate of an emoji pair)
/// snap to that character's start index.
///
/// # Examples
///
/// ```
/// use blue_ide::editor::position::decode_utf16_column;
///
/// assert_eq!(decode_utf16_column("az", 1), 1);
/// assert_eq!(decode_utf16_column("a🙂z", 2), 1);
/// assert_eq!(decode_utf16_column("a🙂z", 3), 2);
/// ```
pub fn decode_utf16_column(line_text: &str, utf16_col: u32) -> usize {
    utf16_column_to_char_index(line_text, clamp_utf16_column(line_text, utf16_col))
}

/// Decode an LSP position to a Rust character index on `line_text`.
///
/// Only `position.utf16_col` is used; `position.line` is ignored because `line_text` is
/// already line-local. Prefer [`decode_utf16_column`] when you already have the line text.
///
/// # Examples
///
/// ```
/// use blue_ide::editor::position::{decode_char_column, LspPosition};
///
/// assert_eq!(decode_char_column("az", LspPosition::new(0, 1)), 1);
/// assert_eq!(decode_char_column("az", LspPosition::new(0, 2)), 2);
/// assert_eq!(decode_char_column("a🙂", LspPosition::new(0, 3)), 2);
/// assert_eq!(decode_char_column("abc", LspPosition::new(0, 2)), 2);
/// assert_eq!(decode_char_column("fn main", LspPosition::new(0, 3)), 3);
/// assert_eq!(decode_char_column("a🙂z", LspPosition::new(0, 2)), 1);
/// assert_eq!(decode_char_column("a🙂z", LspPosition::new(0, 3)), 2);
/// ```
pub fn decode_char_column(line_text: &str, position: LspPosition) -> usize {
    decode_utf16_column(line_text, position.utf16_col)
}

/// Map one line of an LSP UTF-16 range to a Rust character-index span `[start, end)`.
///
/// Returns `None` when `line_index` falls outside the range or the span is empty. Used by
/// diagnostic squiggle rendering (reverse of outbound [`encode_char_column`]).
pub fn lsp_utf16_range_char_span_on_line(
    line_index: usize,
    line_text: &str,
    start_line: u32,
    start_utf16_col: u32,
    end_line: u32,
    end_utf16_col: u32,
) -> Option<(usize, usize)> {
    let start_line = usize::try_from(start_line).ok()?;
    let end_line = usize::try_from(end_line).ok()?;
    if start_line > end_line || line_index < start_line || line_index > end_line {
        return None;
    }

    let char_count = line_char_count(line_text);
    let start = if line_index == start_line {
        decode_utf16_column(line_text, start_utf16_col)
    } else {
        0
    };
    let end = if line_index == end_line {
        decode_utf16_column(line_text, end_utf16_col)
    } else {
        char_count
    };
    (start <= end).then_some((start, end))
}

fn sum_utf16_len(characters: impl Iterator<Item = char>) -> u32 {
    characters.fold(0_u32, |offset, character| {
        offset.saturating_add(character.len_utf16() as u32)
    })
}

fn utf16_column_to_char_index(line: &str, utf16_col: u32) -> usize {
    debug_assert!(utf16_col <= line_utf16_len(line));
    let mut current_utf16_offset = 0_u32;
    for (index, character) in line.chars().enumerate() {
        let next_offset = current_utf16_offset.saturating_add(character.len_utf16() as u32);
        if next_offset > utf16_col {
            return index;
        }
        current_utf16_offset = next_offset;
    }
    line_char_count(line)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::all)]
    use super::*;

    // Position encoding regressions — see module docs (Position tests).

    #[test]
    fn character_to_utf16_conversion_for_ascii() {
        for ch in "abc fn main az hello".chars() {
            assert_eq!(ch.len_utf16(), 1, "ASCII must be one UTF-16 code unit");
        }

        assert_eq!(char_column_to_utf16("abc", 0), 0);
        assert_eq!(char_column_to_utf16("abc", 1), 1);
        assert_eq!(char_column_to_utf16("abc", 2), 2);
        assert_eq!(char_column_to_utf16("abc", 3), 3);

        assert_eq!(char_column_to_utf16("az", 1), 1);
        assert_eq!(char_column_to_utf16("az", 2), 2);

        assert_eq!(char_column_to_utf16("fn main", 3), 3);

        assert_eq!(encode_char_column("abc", 0, 2), LspPosition::new(0, 2));
        assert_eq!(encode_char_column("az", 0, 1), LspPosition::new(0, 1));
        assert_eq!(encode_char_column("az", 0, 2), LspPosition::new(0, 2));
        assert_eq!(encode_char_column("fn main", 0, 3), LspPosition::new(0, 3));

        let ascii_line = "hello";
        for char_col in 0..=ascii_line.chars().count() {
            assert_eq!(
                char_column_to_utf16(ascii_line, char_col),
                char_col as u32,
                "ASCII char index {char_col} should equal UTF-16 column"
            );
            let lsp = encode_char_column(ascii_line, 0, char_col);
            assert_eq!(lsp.utf16_col, char_col as u32);
            assert_eq!(
                decode_char_column(ascii_line, lsp),
                char_col,
                "ASCII column {char_col} should round-trip"
            );
        }
    }

    #[test]
    fn documented_conversion_examples_match_module_tables() {
        assert_eq!(char_column_to_utf16("az", 1), 1);
        assert_eq!(char_column_to_utf16("az", 2), 2);
        assert_eq!(char_column_to_utf16("a🙂", 2), 3);
        assert_eq!(char_column_to_utf16("abc", 2), 2);
        assert_eq!(char_column_to_utf16("fn main", 3), 3);
        assert_eq!(char_column_to_utf16("a🙂z", 2), 3);
        assert_eq!(char_column_to_utf16("a🙂z", 99), 4);
        assert_eq!(char_column_to_utf16("", 5), 0);

        assert_eq!(encode_char_column("az", 0, 1), LspPosition::new(0, 1));
        assert_eq!(encode_char_column("az", 0, 2), LspPosition::new(0, 2));
        assert_eq!(encode_char_column("a🙂", 0, 2), LspPosition::new(0, 3));
        assert_eq!(encode_char_column("abc", 0, 2), LspPosition::new(0, 2));
        assert_eq!(encode_char_column("fn main", 0, 3), LspPosition::new(0, 3));
        assert_eq!(encode_char_column("a🙂z", 0, 2), LspPosition::new(0, 3));
        assert_eq!(encode_char_column("a🙂z", 0, 99), LspPosition::new(0, 4));
        assert_eq!(encode_char_column("", 0, 5), LspPosition::new(0, 0));

        assert_eq!(decode_char_column("az", LspPosition::new(0, 1)), 1);
        assert_eq!(decode_char_column("az", LspPosition::new(0, 2)), 2);
        assert_eq!(decode_char_column("a🙂", LspPosition::new(0, 3)), 2);
        assert_eq!(decode_char_column("abc", LspPosition::new(0, 2)), 2);
        assert_eq!(decode_char_column("a🙂z", LspPosition::new(0, 2)), 1);
        assert_eq!(decode_char_column("a🙂z", LspPosition::new(0, 3)), 2);
    }

    #[test]
    fn encode_and_decode_char_column_cover_ascii_empty_and_unicode() {
        let ascii_line = "hello";
        assert_eq!(encode_char_column(ascii_line, 0, 0).utf16_col, 0);
        assert_eq!(encode_char_column(ascii_line, 0, 3).utf16_col, 3);
        assert_eq!(encode_char_column(ascii_line, 0, 5).utf16_col, 5);
        assert_eq!(encode_char_column(ascii_line, 0, 10).utf16_col, 5);

        assert_eq!(decode_char_column(ascii_line, LspPosition::new(0, 0)), 0);
        assert_eq!(decode_char_column(ascii_line, LspPosition::new(0, 3)), 3);
        assert_eq!(decode_char_column(ascii_line, LspPosition::new(0, 5)), 5);
        assert_eq!(decode_char_column(ascii_line, LspPosition::new(0, 10)), 5);

        let empty_line = "";
        assert_eq!(encode_char_column(empty_line, 0, 0).utf16_col, 0);
        assert_eq!(encode_char_column(empty_line, 0, 5).utf16_col, 0);
        assert_eq!(decode_char_column(empty_line, LspPosition::new(0, 0)), 0);
        assert_eq!(decode_char_column(empty_line, LspPosition::new(0, 5)), 0);

        let emoji_line = "a🙂z";
        assert_eq!(encode_char_column(emoji_line, 0, 2).utf16_col, 3);
        assert_eq!(decode_char_column(emoji_line, LspPosition::new(0, 2)), 1);
        assert_eq!(decode_char_column(emoji_line, LspPosition::new(0, 3)), 2);

        let mixed_line = "Rust 🙂 code 🦀";
        assert_eq!(encode_char_column(mixed_line, 0, 6).utf16_col, 7);
        assert_eq!(decode_char_column(mixed_line, LspPosition::new(0, 6)), 5);
        assert_eq!(decode_char_column(mixed_line, LspPosition::new(0, 15)), 13);
    }

    #[test]
    fn supplementary_characters_diverge_char_index_from_utf16_column() {
        let line = "a🙂z";
        let lsp = encode_char_column(line, 0, 2);
        assert_eq!(lsp.utf16_col, 3);
        assert_ne!(lsp.utf16_col, 2);
        assert_eq!(decode_char_column(line, lsp), 2);
    }

    #[test]
    fn conversion_with_emoji_surrogate_pair() {
        let emoji = '🙂';
        assert_eq!(
            emoji.len_utf16(),
            2,
            "supplementary emoji must occupy a UTF-16 surrogate pair"
        );

        let line = "a🙂z";
        assert_eq!(line_char_count(line), 3);
        assert_eq!(line_utf16_len(line), 4);

        assert_eq!(char_column_to_utf16("a🙂", 2), 3);
        assert_eq!(char_column_to_utf16(line, 0), 0);
        assert_eq!(char_column_to_utf16(line, 1), 1);
        assert_eq!(char_column_to_utf16(line, 2), 3);
        assert_eq!(char_column_to_utf16(line, 3), 4);

        assert_eq!(encode_char_column(line, 0, 2), LspPosition::new(0, 3));
        assert_ne!(
            encode_char_column(line, 0, 2).utf16_col,
            2,
            "char index past emoji must not equal UTF-16 column"
        );

        assert_eq!(
            decode_utf16_column(line, 2),
            1,
            "low surrogate snaps to emoji start"
        );
        assert_eq!(
            decode_utf16_column(line, 3),
            2,
            "offset after surrogate pair"
        );
        assert_eq!(decode_char_column(line, LspPosition::new(0, 2)), 1);
        assert_eq!(decode_char_column(line, LspPosition::new(0, 3)), 2);

        for (char_col, utf16_col) in [(0, 0), (1, 1), (2, 3), (3, 4)] {
            let lsp = encode_char_column(line, 0, char_col);
            assert_eq!(lsp.utf16_col, utf16_col, "char col {char_col}");
            assert_eq!(
                decode_char_column(line, lsp),
                char_col,
                "round-trip char col {char_col}"
            );
        }

        let second_emoji = "a😀b";
        assert_eq!(char_column_to_utf16(second_emoji, 2), 3);
        assert_eq!(decode_utf16_column(second_emoji, 2), 1);
        assert_eq!(decode_utf16_column(second_emoji, 3), 2);
    }

    #[test]
    fn conversion_with_other_multibyte_unicode_characters() {
        let cjk = "文字列";
        for ch in cjk.chars() {
            assert_eq!(ch.len_utf16(), 1, "BMP CJK must be one UTF-16 code unit");
            assert!(ch.len_utf8() > 1, "CJK is multibyte in UTF-8");
        }
        assert!(cjk.len() > cjk.chars().count());
        assert_eq!(line_char_count(cjk), line_utf16_len(cjk) as usize);

        for char_col in 0..=cjk.chars().count() {
            assert_eq!(
                char_column_to_utf16(cjk, char_col),
                char_col as u32,
                "CJK char index {char_col} should equal UTF-16 column"
            );
            let lsp = encode_char_column(cjk, 0, char_col);
            assert_eq!(lsp.utf16_col, char_col as u32);
            assert_eq!(decode_char_column(cjk, lsp), char_col);
            assert_eq!(decode_utf16_column(cjk, char_col as u32), char_col);
        }

        let mixed = "fn 中文()";
        assert_eq!(char_column_to_utf16(mixed, 3), 3);
        assert_eq!(char_column_to_utf16(mixed, 4), 4);
        assert_eq!(char_column_to_utf16(mixed, 5), 5);
        assert_eq!(encode_char_column(mixed, 0, 4), LspPosition::new(0, 4));
        assert_eq!(
            decode_char_column(mixed, LspPosition::new(0, 4)),
            4,
            "middle CJK character"
        );

        let accented = "café naïve";
        for ch in accented.chars().filter(|ch| !ch.is_ascii()) {
            assert_eq!(
                ch.len_utf16(),
                1,
                "accented BMP Latin is one UTF-16 code unit"
            );
            assert!(ch.len_utf8() > 1);
        }
        let accent_col = accented.chars().count();
        assert_eq!(
            char_column_to_utf16(accented, accent_col),
            accent_col as u32
        );
        assert_eq!(
            decode_char_column(accented, encode_char_column(accented, 0, accent_col)),
            accent_col
        );
    }

    #[test]
    fn end_of_line_and_clamped_positions() {
        let empty = "";
        assert_eq!(line_char_count(empty), 0);
        assert_eq!(line_utf16_len(empty), 0);
        assert_eq!(char_column_to_utf16(empty, 0), 0);
        assert_eq!(encode_char_column(empty, 0, 0), LspPosition::new(0, 0));
        assert_eq!(decode_utf16_column(empty, 0), 0);
        assert_eq!(encode_char_column(empty, 0, 99), LspPosition::new(0, 0));
        assert_eq!(decode_utf16_column(empty, 99), 0);

        let ascii = "hello";
        let ascii_chars = line_char_count(ascii);
        let ascii_utf16 = line_utf16_len(ascii);
        assert_eq!(ascii_chars, 5);
        assert_eq!(ascii_utf16, 5);
        assert_eq!(char_column_to_utf16(ascii, ascii_chars), ascii_utf16);
        assert_eq!(decode_utf16_column(ascii, ascii_utf16), ascii_chars);
        assert_eq!(
            encode_char_column(ascii, 0, ascii_chars).utf16_col,
            ascii_utf16
        );
        assert_eq!(
            decode_char_column(ascii, LspPosition::new(0, ascii_utf16)),
            ascii_chars
        );

        let emoji = "a🙂z";
        let emoji_chars = line_char_count(emoji);
        let emoji_utf16 = line_utf16_len(emoji);
        assert_eq!(emoji_chars, 3);
        assert_eq!(emoji_utf16, 4);
        assert_eq!(char_column_to_utf16(emoji, emoji_chars), emoji_utf16);
        assert_eq!(decode_utf16_column(emoji, emoji_utf16), emoji_chars);
        assert_eq!(
            encode_char_column(emoji, 0, emoji_chars).utf16_col,
            emoji_utf16,
            "char EOL must map to UTF-16 EOL even when counts diverge"
        );

        let cjk = "文字列";
        let cjk_chars = line_char_count(cjk);
        assert_eq!(char_column_to_utf16(cjk, cjk_chars), cjk_chars as u32);

        for (line, char_eol, utf16_eol) in [
            (ascii, 5_usize, 5_u32),
            (emoji, 3, 4),
            (cjk, 3, 3),
            (empty, 0, 0),
        ] {
            assert_eq!(clamp_char_column(line, 99), char_eol);
            assert_eq!(clamp_utf16_column(line, 99), utf16_eol);
            assert_eq!(char_column_to_utf16(line, 99), utf16_eol);
            assert_eq!(char_column_to_utf16(line, usize::MAX), utf16_eol);
            assert_eq!(decode_utf16_column(line, 99), char_eol);
            assert_eq!(decode_utf16_column(line, u32::MAX), char_eol);
            assert_eq!(encode_char_column(line, 0, 99).utf16_col, utf16_eol);
            assert_eq!(decode_char_column(line, LspPosition::new(0, 99)), char_eol);
        }

        assert_eq!(
            lsp_utf16_range_char_span_on_line(0, emoji, 0, 0, 0, 99),
            Some((0, 3)),
            "clamped UTF-16 end column should span through char EOL"
        );
    }

    #[test]
    fn encode_decode_round_trips_rust_char_columns_on_a_line() {
        let line = "fn ma🙂in()";
        for char_col in 0..=line.chars().count() {
            let lsp = encode_char_column(line, 0, char_col);
            assert_eq!(lsp.line, 0);
            assert_eq!(
                decode_char_column(line, lsp),
                char_col,
                "char index {char_col} should round-trip"
            );
        }
    }

    #[test]
    fn encode_preserves_line_number_in_lsp_position() {
        let lsp = encode_char_column("abc", 7, 1);
        assert_eq!(lsp, LspPosition::new(7, 1));
    }

    #[test]
    fn char_column_to_utf16_sums_len_utf16_of_preceding_characters() {
        let line = "a🙂🦀z";
        assert_eq!(char_column_to_utf16(line, 0), 0);
        assert_eq!(char_column_to_utf16(line, 1), 'a'.len_utf16() as u32);
        assert_eq!(
            char_column_to_utf16(line, 2),
            ('a'.len_utf16() + '🙂'.len_utf16()) as u32
        );
        assert_eq!(
            char_column_to_utf16(line, 3),
            ('a'.len_utf16() + '🙂'.len_utf16() + '🦀'.len_utf16()) as u32
        );
        assert_eq!(char_column_to_utf16(line, 4), line_utf16_len(line));

        let cjk = "fn 中文()";
        assert_eq!(char_column_to_utf16(cjk, 3), 3);
        assert_eq!(char_column_to_utf16(cjk, 4), 4);
        assert_eq!(char_column_to_utf16(cjk, 5), 5);
    }

    #[test]
    fn char_column_to_utf16_accepts_unclamped_columns_without_panicking() {
        let line = "hello";
        assert_eq!(char_column_to_utf16(line, 100), 5);
        assert_eq!(char_column_to_utf16(line, usize::MAX), 5);
    }

    #[test]
    fn sum_utf16_len_saturates_at_u32_max_instead_of_overflowing() {
        let almost_full = u32::MAX - 1;
        assert_eq!(
            almost_full.saturating_add('\u{10000}'.len_utf16() as u32),
            u32::MAX,
            "saturating addition must not panic in debug builds"
        );
        assert_eq!(sum_utf16_len("a\u{10000}".chars()), 3);
    }

    #[test]
    fn clamp_helpers_bound_columns_to_line_lengths() {
        let line = "a🙂z";
        assert_eq!(line_char_count(line), 3);
        assert_eq!(line_utf16_len(line), 4);
        assert_eq!(clamp_char_column(line, 99), 3);
        assert_eq!(clamp_utf16_column(line, 99), 4);
    }

    #[test]
    fn lsp_utf16_range_char_span_decodes_diagnostic_columns() {
        assert_eq!(
            lsp_utf16_range_char_span_on_line(0, "a🙂z", 0, 1, 0, 99),
            Some((1, 3))
        );
        assert_eq!(
            lsp_utf16_range_char_span_on_line(1, "other", 0, 1, 0, 99),
            None
        );
    }

    #[test]
    fn lsp_utf16_range_char_span_covers_multiline_intermediate_lines() {
        assert_eq!(
            lsp_utf16_range_char_span_on_line(1, "abcd", 1, 2, 3, 1),
            Some((2, 4))
        );
        assert_eq!(
            lsp_utf16_range_char_span_on_line(2, "middle", 1, 2, 3, 1),
            Some((0, 6))
        );
        assert_eq!(
            lsp_utf16_range_char_span_on_line(3, "end", 1, 2, 3, 1),
            Some((0, 1))
        );
    }

    #[test]
    fn decode_utf16_column_matches_decode_char_column() {
        let line = "a🙂z";
        for utf16_col in [0, 1, 2, 3, 4, 99] {
            assert_eq!(
                decode_utf16_column(line, utf16_col),
                decode_char_column(line, LspPosition::new(0, utf16_col)),
                "utf16 column {utf16_col} should decode the same"
            );
        }
    }

    #[test]
    fn encode_and_decode_clamp_past_line_end_safely() {
        let line = "a🙂z";
        assert_eq!(encode_char_column(line, 0, 99), LspPosition::new(0, 4));
        assert_eq!(decode_char_column(line, LspPosition::new(0, 99)), 3);
        assert_eq!(
            decode_char_column(line, LspPosition::new(0, u32::MAX)),
            3,
            "oversized UTF-16 columns must not index past the line"
        );
    }

    fn rust_source_without_comments(source: &str) -> String {
        source
            .lines()
            .filter(|line| {
                let trimmed = line.trim();
                !trimmed.starts_with("//")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn extract_fn_body(source: &str, fn_name: &str) -> Option<String> {
        let signature = format!("fn {fn_name}");
        let start = source.find(&signature)?;
        let brace_start = source[start..].find('{')? + start;
        let bytes = source.as_bytes();
        let mut depth = 0usize;
        let mut started = false;
        for (offset, byte) in bytes[brace_start..].iter().enumerate() {
            match byte {
                b'{' => {
                    depth += 1;
                    started = true;
                }
                b'}' => {
                    if started {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            let end = brace_start + offset;
                            return Some(source[start..=end].to_string());
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn assert_outbound_lsp_positions_use_utf16_not_raw_char_columns() {
        let position_rs = include_str!("position.rs");
        assert!(
            position_rs.contains("utf16_col: char_column_to_utf16(line_text, char_col)"),
            "encode_char_column must derive utf16_col from char indices"
        );
        assert!(
            position_rs.contains("UTF-16 code unit offset"),
            "LspPosition must document UTF-16 column units"
        );

        let buffer_rs = include_str!("buffer.rs");
        let buffer_production = buffer_rs
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(buffer_rs);
        let position_lsp_body = extract_fn_body(buffer_production, "position_lsp_position")
            .expect("position_lsp_position should exist");
        assert!(
            position_lsp_body.contains("encode_char_column"),
            "buffer outbound LSP positions must encode char columns to UTF-16"
        );

        let app_rs = include_str!("../app.rs");
        let app_production = app_rs
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(app_rs);
        let completion_body = extract_fn_body(app_production, "request_completion_at_cursor")
            .expect("request_completion_at_cursor should exist");
        assert!(
            completion_body.contains("buffer.cursor_lsp_position()"),
            "completion requests must encode cursor via cursor_lsp_position"
        );
        assert!(
            completion_body.contains("lsp_position.utf16_col"),
            "completion wire column must use utf16_col, not raw char index"
        );
        assert!(
            !rust_source_without_comments(&completion_body).contains("cursor.col,"),
            "completion must not pass raw cursor.col as the LSP column"
        );

        let lsp_mod = include_str!("../lsp/mod.rs");
        assert!(
            lsp_mod.contains("UTF-16 code unit column"),
            "LSP client API must document UTF-16 wire columns"
        );

        let transport_rs = include_str!("../lsp/transport.rs");
        assert!(
            transport_rs.contains("columns are UTF-16"),
            "transport must document UTF-16 range columns"
        );
    }

    /// Always boundary: Unicode/LSP position conversion must stay correct across encode,
    /// decode, buffer round-trips, inbound diagnostics/text edits, and outbound requests
    /// (see **Boundaries → Always** A2, A18).
    #[test]
    fn unicode_lsp_position_conversion_is_correct() {
        use crate::editor::buffer::{CursorPosition, TextBuffer};
        use crate::lsp::types::LspTextEdit;

        assert_outbound_lsp_positions_use_utf16_not_raw_char_columns();

        let app_rs = include_str!("../app.rs");
        assert!(
            app_rs
                .contains("fn unicode_earlier_on_the_same_line_does_not_offset_the_hover_request"),
            "app must regression-test hover UTF-16 encoding with earlier Unicode on the line"
        );
        assert!(
            app_rs.contains(
                "fn unicode_earlier_on_the_same_line_does_not_offset_the_completion_request"
            ),
            "app must regression-test completion UTF-16 encoding with earlier Unicode on the line"
        );

        let emoji_line = "a🙂z";
        assert_eq!(char_column_to_utf16(emoji_line, 2), 3);
        assert_eq!(decode_utf16_column(emoji_line, 2), 1);
        assert_eq!(decode_utf16_column(emoji_line, 3), 2);
        assert_eq!(encode_char_column(emoji_line, 0, 2), LspPosition::new(0, 3));
        assert_eq!(decode_char_column(emoji_line, LspPosition::new(0, 3)), 2);

        let cjk_line = "文字列";
        assert_eq!(char_column_to_utf16(cjk_line, 2), 2);

        let identifier_line = "let 🙂pri = 1;";
        let caret_after_pri = 8usize;
        let utf16_after_pri = char_column_to_utf16(identifier_line, caret_after_pri);
        assert_eq!(utf16_after_pri, 9);
        assert_ne!(
            utf16_after_pri, caret_after_pri as u32,
            "supplementary Unicode earlier on the line must widen the wire column"
        );

        let mut buffer = TextBuffer::from_text("a🙂z\n");
        buffer.set_cursor(CursorPosition { line: 0, col: 2 });
        let wire = buffer.cursor_lsp_position();
        assert_eq!(wire, LspPosition::new(0, 3));
        assert_eq!(
            buffer.lsp_position_to_cursor(wire),
            CursorPosition { line: 0, col: 2 }
        );
        assert_ne!(wire.utf16_col, buffer.cursor().col as u32);

        assert_eq!(
            lsp_utf16_range_char_span_on_line(0, emoji_line, 0, 1, 0, 3),
            Some((1, 2)),
            "diagnostic UTF-16 ranges must decode to the supplementary character span"
        );

        let mut edit_buffer = TextBuffer::from_text("a🙂z\n");
        edit_buffer
            .apply_lsp_text_edit(&LspTextEdit {
                line_start: 0,
                col_start: 1,
                line_end: 0,
                col_end: 3,
                new_text: "x".to_owned(),
            })
            .unwrap();
        assert_eq!(edit_buffer.text(), "axz\n");
    }

    /// Never boundary: Rust character indices must not be sent as LSP UTF-16 columns (see
    /// **Boundaries → Never** §13).
    #[test]
    fn use_raw_character_columns_as_lsp_utf16_columns() {
        use crate::editor::buffer::{CursorPosition, TextBuffer};

        assert_outbound_lsp_positions_use_utf16_not_raw_char_columns();

        let line = "a🙂z";
        let char_col = 2usize;
        let utf16_col = char_column_to_utf16(line, char_col);
        assert_eq!(utf16_col, 3, "emoji widens UTF-16 past the char index");
        assert_ne!(
            utf16_col, char_col as u32,
            "raw char index must differ from UTF-16 column on this line"
        );
        assert_eq!(
            encode_char_column(line, 0, char_col),
            LspPosition::new(0, utf16_col)
        );

        let mut buffer = TextBuffer::from_text("let 🙂pri = 1;\n");
        let cursor = CursorPosition { line: 0, col: 8 };
        buffer.set_cursor(cursor);
        let lsp = buffer.cursor_lsp_position();
        assert_eq!(
            lsp.utf16_col,
            char_column_to_utf16("let 🙂pri = 1;", cursor.col),
            "buffer wire position must use UTF-16 encoding"
        );
        assert_ne!(
            lsp.utf16_col, cursor.col as u32,
            "buffer must not expose raw char column as utf16_col after supplementary Unicode"
        );
    }
}
