//! JSON-RPC encoding and parsing only: framed stdin/stdout I/O for the LSP child process.
//! Encodes `LspRequest` values to wire messages and parses inbound JSON into typed
//! `LspResponse` values (including flattening `Hover` wire shapes). Does not apply
//! display policy, debounce hover, gate UI, or render popups (`editor/hover.rs`, `app.rs`).
//!
//! # LSP transport tests
//!
//! Wire-shape regressions; part of crate-level **Regression tests** (`lib.rs`).
//!
//! Focused unit and wire tests for framing, request encoding, response parsing, typed-response
//! correlation, diagnostics notifications, and process lifecycle. Prefer these over manual
//! rust-analyzer sessions.
//!
//! | Area | Filter / examples | Run |
//! |------|-------------------|-----|
//! | UTF-8 framing | `utf8_framing_behavior_remains_unchanged`, `framing_uses_utf8_byte_length_and_accepts_extra_headers` | `cargo test --lib utf8_framing` |
//! | Framing / sync | `did_change_encodes_full_document_and_monotonic_wire_ids` | `cargo test --lib did_change` |
//! | LSP sync strategy fingerprint | `ask_before_changing_the_lsp_synchronization_strategy` (integration) | `cargo test --test integration_test ask_before_changing_the_lsp_synchronization_strategy` |
//! | Completion wire method | `completion_requests_use_text_document_completion` | `cargo test --lib completion_requests_use_text_document` |
//! | Completion result shapes | `completion_list_and_direct_array_results_parse_correctly`, `completion_list_and_direct_array_wire_shapes_stay_equivalent` | `cargo test --lib completion_list_and_direct_array` |
//! | Completion parse | `completion_*` | `cargo test --lib lsp::transport::tests::completion` |
//! | Completion acceptance | `insert_text_falls_back_correctly_at_acceptance_time`, `completion_text_edit_beats_insert_text_at_acceptance_time` | `cargo test --lib insert_text_falls_back` |
//! | Hover wire method | `hover_requests_use_text_document_hover` | `cargo test --lib hover_requests_use_text_document` |
//! | Hover result shapes | `hover_string_marked_string_array_markup_content_and_null_results_parse_safely` | `cargo test --lib hover_string_marked_string_array_markup_content_and_null` |
//! | Hover parse | `hover_*`, `hover_unit_*` | `cargo test --lib lsp::transport::tests::hover` |
//! | Never: hard-coded completion/hover mock data | `render_completion_or_hover_using_hard_coded_mock_data` | `cargo test --lib render_completion_or_hover_using_hard_coded_mock_data` |
//! | Goto parse | `parse_goto_definition_responses`, `goto_definition_request_encoding` | `cargo test --lib lsp::transport::tests::goto` |
//! | Correlation | `ui_correlation_ids_survive_independent_wire_id_mapping`, `typed_responses_use_the_caller_correlation_id`, `*_correlation_*` | `cargo test --lib ui_correlation_ids_survive` / `correlation` |
//! | Lifecycle | `missing_server_is_reported_without_panicking` | `cargo test --lib missing_server` |
//!
//! Client facade tests: `cargo test --lib lsp::tests` (`lsp/mod.rs`).
//!
//! # Completion parsing considerations
//!
//! `textDocument/completion` responses are normalized to [`LspCompletionItem`] values here.
//! Downstream code (`app.rs`, `editor/buffer.rs`) applies accepted items; this module never
//! mutates buffers or interprets snippet tab stops.
//!
//! ## `textDocument/completion` result shapes (both must keep working)
//!
//! LSP allows two equivalent wire shapes for the JSON-RPC `result`:
//!
//! - **Direct array** — `[{ "label": "…" }, …]` (`CompletionItem[]`)
//! - **CompletionList** — `{ "items": […], "isIncomplete": … }` (only `items` is read)
//!
//! `null` maps to an empty list. Extra CompletionList fields are ignored at parse time.
//!
//! | Wire field | Parsed into | Notes |
//! |------------|---------------|-------|
//! | `result` (either shape above) | `Vec<LspCompletionItem>` | `null` → empty list |
//! | `label` (string) | `label` | Retained for popup display; never copied from `insertText` / `newText` |
//! | `kind` (number or string) | `kind` | Retained for popup display; numeric kinds map to LSP labels (`Function`, `Snippet`, …) |
//! | `detail` | `detail` | Retained for popup display; not copied from `label` / `insertText` |
//! | `insertText` | `insert_text` | Retained verbatim (including `""` and snippet-marker text); not copied from `label` / `detail` |
//! | `textEdit` (`range` + `newText`) | `text_edit` | Single-span `TextEdit`; columns are UTF-16 |
//! | `textEdit` (`InsertReplaceEdit`) | `text_edit` | Normalized to one range (`insert` only; `replace` is not applied) |
//! | `textEdit.newText` (no `insertText`) | `insert_text` fallback | First fallback when wire `insertText` is absent |
//! | `label` (no `insertText` or `textEdit.newText`) | `insert_text` fallback | Second fallback for the plain apply path |
//!
//! ## `textEdit` and snippet support boundaries
//!
//! Do not describe Blue IDE as supporting full LSP `textEdit` or snippet placeholders.
//! Only the subset below is modeled end-to-end (parse → accept → buffer edit):
//!
//! **Modeled**
//!
//! - One primary `textEdit` per item (plain `TextEdit` or `InsertReplaceEdit` → [`LspTextEdit`])
//! - UTF-16 range columns decoded to buffer indices in [`TextBuffer::apply_lsp_text_edit`]
//! - `newText` / `insertText` inserted as **literal** characters (one `replace_char_range`)
//!
//! **Not modeled or applied** (wire values may be retained verbatim but are not interpreted)
//!
//! - `insertTextFormat`, snippet tab stops, and `${…}` / `$0` placeholder expansion
//! - `additionalTextEdits` and multi-edit completion transactions
//! - `InsertReplaceEdit.replace` (only the `insert` range is used on accept)
//!
//! Intentionally **not** interpreted at parse time:
//!
//! - `filterText`, `sortText`, `preselect`, `isIncomplete`
//! - `documentation`, `command`, `data` (`labelDetails.detail` / `.description` are retained as `detail`)
//!
//! Parse-time `insert_text` fallbacks (`textEdit.newText`, then `label`) live here.
//! Acceptance-time fallback on the plain path (non-empty `insert_text`, else `label`; empty
//! wire `insertText` treated as absent) lives in
//! [`completion_acceptance_insert_text`](crate::editor::completion::completion_acceptance_insert_text).
//! `text_edit` still beats plain-path text in
//! [`TextBuffer::apply_completion_insertion`](crate::editor::buffer::TextBuffer::apply_completion_insertion).
//! Both paths insert literal text; neither expands snippet placeholders.
//!
//! # Hover parsing considerations
//!
//! `textDocument/hover` responses are flattened to one display string in
//! [`LspResponse::HoverResult::content`]. Downstream code (`app.rs`, `editor/hover.rs`)
//! debounces requests, rejects stale or undisplayable text, and renders the popup; this
//! module never hit-tests the editor or runs display heuristics.
//!
//! ## `contents` top-level shapes (all must keep working)
//!
//! LSP allows three equivalent wire shapes for `result.contents`:
//!
//! - **String** — `"docs line"` (`MarkedString` shorthand)
//! - **MarkedString array** — `[ "plain", { "language": "rust", "value": "…" }, … ]` (may also
//!   include `MarkupContent` objects); elements flattened in order
//! - **Object** — `{ "language": "rust", "value": "…" }` (`MarkedString`) or
//!   `{ "kind": "markdown", "value": "…" }` (`MarkupContent`)
//!
//! `null` / missing `contents` map to `""`. Parsing must not assume a single wire shape.
//!
//! | Wire shape | Parsed into | Notes |
//! |------------|-------------|-------|
//! | `result: null` | `""` | No documentation at the target position; not an LSP error |
//! | `contents` absent | `""` | Minimal / malformed hover object |
//! | `contents` (string) | `content` | `MarkedString` shorthand; retained verbatim except CRLF → LF |
//! | `contents` (array) | `content` | `MarkedString` array; elements flattened in wire order, joined with `\n` |
//! | `contents` (object) | `content` | Single `MarkedString` or `MarkupContent` |
//!
//! ## `contents` element flattening
//!
//! | Wire element | Flattened text |
//! |--------------|----------------|
//! | string | CRLF-normalized body |
//! | `{ "language": "rust", "value": "…" }` | `MarkedString` object; `value` retained (CRLF → LF), fenced when `language` is non-empty |
//! | `{ "kind": "markdown", "value": "…" }` | `MarkupContent` object; `value` retained (CRLF → LF), `kind` not rendered |
//! | object with `value` only | `value` text (no fence) |
//!
//! ## Null results
//!
//! Wire `result: null` maps to [`LspResponse::HoverResult`] with `content: ""`. This is the
//! typed success path (not `LspResponse::Error`). `app.rs` closes hover silently when content
//! is empty.
//!
//! ## Tests
//!
//! Hover parsing regressions are split by responsibility:
//!
//! | Layer | Focus | Run |
//! |-------|-------|-----|
//! | `lsp/transport.rs` | Wire flattening, shapes, null, correlation | `cargo test --lib lsp::transport::tests::hover` |
//! | `app.rs` | `poll_lsp` / `receive_hover` gates, silent null/empty close | `cargo test --lib app::tests::hover` and `null_hover` |
//! | `editor/hover.rs` | Display policy, popup layout | `cargo test --lib editor::hover::tests` |
//!
//! Checklist coverage in transport tests includes:
//! `hover_string_marked_string_array_markup_content_and_null_results_parse_safely` (string,
//! `MarkedString` array, `MarkupContent`, null, and malformed inputs), plus caller correlation
//! id echo.
//! Display filtering (`is_undisplayable_hover_text`) is tested in `editor/hover.rs` and
//! `app.rs`, not at parse time.
//!
//! Prefer **focused unit tests** on pure helpers (`normalize_hover_line_breaks`,
//! `join_hover_content_pieces`, `format_hover_content_piece`, …) plus thinner wire/integration
//! tests — do not rely only on manual hover checks in a running IDE.
//!
//! Intentionally **not** interpreted at parse time:
//!
//! - Markdown / HTML rendering (popup uses plain-text layout in `editor/hover.rs`)
//! - `range` and other hover metadata on the wire result
//! - Rejection of raw JSON, leaked wire JSON, or Rust `Debug` strings — transport may pass
//!   these through verbatim; [`is_undisplayable_hover_text`](crate::editor::hover::is_undisplayable_hover_text)
//!   and `app.rs` `receive_hover()` apply display policy
//!
//! ## Typed-response correlation (do not break)
//!
//! Position requests (`textDocument/completion`, `/hover`, `/definition`) use two ids:
//!
//! - **Wire JSON-RPC `id`** — allocated by the transport thread (`next_id`) and stored in
//!   `pending` until a matching inbound response arrives.
//! - **Caller correlation `id`** — owned by `app.rs` (`next_ui_correlation_id`), passed in
//!   [`LspRequest::Completion`] / [`LspRequest::Hover`] / [`LspRequest::GotoDefinition`], and
//!   echoed on typed [`LspResponse`] variants (`CompletionList`, `HoverResult`, …).
//!
//! `parse_lsp_message` resolves `pending[wire_id]` **before** calling `parse_completion_items`
//! or `parse_hover_content`. Wire normalization must never read, rewrite, or depend on either
//! id. Responses whose wire `id` is not in `pending` are dropped (`ParsedMessage::Ignored`).

use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::path_to_uri;
use super::types::{
    CallHierarchyItem, CodeLensEntry, CodeLensItem, DiagnosticSeverity, IncomingCall,
    LspCompletionItem, LspDiagnostic, LspPositionWire, LspRange, LspRequest, LspResponse,
    LspTextEdit, MessageLevel, OutgoingCall, ProgressKind, SemanticModifiers, SemanticToken,
    TypeHierarchyItem,
};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
enum RequestKind {
    Initialize,
    Completion(u64),
    Hover(u64),
    GotoDefinition(u64),
    References(u64),
    PrepareRename(u64),
    Rename(u64),
    Format(u64),
    RangeFormat(u64),
    InlayHint(u64),
    SignatureHelp(u64),
    WorkspaceSymbol(u64),
    CodeAction(u64),
    Shutdown,
    DocumentSymbol { id: u64, path: PathBuf },
    CodeLens(u64),
    CodeLensResolve(u64),
    SemanticTokensFull(u64),
    SemanticTokensRange(u64),
    PrepareCallHierarchy(u64),
    IncomingCalls(u64),
    OutgoingCalls(u64),
    PrepareTypeHierarchy(u64),
    Supertypes(u64),
    Subtypes(u64),
    ExecuteCommand(u64),
}

enum ReaderEvent {
    Message(String),
    Eof,
}

enum ParsedMessage {
    Response(LspResponse),
    Initialized { token_types: Vec<String> },
    ShutdownAcknowledged,
    Ignored,
}

pub fn spawn_lsp_thread(
    lsp_binary: &str,
    args: &[String],
    root_path: &str,
    to_ui: Sender<LspResponse>,
    from_ui: Receiver<LspRequest>,
) -> JoinHandle<()> {
    let binary = lsp_binary.to_owned();
    let args_owned = args.to_vec();
    let root = root_path.to_owned();
    std::thread::spawn(move || run_transport(binary, args_owned, root, to_ui, from_ui))
}

fn run_transport(
    binary: String,
    args: Vec<String>,
    root: String,
    to_ui: Sender<LspResponse>,
    from_ui: Receiver<LspRequest>,
) {
    let mut child = match Command::new(&binary)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            let _ = to_ui.send(LspResponse::ServerUnavailable {
                message: format!("{} not found — LSP disabled", binary),
            });
            return;
        }
    };

    let Some(mut stdin) = child.stdin.take() else {
        report_unavailable(&binary, &to_ui);
        reap_child(&mut child);
        return;
    };
    let Some(stdout) = child.stdout.take() else {
        report_unavailable(&binary, &to_ui);
        reap_child(&mut child);
        return;
    };

    let (reader_tx, reader_rx) = mpsc::channel();
    let reader_handle = std::thread::spawn(move || read_lsp_messages(stdout, reader_tx));
    let ids = Arc::new(AtomicU64::new(1));
    let mut pending = HashMap::new();
    let mut queued = VecDeque::new();
    let mut initialized = false;
    let mut shutdown_started = None;
    let mut sent_exit = false;

    'transport: loop {
        while let Ok(event) = reader_rx.try_recv() {
            match event {
                ReaderEvent::Message(body) => match parse_lsp_message(&body, &mut pending) {
                    ParsedMessage::Response(response) => {
                        let _ = to_ui.send(response);
                    }
                    ParsedMessage::Initialized { token_types } => {
                        if write_json(
                            &mut stdin,
                            &json!({
                                "jsonrpc": "2.0",
                                "method": "initialized",
                                "params": {}
                            }),
                        )
                        .is_err()
                        {
                            report_unavailable(&binary, &to_ui);
                            break 'transport;
                        }
                        initialized = true;
                        let _ = to_ui.send(LspResponse::Initialized { token_types });
                        while let Some(request) = queued.pop_front() {
                            if send_request(request, &mut stdin, &ids, &mut pending, &root).is_err()
                            {
                                report_unavailable(&binary, &to_ui);
                                break 'transport;
                            }
                        }
                    }
                    ParsedMessage::ShutdownAcknowledged => {
                        let _ =
                            write_json(&mut stdin, &json!({"jsonrpc": "2.0", "method": "exit"}));
                        sent_exit = true;
                        break 'transport;
                    }
                    ParsedMessage::Ignored => {}
                },
                ReaderEvent::Eof => {
                    if shutdown_started.is_none() {
                        report_unavailable(&binary, &to_ui);
                    }
                    break 'transport;
                }
            }
        }

        if shutdown_started.is_some_and(|started: Instant| started.elapsed() >= SHUTDOWN_TIMEOUT) {
            let _ = write_json(&mut stdin, &json!({"jsonrpc": "2.0", "method": "exit"}));
            sent_exit = true;
            break;
        }

        match from_ui.recv_timeout(POLL_INTERVAL) {
            Ok(LspRequest::Shutdown) => {
                if shutdown_started.is_none() {
                    let id = next_id(&ids);
                    pending.insert(id, RequestKind::Shutdown);
                    let message = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "method": "shutdown",
                        "params": null
                    });
                    if write_json(&mut stdin, &message).is_err() {
                        break;
                    }
                    shutdown_started = Some(Instant::now());
                }
            }
            Ok(request @ LspRequest::Initialize { .. }) => {
                if send_request(request, &mut stdin, &ids, &mut pending, &root).is_err() {
                    report_unavailable(&binary, &to_ui);
                    break;
                }
            }
            Ok(request) if initialized => {
                if send_request(request, &mut stdin, &ids, &mut pending, &root).is_err() {
                    report_unavailable(&binary, &to_ui);
                    break;
                }
            }
            Ok(request) => queued.push_back(request),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                if shutdown_started.is_none() {
                    let id = next_id(&ids);
                    pending.insert(id, RequestKind::Shutdown);
                    let _ = write_json(
                        &mut stdin,
                        &json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "method": "shutdown",
                            "params": null
                        }),
                    );
                    shutdown_started = Some(Instant::now());
                }
            }
        }
    }

    if !sent_exit && shutdown_started.is_some() {
        let _ = write_json(&mut stdin, &json!({"jsonrpc": "2.0", "method": "exit"}));
    }
    drop(stdin);
    finish_child(&mut child);
    let _ = reader_handle.join();
}

