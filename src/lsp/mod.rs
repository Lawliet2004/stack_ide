//! LSP client facade: process lifecycle, non-blocking `poll`, and typed request enqueue.
//! Does not encode/decode JSON-RPC (`transport.rs`), render UI (`hover.rs` / `app.rs`), or debounce hover.
//!
//! # LSP client tests
//!
//! Facade regressions; wire parsing in `transport.rs`. Index: **Regression tests** (`lib.rs`).
//!
//! Focused unit tests for the facade live in this module's `#[cfg(test)]` block. They exercise
//! request enqueue, `poll` routing, and diagnostics caching **without** a live language server
//! or manual IDE checks.
//!
//! | Area | Example tests | Run |
//! |------|---------------|-----|
//! | Request gating | `completion_and_hover_requests_require_a_running_server` | `cargo test --lib lsp::tests` |
//! | Completion wire method | `client_completion_request_carries_correlation_id` (+ transport `completion_requests_use_text_document_completion`) | same |
//! | Hover wire method | `hover_request_carries_only_lsp_position_and_correlation_id` (+ transport `hover_requests_use_text_document_hover`) | same |
//! | Correlation enqueue | `hover_request_carries_only_lsp_position_and_correlation_id` | same |
//! | UI/wire correlation | `client_poll_caches_diagnostics_and_forwards_typed_responses` (+ transport `ui_correlation_ids_survive_independent_wire_id_mapping`) | same |
//! | `poll` routing | `poll_forwards_hover_results_without_parsing_or_ui_state` | same |
//! | Non-blocking UI drain | `preserve_the_non_blocking_ui_thread` | `cargo test --lib preserve_the_non_blocking_ui_thread` |
//! | Never: block on rust-analyzer (UI thread) | `block_waiting_for_rust_analyzer_on_the_ui_thread` | `cargo test --lib block_waiting_for_rust_analyzer_on_the_ui_thread` |
//! | Channel + transport contract | `use_existing_channels_and_lsp_transport` | `cargo test --lib use_existing_channels_and_lsp_transport` |
//! | URI helpers | `file_uri_round_trips_absolute_paths` | same |
//!
//! Wire encode/decode regressions live in `lsp/transport.rs` (`cargo test --lib lsp::transport::tests`).
//! App-layer `poll_lsp` / stale gates live in `app.rs`.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

pub mod manager;
pub mod transport;
pub mod types;

use types::{
    CallHierarchyItem, CodeLensItem, LspDiagnostic, LspRequest, LspResponse, ProgressKind,
    ProgressState, TypeHierarchyItem,
};

pub struct LspClient {
    sender: Sender<LspRequest>,
    receiver: Receiver<LspResponse>,
    #[cfg(test)]
    request_rx: Option<Receiver<LspRequest>>,
    #[cfg(test)]
    response_tx: Option<Sender<LspResponse>>,
    transport: Option<JoinHandle<()>>,
    diagnostics: HashMap<PathBuf, Vec<LspDiagnostic>>,
    running: bool,
    pub token_type_legend: Vec<String>,
    pub active_progress: HashMap<String, ProgressState>,
}

impl LspClient {
    pub fn start(root_path: PathBuf) -> Self {
        Self::start_with_config("rust-analyzer", &[], root_path)
    }

