#![allow(non_snake_case)]
use std::path::PathBuf;

// ── Inlay hint model ─────────────────────────────────────────────────────────

/// LSP 3.17 `InlayHintKind`. Values outside the spec map to `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InlayHintKind {
    Type,
    Parameter,
    #[default]
    Other,
}

/// A single text segment from an `InlayHintLabelPart` or a plain string label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlayHintLabelPart {
    /// Visible text for this part.
    pub value: String,
    /// Optional tooltip — either plaintext or Markdown.
    pub tooltip: Option<InlayHintTooltip>,
}

/// Tooltip content attached to a hint or label part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InlayHintTooltip {
    PlainText(String),
    Markdown(String),
}

/// One LSP inlay hint, normalised for rendering.
///
/// `position` is the LSP UTF-16 insertion point. `label` is a non-empty sequence of
/// parts (always at least one). `padding_left` / `padding_right` add a space on either
/// side in the visual layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspInlayHint {
    /// UTF-16 line/column where the hint is inserted (zero-based).
    pub position: crate::editor::position::LspPosition,
    /// Rendered label parts in wire order.
    pub label: Vec<InlayHintLabelPart>,
    pub kind: InlayHintKind,
    /// Optional hint-level tooltip (may differ from per-part tooltips).
    pub tooltip: Option<InlayHintTooltip>,
    pub padding_left: bool,
    pub padding_right: bool,
}

impl LspInlayHint {
    /// Concatenate all label part values into one display string.
    pub fn display_text(&self) -> String {
        self.label.iter().map(|p| p.value.as_str()).collect()
    }
}

// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum LspRequest {
    Initialize {
        root_uri: String,
    },
    DidOpen {
        path: PathBuf,
        language_id: String,
        text: String,
        version: i32,
    },
    DidChange {
        path: PathBuf,
        text: String,
        version: i32,
    },
    DidClose {
        path: PathBuf,
    },
    /// Encoded as `textDocument/completion` by `lsp/transport.rs`.
    Completion {
        path: PathBuf,
        line: u32,
        col: u32,
        /// Caller-owned UI correlation id. Independent of the JSON-RPC wire `id` allocated
        /// by `lsp/transport.rs`; echoed on [`LspResponse::CompletionList`].
        id: u64,
    },
    /// Encoded as `textDocument/hover` by `lsp/transport.rs`.
    Hover {
        path: PathBuf,
        line: u32,
        col: u32,
        /// Caller-owned UI correlation id. Independent of the JSON-RPC wire `id` allocated
        /// by `lsp/transport.rs`; echoed on [`LspResponse::HoverResult`].
        id: u64,
    },
    GotoDefinition {
        path: PathBuf,
        line: u32,
        col: u32,
        /// Caller-owned UI correlation id. Independent of the JSON-RPC wire `id` allocated
        /// by `lsp/transport.rs`; echoed on [`LspResponse::GotoResult`] / [`LspResponse::GotoNone`].
        id: u64,
    },
    /// Encoded as `textDocument/references` by `lsp/transport.rs`.
    References {
        path: PathBuf,
        line: u32,
        col: u32,
        /// Caller-owned UI correlation id. Independent of the JSON-RPC wire `id` allocated
        /// by `lsp/transport.rs`; echoed on [`LspResponse::ReferencesResult`].
        id: u64,
    },
    /// Encoded as `textDocument/prepareRename` by `lsp/transport.rs`.
    PrepareRename {
        path: PathBuf,
        line: u32,
        col: u32,
        /// Caller-owned UI correlation id. Independent of the JSON-RPC wire `id` allocated
        /// by `lsp/transport.rs`; echoed on [`LspResponse::PrepareRenameResult`].
        id: u64,
    },
    /// Encoded as `textDocument/rename` by `lsp/transport.rs`.
    Rename {
        path: PathBuf,
        line: u32,
        col: u32,
        new_name: String,
        /// Caller-owned UI correlation id. Independent of the JSON-RPC wire `id` allocated
        /// by `lsp/transport.rs`; echoed on [`LspResponse::RenameResult`].
        id: u64,
    },
    DocumentSymbol {
        path: PathBuf,
        id: u64,
    },
    /// Encoded as `textDocument/inlayHint` by `lsp/transport.rs`.
    InlayHint {
        path: PathBuf,
        /// Start line (0-based, inclusive). UTF-16 positions.
        start_line: u32,
        /// End line (0-based, exclusive). UTF-16 positions.
        end_line: u32,
        /// Caller-owned UI correlation id echoed on [`LspResponse::InlayHintResult`].
        id: u64,
    },
    /// Encoded as `textDocument/formatting` by `lsp/transport.rs`.
    Format {
        path: PathBuf,
        tab_size: u32,
        insert_spaces: bool,
        /// Caller-owned UI correlation id echoed on [`LspResponse::FormatResult`].
        id: u64,
    },
    /// Encoded as `textDocument/rangeFormatting` by `lsp/transport.rs`.
    RangeFormat {
        path: PathBuf,
        tab_size: u32,
        insert_spaces: bool,
        /// `(start_line, start_char, end_line, end_char)` in UTF-16 columns.
        range: (u32, u32, u32, u32),
        /// Caller-owned UI correlation id echoed on [`LspResponse::FormatResult`].
        id: u64,
    },
    /// Encoded as `textDocument/signatureHelp` by `lsp/transport.rs`.
    SignatureHelp {
        path: PathBuf,
        line: u32,
        col: u32,
        /// Caller-owned UI correlation id echoed on [`LspResponse::SignatureHelpResult`].
        id: u64,
    },
    /// Encoded as `workspace/symbol` by `lsp/transport.rs`.
    WorkspaceSymbol {
        query: String,
        /// Caller-owned UI correlation id echoed on [`LspResponse::WorkspaceSymbolResult`].
        id: u64,
    },
    /// Encoded as `textDocument/codeAction` by `lsp/transport.rs`.
    CodeAction {
        path: PathBuf,
        /// Range in UTF-16 columns: (start_line, start_char, end_line, end_char).
        range: (u32, u32, u32, u32),
        diagnostics: Vec<LspDiagnostic>,
        /// Caller-owned UI correlation id echoed on [`LspResponse::CodeActionResult`].
        id: u64,
    },
    CodeLens {
        path: PathBuf,
        id: u64,
    },
    CodeLensResolve {
        item: CodeLensItem,
        id: u64,
    },
    SemanticTokensFull {
        path: PathBuf,
        id: u64,
    },
    SemanticTokensRange {
        path: PathBuf,
        start_line: u32,
        end_line: u32,
        id: u64,
    },
    PrepareCallHierarchy {
        path: PathBuf,
        line: u32,
        col: u32,
        id: u64,
    },
    IncomingCalls {
        item: CallHierarchyItem,
        id: u64,
    },
    OutgoingCalls {
        item: CallHierarchyItem,
        id: u64,
    },
    PrepareTypeHierarchy {
        path: PathBuf,
        line: u32,
        col: u32,
        id: u64,
    },
    Supertypes {
        item: TypeHierarchyItem,
        id: u64,
    },
    Subtypes {
        item: TypeHierarchyItem,
        id: u64,
    },
    ExecuteCommand {
        command: String,
        args: serde_json::Value,
        id: u64,
    },
    Shutdown,
}

