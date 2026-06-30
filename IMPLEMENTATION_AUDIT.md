# Advanced IDE Capabilities Audit

Audited against the phased master implementation plan on 2026-06-21.

## Status key

- **Implemented**: usable production path exists and has meaningful test coverage.
- **Partial**: some supporting behavior exists, but the plan's end-to-end acceptance criteria are not met.
- **Foundation added**: reusable, tested domain implementation now exists but is not yet wired through the application UI and every subsystem.
- **Missing**: no substantive implementation found.

## Results

| Capability | Status | Evidence / remaining work |
|---|---|---|
| Stateless editor widget and upward action flow | Implemented | `src/editor/widget.rs` retains the frame handoff architecture and extensive regression tests. |
| Non-blocking LSP worker and typed routing | Implemented for current features | `src/lsp/transport.rs`, `src/lsp/types.rs`, and `src/app.rs` use correlated typed completion, hover, goto, symbol, and format responses. |
| Existing plugin sandbox | Implemented | `src/plugins/sandbox.rs` and `src/plugins/api.rs` restrict plugin APIs and workspace file access. Workspace trust still needs wiring before plugin load. |
| Git hunks and basic panel | Partial | `src/git/diff.rs` implements hunk parsing/operations; the generalized diff document, side-by-side UI, sources, synchronized scrolling, and intraline highlighting are missing. |
| Panes and terminal tabs | Partial | Pane layout and multiple `TerminalPane` values exist. Named/reorderable/splittable/restored terminal sessions and lifecycle metadata are missing. |
| Multi-root workspace and deepest-root ownership | Foundation added | `src/workspace.rs` adds ordered roots, stable `RootId`, canonical duplicate handling, and deepest-root ownership with tests. App, explorer, LSP, Git, plugins, terminals, and persistence still use the legacy `FileTree::root_path`. |
| URI-backed local/SSH document identity | Foundation added | `DocumentUri` distinguishes local and `ssh://` documents. Buffers/tabs still use `PathBuf`; no SSH transport is wired. |
| Per-root workspace trust | Foundation added | Default-deny `TrustStore` persists canonical roots atomically outside caller-selected workspace paths and gates executable capabilities. The application must enforce it at all LSP/plugin/terminal/debug/profiler entry points and add trust UI. |
| Versioned atomic session persistence | Foundation added | `SessionState` atomically saves/loads roots, URI tabs, active document, panel height, terminal names, and recovery text; corrupt state and missing local files recover safely. Pane/cursor/selection/scroll capture and application startup/shutdown wiring remain. |
| Shared background jobs | Foundation added | `BackgroundJobs` supplies root-scoped progress, cancellation, completion, failure, and structured errors. Git/SSH/marketplace/DAP/profiler workers are not yet connected. |
| Multiple cursors and multi-selection edits | Missing | Buffer/editor state remains single-cursor. Alt+Click, Ctrl+D, normalization, right-to-left multi-edit transactions, and single-revision LSP sync are absent. |
| Transaction history and history panel | Missing | Existing undo behavior is not the specified bounded `EditTransaction` model and has no jumpable history panel. |
| Ctrl+G line/column UI | Partial | Key binding exists in `src/config/keybinds.rs`; no focused line/column input and navigation flow was found. |
| Ctrl+T multi-root workspace symbols | Missing | Document symbols exist, but no workspace-symbol request variant, aggregation, debounce, fuzzy ranking, stale rejection, or Ctrl+T UI exists. |
| Signature help | Missing | No typed request/response, trigger policy, stale gate, or caret popup exists. |
| Code actions and workspace edits | Missing | No typed code-action request/UI, command execution, validated multi-file/resource edit application, preview, or partial-failure reporting exists. |
| Git fetch/pull/push worker UX | Missing | No authenticated/cancellable progress worker or remote/ref/conflict UX exists. |
| SSH/SFTP remote editing | Missing | No connection manager, known-host verification, browsing, save conflict detection, reconnect, or atomic upload exists. |
| Signed extension marketplace | Missing | No index client, signature/hash verification, safe archive extraction, staged install/rollback/update/uninstall implementation exists. |
| DAP debugging | Missing | No DAP framing/client/session types, adapter lifecycle, breakpoint/step/thread/stack/scope/variable UX exists. |
| Rust flamegraph profiling | Missing | No trust-gated profiler worker or vetted SVG viewer exists. |

## Verification

- `cargo test --lib workspace::tests`: 5 passed.
- Full library suite invoked by integration guards: 468 passed.
- `cargo check`: passed with existing warnings.
- `cargo test --test integration_test`: one pre-existing policy failure because `mlua` is present in `Cargo.toml` but absent from the dependency allowlist; two nested full-suite guards make this command exceed 120 seconds despite the 468 library tests passing.
- `cargo fmt --check`: pre-existing failure in `src/app.rs` due broad formatting differences and trailing whitespace. The new `src/workspace.rs` is rustfmt-formatted.

## Required implementation order

1. Wire `Workspace`, `TrustStore`, `SessionState`, and `BackgroundJobs` into `BlueIdeApp`; migrate subsystem maps to `RootId` and enforce default-deny trust.
2. Implement cursor sets and transaction-based edits/history because editor, completion, and LSP work depend on them.
3. Add typed workspace-symbol, signature-help, and code-action protocol paths with stale/version/root validation.
4. Build generalized diff and named terminal session models, then persist their metadata.
5. Add remote editing, marketplace, DAP, and profiler as separate worker-backed vertical slices with dedicated security tests.

The complete master plan is a multi-phase product program, not a single safe patch. Treating foundational types as if they were already integrated would leave executable features outside trust enforcement and create security regressions.
