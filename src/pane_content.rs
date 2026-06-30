//! PaneContent — describes what a pane leaf is currently rendering.
//!
//! Each pane leaf in the `PaneTree` shows one `PaneContent` variant.
//! The app detects the content type on file open and assigns the correct variant.
//! A per-app `content_type_override` map lets users force a specific renderer.

use std::path::PathBuf;

/// What a pane leaf is currently rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneContent {
    /// Syntax-highlighted code editor — the default.
    CodeEditor { path: PathBuf },
    /// Visual image preview.
    ImageViewer { path: PathBuf },
    /// Rendered Markdown preview.
    MarkdownPreview { path: PathBuf },
    /// PDF page viewer.
    PdfViewer { path: PathBuf },
    /// Side-by-side or inline diff view.
    DiffViewer { left: DiffSource, right: DiffSource },
    /// Color picker overlay — rendered on top of a `CodeEditor`. Not a
    /// standalone pane; stored here so the pane knows to activate the overlay.
    ColorPickerOverlay { path: PathBuf },
    /// Nothing open yet.
    Empty,
}

/// One side of a diff view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffSource {
    /// A file on disk.
    File(PathBuf),
    /// A git revision of a file (e.g. "HEAD", "HEAD~1", commit SHA).
    GitRevision { path: PathBuf, rev: String },
    /// The current unsaved buffer content.
    Buffer(PathBuf),
}

impl DiffSource {
    /// Return the file path this diff source refers to.
    pub fn path(&self) -> &PathBuf {
        match self {
            DiffSource::File(p) => p,
            DiffSource::GitRevision { path, .. } => path,
            DiffSource::Buffer(p) => p,
        }
    }
}

impl PaneContent {
    /// Detect the best `PaneContent` variant for the given file path.
    /// Does NOT read file bytes — only inspects the extension. Call
    /// `detect_with_binary_check` when you also have the first 8 KB.
    pub fn detect_from_path(path: &PathBuf, markdown_preview_default: bool) -> Self {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tiff" => {
                PaneContent::ImageViewer { path: path.clone() }
            }
            "pdf" => PaneContent::PdfViewer { path: path.clone() },
            "md" | "markdown" => {
                if markdown_preview_default {
                    PaneContent::MarkdownPreview { path: path.clone() }
                } else {
                    PaneContent::CodeEditor { path: path.clone() }
                }
            }
            _ => PaneContent::CodeEditor { path: path.clone() },
        }
    }

    /// Returns the primary path associated with this content, if any.
    pub fn path(&self) -> Option<&PathBuf> {
        match self {
            PaneContent::CodeEditor { path }
            | PaneContent::ImageViewer { path }
            | PaneContent::MarkdownPreview { path }
            | PaneContent::PdfViewer { path }
            | PaneContent::ColorPickerOverlay { path } => Some(path),
            PaneContent::DiffViewer { left, .. } => Some(left.path()),
            PaneContent::Empty => None,
        }
    }

    /// Short tab prefix label for the content type (shown before the filename).
    pub fn tab_prefix(&self) -> &'static str {
        match self {
            PaneContent::CodeEditor { .. } => "",
            PaneContent::ImageViewer { .. } => "🖼 ",
            PaneContent::MarkdownPreview { .. } => "[md] ",
            PaneContent::PdfViewer { .. } => "[pdf] ",
            PaneContent::DiffViewer { .. } => "[diff] ",
            PaneContent::ColorPickerOverlay { .. } => "",
            PaneContent::Empty => "",
        }
    }

    /// True when the pane is showing a code editor (for keybinding decisions).
    pub fn is_code_editor(&self) -> bool {
        matches!(
            self,
            PaneContent::CodeEditor { .. } | PaneContent::ColorPickerOverlay { .. }
        )
    }

    /// True when a markdown toggle button should be shown.
    pub fn is_markdown_related(&self) -> bool {
        match self {
            PaneContent::MarkdownPreview { .. } => true,
            PaneContent::CodeEditor { path } => path
                .extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| {
                    matches!(extension.to_ascii_lowercase().as_str(), "md" | "markdown")
                })
                .unwrap_or(false),
            _ => false,
        }
    }
}

/// Check the first 8 KB of a file for null bytes — if found, it's binary.
pub fn is_binary(first_bytes: &[u8]) -> bool {
    first_bytes.contains(&0u8)
}