#[derive(Debug)]
pub enum LspResponse {
    Initialized {
        token_types: Vec<String>,
    },
    Diagnostics {
        path: PathBuf,
        diagnostics: Vec<LspDiagnostic>,
    },
    CompletionList {
        /// UI correlation id from the outbound [`LspRequest::Completion`], not the wire JSON-RPC `id`.
        id: u64,
        items: Vec<LspCompletionItem>,
    },
    /// Flattened hover documentation. `content` is built by `lsp/transport.rs`; display
    /// policy and popup rendering live in `editor/hover.rs` / `app.rs`.
    HoverResult {
        /// UI correlation id from the outbound [`LspRequest::Hover`], not the wire JSON-RPC `id`.
        id: u64,
        content: String,
    },
    GotoResult {
        /// UI correlation id from the outbound [`LspRequest::GotoDefinition`], not the wire JSON-RPC `id`.
        id: u64,
        path: PathBuf,
        line: u32,
        col: u32,
    },
    GotoNone {
        /// UI correlation id from the outbound [`LspRequest::GotoDefinition`], not the wire JSON-RPC `id`.
        id: u64,
    },
    ReferencesResult {
        /// UI correlation id from the outbound [`LspRequest::References`], not the wire JSON-RPC `id`.
        id: u64,
        locations: Vec<ReferenceLocation>,
    },
    PrepareRenameResult {
        /// UI correlation id from the outbound [`LspRequest::PrepareRename`], not the wire JSON-RPC `id`.
        id: u64,
        /// Range of the symbol that can be renamed (UTF-16 columns).
        range: Option<(u32, u32, u32, u32)>,
    },
    RenameResult {
        /// UI correlation id from the outbound [`LspRequest::Rename`], not the wire JSON-RPC `id`.
        id: u64,
        /// All file edits from the workspace edit.
        edits: Vec<FileEdit>,
    },
    SymbolList {
        id: u64,
        path: PathBuf,
        symbols: Vec<OutlineNode>,
    },
    /// Result of `textDocument/inlayHint`.
    InlayHintResult {
        /// Caller-owned UI correlation id from the outbound [`LspRequest::InlayHint`].
        id: u64,
        hints: Vec<LspInlayHint>,
    },
    /// Result of `textDocument/formatting` or `textDocument/rangeFormatting`.
    FormatResult {
        /// Caller-owned UI correlation id from the outbound [`LspRequest::Format`] / [`LspRequest::RangeFormat`].
        id: u64,
        /// Text edits to apply, in LSP UTF-16 coordinates. Apply in reverse order.
        edits: Vec<TextEdit>,
    },
    /// Result of `textDocument/signatureHelp`.
    SignatureHelpResult {
        /// Caller-owned UI correlation id from the outbound [`LspRequest::SignatureHelp`].
        id: u64,
        /// Active signature, if any.
        active: Option<SignatureInfo>,
    },
    /// Result of `workspace/symbol`.
    WorkspaceSymbolResult {
        /// Caller-owned UI correlation id.
        id: u64,
        /// Matched symbols across all roots.
        symbols: Vec<WorkspaceSymbol>,
    },
    /// Result of `textDocument/codeAction`.
    CodeActionResult {
        /// Caller-owned UI correlation id.
        id: u64,
        /// Available actions at the requested range.
        actions: Vec<CodeAction>,
    },
    CodeLensResult {
        id: u64,
        lenses: Vec<CodeLensItem>,
    },
    SemanticTokensResult {
        id: u64,
        tokens: Vec<SemanticToken>,
    },
    CallHierarchyPrepareResult {
        id: u64,
        items: Vec<CallHierarchyItem>,
    },
    IncomingCallsResult {
        id: u64,
        calls: Vec<IncomingCall>,
    },
    OutgoingCallsResult {
        id: u64,
        calls: Vec<OutgoingCall>,
    },
    TypeHierarchyPrepareResult {
        id: u64,
        items: Vec<TypeHierarchyItem>,
    },
    SupertypesResult {
        id: u64,
        items: Vec<TypeHierarchyItem>,
    },
    SubtypesResult {
        id: u64,
        items: Vec<TypeHierarchyItem>,
    },
    Progress {
        token: String,
        kind: ProgressKind,
        title: String,
        message: Option<String>,
        percentage: Option<u32>,
    },
    ServerMessage {
        level: MessageLevel,
        message: String,
    },
    Error {
        /// UI correlation id from the outbound position request, not the wire JSON-RPC `id`.
        id: u64,
        message: String,
    },
    ServerUnavailable {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    Impl,
    Constant,
    Module,
    Field,
    Other,
}

impl SymbolKind {
    pub fn icon_text(&self) -> &'static str {
        match self {
            SymbolKind::Function => "fn",
            SymbolKind::Struct => "st",
            SymbolKind::Enum => "en",
            SymbolKind::Trait => "tr",
            SymbolKind::Impl => "im",
            SymbolKind::Constant => "co",
            SymbolKind::Module => "mo",
            SymbolKind::Field => "fi",
            SymbolKind::Other => "·",
        }
    }

