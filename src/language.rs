use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LanguageId {
    Rust,
    Python,
    JavaScript,
    JavaScriptReact,
    TypeScript,
    TypeScriptReact,
    PlainText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LanguageServerId {
    Rust,
    Python,
    TypeScript,
}

impl LanguageId {
    pub fn from_path<P: AsRef<Path>>(path: P) -> Self {
        let path = path.as_ref();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        match ext.as_str() {
            "rs" => LanguageId::Rust,
            "py" | "pyi" => LanguageId::Python,
            "js" | "mjs" | "cjs" => LanguageId::JavaScript,
            "jsx" => LanguageId::JavaScriptReact,
            "ts" | "mts" | "cts" => LanguageId::TypeScript,
            "tsx" => LanguageId::TypeScriptReact,
            _ => LanguageId::PlainText,
        }
    }

    pub fn lsp_language_id(&self) -> Option<&'static str> {
        match self {
            LanguageId::Rust => Some("rust"),
            LanguageId::Python => Some("python"),
            LanguageId::JavaScript => Some("javascript"),
            LanguageId::JavaScriptReact => Some("javascriptreact"),
            LanguageId::TypeScript => Some("typescript"),
            LanguageId::TypeScriptReact => Some("typescriptreact"),
            LanguageId::PlainText => None,
        }
    }

    pub fn server_id(&self) -> Option<LanguageServerId> {
        match self {
            LanguageId::Rust => Some(LanguageServerId::Rust),
            LanguageId::Python => Some(LanguageServerId::Python),
            LanguageId::JavaScript
            | LanguageId::JavaScriptReact
            | LanguageId::TypeScript
            | LanguageId::TypeScriptReact => Some(LanguageServerId::TypeScript),
            LanguageId::PlainText => None,
        }
    }

    pub fn display_label(&self) -> &'static str {
        match self {
            LanguageId::Rust => "Rust",
            LanguageId::Python => "Python",
            LanguageId::JavaScript => "JavaScript",
            LanguageId::JavaScriptReact => "JavaScript JSX",
            LanguageId::TypeScript => "TypeScript",
            LanguageId::TypeScriptReact => "TypeScript TSX",
            LanguageId::PlainText => "Plain Text",
        }
    }

    pub fn has_syntax_parser(&self) -> bool {
        !matches!(self, LanguageId::PlainText)
    }

    pub fn all_extensions() -> &'static [&'static str] {
        &[
            "rs", "py", "pyi", "js", "mjs", "cjs", "jsx", "ts", "mts", "cts", "tsx",
        ]
    }
}

impl LanguageServerId {
    pub fn display_name(&self) -> &'static str {
        match self {
            LanguageServerId::Rust => "rust-analyzer",
            LanguageServerId::Python => "pyright-langserver",
            LanguageServerId::TypeScript => "typescript-language-server",
        }
    }
}
