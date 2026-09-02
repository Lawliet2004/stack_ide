use crate::language::LanguageId;
use crate::theme::SyntaxPalette;
use egui::{Color32, FontId, TextFormat};
use std::sync::OnceLock;
use tree_sitter::{Parser, Query, Tree};

static RUST_QUERY: OnceLock<Query> = OnceLock::new();
static PYTHON_QUERY: OnceLock<Query> = OnceLock::new();
static JAVASCRIPT_QUERY: OnceLock<Query> = OnceLock::new();
static TYPESCRIPT_QUERY: OnceLock<Query> = OnceLock::new();
static TSX_QUERY: OnceLock<Query> = OnceLock::new();

const RUST_HIGHLIGHT_QUERY: &str = r#"
    (line_comment) @comment
    (block_comment) @comment
    (string_literal) @string
    (raw_string_literal) @string
    (char_literal) @string
    (integer_literal) @number
    (float_literal) @number
    (boolean_literal) @keyword
    [
      "fn" "let" "pub" "use" "struct" "impl" "match" "if" "else"
      "return" "for" "while" "loop" "enum" "trait" "mod" "type" "where"
      "async" "await" "move" "const" "static"
      "as" "in" "ref" "unsafe"
    ] @keyword
    (self) @keyword
    (super) @keyword
    (crate) @keyword
    (mutable_specifier) @keyword
    (type_identifier) @type
    (primitive_type) @type
    (macro_invocation) @macro
    (macro_definition) @macro
    (lifetime) @lifetime
    (function_item name: (identifier) @function)
    (function_signature_item name: (identifier) @function)
    (call_expression function: (identifier) @function)
    (call_expression function: (field_expression field: (field_identifier) @function))
"#;

const PYTHON_HIGHLIGHT_QUERY: &str = r#"
    (comment) @comment
    (string) @string
    (integer) @number
    (float) @number
    [
      "def" "class" "import" "from" "as" "if" "elif" "else" "return"
      "for" "while" "break" "continue" "in" "is" "not" "and" "or"
      "try" "except" "finally" "raise" "assert" "with" "yield" "pass"
      "global" "nonlocal" "lambda" "del"
    ] @keyword
    (function_definition name: (identifier) @function)
    (class_definition name: (identifier) @type)
    (call function: (identifier) @function)
    (call function: (attribute attribute: (identifier) @function))
"#;

const JAVASCRIPT_HIGHLIGHT_QUERY: &str = r#"
    (comment) @comment
    (string) @string
    (template_string) @string
    (number) @number
    [
      "const" "let" "var" "function" "class" "extends" "export" "import"
      "from" "default" "if" "else" "return" "for" "while" "do" "switch"
      "case" "break" "continue" "new" "this" "typeof" "instanceof"
      "in" "of" "try" "catch" "finally" "throw" "async" "await" "yield"
    ] @keyword
    (function_declaration name: (identifier) @function)
    (method_definition name: (property_identifier) @function)
    (class_declaration name: (identifier) @type)
    (call_expression function: (identifier) @function)
    (call_expression function: (member_expression property: (property_identifier) @function))
"#;

const TYPESCRIPT_HIGHLIGHT_QUERY: &str = r#"
    (comment) @comment
    (string) @string
    (template_string) @string
    (number) @number
    [
      "const" "let" "var" "function" "class" "extends" "export" "import"
      "from" "default" "if" "else" "return" "for" "while" "do" "switch"
      "case" "break" "continue" "new" "this" "typeof" "instanceof"
      "in" "of" "try" "catch" "finally" "throw" "async" "await" "yield"
      "type" "interface" "namespace" "enum" "declare" "implements" "private"
      "public" "protected" "readonly" "keyof" "unique" "as"
    ] @keyword
    (mutable_specifier) @keyword
    (self) @keyword
    (super) @keyword
    (crate) @keyword
    (type_identifier) @type
    (predefined_type) @type
    (function_declaration name: (identifier) @function)
    (method_definition name: (property_identifier) @function)
    (class_declaration name: (identifier) @type)
    (interface_declaration name: (identifier) @type)
    (call_expression function: (identifier) @function)
    (call_expression function: (member_expression property: (property_identifier) @function))
"#;

fn get_rust_query() -> &'static Query {
    RUST_QUERY
        .get_or_init(|| Query::new(&tree_sitter_rust::language(), RUST_HIGHLIGHT_QUERY).unwrap())
}