    pub fn icon_color(&self, palette: &crate::theme::ThemePalette) -> egui::Color32 {
        match self {
            SymbolKind::Function => palette.syntax.function,
            SymbolKind::Struct => palette.syntax.type_name,
            SymbolKind::Enum => palette.syntax.type_name,
            SymbolKind::Trait => egui::Color32::from_rgb(220, 220, 0),
            SymbolKind::Impl => egui::Color32::from_rgb(235, 120, 30),
            SymbolKind::Constant => egui::Color32::from_rgb(40, 167, 69),
            SymbolKind::Module => palette.semantic.completion_module,
            SymbolKind::Field => palette.semantic.muted_text,
            SymbolKind::Other => palette.semantic.muted_text,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OutlineNode {
    pub name: String,
    pub kind: SymbolKind,
    pub line: usize,
    pub end_line: usize,
    pub children: Vec<OutlineNode>,
    pub expanded: bool,
}

#[derive(Debug, Clone)]
pub struct LspDiagnostic {
    pub line_start: u32,
    pub col_start: u32,
    pub line_end: u32,
    pub col_end: u32,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

/// Single-span text replacement from a completion `textEdit`.
///
/// UTF-16 columns. `new_text` is applied literally by `TextBuffer::apply_lsp_text_edit`;
/// snippet placeholder syntax is not expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspTextEdit {
    pub line_start: u32,
    pub col_start: u32,
    pub line_end: u32,
    pub col_end: u32,
    pub new_text: String,
}

/// One completion candidate after `lsp/transport.rs` wire normalization.
///
/// See `lsp/transport.rs` completion parsing docs for `textEdit` and snippet boundaries.
/// Both `insert_text` and `text_edit.new_text` may contain snippet-marker syntax; acceptance
/// always inserts literal characters (no tab-stop navigation).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LspCompletionItem {
    /// Popup display text from wire `label`. Retained even when `insert_text` differs.
    pub label: String,
    /// Optional popup kind from wire `kind`. Numeric values map to LSP labels; strings are kept verbatim.
    pub kind: Option<String>,
    /// Optional secondary popup line from wire `detail` or `labelDetails`.
    pub detail: Option<String>,
    /// Parse-time plain-path text: wire `insertText`, else `textEdit.newText`, else `label`.
    /// At acceptance, non-empty `insert_text` is used on the plain path; empty/missing values
    /// fall back to `label` via `completion_acceptance_insert_text`.
    pub insert_text: Option<String>,
    /// Single primary `textEdit` when present. Not a full LSP multi-edit transaction.
    pub text_edit: Option<LspTextEdit>,
    /// Optional filter text to match against. If absent, defaults to label.
    pub filter_text: Option<String>,
}

/// One reference location from textDocument/references response.
#[derive(Debug, Clone)]
pub struct ReferenceLocation {
    pub path: PathBuf,
    pub line_start: u32,
    pub col_start: u32,
    pub line_end: u32,
    pub col_end: u32,
    /// Preview of the line containing the reference, for display.
    pub line_text: Option<String>,
}

/// One file's edits from a WorkspaceEdit (textDocument/rename response).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEdit {
    pub path: PathBuf,
    pub edits: Vec<TextEdit>,
}

/// One text replacement from a WorkspaceEdit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub line_start: u32,
    pub col_start: u32,
    pub line_end: u32,
    pub col_end: u32,
    pub new_text: String,
}

// ─── Signature help types ────────────────────────────────────────────────────

/// A single parameter in a signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterInfo {
    /// Display label (either a substring of the signature label or standalone text).
    pub label: String,
    /// Optional documentation for this parameter.
    pub documentation: Option<String>,
}

/// One callable signature returned by `textDocument/signatureHelp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureInfo {
    /// Full signature label, e.g. `fn foo(a: i32, b: &str) -> bool`.
    pub label: String,
    /// Optional documentation for the whole signature.
    pub documentation: Option<String>,
    /// Ordered list of parameters.
    pub parameters: Vec<ParameterInfo>,
    /// 0-based index of the active parameter, if known.
    pub active_parameter: Option<usize>,
}