    pub fn start_with_config(command: &str, args: &[String], root_path: PathBuf) -> Self {
        let (to_lsp_tx, to_lsp_rx) = mpsc::channel();
        let (from_lsp_tx, from_lsp_rx) = mpsc::channel();
        let transport = transport::spawn_lsp_thread(
            command,
            args,
            &root_path.to_string_lossy(),
            from_lsp_tx,
            to_lsp_rx,
        );

        let client = Self {
            sender: to_lsp_tx,
            receiver: from_lsp_rx,
            #[cfg(test)]
            request_rx: None,
            #[cfg(test)]
            response_tx: None,
            transport: Some(transport),
            diagnostics: HashMap::new(),
            running: false,
            token_type_legend: Vec::new(),
            active_progress: HashMap::new(),
        };
        if let Ok(root_uri) = path_to_uri(&root_path) {
            let _ = client.send_request(LspRequest::Initialize { root_uri });
        }
        client
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn diagnostics(&self) -> &HashMap<PathBuf, Vec<LspDiagnostic>> {
        &self.diagnostics
    }

    pub fn diagnostics_for(&self, path: &Path) -> Option<&[LspDiagnostic]> {
        self.diagnostics.get(path).map(Vec::as_slice)
    }

    #[cfg(test)]
    pub fn set_running_for_test(&mut self, running: bool) {
        self.running = running;
    }

    #[cfg(test)]
    pub fn insert_diagnostics_for_test(&mut self, path: PathBuf, diagnostics: Vec<LspDiagnostic>) {
        self.diagnostics.insert(path, diagnostics);
    }

    #[cfg(test)]
    pub fn new_test_client() -> Self {
        Self::new_test_client_with_running(true)
    }

    #[cfg(test)]
    pub fn new_test_client_with_running(running: bool) -> Self {
        let (to_lsp_tx, to_lsp_rx) = mpsc::channel();
        let (from_lsp_tx, from_lsp_rx) = mpsc::channel();
        Self {
            sender: to_lsp_tx,
            receiver: from_lsp_rx,
            request_rx: Some(to_lsp_rx),
            response_tx: Some(from_lsp_tx),
            transport: None,
            diagnostics: HashMap::new(),
            running,
            token_type_legend: Vec::new(),
            active_progress: HashMap::new(),
        }
    }

    #[cfg(test)]
    pub fn push_test_response(&self, response: LspResponse) {
        if let Some(tx) = &self.response_tx {
            let _ = tx.send(response);
        }
    }

    #[cfg(test)]
    pub fn test_response_sender(&self) -> Option<Sender<LspResponse>> {
        self.response_tx.clone()
    }

    #[cfg(test)]
    pub fn drain_pending_requests(&mut self) -> Vec<LspRequest> {
        let Some(rx) = self.request_rx.as_ref() else {
            return Vec::new();
        };
        let mut requests = Vec::new();
        while let Ok(request) = rx.try_recv() {
            requests.push(request);
        }
        requests
    }

    /// Drain all currently available responses without ever waiting on the UI thread.
    pub fn poll(&mut self) -> Vec<LspResponse> {
        let mut responses = Vec::new();
        while let Ok(message) = self.receiver.try_recv() {
            match message {
                LspResponse::Diagnostics { path, diagnostics } => {
                    self.diagnostics.insert(path, diagnostics);
                }
                LspResponse::Initialized { token_types } => {
                    self.running = true;
                    self.token_type_legend = token_types.clone();
                    responses.push(LspResponse::Initialized { token_types });
                }
                LspResponse::ServerUnavailable { message } => {
                    self.running = false;
                    responses.push(LspResponse::ServerUnavailable { message });
                }
                LspResponse::Progress {
                    token,
                    kind,
                    title,
                    message,
                    percentage,
                } => {
                    match kind {
                        ProgressKind::End => {
                            self.active_progress.remove(&token);
                        }
                        _ => {
                            self.active_progress.insert(
                                token.clone(),
                                ProgressState {
                                    title: title.clone(),
                                    message: message.clone(),
                                    percentage,
                                    kind,
                                },
                            );
                        }
                    }
                    responses.push(LspResponse::Progress {
                        token,
                        kind,
                        title,
                        message,
                        percentage,
                    });
                }
                other => responses.push(other),
            }
        }
        responses
    }

    pub fn did_open(&self, path: &Path, language_id: &str, text: &str, version: i32) -> bool {
        self.send_request(LspRequest::DidOpen {
            path: path.to_path_buf(),
            language_id: language_id.to_owned(),
            text: text.to_owned(),
            version,
        })
    }

    pub fn did_change(&self, path: &Path, text: &str, version: i32) -> bool {
        self.send_request(LspRequest::DidChange {
            path: path.to_path_buf(),
            text: text.to_owned(),
            version,
        })
    }

    pub fn did_close(&mut self, path: &Path) {
        self.diagnostics.remove(path);
        let _ = self.send_request(LspRequest::DidClose {
            path: path.to_path_buf(),
        });
    }

    /// Request completions at an LSP position. Enqueues [`LspRequest::Completion`], which
    /// `transport.rs` encodes as `textDocument/completion`. `line` is 0-based and `col` is a
    /// UTF-16 code unit column. `id` is the caller-owned correlation id echoed back on
    /// [`LspResponse::CompletionList`].
    ///
    /// Returns `false` when the language server is not running or the transport
    /// channel is disconnected.
    pub fn request_completion(&self, path: &Path, line: u32, col: u32, id: u64) -> bool {
        self.send_position_request(LspRequest::Completion {
            path: path.to_path_buf(),
            line,
            col,
            id,
        })
    }

    /// Request hover documentation at an LSP position. Enqueues [`LspRequest::Hover`], which
    /// `transport.rs` encodes as `textDocument/hover`. `line` is 0-based and `col` is a
    /// UTF-16 code unit column. `id` is the caller-owned correlation id echoed back on
    /// [`LspResponse::HoverResult`].
    ///
    /// Returns `false` when the language server is not running or the transport
    /// channel is disconnected.
    pub fn request_hover(&self, path: &Path, line: u32, col: u32, id: u64) -> bool {
        self.send_position_request(LspRequest::Hover {
            path: path.to_path_buf(),
            line,
            col,
            id,
        })
    }

    pub fn request_goto_definition(
        &self,
        path: &Path,
        line: u32,
        utf16_col: u32,
        correlation_id: u64,
    ) -> bool {
        self.send_position_request(LspRequest::GotoDefinition {
            path: path.to_path_buf(),
            line,
            col: utf16_col,
            id: correlation_id,
        })
    }

    /// Request inlay hints for a visible line range. Enqueues [`LspRequest::InlayHint`],
    /// which `transport.rs` encodes as `textDocument/inlayHint`. `start_line` and
    /// `end_line` are 0-based; the range is `[start_line, end_line)`. `id` is the
    /// caller-owned correlation id echoed back on [`LspResponse::InlayHintResult`].
    ///
    /// Returns `false` when the language server is not running or the transport
    /// channel is disconnected.
    pub fn request_inlay_hints(
        &self,
        path: &Path,
        start_line: u32,
        end_line: u32,
        id: u64,
    ) -> bool {
        self.send_position_request(LspRequest::InlayHint {
            path: path.to_path_buf(),
            start_line,
            end_line,
            id,
        })
    }

    pub fn request_document_symbol(&self, path: &Path, id: u64) -> bool {
        self.send_position_request(LspRequest::DocumentSymbol {
            path: path.to_path_buf(),
            id,
        })
    }

    /// Request full-document formatting. Enqueues [`LspRequest::Format`], which
    /// `transport.rs` encodes as `textDocument/formatting`. `id` is the caller-owned
    /// correlation id echoed back on [`LspResponse::FormatResult`].
    ///
    /// Returns `false` when the language server is not running.
    pub fn request_format(&self, path: &Path, tab_size: u32, insert_spaces: bool, id: u64) -> bool {
        self.send_position_request(LspRequest::Format {
            path: path.to_path_buf(),
            tab_size,
            insert_spaces,
            id,
        })
    }

    /// Request range formatting. Enqueues [`LspRequest::RangeFormat`], which
    /// `transport.rs` encodes as `textDocument/rangeFormatting`. `id` is the caller-owned
    /// correlation id echoed back on [`LspResponse::FormatResult`].
    ///
    /// Returns `false` when the language server is not running.
    pub fn request_range_format(
        &self,
        path: &Path,
        tab_size: u32,
        insert_spaces: bool,
        range: (u32, u32, u32, u32),
        id: u64,
    ) -> bool {
        self.send_position_request(LspRequest::RangeFormat {
            path: path.to_path_buf(),
            tab_size,
            insert_spaces,
            range,
            id,
        })
    }

    /// Request signature help at a position. Enqueues [`LspRequest::SignatureHelp`], which
    /// `transport.rs` encodes as `textDocument/signatureHelp`. `id` is the caller-owned
    /// correlation id echoed back on [`LspResponse::SignatureHelpResult`].
    ///
    /// Returns `false` when the language server is not running.
    pub fn request_signature_help(&self, path: &Path, line: u32, col: u32, id: u64) -> bool {
        self.send_position_request(LspRequest::SignatureHelp {
            path: path.to_path_buf(),
            line,
            col,
            id,
        })
    }

    /// Request workspace symbols matching `query`. Enqueues [`LspRequest::WorkspaceSymbol`],
    /// which `transport.rs` encodes as `workspace/symbol`. `id` is the caller-owned
    /// correlation id echoed back on [`LspResponse::WorkspaceSymbolResult`].
    ///
    /// Returns `false` when the language server is not running.
    pub fn request_workspace_symbol(&self, query: &str, id: u64) -> bool {
        self.send_position_request(LspRequest::WorkspaceSymbol {
            query: query.to_owned(),
            id,
        })
    }

    /// Request code actions at a range. Enqueues [`LspRequest::CodeAction`], which
    /// `transport.rs` encodes as `textDocument/codeAction`. `id` is the caller-owned
    /// correlation id echoed back on [`LspResponse::CodeActionResult`].
    ///
    /// Returns `false` when the language server is not running.
    pub fn request_code_action(
        &self,
        path: &Path,
        range: (u32, u32, u32, u32),
        diagnostics: Vec<LspDiagnostic>,
        id: u64,
    ) -> bool {
        self.send_position_request(LspRequest::CodeAction {
            path: path.to_path_buf(),
            range,
            diagnostics,
            id,
        })
    }

    pub fn request_code_lens(&self, path: &Path, id: u64) -> bool {
        self.send_position_request(LspRequest::CodeLens {
            path: path.to_path_buf(),
            id,
        })
    }

    pub fn request_code_lens_resolve(&self, item: CodeLensItem, id: u64) -> bool {
        self.send_position_request(LspRequest::CodeLensResolve { item, id })
    }

    pub fn request_semantic_tokens_full(&self, path: &Path, id: u64) -> bool {
        self.send_position_request(LspRequest::SemanticTokensFull {
            path: path.to_path_buf(),
            id,
        })
    }

    pub fn request_semantic_tokens_range(
        &self,
        path: &Path,
        start_line: u32,
        end_line: u32,
        id: u64,
    ) -> bool {
        self.send_position_request(LspRequest::SemanticTokensRange {
            path: path.to_path_buf(),
            start_line,
            end_line,
            id,
        })
    }

    pub fn request_prepare_call_hierarchy(
        &self,
        path: &Path,
        line: u32,
        col: u32,
        id: u64,
    ) -> bool {
        self.send_position_request(LspRequest::PrepareCallHierarchy {
            path: path.to_path_buf(),
            line,
            col,
            id,
        })
    }

    pub fn request_incoming_calls(&self, item: CallHierarchyItem, id: u64) -> bool {
        self.send_position_request(LspRequest::IncomingCalls { item, id })
    }

    pub fn request_outgoing_calls(&self, item: CallHierarchyItem, id: u64) -> bool {
        self.send_position_request(LspRequest::OutgoingCalls { item, id })
    }

    pub fn request_prepare_type_hierarchy(
        &self,
        path: &Path,
        line: u32,
        col: u32,
        id: u64,
    ) -> bool {
        self.send_position_request(LspRequest::PrepareTypeHierarchy {
            path: path.to_path_buf(),
            line,
            col,
            id,
        })
    }

    pub fn request_supertypes(&self, item: TypeHierarchyItem, id: u64) -> bool {
        self.send_position_request(LspRequest::Supertypes { item, id })
    }

    pub fn request_subtypes(&self, item: TypeHierarchyItem, id: u64) -> bool {
        self.send_position_request(LspRequest::Subtypes { item, id })
    }

    pub fn request_execute_command(
        &self,
        command: String,
        args: serde_json::Value,
        id: u64,
    ) -> bool {
        self.send_position_request(LspRequest::ExecuteCommand { command, args, id })
    }

    fn send_position_request(&self, request: LspRequest) -> bool {
        if !self.running {
            return false;
        }
        self.send_request(request)
    }

    fn send_request(&self, request: LspRequest) -> bool {
        self.sender.send(request).is_ok()
    }

    pub fn request_shutdown(&self) {
        let _ = self.send_request(LspRequest::Shutdown);
    }

    pub fn shutdown_and_join(&mut self) {
        self.request_shutdown();
        if let Some(transport) = self.transport.take() {
            let _ = transport.join();
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        self.request_shutdown();
        // Dropping a JoinHandle detaches it. The transport retains ownership of
        // the child process and completes its bounded shutdown in the background.
    }
}

pub fn path_to_uri(path: &Path) -> io::Result<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    lsp_types::Url::from_file_path(absolute)
        .map(String::from)
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "path cannot be represented as URI",
            )
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::all)]
    use super::*;

    // LSP client facade regressions — run: cargo test --lib lsp::tests

    fn test_client(running: bool) -> (LspClient, Receiver<LspRequest>) {
        let (to_lsp_tx, to_lsp_rx) = mpsc::channel();
        let (_from_lsp_tx, from_lsp_rx) = mpsc::channel();
        let client = LspClient {
            sender: to_lsp_tx,
            receiver: from_lsp_rx,
            request_rx: None,
            response_tx: None,
            transport: None,
            diagnostics: HashMap::new(),
            running,
            token_type_legend: Vec::new(),
            active_progress: HashMap::new(),
        };
        (client, to_lsp_rx)
    }

    #[test]
    fn completion_and_hover_requests_require_a_running_server() {
        let path = std::env::current_dir().unwrap().join("src/main.rs");
        let (client, _requests) = test_client(false);
        assert!(!client.request_completion(&path, 0, 0, 1));
        assert!(!client.request_hover(&path, 0, 0, 2));
    }

    #[test]
    fn completion_and_hover_requests_enqueue_when_running() {
        let path = std::env::current_dir().unwrap().join("src/main.rs");
        let (client, requests) = test_client(true);
        assert!(client.request_completion(&path, 1, 2, 3));
        assert!(client.request_hover(&path, 4, 5, 6));

        assert!(matches!(
            requests.try_recv().ok(),
            Some(LspRequest::Completion { id: 3, .. })
        ));
        assert!(matches!(
            requests.try_recv().ok(),
            Some(LspRequest::Hover { id: 6, .. })
        ));
    }

    #[test]
    fn hover_request_carries_only_lsp_position_and_correlation_id() {
        let path = std::env::current_dir().unwrap().join("src/main.rs");
        let (client, requests) = test_client(true);
        assert!(client.request_hover(&path, 2, 7, 99));

        match requests.try_recv().ok() {
            Some(LspRequest::Hover {
                path: requested_path,
                line,
                col,
                id,
            }) => {
                assert_eq!(requested_path, path);
                assert_eq!(line, 2);
                assert_eq!(col, 7);
                assert_eq!(id, 99);
            }
            other => panic!("expected Hover request, got {other:?}"),
        }
        assert!(
            requests.try_recv().is_err(),
            "hover must enqueue a single LspRequest::Hover for transport encoding"
        );
    }

    #[test]
    fn poll_forwards_hover_results_without_parsing_or_ui_state() {
        let (to_lsp_tx, _to_lsp_rx) = mpsc::channel();
        let (from_lsp_tx, from_lsp_rx) = mpsc::channel();
        let mut client = LspClient {
            sender: to_lsp_tx,
            receiver: from_lsp_rx,
            request_rx: None,
            response_tx: None,
            transport: None,
            diagnostics: HashMap::new(),
            running: true,
            token_type_legend: Vec::new(),
            active_progress: HashMap::new(),
        };
        from_lsp_tx
            .send(LspResponse::HoverResult {
                id: 42,
                content: "fn main — documentation".to_owned(),
            })
            .unwrap();

        let responses = client.poll();
        assert_eq!(responses.len(), 1);
        assert!(matches!(
            responses[0],
            LspResponse::HoverResult {
                id: 42,
                ref content,
            } if content == "fn main — documentation"
        ));
        assert!(
            client.diagnostics.is_empty(),
            "hover responses must not be cached as diagnostics"
        );
    }

    #[test]
    fn hover_is_sent_only_through_public_request_api() {
        let path = std::env::current_dir().unwrap().join("src/main.rs");
        let (client, requests) = test_client(true);
        assert!(client.is_running());
        assert!(client.request_hover(&path, 0, 1, 7));
        assert!(matches!(
            requests.try_recv().ok(),
            Some(LspRequest::Hover { id: 7, .. })
        ));
    }

    #[test]
    fn file_uri_round_trips_absolute_paths() {
        let path = std::env::current_dir().unwrap().join("src/main.rs");
        let uri = path_to_uri(&path).unwrap();
        assert_eq!(
            lsp_types::Url::parse(&uri).unwrap().to_file_path().unwrap(),
            path
        );
    }

    #[test]
    fn client_completion_request_carries_correlation_id() {
        let path = std::env::current_dir().unwrap().join("src/main.rs");
        let (client, requests) = test_client(true);
        assert!(client.request_completion(&path, 3, 8, 55));

        match requests.try_recv().ok() {
            Some(LspRequest::Completion {
                path: requested_path,
                line,
                col,
                id,
            }) => {
                assert_eq!(requested_path, path);
                assert_eq!(line, 3);
                assert_eq!(col, 8);
                assert_eq!(id, 55);
            }
            other => panic!("expected Completion request, got {other:?}"),
        }
        assert!(
            requests.try_recv().is_err(),
            "completion must enqueue a single LspRequest::Completion for transport encoding"
        );
    }

    #[test]
    fn client_goto_definition_requires_running_server() {
        let path = std::env::current_dir().unwrap().join("src/main.rs");
        let (client, _requests) = test_client(false);
        assert!(!client.request_goto_definition(&path, 0, 0, 1));

        let (client, requests) = test_client(true);
        assert!(client.request_goto_definition(&path, 2, 4, 9));
        assert!(matches!(
            requests.try_recv().ok(),
            Some(LspRequest::GotoDefinition {
                id: 9,
                line: 2,
                col: 4,
                ..
            })
        ));
    }

    #[test]
    fn client_poll_caches_diagnostics_and_forwards_typed_responses() {
        let (to_lsp_tx, _to_lsp_rx) = mpsc::channel();
        let (from_lsp_tx, from_lsp_rx) = mpsc::channel();
        let mut client = LspClient {
            sender: to_lsp_tx,
            receiver: from_lsp_rx,
            request_rx: None,
            response_tx: None,
            transport: None,
            diagnostics: HashMap::new(),
            running: true,
            token_type_legend: Vec::new(),
            active_progress: HashMap::new(),
        };
        let path = std::env::current_dir().unwrap().join("src/main.rs");
        use super::types::{DiagnosticSeverity, LspDiagnostic};

        from_lsp_tx
            .send(LspResponse::Diagnostics {
                path: path.clone(),
                diagnostics: vec![LspDiagnostic {
                    line_start: 0,
                    col_start: 0,
                    line_end: 0,
                    col_end: 1,
                    severity: DiagnosticSeverity::Error,
                    message: "expected expression".to_owned(),
                    code: None,
                }],
            })
            .unwrap();
        from_lsp_tx
            .send(LspResponse::CompletionList {
                id: 12,
                items: Vec::new(),
            })
            .unwrap();

        let responses = client.poll();
        assert_eq!(responses.len(), 1);
        assert!(matches!(
            responses[0],
            LspResponse::CompletionList { id: 12, .. }
        ));
        assert_eq!(client.diagnostics_for(&path).map(|d| d.len()), Some(1));
    }

    #[test]
    fn use_existing_channels_and_lsp_transport() {
        let path = std::env::current_dir().unwrap().join("src/main.rs");
        let (to_lsp_tx, from_ui) = mpsc::channel();
        let (to_ui, from_lsp_rx) = mpsc::channel();
        let mut client = LspClient {
            sender: to_lsp_tx,
            receiver: from_lsp_rx,
            request_rx: None,
            response_tx: None,
            transport: None,
            diagnostics: HashMap::new(),
            running: true,
            token_type_legend: Vec::new(),
            active_progress: HashMap::new(),
        };

        assert!(client.did_open(&path, "rust", "fn main() {}\n", 1));
        assert!(client.request_completion(&path, 1, 2, 10));
        assert!(client.request_hover(&path, 3, 4, 20));
        assert!(client.request_goto_definition(&path, 5, 6, 30));

        assert!(matches!(
            from_ui.try_recv().ok(),
            Some(LspRequest::DidOpen { .. })
        ));
        assert!(matches!(
            from_ui.try_recv().ok(),
            Some(LspRequest::Completion { id: 10, .. })
        ));
        assert!(matches!(
            from_ui.try_recv().ok(),
            Some(LspRequest::Hover { id: 20, .. })
        ));
        assert!(matches!(
            from_ui.try_recv().ok(),
            Some(LspRequest::GotoDefinition { id: 30, .. })
        ));
        assert!(
            from_ui.try_recv().is_err(),
            "all outbound LSP traffic must share the single LspRequest channel consumed by transport"
        );

        to_ui
            .send(LspResponse::CompletionList {
                id: 10,
                items: Vec::new(),
            })
            .unwrap();
        to_ui
            .send(LspResponse::HoverResult {
                id: 20,
                content: "docs".to_owned(),
            })
            .unwrap();
        to_ui
            .send(LspResponse::GotoResult {
                id: 30,
                path: path.clone(),
                line: 7,
                col: 8,
            })
            .unwrap();

        let responses = client.poll();
        assert_eq!(responses.len(), 3);
        assert!(matches!(
            responses[0],
            LspResponse::CompletionList { id: 10, .. }
        ));
        assert!(matches!(
            responses[1],
            LspResponse::HoverResult {
                id: 20,
                ref content,
            } if content == "docs"
        ));
        assert!(matches!(
            responses[2],
            LspResponse::GotoResult {
                id: 30,
                line: 7,
                col: 8,
                ..
            }
        ));
        assert!(
            client.poll().is_empty(),
            "inbound LSP traffic must arrive as typed LspResponse values on the client channel"
        );
    }

    #[test]
    fn preserve_the_non_blocking_ui_thread() {
        use std::time::{Duration, Instant};

        let (to_lsp_tx, _to_lsp_rx) = mpsc::channel();
        let (from_lsp_tx, from_lsp_rx) = mpsc::channel();
        let mut client = LspClient {
            sender: to_lsp_tx,
            receiver: from_lsp_rx,
            request_rx: None,
            response_tx: None,
            transport: None,
            diagnostics: HashMap::new(),
            running: false,
            token_type_legend: Vec::new(),
            active_progress: HashMap::new(),
        };

        let delayed_tx = from_lsp_tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let _ = delayed_tx.send(LspResponse::Initialized {
                token_types: Vec::new(),
            });
        });

        let start = Instant::now();
        let responses = client.poll();
        let elapsed = start.elapsed();

        assert!(
            responses.is_empty(),
            "poll must drain only currently available responses"
        );
        assert!(
            elapsed < Duration::from_millis(100),
            "poll must not block the UI thread waiting for in-flight LSP work ({elapsed:?})"
        );

        let deadline = Instant::now() + Duration::from_secs(1);
        let responses = loop {
            let responses = client.poll();
            if !responses.is_empty() || Instant::now() >= deadline {
                break responses;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(responses.len(), 1);
        assert!(matches!(responses[0], LspResponse::Initialized { .. }));
        assert!(client.is_running());
    }

    #[test]
    fn client_poll_marks_running_on_initialized() {
        let (to_lsp_tx, _to_lsp_rx) = mpsc::channel();
        let (from_lsp_tx, from_lsp_rx) = mpsc::channel();
        let mut client = LspClient {
            sender: to_lsp_tx,
            receiver: from_lsp_rx,
            request_rx: None,
            response_tx: None,
            transport: None,
            diagnostics: HashMap::new(),
            running: false,
            token_type_legend: Vec::new(),
            active_progress: HashMap::new(),
        };
        from_lsp_tx
            .send(LspResponse::Initialized {
                token_types: Vec::new(),
            })
            .unwrap();

        let responses = client.poll();
        assert_eq!(responses.len(), 1);
        assert!(matches!(responses[0], LspResponse::Initialized { .. }));
        assert!(client.is_running());
    }

    #[test]
    fn client_did_close_removes_cached_diagnostics() {
        let path = std::env::current_dir().unwrap().join("src/main.rs");
        let (mut client, _requests) = test_client(true);
        client.insert_diagnostics_for_test(path.clone(), Vec::new());
        assert!(client.diagnostics_for(&path).is_some());

        client.did_close(&path);
        assert!(client.diagnostics_for(&path).is_none());
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

    fn assert_ui_poll_paths_never_block_on_lsp() {
        let lsp_mod = include_str!("mod.rs");
        let lsp_production = lsp_mod
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(lsp_mod);
        assert!(
            lsp_production.contains("while let Ok(message) = self.receiver.try_recv()"),
            "LspClient::poll must drain with try_recv"
        );
        assert!(
            lsp_production.contains("without ever waiting on the UI thread"),
            "poll contract must document non-blocking UI drain"
        );
        let poll_body = extract_fn_body(lsp_production, "poll").expect("poll should exist");
        assert!(
            !rust_source_without_comments(&poll_body).contains(".recv("),
            "LspClient::poll must not call blocking Receiver::recv"
        );

        let app_rs = include_str!("../app.rs");
        let app_production = app_rs
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(app_rs);
        let poll_lsp_body =
            extract_fn_body(app_production, "poll_lsp").expect("poll_lsp should exist");
        assert!(
            poll_lsp_body.contains("LspClient::poll"),
            "poll_lsp must delegate to LspClient::poll"
        );
        assert!(
            !rust_source_without_comments(&poll_lsp_body).contains(".recv("),
            "poll_lsp must not call blocking Receiver::recv"
        );
        assert!(
            app_production.contains("self.poll_lsp();"),
            "each frame must drain LSP via poll_lsp"
        );
    }

    /// Never boundary: the UI thread must not block waiting for rust-analyzer (see
    /// **Boundaries → Never** §11).
    #[test]
    fn block_waiting_for_rust_analyzer_on_the_ui_thread() {
        use std::time::{Duration, Instant};

        assert_ui_poll_paths_never_block_on_lsp();

        let (to_lsp_tx, _to_lsp_rx) = mpsc::channel();
        let (from_lsp_tx, from_lsp_rx) = mpsc::channel();
        let mut client = LspClient {
            sender: to_lsp_tx,
            receiver: from_lsp_rx,
            request_rx: None,
            response_tx: None,
            transport: None,
            diagnostics: HashMap::new(),
            running: false,
            token_type_legend: Vec::new(),
            active_progress: HashMap::new(),
        };

        let delayed_tx = from_lsp_tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let _ = delayed_tx.send(LspResponse::Initialized {
                token_types: Vec::new(),
            });
        });

        let start = Instant::now();
        let responses = client.poll();
        let elapsed = start.elapsed();
        assert!(
            responses.is_empty(),
            "poll_lsp drain must not wait for in-flight rust-analyzer responses"
        );
        assert!(
            elapsed < Duration::from_millis(100),
            "UI-thread LSP drain must return immediately ({elapsed:?})"
        );
    }
}