fn report_unavailable(binary: &str, to_ui: &Sender<LspResponse>) {
    let _ = to_ui.send(LspResponse::ServerUnavailable {
        message: format!("{} not found — LSP disabled", binary),
    });
}

fn next_id(ids: &AtomicU64) -> u64 {
    ids.fetch_add(1, Ordering::Relaxed)
}

fn send_request(
    request: LspRequest,
    stdin: &mut ChildStdin,
    ids: &AtomicU64,
    pending: &mut HashMap<u64, RequestKind>,
    _root: &str,
) -> io::Result<()> {
    let (message, request_kind) = encode_request(request, ids)?;
    if let Some((id, kind)) = request_kind {
        pending.insert(id, kind);
    }
    write_json(stdin, &message)
}

fn encode_request(
    request: LspRequest,
    ids: &AtomicU64,
) -> io::Result<(Value, Option<(u64, RequestKind)>)> {
    let position_request = |method: &str,
                            path: PathBuf,
                            line: u32,
                            col: u32,
                            id: u64,
                            kind: RequestKind|
     -> io::Result<(Value, Option<(u64, RequestKind)>)> {
        // Wire JSON-RPC `id` keys `pending`; caller `id` is stored only in `RequestKind`.
        let wire_id = next_id(ids);
        let uri = path_to_uri(&path)?;
        Ok((
            json!({
                "jsonrpc": "2.0",
                "id": wire_id,
                "method": method,
                "params": {
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": col }
                }
            }),
            Some((
                wire_id,
                match kind {
                    RequestKind::Completion(_) => RequestKind::Completion(id),
                    RequestKind::Hover(_) => RequestKind::Hover(id),
                    RequestKind::GotoDefinition(_) => RequestKind::GotoDefinition(id),
                    RequestKind::References(_) => RequestKind::References(id),
                    RequestKind::PrepareRename(_) => RequestKind::PrepareRename(id),
                    other => other,
                },
            )),
        ))
    };

    match request {
        LspRequest::Initialize { root_uri } => {
            let id = next_id(ids);
            Ok((
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": "initialize",
                    "params": {
                        "processId": std::process::id(),
                        "rootUri": root_uri,
                        "capabilities": {
                            "textDocument": {
                                "synchronization": { "didSave": false },
                                "completion": {},
                                "hover": {},
                                "definition": {},
                                "publishDiagnostics": {},
                                "inlayHint": {
                                    "dynamicRegistration": false
                                },
                                "semanticTokens": {
                                    "requests": {
                                        "full": true,
                                        "range": true
                                    },
                                    "tokenTypes": [
                                        "namespace", "type", "class", "enum", "interface",
                                        "struct", "typeParameter", "parameter", "variable",
                                        "property", "enumMember", "event", "function",
                                        "method", "macro", "keyword", "modifier", "comment",
                                        "string", "number", "regexp", "operator"
                                    ],
                                    "tokenModifiers": [
                                        "declaration", "definition", "readonly", "static",
                                        "deprecated", "abstract", "async", "modification",
                                        "documentation", "defaultLibrary"
                                    ],
                                    "formats": ["relative"]
                                }
                            }
                        },
                        "clientInfo": { "name": "blue-ide", "version": env!("CARGO_PKG_VERSION") }
                    }
                }),
                Some((id, RequestKind::Initialize)),
            ))
        }
        LspRequest::DidOpen {
            path,
            language_id,
            text,
            version,
        } => Ok((
            json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": { "textDocument": {
                    "uri": path_to_uri(&path)?,
                    "languageId": language_id,
                    "version": version,
                    "text": text
                }}
            }),
            None,
        )),
        LspRequest::DidChange {
            path,
            text,
            version,
        } => Ok((
            json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didChange",
                "params": {
                    "textDocument": { "uri": path_to_uri(&path)?, "version": version },
                    "contentChanges": [{ "text": text }]
                }
            }),
            None,
        )),
        LspRequest::DidClose { path } => Ok((
            json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didClose",
                "params": { "textDocument": { "uri": path_to_uri(&path)? } }
            }),
            None,
        )),
        LspRequest::Completion {
            path,
            line,
            col,
            id,
        } => position_request(
            // LSP method for completion requests — do not substitute another method name.
            "textDocument/completion",
            path,
            line,
            col,
            id,
            RequestKind::Completion(id),
        ),
        LspRequest::Hover {
            path,
            line,
            col,
            id,
        } => position_request(
            // LSP method for hover requests — do not substitute another method name.
            "textDocument/hover",
            path,
            line,
            col,
            id,
            RequestKind::Hover(id),
        ),
        LspRequest::GotoDefinition {
            path,
            line,
            col,
            id,
        } => position_request(
            "textDocument/definition",
            path,
            line,
            col,
            id,
            RequestKind::GotoDefinition(id),
        ),
        LspRequest::References {
            path,
            line,
            col,
            id,
        } => position_request(
            "textDocument/references",
            path,
            line,
            col,
            id,
            RequestKind::References(id),
        ),
        LspRequest::PrepareRename {
            path,
            line,
            col,
            id,
        } => position_request(
            "textDocument/prepareRename",
            path,
            line,
            col,
            id,
            RequestKind::PrepareRename(id),
        ),
        LspRequest::Rename {
            path,
            line,
            col,
            new_name,
            id,
        } => {
            let wire_id = next_id(ids);
            let uri = path_to_uri(&path)?;
            Ok((
                json!({
                    "jsonrpc": "2.0",
                    "id": wire_id,
                    "method": "textDocument/rename",
                    "params": {
                        "textDocument": { "uri": uri },
                        "position": { "line": line, "character": col },
                        "newName": new_name
                    }
                }),
                Some((wire_id, RequestKind::Rename(id))),
            ))
        }
        LspRequest::DocumentSymbol { path, id } => {
            let wire_id = next_id(ids);
            let uri = path_to_uri(&path)?;
            Ok((
                json!({
                    "jsonrpc": "2.0",
                    "id": wire_id,
                    "method": "textDocument/documentSymbol",
                    "params": {
                        "textDocument": { "uri": uri }
                    }
                }),
                Some((
                    wire_id,
                    RequestKind::DocumentSymbol {
                        id,
                        path: path.clone(),
                    },
                )),
            ))
        }
        LspRequest::Format {
            path,
            tab_size,
            insert_spaces,
            id,
        } => {
            let wire_id = next_id(ids);
            let uri = path_to_uri(&path)?;
            Ok((
                json!({
                    "jsonrpc": "2.0",
                    "id": wire_id,
                    "method": "textDocument/formatting",
                    "params": {
                        "textDocument": { "uri": uri },
                        "options": {
                            "tabSize": tab_size,
                            "insertSpaces": insert_spaces
                        }
                    }
                }),
                Some((wire_id, RequestKind::Format(id))),
            ))
        }
        LspRequest::RangeFormat {
            path,
            tab_size,
            insert_spaces,
            range: (start_line, start_char, end_line, end_char),
            id,
        } => {
            let wire_id = next_id(ids);
            let uri = path_to_uri(&path)?;
            Ok((
                json!({
                    "jsonrpc": "2.0",
                    "id": wire_id,
                    "method": "textDocument/rangeFormatting",
                    "params": {
                        "textDocument": { "uri": uri },
                        "range": {
                            "start": { "line": start_line, "character": start_char },
                            "end":   { "line": end_line,   "character": end_char }
                        },
                        "options": {
                            "tabSize": tab_size,
                            "insertSpaces": insert_spaces
                        }
                    }
                }),
                Some((wire_id, RequestKind::RangeFormat(id))),
            ))
        }
        LspRequest::InlayHint {
            path,
            start_line,
            end_line,
            id,
        } => {
            let wire_id = next_id(ids);
            let uri = path_to_uri(&path)?;
            Ok((
                json!({
                    "jsonrpc": "2.0",
                    "id": wire_id,
                    "method": "textDocument/inlayHint",
                    "params": {
                        "textDocument": { "uri": uri },
                        "range": {
                            "start": { "line": start_line, "character": 0 },
                            "end":   { "line": end_line,   "character": 0 }
                        }
                    }
                }),
                Some((wire_id, RequestKind::InlayHint(id))),
            ))
        }
        LspRequest::Shutdown => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "shutdown is handled by the transport lifecycle",
        )),
        LspRequest::SignatureHelp {
            path,
            line,
            col,
            id,
        } => position_request(
            "textDocument/signatureHelp",
            path,
            line,
            col,
            id,
            RequestKind::SignatureHelp(id),
        ),
        LspRequest::WorkspaceSymbol { query, id } => {
            let wire_id = next_id(ids);
            Ok((
                json!({
                    "jsonrpc": "2.0",
                    "id": wire_id,
                    "method": "workspace/symbol",
                    "params": { "query": query }
                }),
                Some((wire_id, RequestKind::WorkspaceSymbol(id))),
            ))
        }
        LspRequest::CodeAction {
            path,
            range: (start_line, start_char, end_line, end_char),
            diagnostics,
            id,
        } => {
            let wire_id = next_id(ids);
            let uri = path_to_uri(&path)?;
            let context_diags: Vec<Value> = diagnostics
                .iter()
                .map(|diag| {
                    json!({
                        "range": {
                            "start": { "line": diag.line_start, "character": diag.col_start },
                            "end": { "line": diag.line_end, "character": diag.col_end }
                        },
                        "severity": match diag.severity {
                            DiagnosticSeverity::Error => 1,
                            DiagnosticSeverity::Warning => 2,
                            DiagnosticSeverity::Information => 3,
                            DiagnosticSeverity::Hint => 4,
                        },
                        "message": diag.message,
                        "code": diag.code
                    })
                })
                .collect();
            Ok((
                json!({
                    "jsonrpc": "2.0",
                    "id": wire_id,
                    "method": "textDocument/codeAction",
                    "params": {
                        "textDocument": { "uri": uri },
                        "range": {
                            "start": { "line": start_line, "character": start_char },
                            "end":   { "line": end_line,   "character": end_char }
                        },
                        "context": { "diagnostics": context_diags }
                    }
                }),
                Some((wire_id, RequestKind::CodeAction(id))),
            ))
        }
        LspRequest::CodeLens { path, id } => {
            let wire_id = next_id(ids);
            let uri = path_to_uri(&path)?;
            Ok((
                json!({
                    "jsonrpc": "2.0",
                    "id": wire_id,
                    "method": "textDocument/codeLens",
                    "params": {
                        "textDocument": { "uri": uri }
                    }
                }),
                Some((wire_id, RequestKind::CodeLens(id))),
            ))
        }
        LspRequest::CodeLensResolve { item, id } => {
            let wire_id = next_id(ids);
            let entry = item.entries.first().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "CodeLensResolve requires at least one entry",
                )
            })?;
            let start_line = item.line as u32;
            let range = entry.range.clone().unwrap_or(LspRange {
                start: LspPositionWire {
                    line: start_line,
                    character: 0,
                },
                end: LspPositionWire {
                    line: start_line,
                    character: 0,
                },
            });
            Ok((
                json!({
                    "jsonrpc": "2.0",
                    "id": wire_id,
                    "method": "codeLens/resolve",
                    "params": {
                        "range": range,
                        "data": entry.data
                    }
                }),
                Some((wire_id, RequestKind::CodeLensResolve(id))),
            ))
        }
        LspRequest::SemanticTokensFull { path, id } => {
            let wire_id = next_id(ids);
            let uri = path_to_uri(&path)?;
            Ok((
                json!({
                    "jsonrpc": "2.0",
                    "id": wire_id,
                    "method": "textDocument/semanticTokens/full",
                    "params": {
                        "textDocument": { "uri": uri }
                    }
                }),
                Some((wire_id, RequestKind::SemanticTokensFull(id))),
            ))
        }
        LspRequest::SemanticTokensRange {
            path,
            start_line,
            end_line,
            id,
        } => {
            let wire_id = next_id(ids);
            let uri = path_to_uri(&path)?;
            Ok((
                json!({
                    "jsonrpc": "2.0",
                    "id": wire_id,
                    "method": "textDocument/semanticTokens/range",
                    "params": {
                        "textDocument": { "uri": uri },
                        "range": {
                            "start": { "line": start_line, "character": 0 },
                            "end": { "line": end_line, "character": 0 }
                        }
                    }
                }),
                Some((wire_id, RequestKind::SemanticTokensRange(id))),
            ))
        }
        LspRequest::PrepareCallHierarchy {
            path,
            line,
            col,
            id,
        } => {
            let wire_id = next_id(ids);
            let uri = path_to_uri(&path)?;
            Ok((
                json!({
                    "jsonrpc": "2.0",
                    "id": wire_id,
                    "method": "textDocument/prepareCallHierarchy",
                    "params": {
                        "textDocument": { "uri": uri },
                        "position": { "line": line, "character": col }
                    }
                }),
                Some((wire_id, RequestKind::PrepareCallHierarchy(id))),
            ))
        }
        LspRequest::IncomingCalls { item, id } => {
            let wire_id = next_id(ids);
            Ok((
                json!({
                    "jsonrpc": "2.0",
                    "id": wire_id,
                    "method": "callHierarchy/incomingCalls",
                    "params": {
                        "item": item
                    }
                }),
                Some((wire_id, RequestKind::IncomingCalls(id))),
            ))
        }
        LspRequest::OutgoingCalls { item, id } => {
            let wire_id = next_id(ids);
            Ok((
                json!({
                    "jsonrpc": "2.0",
                    "id": wire_id,
                    "method": "callHierarchy/outgoingCalls",
                    "params": {
                        "item": item
                    }
                }),
                Some((wire_id, RequestKind::OutgoingCalls(id))),
            ))
        }
        LspRequest::PrepareTypeHierarchy {
            path,
            line,
            col,
            id,
        } => {
            let wire_id = next_id(ids);
            let uri = path_to_uri(&path)?;
            Ok((
                json!({
                    "jsonrpc": "2.0",
                    "id": wire_id,
                    "method": "textDocument/prepareTypeHierarchy",
                    "params": {
                        "textDocument": { "uri": uri },
                        "position": { "line": line, "character": col }
                    }
                }),
                Some((wire_id, RequestKind::PrepareTypeHierarchy(id))),
            ))
        }
        LspRequest::Supertypes { item, id } => {
            let wire_id = next_id(ids);
            Ok((
                json!({
                    "jsonrpc": "2.0",
                    "id": wire_id,
                    "method": "typeHierarchy/supertypes",
                    "params": {
                        "item": item
                    }
                }),
                Some((wire_id, RequestKind::Supertypes(id))),
            ))
        }
        LspRequest::Subtypes { item, id } => {
            let wire_id = next_id(ids);
            Ok((
                json!({
                    "jsonrpc": "2.0",
                    "id": wire_id,
                    "method": "typeHierarchy/subtypes",
                    "params": {
                        "item": item
                    }
                }),
                Some((wire_id, RequestKind::Subtypes(id))),
            ))
        }
        LspRequest::ExecuteCommand { command, args, id } => {
            let wire_id = next_id(ids);
            let arguments = if args.is_array() {
                args.clone()
            } else {
                json!([args])
            };
            Ok((
                json!({
                    "jsonrpc": "2.0",
                    "id": wire_id,
                    "method": "workspace/executeCommand",
                    "params": {
                        "command": command,
                        "arguments": arguments
                    }
                }),
                Some((wire_id, RequestKind::ExecuteCommand(id))),
            ))
        }
    }
}

fn write_json<W: Write>(writer: &mut W, value: &Value) -> io::Result<()> {
    let content = serde_json::to_string(value).map_err(io::Error::other)?;
    write_message(writer, &content)
}