// ─── Workspace symbol types ──────────────────────────────────────────────────

/// One symbol returned by `workspace/symbol`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub path: PathBuf,
    pub line: u32,
    pub col: u32,
    /// Optional container name (module, struct, …).
    pub container: Option<String>,
}

// ─── Code action types ───────────────────────────────────────────────────────

/// The kind of a code action (mirrors LSP `CodeActionKind`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeActionKind {
    QuickFix,
    Refactor,
    RefactorExtract,
    RefactorInline,
    RefactorRewrite,
    Source,
    Other(String),
}

impl CodeActionKind {
    pub fn from_str(s: &str) -> Self {
        match s {
            "quickfix" => Self::QuickFix,
            "refactor" => Self::Refactor,
            "refactor.extract" => Self::RefactorExtract,
            "refactor.inline" => Self::RefactorInline,
            "refactor.rewrite" => Self::RefactorRewrite,
            "source" => Self::Source,
            other => Self::Other(other.to_owned()),
        }
    }

    pub fn display(&self) -> &str {
        match self {
            Self::QuickFix => "Quick Fix",
            Self::Refactor => "Refactor",
            Self::RefactorExtract => "Extract",
            Self::RefactorInline => "Inline",
            Self::RefactorRewrite => "Rewrite",
            Self::Source => "Source",
            Self::Other(s) => s.as_str(),
        }
    }
}

/// One code action returned by `textDocument/codeAction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeAction {
    pub title: String,
    pub kind: Option<CodeActionKind>,
    /// Pre-computed workspace edit. When `None` the client should send
    /// `codeAction/resolve` to obtain the edits (not yet implemented).
    pub edits: Vec<FileEdit>,
    /// Whether this is a preferred (primary) action.
    pub is_preferred: bool,
}

// ─── Code Lens types ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CodeLensItem {
    pub line: usize,
    pub entries: Vec<CodeLensEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CodeLensEntry {
    pub title: String,
    pub command: String,
    pub arguments: serde_json::Value,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
    #[serde(default)]
    pub range: Option<LspRange>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct LspRange {
    pub start: LspPositionWire,
    pub end: LspPositionWire,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct LspPositionWire {
    pub line: u32,
    pub character: u32,
}

// ─── Semantic Token types ───────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticToken {
    pub line: u32,
    pub col: u32,
    pub length: u32,
    pub token_type: String,
    pub modifiers: SemanticModifiers,
    pub color: egui::Color32,
    pub italic: bool,
    pub underline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SemanticModifiers {
    pub declaration: bool,
    pub definition: bool,
    pub readonly: bool,
    pub r#static: bool,
    pub deprecated: bool,
    pub r#abstract: bool,
    pub r#async: bool,
    pub modification: bool,
    pub documentation: bool,
    pub default_library: bool,
}

// ─── Call Hierarchy types ───────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CallHierarchyItem {
    pub name: String,
    pub kind: u64,
    #[serde(default)]
    pub detail: Option<String>,
    pub uri: String,
    pub range: LspRange,
    pub selectionRange: LspRange,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct IncomingCall {
    pub from: CallHierarchyItem,
    pub fromRanges: Vec<LspRange>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct OutgoingCall {
    pub to: CallHierarchyItem,
    pub fromRanges: Vec<LspRange>,
}

// ─── Type Hierarchy types ───────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct TypeHierarchyItem {
    pub name: String,
    pub kind: u64,
    #[serde(default)]
    pub detail: Option<String>,
    pub uri: String,
    pub range: LspRange,
    pub selectionRange: LspRange,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

// ─── LSP Progress / Notification types ──────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressKind {
    Begin,
    Work,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageLevel {
    Error,
    Warning,
    Info,
    Log,
}

#[derive(Debug, Clone)]
pub struct ProgressState {
    pub title: String,
    pub message: Option<String>,
    pub percentage: Option<u32>,
    pub kind: ProgressKind,
}
