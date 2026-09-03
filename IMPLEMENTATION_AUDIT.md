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
| Existing plugin sandbox | Implemented | `src/plugins/sandbox.rs` and `src/plugins/api.rs` restrict plugin APIs, remove `os`/`io`/`package`/`require`/`dofile`, and cap instructions. Workspace trust now gates plugin loading; tests assert the removed globals are `nil`. |
| Git hunks and basic panel | Partial | `src/git/diff.rs` implements hunk parsing/operations plus per-hunk `stage_hunk`/`unstage_hunk` and commit amend. The generalized diff document, side-by-side UI, sources, synchronized scrolling, and intraline highlighting are still missing. |
| Panes and terminal tabs | Partial | Pane layout and multiple `TerminalPane` values exist. Named/reorderable/splittable/restored terminal sessions and lifecycle metadata are missing. |
| Multi-root workspace and deepest-root ownership | Partial | `src/workspace.rs` adds ordered roots, stable `RootId`, canonical duplicate handling, and deepest-root ownership with tests. App LSP/hover/outline/task/terminal/profiler/plugin paths now resolve roots through `workspace_root_for_path`; a single-file fallback to `FileTree::root_path` remains in the editor render loop. |
| URI-backed local/SSH document identity | Foundation added | `DocumentUri` distinguishes local and `ssh://` documents. Buffers/tabs still use `PathBuf`; no SSH transport is wired. |
| Per-root workspace trust | Implemented (core paths) | Default-deny `TrustStore` persists canonical roots atomically outside caller-selected workspace paths. `trust_allows` fails closed; terminals, profiler, tasks, plugin reload, and LSP lazy-server spawn are gated. Trust prompt is shown before LSP/plugins start. Git remote operations still use `git2` and are **not** gated yet — documented as a deliberate remaining gap. |
| Versioned atomic session persistence | Removed (deliberate) | The plan chose delete over wire for `SessionState`; `BlueIdeApp` continues to use `AppSessionState` (`src/app.rs`) for pane/tab/bookmark/root restore. |
| Shared background jobs | Removed (deliberate) | `BackgroundJobs` and its lifecycle tests were removed. Git/SSH/marketplace/DAP/profiler use their own progress/status state. |
| Multiple cursors and multi-selection edits | Missing | Buffer/editor state remains single-cursor. Alt+Click, Ctrl+D, normalization, right-to-left multi-edit transactions, and single-revision LSP sync are absent. |
| Transaction history and history panel | Missing | Existing undo behavior is not the specified bounded `EditTransaction` model and has no jumpable history panel. |
| Ctrl+G line/column UI | Partial | `GoToLine` binding exists in app command specs; no dedicated focused line/column input and navigation flow. |
| Ctrl+T multi-root workspace symbols | Missing | Document symbols exist, but no workspace-symbol request variant, aggregation, debounce, fuzzy ranking, stale rejection, or Ctrl+T UI exists. |
| Signature help | Partial | Typed request/response plus a caret popup exist; the signature-help state machine and multi-request ordering are less complete than the implementation plan. |
| Code actions and workspace edits | Partial | Code-action request and popup exist; command execution, validated multi-file/resource edit application, preview, and partial-failure reporting are missing. |
| Git fetch/pull/push worker UX | Missing | No authenticated/cancellable progress worker or remote/ref/conflict UX exists. |
| SSH/SFTP remote editing | Missing | No connection manager, known-host verification, browsing, save conflict detection, reconnect, or atomic upload exists. |
| Signed extension marketplace | Missing | No index client, signature/hash verification, safe archive extraction, staged install/rollback/update/uninstall implementation exists. |
| DAP debugging | Partial | `src/debug.rs` has DAP framing/client/session types, a toolbar (start/stop, continue/pause/step), breakpoint toggling at the active cursor, call-stack selection, scopes/variables, and console output wired into the bottom `DebugConsole` panel. Remaining: editor-gutter breakpoint rendering, richer adapter config, and per-adapter launch-arg forms. |
| Rust flamegraph profiling | Partial | Trust-gated profiler panel with a flamegraph/SVG viewer exists; the runner still shells out to `cargo flamegraph` and lacks a full cancellation/progress model. |

## Verification

- No local Rust toolchain was available while the fixes were authored; verification relies entirely on GitHub Actions (`cargo check`, `cargo test --lib`, `cargo test --test integration_test`).
- CI currently gates check + both test suites; `cargo clippy --lib -- -D warnings` and `cargo fmt --check` run non-blocking until lint/format debt is cleaned.

## Required implementation order

1. (Done) Migrate `Workspace`/`TrustStore` into `BlueIdeApp`; enforce default-deny trust for terminals, profiler, tasks, plugins, and LSP server spawn; move the trust prompt before plugin/LSP startup.
2. (Remaining) Implement cursor sets and transaction-based edits/history because editor, completion, and LSP work depend on them.
3. (Remaining) Add typed workspace-symbol, signature-help, and code-action protocol paths with stale/version/root validation.
4. (Remaining) Build generalized diff and named terminal session models, then persist their metadata.
5. (Remaining) Add remote editing, marketplace, DAP, and profiler as separate worker-backed vertical slices with dedicated security tests.

The complete master plan is a multi-phase product program, not a single safe patch. Trust enforcement now covers the executable paths that were reachable at review time; remote/marketplace/DAP remain unwired and should land as separate gated slices.