fn write_message<W: Write>(writer: &mut W, content: &str) -> io::Result<()> {
    // LSP frames JSON with an ASCII header whose length is the UTF-8 byte count,
    // not the number of Rust characters in the JSON string. Header/body separator is CRLF.
    write!(writer, "Content-Length: {}\r\n\r\n", content.len())?;
    writer.write_all(content.as_bytes())?;
    writer.flush()
}

fn read_lsp_messages(stdout: ChildStdout, to_transport: Sender<ReaderEvent>) {
    let mut reader = BufReader::new(stdout);
    loop {
        match read_frame(&mut reader) {
            Ok(Some(body)) => {
                if to_transport.send(ReaderEvent::Message(body)).is_err() {
                    return;
                }
            }
            Ok(None) | Err(_) => {
                let _ = to_transport.send(ReaderEvent::Eof);
                return;
            }
        }
    }
}

/// Read one LSP frame. `Content-Length` is interpreted as a UTF-8 byte count; extra headers
/// are ignored. The body is decoded with [`String::from_utf8`].
fn read_frame<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("Content-Length") {
                content_length = value.trim().parse::<usize>().ok();
            }
        }
    }

    let Some(length) = content_length else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "missing Content-Length header",
        ));
    };
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    String::from_utf8(body)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

use std::sync::Mutex;
static LEGENDS: Mutex<Option<(Vec<String>, Vec<String>)>> = Mutex::new(None);

fn parse_lsp_message(body: &str, pending: &mut HashMap<u64, RequestKind>) -> ParsedMessage {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return ParsedMessage::Ignored;
    };

    if let Some(method) = value.get("method").and_then(Value::as_str) {
        return match method {
            "textDocument/publishDiagnostics" => parse_diagnostics(&value)
                .map(ParsedMessage::Response)
                .unwrap_or(ParsedMessage::Ignored),
            "$/progress" => parse_progress(&value)
                .map(ParsedMessage::Response)
                .unwrap_or(ParsedMessage::Ignored),
            "window/showMessage" => parse_show_message(&value)
                .map(ParsedMessage::Response)
                .unwrap_or(ParsedMessage::Ignored),
            "window/logMessage" => ParsedMessage::Ignored,
            _ => ParsedMessage::Ignored,
        };
    }

    let Some(wire_id) = value.get("id").and_then(Value::as_u64) else {
        return ParsedMessage::Ignored;
    };
    let Some(kind) = pending.remove(&wire_id) else {
        return ParsedMessage::Ignored;
    };

    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("LSP request failed")
            .to_owned();
        return match kind {
            RequestKind::Initialize => ParsedMessage::Response(LspResponse::ServerUnavailable {
                message: "rust-analyzer not found — LSP disabled".to_owned(),
            }),
            RequestKind::Shutdown => ParsedMessage::ShutdownAcknowledged,
            RequestKind::Completion(id)
            | RequestKind::Hover(id)
            | RequestKind::GotoDefinition(id)
            | RequestKind::References(id)
            | RequestKind::PrepareRename(id)
            | RequestKind::Rename(id)
            | RequestKind::Format(id)
            | RequestKind::RangeFormat(id)
            | RequestKind::InlayHint(id)
            | RequestKind::SignatureHelp(id)
            | RequestKind::WorkspaceSymbol(id)
            | RequestKind::CodeAction(id)
            | RequestKind::DocumentSymbol { id, .. }
            | RequestKind::CodeLens(id)
            | RequestKind::CodeLensResolve(id)
            | RequestKind::SemanticTokensFull(id)
            | RequestKind::SemanticTokensRange(id)
            | RequestKind::PrepareCallHierarchy(id)
            | RequestKind::IncomingCalls(id)
            | RequestKind::OutgoingCalls(id)
            | RequestKind::PrepareTypeHierarchy(id)
            | RequestKind::Supertypes(id)
            | RequestKind::Subtypes(id)
            | RequestKind::ExecuteCommand(id) => {
                ParsedMessage::Response(LspResponse::Error { id, message })
            }
        };
    }

    let result = value.get("result").cloned().unwrap_or(Value::Null);
    match kind {
        RequestKind::Initialize => {
            let capabilities = result.get("capabilities");
            let sem_tokens = capabilities.and_then(|c| c.get("semanticTokensProvider"));
            let legend = sem_tokens.and_then(|s| s.get("legend"));
            let token_types = legend
                .and_then(|l| l.get("tokenTypes"))
                .and_then(|t| t.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default();
            let token_modifiers = legend
                .and_then(|l| l.get("tokenModifiers"))
                .and_then(|t| t.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default();
            if let Ok(mut guard) = LEGENDS.lock() {
                *guard = Some((token_types.clone(), token_modifiers));
            }
            ParsedMessage::Initialized { token_types }
        }
        RequestKind::Shutdown => ParsedMessage::ShutdownAcknowledged,
        RequestKind::Completion(id) => {
            // Caller correlation `id` is independent of completion item parsing below.
            ParsedMessage::Response(LspResponse::CompletionList {
                id,
                items: parse_completion_items(&result),
            })
        }
        RequestKind::Hover(id) => {
            // Caller correlation `id` is independent of hover content flattening below.
            ParsedMessage::Response(LspResponse::HoverResult {
                id,
                content: parse_hover_content(&result),
            })
        }
        RequestKind::GotoDefinition(id) => parse_goto_result(id, &result)
            .map(ParsedMessage::Response)
            .unwrap_or_else(|| ParsedMessage::Response(LspResponse::GotoNone { id })),
        RequestKind::References(id) => ParsedMessage::Response(LspResponse::ReferencesResult {
            id,
            locations: parse_references(&result),
        }),
        RequestKind::PrepareRename(id) => {
            ParsedMessage::Response(LspResponse::PrepareRenameResult {
                id,
                range: parse_prepare_rename(&result),
            })
        }
        RequestKind::Rename(id) => ParsedMessage::Response(LspResponse::RenameResult {
            id,
            edits: parse_rename_edits(&result),
        }),
        RequestKind::DocumentSymbol { id, path } => {
            ParsedMessage::Response(LspResponse::SymbolList {
                id,
                path,
                symbols: parse_document_symbols(&result),
            })
        }
        RequestKind::Format(id) | RequestKind::RangeFormat(id) => {
            ParsedMessage::Response(LspResponse::FormatResult {
                id,
                edits: result
                    .as_array()
                    .map(|arr| arr.iter().filter_map(parse_single_text_edit).collect())
                    .unwrap_or_default(),
            })
        }
        RequestKind::InlayHint(id) => ParsedMessage::Response(LspResponse::InlayHintResult {
            id,
            hints: parse_inlay_hints(&result),
        }),
        RequestKind::SignatureHelp(id) => {
            ParsedMessage::Response(LspResponse::SignatureHelpResult {
                id,
                active: parse_signature_help(&result),
            })
        }
        RequestKind::WorkspaceSymbol(id) => {
            ParsedMessage::Response(LspResponse::WorkspaceSymbolResult {
                id,
                symbols: parse_workspace_symbols(&result),
            })
        }
        RequestKind::CodeAction(id) => ParsedMessage::Response(LspResponse::CodeActionResult {
            id,
            actions: parse_code_actions(&result),
        }),
        RequestKind::CodeLens(id) => ParsedMessage::Response(LspResponse::CodeLensResult {
            id,
            lenses: parse_code_lenses(&result),
        }),
        RequestKind::CodeLensResolve(id) => {
            let resolved = parse_single_code_lens(&result);
            ParsedMessage::Response(LspResponse::CodeLensResult {
                id,
                lenses: resolved.into_iter().collect(),
            })
        }
        RequestKind::SemanticTokensFull(id) | RequestKind::SemanticTokensRange(id) => {
            let (token_types, token_modifiers) = if let Ok(guard) = LEGENDS.lock() {
                guard.clone().unwrap_or_default()
            } else {
                (Vec::new(), Vec::new())
            };
            ParsedMessage::Response(LspResponse::SemanticTokensResult {
                id,
                tokens: parse_semantic_tokens(&result, &token_types, &token_modifiers),
            })
        }
        RequestKind::PrepareCallHierarchy(id) => {
            ParsedMessage::Response(LspResponse::CallHierarchyPrepareResult {
                id,
                items: parse_call_hierarchy_items(&result),
            })
        }
        RequestKind::IncomingCalls(id) => {
            ParsedMessage::Response(LspResponse::IncomingCallsResult {
                id,
                calls: parse_incoming_calls(&result),
            })
        }
        RequestKind::OutgoingCalls(id) => {
            ParsedMessage::Response(LspResponse::OutgoingCallsResult {
                id,
                calls: parse_outgoing_calls(&result),
            })
        }
        RequestKind::PrepareTypeHierarchy(id) => {
            ParsedMessage::Response(LspResponse::TypeHierarchyPrepareResult {
                id,
                items: parse_type_hierarchy_items(&result),
            })
        }
        RequestKind::Supertypes(id) => ParsedMessage::Response(LspResponse::SupertypesResult {
            id,
            items: parse_type_hierarchy_items(&result),
        }),
        RequestKind::Subtypes(id) => ParsedMessage::Response(LspResponse::SubtypesResult {
            id,
            items: parse_type_hierarchy_items(&result),
        }),
        RequestKind::ExecuteCommand(id) => ParsedMessage::Response(LspResponse::RenameResult {
            id,
            edits: Vec::new(),
        }),
    }
}

fn parse_progress(value: &Value) -> Option<LspResponse> {
    let params = value.get("params")?;
    let token = params
        .get("token")?
        .as_str()
        .map(String::from)
        .or_else(|| params.get("token")?.as_u64().map(|n| n.to_string()))
        .or_else(|| params.get("token")?.as_i64().map(|n| n.to_string()))?;
    let val_obj = params.get("value")?;
    let kind_str = val_obj.get("kind")?.as_str()?;
    let kind = match kind_str {
        "begin" => ProgressKind::Begin,
        "work" | "report" => ProgressKind::Work,
        "end" => ProgressKind::End,
        _ => return None,
    };
    let title = val_obj
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let message = val_obj
        .get("message")
        .and_then(Value::as_str)
        .map(String::from);
    let percentage = val_obj
        .get("percentage")
        .and_then(Value::as_u64)
        .map(|n| n as u32);
    Some(LspResponse::Progress {
        token,
        kind,
        title,
        message,
        percentage,
    })
}

fn parse_show_message(value: &Value) -> Option<LspResponse> {
    let params = value.get("params")?;
    let msg_type = params.get("type")?.as_u64()?;
    let message = params.get("message")?.as_str()?.to_owned();
    let level = match msg_type {
        1 => MessageLevel::Error,
        2 => MessageLevel::Warning,
        3 => MessageLevel::Info,
        4 => MessageLevel::Log,
        _ => MessageLevel::Log,
    };
    Some(LspResponse::ServerMessage { level, message })
}

fn parse_code_lenses(result: &Value) -> Vec<CodeLensItem> {
    let mut by_line: std::collections::BTreeMap<usize, Vec<CodeLensEntry>> =
        std::collections::BTreeMap::new();
    if let Some(arr) = result.as_array() {
        for val in arr {
            if let Some(lens) = parse_single_lens_entry(val) {
                let line = lens
                    .range
                    .as_ref()
                    .map(|r| r.start.line as usize)
                    .unwrap_or(0);
                by_line.entry(line).or_default().push(lens);
            }
        }
    }
    by_line
        .into_iter()
        .map(|(line, entries)| CodeLensItem { line, entries })
        .collect()
}

fn parse_single_lens_entry(val: &Value) -> Option<CodeLensEntry> {
    let range: LspRange = serde_json::from_value(val.get("range").cloned()?).ok()?;
    let command_val = val.get("command");
    let (title, command, arguments) = if let Some(cmd) = command_val {
        let title = cmd
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let command = cmd
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let arguments = cmd.get("arguments").cloned().unwrap_or(Value::Null);
        (title, command, arguments)
    } else {
        (String::new(), String::new(), Value::Null)
    };
    let data = val.get("data").cloned();
    Some(CodeLensEntry {
        title,
        command,
        arguments,
        data,
        range: Some(range),
    })
}

fn parse_single_code_lens(result: &Value) -> Option<CodeLensItem> {
    let entry = parse_single_lens_entry(result)?;
    let line = entry
        .range
        .as_ref()
        .map(|r| r.start.line as usize)
        .unwrap_or(0);
    Some(CodeLensItem {
        line,
        entries: vec![entry],
    })
}

fn parse_semantic_tokens(
    result: &Value,
    legend_types: &[String],
    legend_modifiers: &[String],
) -> Vec<SemanticToken> {
    let mut tokens = Vec::new();
    let Some(data) = result.get("data").and_then(Value::as_array) else {
        return tokens;
    };

    let mut line = 0;
    let mut col = 0;
    let mut i = 0;
    while i + 4 < data.len() {
        let delta_line = data[i].as_u64().unwrap_or(0) as u32;
        let delta_start = data[i + 1].as_u64().unwrap_or(0) as u32;
        let length = data[i + 2].as_u64().unwrap_or(0) as u32;
        let token_type_idx = data[i + 3].as_u64().unwrap_or(0) as usize;
        let token_modifiers_mask = data[i + 4].as_u64().unwrap_or(0) as u32;

        line += delta_line;
        if delta_line > 0 {
            col = delta_start;
        } else {
            col += delta_start;
        }

        let token_type = legend_types
            .get(token_type_idx)
            .map(|s| s.as_str())
            .unwrap_or("variable")
            .to_string();

        let mut modifiers = SemanticModifiers::default();
        for (bit_idx, modifier_name) in legend_modifiers.iter().enumerate() {
            if (token_modifiers_mask & (1 << bit_idx)) != 0 {
                match modifier_name.as_str() {
                    "declaration" => modifiers.declaration = true,
                    "definition" => modifiers.definition = true,
                    "readonly" => modifiers.readonly = true,
                    "static" => modifiers.r#static = true,
                    "deprecated" => modifiers.deprecated = true,
                    "abstract" => modifiers.r#abstract = true,
                    "async" => modifiers.r#async = true,
                    "modification" => modifiers.modification = true,
                    "documentation" => modifiers.documentation = true,
                    "defaultLibrary" => modifiers.default_library = true,
                    _ => {}
                }
            }
        }

        let (color, italic, underline) = get_semantic_styles(&token_type, &modifiers);

        tokens.push(SemanticToken {
            line,
            col,
            length,
            token_type,
            modifiers,
            color,
            italic,
            underline,
        });

        i += 5;
    }
    tokens
}

fn get_semantic_styles(
    token_type: &str,
    modifiers: &SemanticModifiers,
) -> (egui::Color32, bool, bool) {
    let mut color = match token_type {
        "namespace" => egui::Color32::from_rgb(186, 104, 200), // purple
        "type" | "class" | "enum" | "struct" | "typeParameter" => {
            egui::Color32::from_rgb(79, 195, 247)
        } // teal
        "interface" | "event" => egui::Color32::from_rgb(255, 213, 79), // yellow
        "parameter" => egui::Color32::from_rgb(144, 202, 249), // light blue
        "property" => egui::Color32::from_rgb(128, 222, 234),  // light teal
        "enumMember" => egui::Color32::from_rgb(129, 199, 132), // green
        "function" | "method" => egui::Color32::from_rgb(41, 121, 255), // blue
        "macro" => egui::Color32::from_rgb(255, 183, 77),      // yellow-orange
        "keyword" | "modifier" => egui::Color32::from_rgb(33, 150, 243), // blue
        "comment" => egui::Color32::from_rgb(158, 158, 158),   // gray
        "string" | "regexp" => egui::Color32::from_rgb(255, 138, 101), // orange
        "number" => egui::Color32::from_rgb(197, 225, 165),    // light green
        _ => egui::Color32::from_rgb(240, 240, 240),           // default
    };

    if modifiers.declaration || modifiers.definition {
        color = egui::Color32::from_rgb(
            color.r().saturating_add(30),
            color.g().saturating_add(30),
            color.b().saturating_add(30),
        );
    }
    if modifiers.default_library {
        color = egui::Color32::from_rgb(
            color.r().saturating_sub(30),
            color.g().saturating_sub(30),
            color.b().saturating_sub(30),
        );
    }
    if modifiers.documentation {
        color = egui::Color32::from_rgb(76, 175, 80); // green
    }

    let italic = modifiers.readonly || modifiers.r#static || modifiers.r#abstract;
    let underline = modifiers.r#async;

    (color, italic, underline)
}

fn parse_call_hierarchy_items(result: &Value) -> Vec<CallHierarchyItem> {
    serde_json::from_value(result.clone()).unwrap_or_default()
}

fn parse_incoming_calls(result: &Value) -> Vec<IncomingCall> {
    serde_json::from_value(result.clone()).unwrap_or_default()
}

fn parse_outgoing_calls(result: &Value) -> Vec<OutgoingCall> {
    serde_json::from_value(result.clone()).unwrap_or_default()
}

fn parse_type_hierarchy_items(result: &Value) -> Vec<TypeHierarchyItem> {
    serde_json::from_value(result.clone()).unwrap_or_default()
}

fn parse_diagnostics(value: &Value) -> Option<LspResponse> {
    let params = value.get("params")?;
    let uri = params.get("uri")?.as_str()?;
    let path = lsp_types::Url::parse(uri).ok()?.to_file_path().ok()?;
    let diagnostics = params
        .get("diagnostics")?
        .as_array()?
        .iter()
        .filter_map(|diagnostic| {
            let range = diagnostic.get("range")?;
            let start = range.get("start")?;
            let end = range.get("end")?;
            Some(LspDiagnostic {
                line_start: u32::try_from(start.get("line")?.as_u64()?).ok()?,
                col_start: u32::try_from(start.get("character")?.as_u64()?).ok()?,
                line_end: u32::try_from(end.get("line")?.as_u64()?).ok()?,
                col_end: u32::try_from(end.get("character")?.as_u64()?).ok()?,
                severity: match diagnostic.get("severity").and_then(Value::as_u64) {
                    Some(1) => DiagnosticSeverity::Error,
                    Some(2) => DiagnosticSeverity::Warning,
                    Some(4) => DiagnosticSeverity::Hint,
                    _ => DiagnosticSeverity::Information,
                },
                message: diagnostic.get("message")?.as_str()?.to_owned(),
                code: diagnostic.get("code").and_then(value_to_string),
            })
        })
        .collect();
    Some(LspResponse::Diagnostics { path, diagnostics })
}

fn value_to_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|number| number.to_string()))
}