fn get_python_query() -> &'static Query {
    PYTHON_QUERY.get_or_init(|| {
        Query::new(&tree_sitter_python::language(), PYTHON_HIGHLIGHT_QUERY).unwrap()
    })
}

fn get_javascript_query() -> &'static Query {
    JAVASCRIPT_QUERY.get_or_init(|| {
        Query::new(
            &tree_sitter_javascript::language(),
            JAVASCRIPT_HIGHLIGHT_QUERY,
        )
        .unwrap()
    })
}

fn get_typescript_query() -> &'static Query {
    TYPESCRIPT_QUERY.get_or_init(|| {
        Query::new(
            &tree_sitter_typescript::language_typescript(),
            TYPESCRIPT_HIGHLIGHT_QUERY,
        )
        .unwrap()
    })
}

fn get_tsx_query() -> &'static Query {
    TSX_QUERY.get_or_init(|| {
        Query::new(
            &tree_sitter_typescript::language_tsx(),
            TYPESCRIPT_HIGHLIGHT_QUERY,
        )
        .unwrap()
    })
}

fn capture_name_to_color(name: &str, palette: SyntaxPalette) -> Color32 {
    match name {
        "comment" => palette.comment,
        "string" => palette.string,
        "number" => palette.number,
        "keyword" => palette.keyword,
        "type" => palette.type_name,
        "macro" => palette.macro_name,
        "lifetime" => palette.lifetime,
        "function" => palette.function,
        "symbol" => palette.symbol,
        _ => palette.default,
    }
}

fn node_depth(node: tree_sitter::Node) -> usize {
    let mut depth = 0;
    let mut curr = node;
    while let Some(parent) = curr.parent() {
        depth += 1;
        curr = parent;
    }
    depth
}

pub struct Highlighter {
    parser: Parser,
    tree: Option<Tree>,
    language: LanguageId,
}