fn parse_lsp_range(range: &Value) -> Option<LspTextEdit> {
    let start = range.get("start")?;
    let end = range.get("end")?;
    Some(LspTextEdit {
        line_start: u32::try_from(start.get("line")?.as_u64()?).ok()?,
        col_start: u32::try_from(start.get("character")?.as_u64()?).ok()?,
        line_end: u32::try_from(end.get("line")?.as_u64()?).ok()?,
        col_end: u32::try_from(end.get("character")?.as_u64()?).ok()?,
        new_text: String::new(),
    })
}

/// Parse the single primary completion `textEdit` wire value.
///
/// Accepts a plain `TextEdit` (`range` + `newText`) or an `InsertReplaceEdit` (`insert`,
/// `replace`, `newText`). Normalizes to one [`LspTextEdit`]; `replace` is stored only
/// implicitly as ignored. `newText` is kept verbatim — snippet placeholders are not expanded.
fn parse_completion_text_edit(value: &Value) -> Option<LspTextEdit> {
    let new_text = value.get("newText")?.as_str()?.to_owned();
    let range = value
        .get("range")
        .or_else(|| value.get("insert"))
        .and_then(parse_lsp_range)?;
    Some(LspTextEdit { new_text, ..range })
}

fn completion_kind_wire_value(value: &Value) -> Option<String> {
    if let Some(name) = value.as_str() {
        return Some(name.to_owned());
    }
    let kind = value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))?;
    Some(completion_kind_from_lsp(kind).to_owned())
}

/// Wire popup kind. Kept separate from `label`, `detail`, `insert_text`, and `text_edit`.
fn completion_item_kind(item: &Value) -> Option<String> {
    item.get("kind").and_then(completion_kind_wire_value)
}

fn completion_kind_from_lsp(kind: u64) -> &'static str {
    match kind {
        1 => "Text",
        2 => "Method",
        3 => "Function",
        4 => "Constructor",
        5 => "Field",
        6 => "Variable",
        7 => "Class",
        8 => "Interface",
        9 => "Module",
        10 => "Property",
        11 => "Unit",
        12 => "Value",
        13 => "Enum",
        14 => "Keyword",
        15 => "Snippet",
        16 => "Color",
        17 => "File",
        18 => "Reference",
        19 => "Folder",
        20 => "EnumMember",
        21 => "Constant",
        22 => "Struct",
        23 => "Event",
        24 => "Operator",
        25 => "TypeParameter",
        _ => "Text",
    }
}

fn completion_detail_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

/// Wire popup detail. Kept separate from `label`, `insert_text`, and `text_edit.new_text`.
fn completion_item_detail(item: &Value) -> Option<String> {
    completion_detail_string(item.get("detail"))
        .or_else(|| {
            item.get("label")
                .and_then(Value::as_object)
                .and_then(|object| completion_detail_string(object.get("detail")))
        })
        .or_else(|| {
            item.get("labelDetails")
                .and_then(|details| completion_detail_string(details.get("detail")))
        })
        .or_else(|| {
            item.get("labelDetails")
                .and_then(|details| completion_detail_string(details.get("description")))
        })
}

fn completion_insert_text_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

/// Wire insertion text for the plain apply path.
///
/// Wire `insertText` is retained verbatim when present. When absent, `textEdit.newText` is
/// used first, then `label`. `detail` is never copied into `insert_text`.
fn completion_item_insert_text(
    item: &Value,
    label: &str,
    text_edit: Option<&LspTextEdit>,
) -> Option<String> {
    completion_insert_text_string(item.get("insertText"))
        .or_else(|| text_edit.map(|edit| edit.new_text.clone()))
        .or_else(|| Some(label.to_owned()))
}

/// Wire display label. Kept separate from `insert_text` / `text_edit.new_text`.
fn completion_item_label(item: &Value) -> Option<String> {
    let label = item.get("label")?;
    if let Some(text) = label.as_str() {
        return Some(text.to_owned());
    }
    label
        .as_object()
        .and_then(|object| object.get("label"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Item array from either a bare `CompletionItem[]` or a `CompletionList.items` array.
///
/// `isIncomplete` and other CompletionList fields are ignored; only `items` is read.
fn completion_result_item_array(result: &Value) -> Option<&Vec<Value>> {
    result
        .as_array()
        .or_else(|| result.get("items").and_then(Value::as_array))
}

/// Normalize a `textDocument/completion` wire `result` to [`LspCompletionItem`] values.
///
/// Accepts direct `CompletionItem[]` or `CompletionList` (`items` only).
fn parse_completion_items(result: &Value) -> Vec<LspCompletionItem> {
    if result.is_null() {
        return Vec::new();
    }
    completion_result_item_array(result)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let label = completion_item_label(item)?;
            let text_edit = item.get("textEdit").and_then(parse_completion_text_edit);
            let insert_text = completion_item_insert_text(item, &label, text_edit.as_ref());
            let filter_text = item
                .get("filterText")
                .and_then(Value::as_str)
                .map(str::to_owned);
            Some(LspCompletionItem {
                label,
                kind: completion_item_kind(item),
                detail: completion_item_detail(item),
                insert_text,
                text_edit,
                filter_text,
            })
        })
        .collect()
}

/// Wire `contents` string (`MarkedString` shorthand).
///
/// The body is kept verbatim; only CRLF/CR line endings are normalized to LF.
fn hover_string_content(text: &str) -> String {
    normalize_hover_line_breaks(text)
}

/// Flatten a `textDocument/hover` wire `result` to one display string.
///
/// Accepts string, `MarkedString` array, `MarkupContent` / `MarkedString` object, and `null`
/// results. Unknown `contents` types and invalid array elements are ignored without panicking.
/// Does not apply display heuristics.
fn parse_hover_content(result: &Value) -> String {
    if result.is_null() {
        return String::new();
    }
    let Some(contents) = result.get("contents") else {
        return String::new();
    };
    match contents {
        // LSP MarkedString shorthand.
        Value::String(text) => hover_string_content(text),
        // MarkedString array (string | MarkedString object | MarkupContent per element).
        Value::Array(values) => hover_contents_array(values),
        // Single MarkedString or MarkupContent object (`value` field carries the body).
        Value::Object(_) => format_hover_content_piece(contents).unwrap_or_default(),
        _ => String::new(),
    }
}

/// Normalize CRLF/CR to LF without collapsing paragraph breaks.
fn normalize_hover_line_breaks(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                normalized.push('\n');
            }
            other => normalized.push(other),
        }
    }
    normalized
}

/// Wire `contents` array (`MarkedString` / `MarkupContent` elements).
///
/// Each element is flattened in wire order. Invalid elements are skipped. Pieces are joined with
/// single newlines, avoiding duplicate blank lines between adjacent segments.
fn hover_contents_array(values: &[Value]) -> String {
    let pieces = values
        .iter()
        .filter_map(format_hover_content_piece)
        .collect::<Vec<_>>();
    join_hover_content_pieces(&pieces)
}

/// Join flattened hover pieces with single newlines, avoiding duplicate separators.
fn join_hover_content_pieces(pieces: &[String]) -> String {
    let mut output = String::new();
    for (index, piece) in pieces.iter().enumerate() {
        if index > 0 {
            let needs_separator = !output.ends_with('\n') && !piece.starts_with('\n');
            if needs_separator {
                output.push('\n');
            }
        }
        output.push_str(piece);
    }
    output
}

/// Wire `MarkupContent` object containing `value`.
///
/// `value` is retained verbatim except CRLF → LF. `kind` (`markdown`, `plaintext`, …) is not
/// rendered or interpreted at parse time.
fn hover_markup_content_object(object: &serde_json::Map<String, Value>) -> Option<String> {
    let _kind = object.get("kind").and_then(Value::as_str)?;
    object
        .get("value")
        .and_then(Value::as_str)
        .map(hover_string_content)
}

/// Wire `MarkedString` object (`language` + `value`).
///
/// `value` is retained verbatim except CRLF → LF. Non-empty `language` wraps the body in a
/// fenced code block; empty or absent `language` returns the body only.
fn hover_marked_string_object(object: &serde_json::Map<String, Value>) -> Option<String> {
    if object.get("kind").is_some() {
        return None;
    }
    let body = hover_string_content(object.get("value")?.as_str()?);
    match object.get("language").and_then(Value::as_str) {
        Some(language) if !language.is_empty() => Some(format!("```{language}\n{body}\n```")),
        _ => Some(body),
    }
}

/// Flatten one LSP hover content item (plain string, MarkedString, or MarkupContent).
fn format_hover_content_piece(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(hover_string_content(text));
    }

    let object = value.as_object()?;
    hover_markup_content_object(object).or_else(|| hover_marked_string_object(object))
}

fn parse_goto_result(id: u64, result: &Value) -> Option<LspResponse> {
    if result.is_null() {
        return None;
    }
    let location = result
        .as_array()
        .and_then(|locations| locations.first())
        .unwrap_or(result);
    if location.is_null() {
        return None;
    }
    let uri = location
        .get("uri")
        .or_else(|| location.get("targetUri"))?
        .as_str()?;

    // For multiple results, we navigate to the first returned location for now.
    // Limitation: If there are multiple definitions (e.g. trait method implementations),
    // we only navigate to the first one. We do not support choosing from a picker.
    let range = location
        .get("targetSelectionRange")
        .or_else(|| location.get("targetRange"))
        .or_else(|| location.get("range"))?;
    let start = range.get("start")?;
    Some(LspResponse::GotoResult {
        id,
        path: lsp_types::Url::parse(uri).ok()?.to_file_path().ok()?,
        line: u32::try_from(start.get("line")?.as_u64()?).ok()?,
        col: u32::try_from(start.get("character")?.as_u64()?).ok()?,
    })
}

fn parse_references(result: &Value) -> Vec<super::types::ReferenceLocation> {
    let mut locations = Vec::new();
    let array = match result.as_array() {
        Some(arr) => arr,
        None => return locations,
    };
    for location in array {
        if let Some(ref_loc) = parse_reference_location(location) {
            locations.push(ref_loc);
        }
    }
    locations
}

fn parse_reference_location(location: &Value) -> Option<super::types::ReferenceLocation> {
    let uri = location.get("uri")?.as_str()?;
    let range = location.get("range")?;
    let start = range.get("start")?;
    let end = range.get("end")?;

    Some(super::types::ReferenceLocation {
        path: lsp_types::Url::parse(uri).ok()?.to_file_path().ok()?,
        line_start: u32::try_from(start.get("line")?.as_u64()?).ok()?,
        col_start: u32::try_from(start.get("character")?.as_u64()?).ok()?,
        line_end: u32::try_from(end.get("line")?.as_u64()?).ok()?,
        col_end: u32::try_from(end.get("character")?.as_u64()?).ok()?,
        line_text: None,
    })
}

fn parse_prepare_rename(result: &Value) -> Option<(u32, u32, u32, u32)> {
    if result.is_null() {
        return None;
    }
    let range = if let Some(range_obj) = result.get("range") {
        range_obj
    } else {
        // Some servers return the range directly if it's not null
        result
    };

    let start = range.get("start")?;
    let end = range.get("end")?;

    Some((
        u32::try_from(start.get("line")?.as_u64()?).ok()?,
        u32::try_from(start.get("character")?.as_u64()?).ok()?,
        u32::try_from(end.get("line")?.as_u64()?).ok()?,
        u32::try_from(end.get("character")?.as_u64()?).ok()?,
    ))
}

fn parse_rename_edits(result: &Value) -> Vec<super::types::FileEdit> {
    let mut file_edits = Vec::new();
    let changes = match result.get("changes") {
        Some(Value::Object(map)) => map,
        _ => return file_edits,
    };

    for (uri_str, edits_value) in changes {
        if let Ok(url) = lsp_types::Url::parse(uri_str) {
            if let Ok(path) = url.to_file_path() {
                if let Some(edits) = parse_text_edits(edits_value) {
                    file_edits.push(super::types::FileEdit { path, edits });
                }
            }
        }
    }
    file_edits
}

fn parse_text_edits(value: &Value) -> Option<Vec<super::types::TextEdit>> {
    let array = value.as_array()?;
    let mut edits = Vec::new();
    for edit_value in array {
        if let Some(edit) = parse_single_text_edit(edit_value) {
            edits.push(edit);
        }
    }
    Some(edits)
}

fn parse_single_text_edit(value: &Value) -> Option<super::types::TextEdit> {
    let range = value.get("range")?;
    let start = range.get("start")?;
    let end = range.get("end")?;
    let new_text = value.get("newText")?.as_str()?.to_owned();

    Some(super::types::TextEdit {
        line_start: u32::try_from(start.get("line")?.as_u64()?).ok()?,
        col_start: u32::try_from(start.get("character")?.as_u64()?).ok()?,
        line_end: u32::try_from(end.get("line")?.as_u64()?).ok()?,
        col_end: u32::try_from(end.get("character")?.as_u64()?).ok()?,
        new_text,
    })
}