impl std::fmt::Debug for Highlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Highlighter")
            .field("language", &self.language)
            .field("tree_cached", &self.tree.is_some())
            .finish()
    }
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlighter {
    pub fn new() -> Self {
        let mut parser = Parser::new();
        let _ = parser.set_language(&tree_sitter_rust::language());
        Self {
            parser,
            tree: None,
            language: LanguageId::Rust,
        }
    }

    pub fn language(&self) -> LanguageId {
        self.language
    }

    pub fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    pub fn set_language(&mut self, language: LanguageId) {
        if self.language != language {
            self.language = language;
            self.tree = None;
            self.set_language_internal(language);
        }
    }

    fn set_language_internal(&mut self, language: LanguageId) {
        match language {
            LanguageId::Rust => {
                let _ = self.parser.set_language(&tree_sitter_rust::language());
            }
            LanguageId::Python => {
                let _ = self.parser.set_language(&tree_sitter_python::language());
            }
            LanguageId::JavaScript | LanguageId::JavaScriptReact => {
                let _ = self
                    .parser
                    .set_language(&tree_sitter_javascript::language());
            }
            LanguageId::TypeScript => {
                let _ = self
                    .parser
                    .set_language(&tree_sitter_typescript::language_typescript());
            }
            LanguageId::TypeScriptReact => {
                let _ = self
                    .parser
                    .set_language(&tree_sitter_typescript::language_tsx());
            }
            LanguageId::PlainText => {
                // PlainText has no parser grammar
            }
        }
    }

    pub fn highlight(
        &mut self,
        source: &str,
        font_id: FontId,
        palette: SyntaxPalette,
    ) -> egui::text::LayoutJob {
        if self.language == LanguageId::PlainText || source.is_empty() {
            let mut layout = egui::text::LayoutJob::default();
            if !source.is_empty() {
                layout.append(
                    source,
                    0.0,
                    TextFormat {
                        font_id,
                        color: palette.default,
                        ..Default::default()
                    },
                );
            }
            return layout;
        }

        // Parse with incremental parsing: always pass old tree
        let new_tree = match self.parser.parse(source, self.tree.as_ref()) {
            Some(tree) => tree,
            None => {
                eprintln!("tree-sitter parse failed, falling back to plain text");
                let mut layout = egui::text::LayoutJob::default();
                layout.append(
                    source,
                    0.0,
                    TextFormat {
                        font_id,
                        color: palette.default,
                        ..Default::default()
                    },
                );
                return layout;
            }
        };

        self.tree = Some(new_tree.clone());
        let root = new_tree.root_node();

        let query = match self.language {
            LanguageId::Rust => Some(get_rust_query()),
            LanguageId::Python => Some(get_python_query()),
            LanguageId::JavaScript | LanguageId::JavaScriptReact => Some(get_javascript_query()),
            LanguageId::TypeScript => Some(get_typescript_query()),
            LanguageId::TypeScriptReact => Some(get_tsx_query()),
            LanguageId::PlainText => None,
        };

        let mut byte_colors = vec![palette.default; source.len()];
        let mut byte_depths = vec![0; source.len()];

        if let Some(q) = query {
            let mut query_cursor = tree_sitter::QueryCursor::new();
            let matches = query_cursor.matches(q, root, source.as_bytes());
            for m in matches {
                for capture in m.captures {
                    let start = capture.node.start_byte();
                    let end = capture.node.end_byte();
                    let capture_name = &q.capture_names()[capture.index as usize];
                    let color = capture_name_to_color(capture_name, palette);
                    let depth = node_depth(capture.node);

                    let start_idx = start.min(source.len());
                    let end_idx = end.min(source.len());
                    for idx in start_idx..end_idx {
                        if depth >= byte_depths[idx] {
                            byte_colors[idx] = color;
                            byte_depths[idx] = depth;
                        }
                    }
                }
            }
        }

        let mut layout = egui::text::LayoutJob::default();
        let mut chars = source.char_indices().peekable();

        while let Some((start_byte, ch)) = chars.next() {
            let color = byte_colors[start_byte];
            let mut end_byte = start_byte + ch.len_utf8();

            while let Some(&(next_start, next_ch)) = chars.peek() {
                if byte_colors[next_start] == color {
                    end_byte = next_start + next_ch.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }

            layout.append(
                &source[start_byte..end_byte],
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    color,
                    ..Default::default()
                },
            );
        }

        layout
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Theme;
    use crate::theme::{built_in_theme, ColorScheme};

    fn palette() -> SyntaxPalette {
        built_in_theme(Theme::Dark, Some(ColorScheme::Dark))
            .palette
            .syntax
    }

    fn check_no_gaps(job: &egui::text::LayoutJob, source_len: usize) {
        let mut covered = vec![false; source_len];
        for section in &job.sections {
            let byte_range = section.byte_range.clone();
            for byte in byte_range {
                if byte >= source_len {
                    panic!(
                        "byte {} out of range for source length {}",
                        byte, source_len
                    );
                }
                if covered[byte] {
                    panic!("byte {} covered twice", byte);
                }
                covered[byte] = true;
            }
        }
        for (i, &covered_byte) in covered.iter().enumerate() {
            if !covered_byte {
                panic!("byte {} not covered", i);
            }
        }
    }

    #[test]
    fn empty_source_returns_empty_layout() {
        let mut highlighter = Highlighter::new();
        let font_id = FontId::monospace(14.0);
        let layout = highlighter.highlight("", font_id, palette());
        assert_eq!(layout.sections.len(), 0);
    }

    #[test]
    fn single_char_is_covered() {
        let mut highlighter = Highlighter::new();
        let font_id = FontId::monospace(14.0);
        let layout = highlighter.highlight("x", font_id, palette());
        check_no_gaps(&layout, 1);
        assert!(!layout.sections.is_empty());
    }

    #[test]
    fn diag_buffer_path_vs_direct_highlight() {
        let font_id = FontId::monospace(14.0);
        let pal = crate::theme::default_syntax_palette();
        let mut direct = Highlighter::new();
        let d1 = distinct(&direct.highlight("fn let if else", font_id.clone(), pal));
        let mut b = crate::editor::buffer::TextBuffer::from_text("fn let if else");
        let d2 = distinct(&b.get_layout(font_id.clone()));
        let mut b2 = crate::editor::buffer::TextBuffer::from_text("// hello");
        let d3 = distinct(&b2.get_layout(font_id));
        panic!(
            "diag: palette default={:?} keyword={:?} | direct={:?} | buffer_keywords={:?} | buffer_comment={:?}",
            pal.default, pal.keyword, d1, d2, d3
        );
    }

    fn distinct(job: &egui::text::LayoutJob) -> Vec<String> {
        let mut colors: Vec<u32> = job
            .sections
            .iter()
            .map(|s| {
                let c = s.format.color;
                ((c.r() as u32) << 16) | ((c.g() as u32) << 8) | c.b() as u32
            })
            .collect();
        colors.sort_unstable();
        colors.dedup();
        colors.iter().map(|c| format!("{c:#06x}")).collect()
    }

    #[test]
    fn keyword_is_colored_blue() {
        let mut highlighter = Highlighter::new();
        let font_id = FontId::monospace(14.0);
        let colors = palette();
        let layout = highlighter.highlight("let", font_id, colors);
        check_no_gaps(&layout, 3);
        assert!(layout
            .sections
            .iter()
            .any(|s| s.format.color == colors.keyword));
    }

    #[test]
    fn mixed_tokens_have_correct_colors() {
        let mut highlighter = Highlighter::new();
        let font_id = FontId::monospace(14.0);
        let source = "let x = 42;";
        let colors = palette();
        let layout = highlighter.highlight(source, font_id, colors);
        check_no_gaps(&layout, source.len());

        assert!(!layout.sections.is_empty());
        assert!(layout
            .sections
            .iter()
            .any(|s| s.format.color == colors.keyword));
        assert!(layout
            .sections
            .iter()
            .any(|s| s.format.color == colors.number));
    }

    #[test]
    fn comment_is_green() {
        let mut highlighter = Highlighter::new();
        let font_id = FontId::monospace(14.0);
        let source = "// hello";
        let colors = palette();
        let layout = highlighter.highlight(source, font_id, colors);
        check_no_gaps(&layout, 8);
        assert!(layout
            .sections
            .iter()
            .any(|s| s.format.color == colors.comment));
    }

    #[test]
    fn string_is_orange() {
        let mut highlighter = Highlighter::new();
        let font_id = FontId::monospace(14.0);
        let source = "\"test\"";
        let colors = palette();
        let layout = highlighter.highlight(source, font_id, colors);
        check_no_gaps(&layout, 6);
        assert!(layout
            .sections
            .iter()
            .any(|s| s.format.color == colors.string));
    }

    #[test]
    fn no_overlapping_or_missing_bytes() {
        let mut highlighter = Highlighter::new();
        let font_id = FontId::monospace(14.0);
        let source = "fn main() { let x = 42; }";
        let layout = highlighter.highlight(source, font_id, palette());
        check_no_gaps(&layout, source.len());
    }

    #[test]
    fn incremental_parse_produces_same_result() {
        let mut highlighter = Highlighter::new();
        let font_id = FontId::monospace(14.0);
        let source = "let x = 10;";

        let layout1 = highlighter.highlight(source, font_id.clone(), palette());
        let layout2 = highlighter.highlight(source, font_id, palette());

        assert_eq!(layout1.sections.len(), layout2.sections.len());
        for (s1, s2) in layout1.sections.iter().zip(layout2.sections.iter()) {
            assert_eq!(s1.format.color, s2.format.color);
            assert_eq!(s1.byte_range, s2.byte_range);
        }
    }

    #[test]
    fn large_function_parses_without_panic() {
        let mut highlighter = Highlighter::new();
        let font_id = FontId::monospace(14.0);
        let source = (0..100)
            .map(|i| format!("let var{} = {};\n", i, i))
            .collect::<String>();
        let layout = highlighter.highlight(&source, font_id, palette());
        check_no_gaps(&layout, source.len());
        assert!(!layout.sections.is_empty());
    }

    #[test]
    fn changing_palette_recolors_without_replacing_the_cached_tree() {
        let mut highlighter = Highlighter::new();
        let font_id = FontId::monospace(14.0);
        let dark = built_in_theme(Theme::Dark, None).palette.syntax;
        let light = built_in_theme(Theme::Light, None).palette.syntax;

        let dark_layout = highlighter.highlight("let value = 1;", font_id.clone(), dark);
        assert!(highlighter.tree.is_some());
        let light_layout = highlighter.highlight("let value = 1;", font_id, light);

        assert!(dark_layout
            .sections
            .iter()
            .any(|section| section.format.color == dark.keyword));
        assert!(light_layout
            .sections
            .iter()
            .any(|section| section.format.color == light.keyword));
    }

    #[test]
    fn test_compile_individual_keywords() {
        let keywords = vec![
            "fn", "let", "pub", "use", "struct", "impl", "match", "if", "else", "return", "for",
            "while", "loop", "enum", "trait", "mod", "type", "where", "self", "super", "crate",
            "async", "await", "move", "const", "static", "as", "in", "ref", "unsafe",
        ];
        for kw in keywords {
            let query_str = format!("\"{}\" @keyword", kw);
            let res = Query::new(&tree_sitter_rust::language(), &query_str);
            println!("kw: {}, ok: {}", kw, res.is_ok());
            if let Err(ref e) = res {
                println!("err: {:?}", e);
            }
        }
    }
}