fn finish_child(child: &mut Child) {
    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn reap_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

use crate::lsp::types::{OutlineNode, SymbolKind as CustomSymbolKind};

fn parse_document_symbols(result: &Value) -> Vec<OutlineNode> {
    if result.is_null() {
        return Vec::new();
    }
    if let Some(nodes) = parse_hierarchical_symbols(result) {
        nodes
    } else {
        parse_flat_symbols(result)
    }
}

fn parse_hierarchical_symbols(val: &Value) -> Option<Vec<OutlineNode>> {
    let arr = val.as_array()?;
    if !arr.is_empty() && arr[0].get("range").is_none() {
        return None;
    }
    let mut nodes = Vec::new();
    for item in arr {
        if let Some(node) = parse_single_document_symbol(item) {
            nodes.push(node);
        }
    }
    Some(nodes)
}

fn parse_single_document_symbol(item: &Value) -> Option<OutlineNode> {
    let name = item.get("name")?.as_str()?.to_owned();
    let kind_num = item.get("kind")?.as_u64()?;
    let kind = map_lsp_kind_to_custom(kind_num);
    let range = item.get("range")?;
    let start_line = range.get("start")?.get("line")?.as_u64()? as usize;
    let end_line = range.get("end")?.get("line")?.as_u64()? as usize;

    let mut children = Vec::new();
    if let Some(children_val) = item.get("children") {
        if let Some(parsed_children) = parse_hierarchical_symbols(children_val) {
            children = parsed_children;
        }
    }

    Some(OutlineNode {
        name,
        kind,
        line: start_line,
        end_line,
        children,
        expanded: false,
    })
}

struct RawFlatSymbol {
    name: String,
    kind: CustomSymbolKind,
    line: usize,
    end_line: usize,
    container_name: Option<String>,
}

fn parse_flat_symbols(val: &Value) -> Vec<OutlineNode> {
    let Some(arr) = val.as_array() else {
        return Vec::new();
    };

    let mut raw_symbols = Vec::new();
    for item in arr {
        if let Some(symbol) = parse_single_flat_symbol(item) {
            raw_symbols.push(symbol);
        }
    }

    let mut children_by_container: HashMap<String, Vec<OutlineNode>> = HashMap::new();
    for sym in &raw_symbols {
        if let Some(ref container) = sym.container_name {
            children_by_container
                .entry(container.clone())
                .or_default()
                .push(OutlineNode {
                    name: sym.name.clone(),
                    kind: sym.kind,
                    line: sym.line,
                    end_line: sym.end_line,
                    children: Vec::new(),
                    expanded: false,
                });
        }
    }

    let all_names: std::collections::HashSet<String> =
        raw_symbols.iter().map(|s| s.name.clone()).collect();
    let mut top_level_nodes = Vec::new();

    for sym in &raw_symbols {
        let is_top = sym.container_name.is_none()
            || !all_names.contains(sym.container_name.as_ref().unwrap());
        if is_top {
            let mut node = OutlineNode {
                name: sym.name.clone(),
                kind: sym.kind,
                line: sym.line,
                end_line: sym.end_line,
                children: Vec::new(),
                expanded: false,
            };
            populate_children_flat(&mut node, &children_by_container);
            top_level_nodes.push(node);
        }
    }

    top_level_nodes
}

fn populate_children_flat(
    node: &mut OutlineNode,
    children_by_container: &HashMap<String, Vec<OutlineNode>>,
) {
    if let Some(children) = children_by_container.get(&node.name) {
        node.children = children.clone();
        for child in &mut node.children {
            populate_children_flat(child, children_by_container);
        }
    }
}

fn parse_single_flat_symbol(item: &Value) -> Option<RawFlatSymbol> {
    let name = item.get("name")?.as_str()?.to_owned();
    let kind_num = item.get("kind")?.as_u64()?;
    let kind = map_lsp_kind_to_custom(kind_num);
    let location = item.get("location")?;
    let range = location.get("range")?;
    let start_line = range.get("start")?.get("line")?.as_u64()? as usize;
    let end_line = range.get("end")?.get("line")?.as_u64()? as usize;
    let container_name = item
        .get("containerName")
        .and_then(Value::as_str)
        .map(String::from);

    Some(RawFlatSymbol {
        name,
        kind,
        line: start_line,
        end_line,
        container_name,
    })
}

fn map_lsp_kind_to_custom(kind: u64) -> CustomSymbolKind {
    match kind {
        12 | 6 | 9 => CustomSymbolKind::Function,
        23 => CustomSymbolKind::Struct,
        10 => CustomSymbolKind::Enum,
        11 => CustomSymbolKind::Trait,
        19 => CustomSymbolKind::Impl,
        14 => CustomSymbolKind::Constant,
        2 | 3 => CustomSymbolKind::Module,
        8 => CustomSymbolKind::Field,
        _ => CustomSymbolKind::Other,
    }
}

// ── Inlay hint parsing ───────────────────────────────────────────────────────

use super::types::{InlayHintKind, InlayHintLabelPart, InlayHintTooltip, LspInlayHint};
use crate::editor::position::LspPosition;

/// Parse a `textDocument/inlayHint` result (array or null) into typed hints.
///
/// Invalid positions and malformed items are silently skipped. Never panics.
fn parse_inlay_hints(result: &Value) -> Vec<LspInlayHint> {
    if result.is_null() {
        return Vec::new();
    }
    let Some(arr) = result.as_array() else {
        return Vec::new();
    };
    arr.iter().filter_map(parse_single_inlay_hint).collect()
}

fn parse_single_inlay_hint(item: &Value) -> Option<LspInlayHint> {
    let pos = item.get("position")?;
    let line = u32::try_from(pos.get("line")?.as_u64()?).ok()?;
    let character = u32::try_from(pos.get("character")?.as_u64()?).ok()?;
    let position = LspPosition::new(line, character);

    let label = item.get("label")?;
    let label_parts: Vec<InlayHintLabelPart> = match label {
        Value::String(s) => vec![InlayHintLabelPart {
            value: s.clone(),
            tooltip: None,
        }],
        Value::Array(parts) => parts
            .iter()
            .filter_map(parse_inlay_hint_label_part)
            .collect(),
        _ => return None,
    };
    if label_parts.is_empty() {
        return None;
    }

    let kind = match item.get("kind").and_then(Value::as_u64) {
        Some(1) => InlayHintKind::Type,
        Some(2) => InlayHintKind::Parameter,
        _ => InlayHintKind::Other,
    };

    let tooltip = item.get("tooltip").and_then(parse_inlay_hint_tooltip);
    let padding_left = item
        .get("paddingLeft")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let padding_right = item
        .get("paddingRight")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    Some(LspInlayHint {
        position,
        label: label_parts,
        kind,
        tooltip,
        padding_left,
        padding_right,
    })
}

fn parse_inlay_hint_label_part(item: &Value) -> Option<InlayHintLabelPart> {
    let value = item.get("value")?.as_str()?.to_owned();
    let tooltip = item.get("tooltip").and_then(parse_inlay_hint_tooltip);
    Some(InlayHintLabelPart { value, tooltip })
}

fn parse_inlay_hint_tooltip(value: &Value) -> Option<InlayHintTooltip> {
    match value {
        Value::String(s) => Some(InlayHintTooltip::PlainText(s.clone())),
        Value::Object(obj) => {
            let kind = obj
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("plaintext");
            let text = obj.get("value").and_then(Value::as_str)?;
            Some(if kind == "markdown" {
                InlayHintTooltip::Markdown(text.to_owned())
            } else {
                InlayHintTooltip::PlainText(text.to_owned())
            })
        }
        _ => None,
    }
}

// ─── Signature help parsing ──────────────────────────────────────────────────

/// Parse a `textDocument/signatureHelp` wire result into an optional `SignatureInfo`.
///
/// Returns `None` for `null` results or when no active signature can be determined.
fn parse_signature_help(result: &Value) -> Option<super::types::SignatureInfo> {
    use super::types::{ParameterInfo, SignatureInfo};
    if result.is_null() {
        return None;
    }
    let sigs = result.get("signatures")?.as_array()?;
    if sigs.is_empty() {
        return None;
    }
    let active_sig_idx = result
        .get("activeSignature")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let active_param_wire = result.get("activeParameter").and_then(Value::as_u64);

    let sig = sigs.get(active_sig_idx).or_else(|| sigs.first())?;
    let label = sig.get("label")?.as_str()?.to_owned();
    let documentation = sig.get("documentation").and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => o.get("value").and_then(Value::as_str).map(str::to_owned),
        _ => None,
    });

    let parameters: Vec<ParameterInfo> = sig
        .get("parameters")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    let param_label = match p.get("label")? {
                        Value::String(s) => s.clone(),
                        Value::Array(pair) => {
                            // [start_char, end_char] byte offsets into the signature label
                            let start = pair.first().and_then(Value::as_u64).unwrap_or(0) as usize;
                            let end = pair.get(1).and_then(Value::as_u64).unwrap_or(0) as usize;
                            label.get(start..end).unwrap_or("").to_owned()
                        }
                        _ => return None,
                    };
                    let doc = p.get("documentation").and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        Value::Object(o) => {
                            o.get("value").and_then(Value::as_str).map(str::to_owned)
                        }
                        _ => None,
                    });
                    Some(ParameterInfo {
                        label: param_label,
                        documentation: doc,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // Active parameter: prefer signature-level, then top-level.
    let active_parameter = sig
        .get("activeParameter")
        .and_then(Value::as_u64)
        .or(active_param_wire)
        .map(|n| n as usize);

    Some(SignatureInfo {
        label,
        documentation,
        parameters,
        active_parameter,
    })
}

// ─── Workspace symbol parsing ────────────────────────────────────────────────

/// Parse a `workspace/symbol` wire result into a list of `WorkspaceSymbol` values.
fn parse_workspace_symbols(result: &Value) -> Vec<super::types::WorkspaceSymbol> {
    use super::types::WorkspaceSymbol;
    if result.is_null() {
        return Vec::new();
    }
    result
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let name = item.get("name")?.as_str()?.to_owned();
            let kind = {
                let raw = item.get("kind").and_then(Value::as_u64).unwrap_or(0);
                symbol_kind_from_lsp(raw)
            };
            let container = item
                .get("containerName")
                .and_then(Value::as_str)
                .map(str::to_owned);
            // Prefer `location.uri` + range; fall back to deprecated `location`.
            let location = item.get("location")?;
            let uri = location.get("uri").and_then(Value::as_str)?;
            let path = lsp_types::Url::parse(uri).ok()?.to_file_path().ok()?;
            let (line, col) = location
                .get("range")
                .and_then(|r| r.get("start"))
                .and_then(|s| {
                    Some((
                        u32::try_from(s.get("line")?.as_u64()?).ok()?,
                        u32::try_from(s.get("character")?.as_u64()?).ok()?,
                    ))
                })
                .unwrap_or((0, 0));
            Some(WorkspaceSymbol {
                name,
                kind,
                path,
                line,
                col,
                container,
            })
        })
        .collect()
}

fn symbol_kind_from_lsp(kind: u64) -> super::types::SymbolKind {
    use super::types::SymbolKind;
    match kind {
        2 => SymbolKind::Function,       // Method
        3 => SymbolKind::Function,       // Function
        5 => SymbolKind::Struct,         // Class
        8 => SymbolKind::Trait,          // Interface
        9 => SymbolKind::Module,         // Module / Namespace
        10 => SymbolKind::Enum,          // Enum
        11 => SymbolKind::Struct,        // Struct
        7 | 13 => SymbolKind::Field,     // Property | EnumMember
        14 | 21 => SymbolKind::Constant, // Constant | Number
        _ => SymbolKind::Other,
    }
}

// ─── Code action parsing ─────────────────────────────────────────────────────

/// Parse a `textDocument/codeAction` wire result into a list of `CodeAction` values.
fn parse_code_actions(result: &Value) -> Vec<super::types::CodeAction> {
    use super::types::{CodeAction, CodeActionKind};
    if result.is_null() {
        return Vec::new();
    }
    result
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            // Each element is either a `Command` (has `command` field but no `edit`) or a
            // `CodeAction` (has `title` and optionally `edit`). We only model the latter.
            let title = item.get("title")?.as_str()?.to_owned();
            let kind = item
                .get("kind")
                .and_then(Value::as_str)
                .map(CodeActionKind::from_str);
            let is_preferred = item
                .get("isPreferred")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let edits = item
                .get("edit")
                .map(parse_workspace_edit)
                .unwrap_or_default();
            Some(CodeAction {
                title,
                kind,
                edits,
                is_preferred,
            })
        })
        .collect()
}

/// Parse a `WorkspaceEdit` wire value into a flat list of `FileEdit` values.
fn parse_workspace_edit(edit: &Value) -> Vec<super::types::FileEdit> {
    use super::types::{FileEdit, TextEdit};
    let mut result: Vec<FileEdit> = Vec::new();

    // `changes`: { uri -> TextEdit[] }
    if let Some(changes) = edit.get("changes").and_then(Value::as_object) {
        for (uri, edits_val) in changes {
            if let Ok(path) = lsp_types::Url::parse(uri)
                .ok()
                .and_then(|u| u.to_file_path().ok())
                .ok_or(())
            {
                let edits: Vec<TextEdit> = edits_val
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(parse_single_text_edit)
                    .map(|e| TextEdit {
                        line_start: e.line_start,
                        col_start: e.col_start,
                        line_end: e.line_end,
                        col_end: e.col_end,
                        new_text: e.new_text,
                    })
                    .collect();
                if !edits.is_empty() {
                    result.push(FileEdit { path, edits });
                }
            }
        }
    }

    // `documentChanges`: [TextDocumentEdit | CreateFile | RenameFile | DeleteFile]
    // We only handle TextDocumentEdit for now.
    if let Some(doc_changes) = edit.get("documentChanges").and_then(Value::as_array) {
        for doc_change in doc_changes {
            let Some(text_doc) = doc_change.get("textDocument") else {
                continue;
            };
            let Some(uri) = text_doc.get("uri").and_then(Value::as_str) else {
                continue;
            };
            let Ok(path) = lsp_types::Url::parse(uri)
                .ok()
                .and_then(|u| u.to_file_path().ok())
                .ok_or(())
            else {
                continue;
            };
            let edits: Vec<super::types::TextEdit> = doc_change
                .get("edits")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(parse_single_text_edit)
                .map(|e| super::types::TextEdit {
                    line_start: e.line_start,
                    col_start: e.col_start,
                    line_end: e.line_end,
                    col_end: e.col_end,
                    new_text: e.new_text,
                })
                .collect();
            if !edits.is_empty() {
                // deduplicate path already added from `changes`
                if let Some(existing) = result.iter_mut().find(|fe| fe.path == path) {
                    existing.edits.extend(edits);
                } else {
                    result.push(super::types::FileEdit { path, edits });
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    // LSP transport regressions — see module docs (LSP transport tests).

    #[test]
    fn framing_uses_utf8_byte_length_and_accepts_extra_headers() {
        let body = r#"{"message":"é"}"#;
        let mut bytes = Vec::new();
        write_message(&mut bytes, body).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.starts_with(&format!("Content-Length: {}", body.len())));

        let framed = format!(
            "Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc\r\n\r\n{}",
            body.len(),
            body
        );
        assert_eq!(
            read_frame(&mut Cursor::new(framed.into_bytes())).unwrap(),
            Some(body.to_owned())
        );
    }

    #[test]
    fn utf8_framing_behavior_remains_unchanged() {
        let unicode_body = r#"{"jsonrpc":"2.0","result":{"contents":"café 🙂 日本"}}"#;
        assert_ne!(unicode_body.len(), unicode_body.chars().count());

        let mut out = Vec::new();
        write_message(&mut out, unicode_body).unwrap();
        let framed = String::from_utf8(out).unwrap();
        let (header, body) = framed.split_once("\r\n\r\n").expect("CRLF frame separator");
        assert_eq!(header, format!("Content-Length: {}", unicode_body.len()));
        assert_eq!(body, unicode_body);

        assert_eq!(
            read_frame(&mut Cursor::new(framed.into_bytes())).unwrap(),
            Some(unicode_body.to_owned())
        );

        let framed_with_extra = format!(
            "Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc\r\nX-Custom: ignored\r\n\r\n{}",
            unicode_body.len(),
            unicode_body
        );
        assert_eq!(
            read_frame(&mut Cursor::new(framed_with_extra.into_bytes())).unwrap(),
            Some(unicode_body.to_owned())
        );

        let value = json!({ "message": "é" });
        let json_body = serde_json::to_string(&value).unwrap();
        let mut json_out = Vec::new();
        write_json(&mut json_out, &value).unwrap();
        let json_framed = String::from_utf8(json_out).unwrap();
        let (json_header, json_payload) = json_framed.split_once("\r\n\r\n").unwrap();
        assert_eq!(json_header, format!("Content-Length: {}", json_body.len()));
        assert_eq!(json_payload, json_body);

        let invalid = b"Content-Length: 2\r\n\r\n\xff\xfe";
        assert!(read_frame(&mut Cursor::new(invalid.to_vec())).is_err());

        let first = r#"{"jsonrpc":"2.0","id":1}"#;
        let second = r#"{"jsonrpc":"2.0","id":2,"text":"🙂"}"#;
        let mut dual = Vec::new();
        write_message(&mut dual, first).unwrap();
        write_message(&mut dual, second).unwrap();
        let mut reader = Cursor::new(dual);
        assert_eq!(read_frame(&mut reader).unwrap(), Some(first.to_owned()));
        assert_eq!(read_frame(&mut reader).unwrap(), Some(second.to_owned()));
        assert_eq!(read_frame(&mut reader).unwrap(), None);
    }

    #[test]
    fn did_change_encodes_full_document_and_monotonic_wire_ids() {
        let ids = AtomicU64::new(1);
        let path = std::env::current_dir().unwrap().join("src/main.rs");
        let (change, pending) = encode_request(
            LspRequest::DidChange {
                path: path.clone(),
                text: "fn main() {}".to_owned(),
                version: 7,
            },
            &ids,
        )
        .unwrap();
        assert!(pending.is_none());
        assert_eq!(
            change["params"]["contentChanges"][0]["text"],
            "fn main() {}"
        );
        assert_eq!(change["params"]["textDocument"]["version"], 7);

        let (_, first) = encode_request(
            LspRequest::Hover {
                path: path.clone(),
                line: 1,
                col: 2,
                id: 40,
            },
            &ids,
        )
        .unwrap();
        let (_, second) = encode_request(
            LspRequest::Completion {
                path,
                line: 1,
                col: 2,
                id: 41,
            },
            &ids,
        )
        .unwrap();
        assert!(first.unwrap().0 < second.unwrap().0);
    }

    #[test]
    fn malformed_messages_are_ignored_and_diagnostics_are_mapped() {
        let mut pending = HashMap::new();
        assert!(matches!(
            parse_lsp_message("not json", &mut pending),
            ParsedMessage::Ignored
        ));

        let path = std::env::current_dir().unwrap().join("src/main.rs");
        let uri = path_to_uri(&path).unwrap();
        let message = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "diagnostics": [{
                    "range": {
                        "start": {"line": 2, "character": 3},
                        "end": {"line": 2, "character": 8}
                    },
                    "severity": 1,
                    "message": "expected expression",
                    "code": "E0001"
                }]
            }
        });
        let ParsedMessage::Response(LspResponse::Diagnostics { diagnostics, .. }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected diagnostics");
        };
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, DiagnosticSeverity::Error);
        assert_eq!(diagnostics[0].code.as_deref(), Some("E0001"));
    }

    #[test]
    fn completion_items_parse_text_edit_and_numeric_kinds() {
        let mut pending = HashMap::from([(3, RequestKind::Completion(7))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "result": {
                "items": [{
                    "label": "println!",
                    "kind": 3,
                    "detail": "macro",
                    "textEdit": {
                        "range": {
                            "start": {"line": 1, "character": 4},
                            "end": {"line": 1, "character": 4}
                        },
                        "newText": "println!(\"{}\", )"
                    }
                }]
            }
        });
        let ParsedMessage::Response(LspResponse::CompletionList { id, items }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected completion response");
        };
        assert_eq!(id, 7);
        assert_eq!(items[0].kind.as_deref(), Some("Function"));
        let edit = items[0].text_edit.as_ref().expect("textEdit");
        assert_eq!(edit.new_text, "println!(\"{}\", )");
        assert_eq!(edit.line_start, 1);
        assert_eq!(edit.col_start, 4);
    }

    #[test]
    fn completion_null_result_parses_to_empty_list() {
        let mut pending = HashMap::from([(4, RequestKind::Completion(5))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "result": null
        });
        let ParsedMessage::Response(LspResponse::CompletionList { id, items }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected completion response");
        };
        assert_eq!(id, 5);
        assert!(items.is_empty());
    }

    fn sample_completion_item_json() -> Value {
        json!({
            "label": "main",
            "kind": 3,
            "detail": "fn",
            "insertText": "main"
        })
    }

    #[test]
    fn completion_list_and_direct_array_wire_shapes_stay_equivalent() {
        let item = sample_completion_item_json();
        let from_array = parse_completion_items(&json!([item.clone()]));
        let from_list = parse_completion_items(&json!({
            "isIncomplete": true,
            "items": [item]
        }));
        assert_eq!(from_array, from_list);
        assert_eq!(from_array[0].label, "main");
        assert_eq!(from_array[0].kind.as_deref(), Some("Function"));
        assert_eq!(from_array[0].detail.as_deref(), Some("fn"));
        assert_eq!(from_array[0].insert_text.as_deref(), Some("main"));
    }

    #[test]
    fn completion_list_and_direct_array_results_parse_correctly() {
        let items = json!([
            {
                "label": "main",
                "kind": 3,
                "detail": "fn",
                "insertText": "main"
            },
            {
                "label": "Vec::new",
                "kind": 2,
                "detail": "assoc fn",
                "textEdit": {
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 0}
                    },
                    "newText": "Vec::new()"
                }
            }
        ]);

        let mut direct_array_pending = HashMap::from([(7, RequestKind::Completion(101))]);
        let direct_array_message = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "result": items.clone()
        });
        let ParsedMessage::Response(LspResponse::CompletionList {
            id: direct_id,
            items: direct_items,
        }) = parse_lsp_message(&direct_array_message.to_string(), &mut direct_array_pending)
        else {
            panic!("expected direct-array completion response");
        };
        assert_eq!(direct_id, 101);
        assert!(direct_array_pending.is_empty());

        let mut completion_list_pending = HashMap::from([(8, RequestKind::Completion(102))]);
        let completion_list_message = json!({
            "jsonrpc": "2.0",
            "id": 8,
            "result": {
                "isIncomplete": true,
                "items": items,
                "ignoredField": {"nested": true}
            }
        });
        let ParsedMessage::Response(LspResponse::CompletionList {
            id: list_id,
            items: list_items,
        }) = parse_lsp_message(
            &completion_list_message.to_string(),
            &mut completion_list_pending,
        )
        else {
            panic!("expected CompletionList completion response");
        };
        assert_eq!(list_id, 102);
        assert!(completion_list_pending.is_empty());

        assert_eq!(direct_items, list_items);
        assert_eq!(direct_items.len(), 2);
        assert_eq!(direct_items[0].label, "main");
        assert_eq!(direct_items[0].kind.as_deref(), Some("Function"));
        assert_eq!(direct_items[0].detail.as_deref(), Some("fn"));
        assert_eq!(direct_items[0].insert_text.as_deref(), Some("main"));
        assert_eq!(direct_items[1].label, "Vec::new");
        assert_eq!(direct_items[1].kind.as_deref(), Some("Method"));
        assert_eq!(direct_items[1].detail.as_deref(), Some("assoc fn"));
        assert_eq!(direct_items[1].insert_text.as_deref(), Some("Vec::new()"));
        let edit = direct_items[1].text_edit.as_ref().expect("textEdit");
        assert_eq!(edit.new_text, "Vec::new()");
    }

    #[test]
    fn completion_bare_array_result_is_accepted() {
        let mut pending = HashMap::from([(6, RequestKind::Completion(8))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 6,
            "result": [{"label": "Vec", "kind": 7}]
        });
        let ParsedMessage::Response(LspResponse::CompletionList { items, .. }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected completion response");
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "Vec");
        assert_eq!(items[0].kind.as_deref(), Some("Class"));
        assert_eq!(items[0].insert_text.as_deref(), Some("Vec"));
    }

    #[test]
    fn completion_insert_text_is_retained_separate_from_label_detail_and_text_edit() {
        let items = parse_completion_items(&json!({
            "items": [{
                "label": "println!",
                "detail": "macro",
                "insertText": "println!(\"{}\", )",
                "textEdit": {
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 0}
                    },
                    "newText": "println!()"
                }
            }]
        }));
        assert_eq!(items[0].label, "println!");
        assert_eq!(items[0].detail.as_deref(), Some("macro"));
        assert_eq!(items[0].insert_text.as_deref(), Some("println!(\"{}\", )"));
        assert_eq!(
            items[0]
                .text_edit
                .as_ref()
                .map(|edit| edit.new_text.as_str()),
            Some("println!()")
        );
    }

    #[test]
    fn completion_empty_insert_text_is_retained_verbatim() {
        let items = parse_completion_items(&json!({
            "items": [{"label": "noop", "insertText": ""}]
        }));
        assert_eq!(items[0].insert_text.as_deref(), Some(""));
    }

    #[test]
    fn completion_label_is_retained_separate_from_insert_text() {
        let items = parse_completion_items(&json!({
            "items": [{
                "label": "println!",
                "insertText": "println!(\"{}\", )"
            }]
        }));
        assert_eq!(items[0].label, "println!");
        assert_eq!(items[0].insert_text.as_deref(), Some("println!(\"{}\", )"));
    }

    #[test]
    fn completion_label_is_retained_when_insert_text_falls_back_to_text_edit() {
        let items = parse_completion_items(&json!({
            "items": [{
                "label": "println!",
                "textEdit": {
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 0}
                    },
                    "newText": "println!(\"{}\", )"
                }
            }]
        }));
        assert_eq!(items[0].label, "println!");
        assert_eq!(items[0].insert_text.as_deref(), Some("println!(\"{}\", )"));
    }

    #[test]
    fn completion_object_label_shape_retains_inner_label_string() {
        let items = parse_completion_items(&json!({
            "items": [
                {"label": "plain"},
                {"label": {"label": "from_object", "detail": "()"}},
                {"label": {"detail": "no_inner_label"}},
                {"kind": 3}
            ]
        }));
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].label, "plain");
        assert_eq!(items[1].label, "from_object");
        assert_eq!(items[1].detail.as_deref(), Some("()"));
    }

    #[test]
    fn completion_kind_is_retained_separate_from_label_and_insert_text() {
        let items = parse_completion_items(&json!({
            "items": [{
                "label": "println!",
                "kind": 3,
                "insertText": "println!(\"{}\", )"
            }]
        }));
        assert_eq!(items[0].label, "println!");
        assert_eq!(items[0].kind.as_deref(), Some("Function"));
        assert_eq!(items[0].insert_text.as_deref(), Some("println!(\"{}\", )"));
        assert!(items[0].detail.is_none());
    }

    #[test]
    fn completion_string_kind_is_retained_verbatim() {
        let items = parse_completion_items(&json!({
            "items": [{"label": "Vec", "kind": "Struct"}]
        }));
        assert_eq!(items[0].kind.as_deref(), Some("Struct"));
    }

    #[test]
    fn completion_unknown_numeric_kind_maps_to_text() {
        let items = parse_completion_items(&json!({
            "items": [{"label": "x", "kind": 99}]
        }));
        assert_eq!(items[0].kind.as_deref(), Some("Text"));
    }

    #[test]
    fn completion_detail_is_retained_separate_from_label_and_insert_text() {
        let items = parse_completion_items(&json!({
            "items": [{
                "label": "println!",
                "detail": "macro",
                "insertText": "println!(\"{}\", )"
            }]
        }));
        assert_eq!(items[0].label, "println!");
        assert_eq!(items[0].detail.as_deref(), Some("macro"));
        assert_eq!(items[0].insert_text.as_deref(), Some("println!(\"{}\", )"));
    }

    #[test]
    fn completion_label_details_detail_and_description_are_retained() {
        let from_detail = parse_completion_items(&json!({
            "items": [{
                "label": "main",
                "labelDetails": {"detail": "( )", "description": "src/main.rs"}
            }]
        }));
        assert_eq!(from_detail[0].detail.as_deref(), Some("( )"));

        let from_description = parse_completion_items(&json!({
            "items": [{
                "label": "main",
                "labelDetails": {"description": "src/main.rs"}
            }]
        }));
        assert_eq!(from_description[0].detail.as_deref(), Some("src/main.rs"));
    }

    #[test]
    fn completion_insert_replace_edit_uses_insert_range_for_insertion() {
        let mut pending = HashMap::from([(10, RequestKind::Completion(12))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 10,
            "result": {
                "items": [{
                    "label": "main",
                    "textEdit": {
                        "newText": "main",
                        "insert": {
                            "start": {"line": 0, "character": 3},
                            "end": {"line": 0, "character": 3}
                        },
                        "replace": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 0, "character": 5}
                        }
                    }
                }]
            }
        });
        let ParsedMessage::Response(LspResponse::CompletionList { items, .. }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected completion response");
        };
        let edit = items[0].text_edit.as_ref().expect("textEdit");
        assert_eq!(edit.new_text, "main");
        assert_eq!(edit.line_start, 0);
        assert_eq!(edit.col_start, 3);
        assert_eq!(edit.line_end, 0);
        assert_eq!(edit.col_end, 3);
        assert_eq!(items[0].insert_text.as_deref(), Some("main"));
    }

    #[test]
    fn completion_insert_text_falls_back_to_label_when_absent() {
        let items = parse_completion_items(&json!({
            "items": [{"label": "main", "kind": 3}]
        }));
        assert_eq!(items[0].label, "main");
        assert_eq!(items[0].insert_text.as_deref(), Some("main"));
        assert!(items[0].text_edit.is_none());
    }

    #[test]
    fn completion_insert_text_fallback_prefers_text_edit_over_label() {
        let items = parse_completion_items(&json!({
            "items": [{
                "label": "println!",
                "textEdit": {
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 0}
                    },
                    "newText": "println!()"
                }
            }]
        }));
        assert_eq!(items[0].label, "println!");
        assert_eq!(items[0].insert_text.as_deref(), Some("println!()"));
    }

    #[test]
    fn completion_insert_text_falls_back_to_text_edit_new_text() {
        let items = parse_completion_items(&json!({
            "items": [{
                "label": "println!",
                "textEdit": {
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 0}
                    },
                    "newText": "println!()"
                }
            }]
        }));
        assert_eq!(items[0].insert_text.as_deref(), Some("println!()"));
        assert!(items[0].text_edit.is_some());
    }

    #[test]
    fn completion_insert_text_format_is_not_interpreted() {
        let mut pending = HashMap::from([(8, RequestKind::Completion(11))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 8,
            "result": {
                "items": [{
                    "label": "for",
                    "kind": 15,
                    "insertText": "for ${1:i} in ${2:iter} {}",
                    "insertTextFormat": 2
                }]
            }
        });
        let ParsedMessage::Response(LspResponse::CompletionList { items, .. }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected completion response");
        };
        assert_eq!(
            items[0].insert_text.as_deref(),
            Some("for ${1:i} in ${2:iter} {}")
        );
        assert!(items[0].text_edit.is_none());
    }

    #[test]
    fn completion_text_edit_snippet_syntax_is_retained_not_expanded() {
        let items = parse_completion_items(&json!({
            "items": [{
                "label": "for loop",
                "insertTextFormat": 2,
                "textEdit": {
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 0}
                    },
                    "newText": "for ${1:i} in ${2:iter} {}"
                }
            }]
        }));
        let edit = items[0].text_edit.as_ref().expect("textEdit");
        assert_eq!(edit.new_text, "for ${1:i} in ${2:iter} {}");
        assert_eq!(
            items[0].insert_text.as_deref(),
            Some("for ${1:i} in ${2:iter} {}")
        );
    }

    #[test]
    fn completion_additional_text_edits_are_not_modeled() {
        let items = parse_completion_items(&json!({
            "items": [{
                "label": "use",
                "textEdit": {
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 0}
                    },
                    "newText": "use std::io;"
                },
                "additionalTextEdits": [{
                    "range": {
                        "start": {"line": 1, "character": 0},
                        "end": {"line": 1, "character": 0}
                    },
                    "newText": "\n"
                }]
            }]
        }));
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0]
                .text_edit
                .as_ref()
                .map(|edit| edit.new_text.as_str()),
            Some("use std::io;")
        );
    }

    // Hover parsing regressions — run: cargo test --lib lsp::transport::tests::hover

    #[test]
    fn hover_unit_normalize_line_breaks_handles_cr_crlf_and_paragraphs() {
        assert_eq!(normalize_hover_line_breaks("a\r\nb"), "a\nb");
        assert_eq!(normalize_hover_line_breaks("a\rb"), "a\nb");
        assert_eq!(normalize_hover_line_breaks("a\n\nb"), "a\n\nb");
        assert_eq!(hover_string_content("x\r\ny"), "x\ny");
    }

    #[test]
    fn hover_unit_join_pieces_inserts_separator_only_when_needed() {
        assert_eq!(join_hover_content_pieces(&[]), "");
        assert_eq!(join_hover_content_pieces(&["one".to_owned()]), "one");
        assert_eq!(
            join_hover_content_pieces(&["one".to_owned(), "two".to_owned()]),
            "one\ntwo"
        );
        assert_eq!(
            join_hover_content_pieces(&["line\n".to_owned(), "next".to_owned()]),
            "line\nnext"
        );
        assert_eq!(
            join_hover_content_pieces(&["one".to_owned(), "\ntwo".to_owned()]),
            "one\ntwo"
        );
    }

    #[test]
    fn hover_unit_format_content_piece_dispatches_element_shapes() {
        assert_eq!(
            format_hover_content_piece(&json!("plain")),
            Some("plain".to_owned())
        );
        assert_eq!(
            format_hover_content_piece(&json!({"language": "rust", "value": "fn main() {}"})),
            Some("```rust\nfn main() {}\n```".to_owned())
        );
        assert_eq!(
            format_hover_content_piece(&json!({"kind": "markdown", "value": "docs"})),
            Some("docs".to_owned())
        );
        assert!(format_hover_content_piece(&Value::Null).is_none());
        assert!(format_hover_content_piece(&json!({"language": "rust"})).is_none());
    }

    #[test]
    fn hover_unit_object_helpers_reject_invalid_maps() {
        let empty = serde_json::Map::new();
        assert!(hover_marked_string_object(&empty).is_none());
        assert!(hover_markup_content_object(&empty).is_none());

        let marked_without_value = json!({"language": "rust"}).as_object().unwrap().clone();
        assert!(hover_marked_string_object(&marked_without_value).is_none());

        let markup_without_value = json!({"kind": "markdown"}).as_object().unwrap().clone();
        assert!(hover_markup_content_object(&markup_without_value).is_none());
    }

    #[test]
    fn hover_response_parses_marked_string_blocks() {
        let mut pending = HashMap::from([(4, RequestKind::Hover(9))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "result": {
                "contents": [
                    {"language": "rust", "value": "fn main() {}"},
                    "plain docs"
                ]
            }
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected hover response");
        };
        assert_eq!(id, 9);
        assert!(content.contains("```rust\nfn main() {}\n```"));
        assert!(content.contains("plain docs"));
    }

    #[test]
    fn hover_response_preserves_multiline_markup_content() {
        let mut pending = HashMap::from([(8, RequestKind::Hover(22))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 8,
            "result": {
                "contents": {
                    "kind": "markdown",
                    "value": "Returns the length.\n\n# Examples\n\n```rust\nlet v = vec![1];\n```"
                }
            }
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected hover response");
        };
        assert_eq!(id, 22);
        assert!(content.contains("Returns the length.\n\n# Examples"));
        assert!(content.contains("let v = vec![1];"));
    }

    #[test]
    fn hover_response_preserves_multiline_rust_signature() {
        let mut pending = HashMap::from([(9, RequestKind::Hover(23))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 9,
            "result": {
                "contents": {
                    "language": "rust",
                    "value": "pub fn foo<T>(\n    value: T,\n) -> T"
                }
            }
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected hover response");
        };
        assert_eq!(id, 23);
        assert!(content.contains("pub fn foo<T>(\n    value: T,\n) -> T"));
    }

    #[test]
    fn hover_response_joins_array_pieces_without_extra_blank_lines() {
        let mut pending = HashMap::from([(10, RequestKind::Hover(24))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 10,
            "result": {
                "contents": [
                    {"language": "rust", "value": "fn main() {}"},
                    "Returns nothing.\n",
                    "Additional detail."
                ]
            }
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected hover response");
        };
        assert_eq!(id, 24);
        assert!(content.contains("```rust\nfn main() {}\n```\nReturns nothing."));
        assert!(!content.contains("Returns nothing.\n\nAdditional"));
    }

    #[test]
    fn hover_response_normalizes_crlf_line_endings() {
        let mut pending = HashMap::from([(11, RequestKind::Hover(25))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 11,
            "result": {
                "contents": "line one\r\nline two\r\n\r\nline four"
            }
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected hover response");
        };
        assert_eq!(id, 25);
        assert_eq!(content, "line one\nline two\n\nline four");
    }

    #[test]
    fn hover_response_parses_markup_content() {
        let mut pending = HashMap::from([(7, RequestKind::Hover(21))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 7,
            "result": {
                "contents": {
                    "kind": "markdown",
                    "value": "**Parameters**\n\n- `x`: the input"
                }
            }
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected hover response");
        };
        assert_eq!(id, 21);
        assert!(content.contains("**Parameters**"));
        assert!(content.contains("`x`"));
    }

    #[test]
    fn hover_wire_parse_returns_string_contents_without_display_filtering() {
        let mut pending = HashMap::from([(12, RequestKind::Hover(26))]);
        let raw_json = "{\"kind\":\"markdown\",\"value\":\"hello\"}";
        let message = json!({
            "jsonrpc": "2.0",
            "id": 12,
            "result": { "contents": raw_json }
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected hover response");
        };
        assert_eq!(id, 26);
        assert_eq!(content, raw_json);
    }

    #[test]
    fn hover_wire_parse_preserves_leaked_lsp_wire_json_strings() {
        let mut pending = HashMap::from([(13, RequestKind::Hover(27))]);
        let leaked = "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"contents\":\"docs\"}}";
        let message = json!({
            "jsonrpc": "2.0",
            "id": 13,
            "result": { "contents": leaked }
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected hover response");
        };
        assert_eq!(id, 27);
        assert_eq!(content, leaked);
    }

    #[test]
    fn hover_wire_parse_preserves_rust_debug_formatted_strings() {
        let mut pending = HashMap::from([(14, RequestKind::Hover(28))]);
        let debug_text = "Some(\"documentation\")";
        let message = json!({
            "jsonrpc": "2.0",
            "id": 14,
            "result": { "contents": debug_text }
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected hover response");
        };
        assert_eq!(id, 28);
        assert_eq!(content, debug_text);
    }

    #[test]
    fn hover_response_keeps_markdown_with_braces_in_code_fence() {
        let mut pending = HashMap::from([(15, RequestKind::Hover(29))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 15,
            "result": {
                "contents": "Returns nothing.\n\n```rust\nfn main() {}\n```"
            }
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected hover response");
        };
        assert_eq!(id, 29);
        assert!(content.contains("fn main() {}"));
        assert!(!content.is_empty());
    }

    #[test]
    fn hover_missing_contents_parses_to_empty_string() {
        let content = parse_hover_content(&json!({}));
        assert!(content.is_empty());

        let mut pending = HashMap::from([(16, RequestKind::Hover(30))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 16,
            "result": {}
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected hover response");
        };
        assert_eq!(id, 30);
        assert!(content.is_empty());
    }

    #[test]
    fn hover_string_contents_are_retained_with_only_crlf_normalized() {
        let docs = "Returns nothing.\n\n```rust\nfn main() {}\n```";
        let wire = format!("line one\r\nline two\r\n\r\n{docs}");
        let content = parse_hover_content(&json!({ "contents": wire }));
        assert_eq!(content, format!("line one\nline two\n\n{docs}"));

        let mut pending = HashMap::from([(18, RequestKind::Hover(32))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 18,
            "result": { "contents": docs }
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected string-contents hover response");
        };
        assert_eq!(id, 32);
        assert_eq!(content, docs);
        assert!(content.contains("```rust\nfn main() {}\n```"));
    }

    #[test]
    fn hover_marked_string_object_fences_when_language_is_non_empty() {
        let body = "pub fn foo<T>(\n    value: T,\n) -> T";
        let from_top_level = parse_hover_content(&json!({
            "contents": { "language": "rust", "value": body }
        }));
        assert_eq!(from_top_level, format!("```rust\n{body}\n```"));

        let from_array = parse_hover_content(&json!({
            "contents": [{ "language": "rust", "value": body }]
        }));
        assert_eq!(from_array, format!("```rust\n{body}\n```"));

        let mut pending = HashMap::from([(20, RequestKind::Hover(34))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 20,
            "result": {
                "contents": { "language": "rust", "value": body }
            }
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected marked-string hover response");
        };
        assert_eq!(id, 34);
        assert!(content.contains("pub fn foo<T>("));
    }

    #[test]
    fn hover_marked_string_object_empty_language_returns_value_only() {
        let body = "plain docs\r\nsecond line";
        let content = parse_hover_content(&json!({
            "contents": { "language": "", "value": body }
        }));
        assert_eq!(content, "plain docs\nsecond line");
        assert!(!content.contains("```"));
    }

    #[test]
    fn hover_marked_string_array_flattens_elements_in_wire_order() {
        let content = parse_hover_content(&json!({
            "contents": [
                "summary line",
                { "language": "rust", "value": "fn main() {}" },
                "footer"
            ]
        }));
        assert_eq!(content, "summary line\n```rust\nfn main() {}\n```\nfooter");

        let mut pending = HashMap::from([(21, RequestKind::Hover(35))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 21,
            "result": {
                "contents": [
                    { "language": "rust", "value": "fn main() {}" },
                    "plain docs"
                ]
            }
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected marked-string-array hover response");
        };
        assert_eq!(id, 35);
        assert!(content.starts_with("```rust\nfn main() {}\n```"));
        assert!(content.ends_with("plain docs"));
    }

    #[test]
    fn hover_marked_string_array_empty_and_invalid_elements_are_handled() {
        assert_eq!(parse_hover_content(&json!({ "contents": [] })), "");

        let content = parse_hover_content(&json!({
            "contents": [
                "kept",
                null,
                42,
                {},
                { "language": "rust", "value": "fn ok() {}" }
            ]
        }));
        assert_eq!(content, "kept\n```rust\nfn ok() {}\n```");
    }

    #[test]
    fn hover_markup_content_object_retains_value_without_rendering_kind() {
        let body = "**Parameters**\n\n- `x`: input\r\n- `y`: output";
        let expected = "**Parameters**\n\n- `x`: input\n- `y`: output";

        let from_top_level = parse_hover_content(&json!({
            "contents": { "kind": "markdown", "value": body }
        }));
        assert_eq!(from_top_level, expected);

        let from_plaintext = parse_hover_content(&json!({
            "contents": { "kind": "plaintext", "value": "plain docs" }
        }));
        assert_eq!(from_plaintext, "plain docs");

        let from_array = parse_hover_content(&json!({
            "contents": [{ "kind": "markdown", "value": body }]
        }));
        assert_eq!(from_array, expected);

        let mut pending = HashMap::from([(22, RequestKind::Hover(36))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 22,
            "result": {
                "contents": {
                    "kind": "markdown",
                    "value": "Returns the length.\n\n# Examples"
                }
            }
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected markup-content hover response");
        };
        assert_eq!(id, 36);
        assert_eq!(content, "Returns the length.\n\n# Examples");
        assert!(!content.contains("kind"));
    }

    #[test]
    fn hover_markup_content_object_empty_value_and_missing_value_are_handled() {
        assert_eq!(
            parse_hover_content(&json!({
                "contents": { "kind": "markdown", "value": "" }
            })),
            ""
        );
        assert_eq!(
            parse_hover_content(&json!({ "contents": { "kind": "markdown" } })),
            ""
        );

        let from_array = parse_hover_content(&json!({
            "contents": [
                { "kind": "markdown" },
                { "kind": "markdown", "value": "kept" }
            ]
        }));
        assert_eq!(from_array, "kept");
    }

    #[test]
    fn hover_marked_string_object_is_distinct_from_markup_content() {
        let marked = parse_hover_content(&json!({
            "contents": { "language": "rust", "value": "fn main() {}" }
        }));
        let markup = parse_hover_content(&json!({
            "contents": { "kind": "markdown", "value": "fn main() {}" }
        }));
        assert_eq!(marked, "```rust\nfn main() {}\n```");
        assert_eq!(markup, "fn main() {}");
    }

    #[test]
    fn hover_empty_string_contents_are_supported() {
        let content = parse_hover_content(&json!({ "contents": "" }));
        assert_eq!(content, "");

        let mut pending = HashMap::from([(19, RequestKind::Hover(33))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 19,
            "result": { "contents": "" }
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected empty string-contents hover response");
        };
        assert_eq!(id, 33);
        assert!(content.is_empty());
    }

    #[test]
    fn hover_contents_string_array_and_object_shapes_keep_working() {
        let plain = "plain documentation";
        let marked_body = "fn example() {}";
        let markup_body = "**Summary**\n\nDetails here.";

        let from_string = parse_hover_content(&json!({ "contents": plain }));
        assert_eq!(from_string, plain);

        let from_array = parse_hover_content(&json!({
            "contents": [
                plain,
                { "language": "rust", "value": marked_body },
                { "kind": "markdown", "value": markup_body }
            ]
        }));
        assert!(from_array.starts_with(plain));
        assert!(from_array.contains(&format!("```rust\n{marked_body}\n```")));
        assert!(from_array.contains(markup_body));

        let from_marked_object = parse_hover_content(&json!({
            "contents": { "language": "rust", "value": marked_body }
        }));
        assert_eq!(from_marked_object, format!("```rust\n{marked_body}\n```"));

        let from_markup_object = parse_hover_content(&json!({
            "contents": { "kind": "markdown", "value": markup_body }
        }));
        assert_eq!(from_markup_object, markup_body);

        let mut pending = HashMap::from([(17, RequestKind::Hover(31))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 17,
            "result": { "contents": plain }
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected string-contents hover response");
        };
        assert_eq!(id, 31);
        assert_eq!(content, plain);
    }

    #[test]
    fn hover_contents_wire_shapes_flatten_to_equivalent_marked_string() {
        let body = "fn main() {}";
        let from_string = parse_hover_content(&json!({ "contents": body }));
        let from_marked = parse_hover_content(&json!({
            "contents": { "language": "rust", "value": body }
        }));
        let from_array = parse_hover_content(&json!({
            "contents": [{ "language": "rust", "value": body }]
        }));

        assert_eq!(from_string, body);
        assert_eq!(from_marked, format!("```rust\n{body}\n```"));
        assert_eq!(from_array, format!("```rust\n{body}\n```"));
    }

    #[test]
    fn hover_null_result_maps_to_empty_content() {
        assert!(parse_hover_content(&Value::Null).is_empty());

        let mut pending = HashMap::from([(5, RequestKind::Hover(11))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 5,
            "result": null
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected hover success response, not error");
        };
        assert_eq!(id, 11);
        assert!(content.is_empty());
        assert!(pending.is_empty());
    }

    #[test]
    fn hover_string_marked_string_array_markup_content_and_null_results_parse_safely() {
        let plain = "summary documentation";
        let marked_body = "fn example() -> i32 { 0 }";
        let markup_body = "**Returns** the value.\n\n# Example";

        let mut string_pending = HashMap::from([(40, RequestKind::Hover(201))]);
        let string_message = json!({
            "jsonrpc": "2.0",
            "id": 40,
            "result": { "contents": plain }
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&string_message.to_string(), &mut string_pending)
        else {
            panic!("expected string-contents hover response");
        };
        assert_eq!(id, 201);
        assert_eq!(content, plain);
        assert!(string_pending.is_empty());

        let mut array_pending = HashMap::from([(41, RequestKind::Hover(202))]);
        let array_message = json!({
            "jsonrpc": "2.0",
            "id": 41,
            "result": {
                "contents": [
                    plain,
                    null,
                    99,
                    {},
                    { "language": "rust", "value": marked_body },
                    { "kind": "markdown", "value": markup_body }
                ]
            }
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&array_message.to_string(), &mut array_pending)
        else {
            panic!("expected marked-string-array hover response");
        };
        assert_eq!(id, 202);
        assert!(content.starts_with(plain));
        assert!(content.contains(&format!("```rust\n{marked_body}\n```")));
        assert!(content.contains(markup_body));
        assert!(array_pending.is_empty());

        let mut markup_pending = HashMap::from([(42, RequestKind::Hover(203))]);
        let markup_message = json!({
            "jsonrpc": "2.0",
            "id": 42,
            "result": {
                "contents": { "kind": "markdown", "value": markup_body }
            }
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&markup_message.to_string(), &mut markup_pending)
        else {
            panic!("expected markup-content hover response");
        };
        assert_eq!(id, 203);
        assert_eq!(content, markup_body);
        assert!(markup_pending.is_empty());

        let mut null_pending = HashMap::from([(43, RequestKind::Hover(204))]);
        let null_message = json!({
            "jsonrpc": "2.0",
            "id": 43,
            "result": null
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&null_message.to_string(), &mut null_pending)
        else {
            panic!("expected null hover success response, not error");
        };
        assert_eq!(id, 204);
        assert!(content.is_empty());
        assert!(null_pending.is_empty());

        assert_eq!(parse_hover_content(&json!({ "contents": 42 })), "");
        assert_eq!(parse_hover_content(&json!({ "contents": true })), "");
        assert_eq!(parse_hover_content(&json!({})), "");
    }

    #[test]
    fn ui_correlation_ids_survive_independent_wire_id_mapping() {
        let wire_ids = AtomicU64::new(50);
        let path = PathBuf::from("src/main.rs");
        const UI_COMPLETION: u64 = 9_001;
        const UI_HOVER: u64 = 9_002;
        const UI_GOTO: u64 = 9_003;

        let (completion_req, completion_pending) = encode_request(
            LspRequest::Completion {
                path: path.clone(),
                line: 1,
                col: 2,
                id: UI_COMPLETION,
            },
            &wire_ids,
        )
        .unwrap();
        let completion_wire = completion_req["id"].as_u64().unwrap();
        assert_eq!(completion_wire, 50);
        assert_ne!(completion_wire, UI_COMPLETION);
        let (stored_wire, completion_kind) = completion_pending.unwrap();
        assert_eq!(stored_wire, completion_wire);
        assert!(matches!(
            completion_kind,
            RequestKind::Completion(UI_COMPLETION)
        ));

        let mut pending = HashMap::from([(completion_wire, completion_kind)]);
        let completion_message = json!({
            "jsonrpc": "2.0",
            "id": completion_wire,
            "result": [{"label": "main", "kind": 3}]
        });
        let ParsedMessage::Response(LspResponse::CompletionList { id, items }) =
            parse_lsp_message(&completion_message.to_string(), &mut pending)
        else {
            panic!("expected completion response");
        };
        assert_eq!(id, UI_COMPLETION);
        assert_eq!(items[0].label, "main");
        assert!(pending.is_empty());

        let (hover_req, hover_pending) = encode_request(
            LspRequest::Hover {
                path: path.clone(),
                line: 3,
                col: 4,
                id: UI_HOVER,
            },
            &wire_ids,
        )
        .unwrap();
        let hover_wire = hover_req["id"].as_u64().unwrap();
        assert_eq!(hover_wire, 51);
        assert_ne!(hover_wire, UI_HOVER);
        let (stored_wire, hover_kind) = hover_pending.unwrap();
        assert_eq!(stored_wire, hover_wire);
        assert!(matches!(hover_kind, RequestKind::Hover(UI_HOVER)));

        pending.insert(hover_wire, hover_kind);
        let hover_message = json!({
            "jsonrpc": "2.0",
            "id": hover_wire,
            "result": {"contents": "hover docs"}
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&hover_message.to_string(), &mut pending)
        else {
            panic!("expected hover response");
        };
        assert_eq!(id, UI_HOVER);
        assert_eq!(content, "hover docs");
        assert!(pending.is_empty());

        let (goto_req, goto_pending) = encode_request(
            LspRequest::GotoDefinition {
                path: path.clone(),
                line: 5,
                col: 6,
                id: UI_GOTO,
            },
            &wire_ids,
        )
        .unwrap();
        let goto_wire = goto_req["id"].as_u64().unwrap();
        assert_eq!(goto_wire, 52);
        assert_ne!(goto_wire, UI_GOTO);
        let (stored_wire, goto_kind) = goto_pending.unwrap();
        assert_eq!(stored_wire, goto_wire);
        assert!(matches!(goto_kind, RequestKind::GotoDefinition(UI_GOTO)));

        let goto_uri = path_to_uri(&path).unwrap();
        pending.insert(goto_wire, goto_kind);
        let goto_message = json!({
            "jsonrpc": "2.0",
            "id": goto_wire,
            "result": {
                "uri": goto_uri,
                "range": {
                    "start": {"line": 7, "character": 8},
                    "end": {"line": 7, "character": 12}
                }
            }
        });
        let ParsedMessage::Response(LspResponse::GotoResult { id, line, col, .. }) =
            parse_lsp_message(&goto_message.to_string(), &mut pending)
        else {
            panic!("expected goto response");
        };
        assert_eq!(id, UI_GOTO);
        assert_eq!(line, 7);
        assert_eq!(col, 8);
        assert!(pending.is_empty());
    }

    #[test]
    fn typed_responses_use_the_caller_correlation_id() {
        let mut pending = HashMap::from([(9, RequestKind::Completion(42))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 9,
            "result": {"items": [{"label": "println", "detail": "macro"}]}
        });
        let ParsedMessage::Response(LspResponse::CompletionList { id, items }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected completion response");
        };
        assert_eq!(id, 42);
        assert_eq!(items[0].label, "println");
        assert!(pending.is_empty());
    }

    #[test]
    fn completion_requests_use_text_document_completion() {
        let ids = AtomicU64::new(10);
        let path = PathBuf::from("src/main.rs");
        let (req, pending) = encode_request(
            LspRequest::Completion {
                path: path.clone(),
                line: 1,
                col: 4,
                id: 77,
            },
            &ids,
        )
        .unwrap();

        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["method"], "textDocument/completion");
        assert_ne!(req["method"], "textDocument/hover");
        assert_ne!(req["method"], "textDocument/definition");
        assert_eq!(req["params"]["position"]["line"], 1);
        assert_eq!(req["params"]["position"]["character"], 4);
        assert!(req["params"]["textDocument"]["uri"].is_string());

        let (wire_id, kind) = pending.unwrap();
        assert_eq!(wire_id, 10);
        assert!(matches!(kind, RequestKind::Completion(77)));
    }

    #[test]
    fn completion_request_encoding_preserves_caller_correlation_id() {
        let ids = AtomicU64::new(10);
        let path = PathBuf::from("src/main.rs");
        let (req, pending) = encode_request(
            LspRequest::Completion {
                path: path.clone(),
                line: 1,
                col: 4,
                id: 77,
            },
            &ids,
        )
        .unwrap();

        assert_eq!(req["method"], "textDocument/completion");
        let (wire_id, kind) = pending.unwrap();
        assert_eq!(wire_id, 10);
        assert!(matches!(kind, RequestKind::Completion(77)));
    }

    #[test]
    fn completion_correlation_id_survives_bare_array_and_completion_list_shapes() {
        let bare_array = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "result": [{"label": "main", "kind": 3}]
        });
        let mut bare_pending = HashMap::from([(3, RequestKind::Completion(88))]);
        let ParsedMessage::Response(LspResponse::CompletionList { id, items }) =
            parse_lsp_message(&bare_array.to_string(), &mut bare_pending)
        else {
            panic!("expected bare-array completion response");
        };
        assert_eq!(id, 88);
        assert_eq!(items[0].label, "main");

        let completion_list = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "result": {
                "isIncomplete": true,
                "items": [{"label": "Vec", "kind": 7}]
            }
        });
        let mut list_pending = HashMap::from([(4, RequestKind::Completion(89))]);
        let ParsedMessage::Response(LspResponse::CompletionList { id, items }) =
            parse_lsp_message(&completion_list.to_string(), &mut list_pending)
        else {
            panic!("expected completion-list response");
        };
        assert_eq!(id, 89);
        assert_eq!(items[0].label, "Vec");
    }

    #[test]
    fn completion_correlation_id_survives_null_and_error_results() {
        let mut null_pending = HashMap::from([(5, RequestKind::Completion(90))]);
        let null_message = json!({"jsonrpc": "2.0", "id": 5, "result": null});
        let ParsedMessage::Response(LspResponse::CompletionList { id, items }) =
            parse_lsp_message(&null_message.to_string(), &mut null_pending)
        else {
            panic!("expected null completion response");
        };
        assert_eq!(id, 90);
        assert!(items.is_empty());

        let mut error_pending = HashMap::from([(6, RequestKind::Completion(91))]);
        let error_message = json!({
            "jsonrpc": "2.0",
            "id": 6,
            "error": {"code": -32803, "message": "server busy"}
        });
        let ParsedMessage::Response(LspResponse::Error { id, message }) =
            parse_lsp_message(&error_message.to_string(), &mut error_pending)
        else {
            panic!("expected completion error response");
        };
        assert_eq!(id, 91);
        assert_eq!(message, "server busy");
    }

    #[test]
    fn uncorrelated_completion_wire_response_is_ignored() {
        let mut pending = HashMap::from([(1, RequestKind::Completion(10))]);
        let orphan = json!({
            "jsonrpc": "2.0",
            "id": 99,
            "result": {"items": [{"label": "orphan"}]}
        });
        assert!(matches!(
            parse_lsp_message(&orphan.to_string(), &mut pending),
            ParsedMessage::Ignored
        ));
        assert_eq!(pending.len(), 1);
        assert!(matches!(pending.get(&1), Some(RequestKind::Completion(10))));
    }

    #[test]
    fn hover_response_uses_the_caller_correlation_id() {
        let mut pending = HashMap::from([(6, RequestKind::Hover(15))]);
        let message = json!({
            "jsonrpc": "2.0",
            "id": 6,
            "result": {"contents": "docs"}
        });
        let ParsedMessage::Response(LspResponse::HoverResult { id, content }) =
            parse_lsp_message(&message.to_string(), &mut pending)
        else {
            panic!("expected hover response");
        };
        assert_eq!(id, 15);
        assert_eq!(content, "docs");
    }

    #[test]
    fn missing_server_is_reported_without_panicking() {
        let (to_ui, from_lsp) = mpsc::channel();
        let (to_lsp, from_ui) = mpsc::channel();
        let binary = "blue-ide-definitely-missing-language-server";
        let transport = spawn_lsp_thread(binary, &[], ".", to_ui, from_ui);

        let response = from_lsp.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            response,
            LspResponse::ServerUnavailable { ref message }
                if message == &format!("{binary} not found — LSP disabled")
        ));
        drop(to_lsp);
        transport.join().unwrap();
    }

    #[test]
    fn hover_requests_use_text_document_hover() {
        let ids = AtomicU64::new(10);
        let path = PathBuf::from("src/main.rs");
        let (req, pending) = encode_request(
            LspRequest::Hover {
                path: path.clone(),
                line: 3,
                col: 8,
                id: 77,
            },
            &ids,
        )
        .unwrap();

        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["method"], "textDocument/hover");
        assert_ne!(req["method"], "textDocument/completion");
        assert_ne!(req["method"], "textDocument/definition");
        assert_eq!(req["params"]["position"]["line"], 3);
        assert_eq!(req["params"]["position"]["character"], 8);
        assert!(req["params"]["textDocument"]["uri"].is_string());

        let (wire_id, kind) = pending.unwrap();
        assert_eq!(wire_id, 10);
        assert!(matches!(kind, RequestKind::Hover(77)));
    }

    #[test]
    fn hover_request_encoding_preserves_caller_correlation_id() {
        let ids = AtomicU64::new(10);
        let path = PathBuf::from("src/main.rs");
        let (req, pending) = encode_request(
            LspRequest::Hover {
                path: path.clone(),
                line: 3,
                col: 8,
                id: 77,
            },
            &ids,
        )
        .unwrap();

        assert_eq!(req["method"], "textDocument/hover");
        let (wire_id, kind) = pending.unwrap();
        assert_eq!(wire_id, 10);
        assert!(matches!(kind, RequestKind::Hover(77)));
    }

    #[test]
    fn goto_definition_request_encoding() {
        let ids = AtomicU64::new(10);
        let path = PathBuf::from("src/main.rs");
        let (req, pending) = encode_request(
            LspRequest::GotoDefinition {
                path: path.clone(),
                line: 5,
                col: 10,
                id: 1234,
            },
            &ids,
        )
        .unwrap();

        assert_eq!(req["method"], "textDocument/definition");
        assert_eq!(req["params"]["position"]["line"], 5);
        assert_eq!(req["params"]["position"]["character"], 10);

        let (wire_id, kind) = pending.unwrap();
        assert_eq!(wire_id, 10);
        assert!(matches!(kind, RequestKind::GotoDefinition(1234)));
    }

    #[test]
    fn parse_goto_definition_responses() {
        // 1. A single Location parses correctly.
        {
            let mut pending = HashMap::from([(10, RequestKind::GotoDefinition(1234))]);
            let path = std::env::current_dir().unwrap().join("src/main.rs");
            let uri = path_to_uri(&path).unwrap();
            let message = json!({
                "jsonrpc": "2.0",
                "id": 10,
                "result": {
                    "uri": uri,
                    "range": {
                        "start": {"line": 15, "character": 20},
                        "end": {"line": 15, "character": 25}
                    }
                }
            });
            let ParsedMessage::Response(LspResponse::GotoResult {
                id,
                path: target_path,
                line,
                col,
            }) = parse_lsp_message(&message.to_string(), &mut pending)
            else {
                panic!("expected GotoResult");
            };
            assert_eq!(id, 1234);
            assert_eq!(target_path, path);
            assert_eq!(line, 15);
            assert_eq!(col, 20);
        }

        // 2. A location array selects the first item.
        {
            let mut pending = HashMap::from([(10, RequestKind::GotoDefinition(1234))]);
            let path1 = std::env::current_dir().unwrap().join("src/main.rs");
            let path2 = std::env::current_dir().unwrap().join("src/lib.rs");
            let uri1 = path_to_uri(&path1).unwrap();
            let uri2 = path_to_uri(&path2).unwrap();
            let message = json!({
                "jsonrpc": "2.0",
                "id": 10,
                "result": [
                    {
                        "uri": uri1,
                        "range": {
                            "start": {"line": 15, "character": 20},
                            "end": {"line": 15, "character": 25}
                        }
                    },
                    {
                        "uri": uri2,
                        "range": {
                            "start": {"line": 30, "character": 40},
                            "end": {"line": 30, "character": 45}
                        }
                    }
                ]
            });
            let ParsedMessage::Response(LspResponse::GotoResult {
                id,
                path: target_path,
                line,
                col,
            }) = parse_lsp_message(&message.to_string(), &mut pending)
            else {
                panic!("expected GotoResult");
            };
            assert_eq!(id, 1234);
            assert_eq!(target_path, path1);
            assert_eq!(line, 15);
            assert_eq!(col, 20);
        }

        // 3. A single LocationLink parses correctly (uses targetSelectionRange).
        {
            let mut pending = HashMap::from([(10, RequestKind::GotoDefinition(1234))]);
            let path = std::env::current_dir().unwrap().join("src/main.rs");
            let uri = path_to_uri(&path).unwrap();
            let message = json!({
                "jsonrpc": "2.0",
                "id": 10,
                "result": {
                    "targetUri": uri,
                    "targetRange": {
                        "start": {"line": 10, "character": 5},
                        "end": {"line": 12, "character": 5}
                    },
                    "targetSelectionRange": {
                        "start": {"line": 11, "character": 10},
                        "end": {"line": 11, "character": 15}
                    }
                }
            });
            let ParsedMessage::Response(LspResponse::GotoResult {
                id,
                path: target_path,
                line,
                col,
            }) = parse_lsp_message(&message.to_string(), &mut pending)
            else {
                panic!("expected GotoResult");
            };
            assert_eq!(id, 1234);
            assert_eq!(target_path, path);
            assert_eq!(line, 11);
            assert_eq!(col, 10);
        }

        // 4. An array of LocationLink objects (selects first, uses targetSelectionRange).
        {
            let mut pending = HashMap::from([(10, RequestKind::GotoDefinition(1234))]);
            let path = std::env::current_dir().unwrap().join("src/main.rs");
            let uri = path_to_uri(&path).unwrap();
            let message = json!({
                "jsonrpc": "2.0",
                "id": 10,
                "result": [
                    {
                        "targetUri": uri,
                        "targetRange": {
                            "start": {"line": 10, "character": 5},
                            "end": {"line": 12, "character": 5}
                        },
                        "targetSelectionRange": {
                            "start": {"line": 11, "character": 10},
                            "end": {"line": 11, "character": 15}
                        }
                    }
                ]
            });
            let ParsedMessage::Response(LspResponse::GotoResult {
                id,
                path: target_path,
                line,
                col,
            }) = parse_lsp_message(&message.to_string(), &mut pending)
            else {
                panic!("expected GotoResult");
            };
            assert_eq!(id, 1234);
            assert_eq!(target_path, path);
            assert_eq!(line, 11);
            assert_eq!(col, 10);
        }

        // 5. An empty array is handled as no definition (produces GotoNone).
        {
            let mut pending = HashMap::from([(10, RequestKind::GotoDefinition(1234))]);
            let message = json!({
                "jsonrpc": "2.0",
                "id": 10,
                "result": []
            });
            let ParsedMessage::Response(LspResponse::GotoNone { id }) =
                parse_lsp_message(&message.to_string(), &mut pending)
            else {
                panic!("expected GotoNone");
            };
            assert_eq!(id, 1234);
        }

        // 6. Null is handled as no definition (produces GotoNone).
        {
            let mut pending = HashMap::from([(10, RequestKind::GotoDefinition(1234))]);
            let message = json!({
                "jsonrpc": "2.0",
                "id": 10,
                "result": null
            });
            let ParsedMessage::Response(LspResponse::GotoNone { id }) =
                parse_lsp_message(&message.to_string(), &mut pending)
            else {
                panic!("expected GotoNone");
            };
            assert_eq!(id, 1234);
        }

        // 7. Unsupported URIs do not panic (produces GotoNone).
        {
            let mut pending = HashMap::from([(10, RequestKind::GotoDefinition(1234))]);
            let message = json!({
                "jsonrpc": "2.0",
                "id": 10,
                "result": {
                    "uri": "http://example.com/source.rs",
                    "range": {
                        "start": {"line": 15, "character": 20},
                        "end": {"line": 15, "character": 25}
                    }
                }
            });
            let ParsedMessage::Response(LspResponse::GotoNone { id }) =
                parse_lsp_message(&message.to_string(), &mut pending)
            else {
                panic!("expected GotoNone");
            };
            assert_eq!(id, 1234);
        }
    }
}
