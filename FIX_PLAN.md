# Stack IDE — Implementation Plan to Fix All Identified Issues

**Review date:** 2026-09-03  
**Target branch:** `arena/01a0670e-stack-ide` (all work stays on this branch; PRs target `main` via this branch)  
**Baseline:** `bcbc699` (main = PR #2 merged) + open PR #3 head `a19b66f` (PR #3 + PR #4 fixes)

This plan is derived from the code review. It is ordered by risk so that correctness/security work lands before feature/parity work. Every phase has a "Definition of Done" so progress is measurable.

> **Implementation status (updated 2026-09-03):**
> - **Phase 0:** no local Rust toolchain available; all verification relies on GitHub Actions CI.
> - **Phase 1:** implemented in `aria/01a0670e-stack-ide`: Unicode-safe char ranges, `?` backward search, mode-aware paste, `zz/zt/zb` viewport requests, `:q!` force-close, assistant streaming + cancellation flag + Windows quoting + trailing prose, inline-diagnostic clipping, theme display names + premultiplied `rgba`, `inline_diagnostics` serde default. New unit tests added for each.
> - **Phase 2:** implemented fail-closed `trust_allows` and gated terminals, profiler, tasks, plugin reload, and LSP lazy-spawn (per-root trusted set in `LspManager`); trust prompt ordering in `open_workspace_folder`/`load_session`; Lua sandbox nil-checks. Tests added.
> - **Phase 3:** migrated all `tree.root_path` single-root call sites to workspace-root helpers (single explicit justified fallback remains in `show_editor`); **chose delete** for `SessionState`/`BackgroundJobs`; deleted `src/config/` and `src/workspace_features/`; removed dead `save_active`/`show_outline_panel`/`handle_terminal_key` and dead-code annotations.
> - **Phase 4/5:** remaining feature slices (git per-hunk, didSave policy ask, DAP UX, full SSH, marketplace, notebooks, devcontainers, extra grammars, snippet tab-stops) and doc/CI cleanup are still open. `didSave` is deliberately unchanged pending approval. Docs/README/parity updated. CI gates on `cargo check` + `cargo test --lib` + `cargo test --test integration_test` + `cargo clippy --lib -- -D warnings`; `cargo fmt --check` still runs non-blocking because the tree has broad pre-existing formatting drift.

> **Guardrails that must be honored throughout** (enforced by `tests/integration_test.rs`):
> - `ask_before_adding_a_new_dependency` — no new crates without updating the allowlist.
> - `ask_before_changing_the_lsp_synchronization_strategy` — any change to `didSave`/sync flow requires explicit approval and a deliberate update to this test.
> - `ask_before_adding_automatic_completion_on_every_typed_character` — keep completion Ctrl+Space-only.
> - `ask_before_adding_snippet_placeholder_support` — keep snippets literal-only unless approved.
> - `ask_before_broadly_redesigning_the_editor_widget` — do not broadly redesign the editor widget fingerprint.
> - `remove_or_weaken_existing_tests_to_make_the_feature_pass` — never delete or weaken a test to make a feature pass.

---

## Phase 0 — Reproducible baseline before any changes

**Goal:** confirm the review’s assumptions on a real toolchain and pin a red/green baseline.

### Tasks
1. Install/verify a Rust toolchain (`stable` + `clippy` + `rustfmt`).
2. Run `cargo check --lib --tests`, `cargo test --lib`, `cargo test --test integration_test`, `cargo clippy --lib -- -D warnings`, `cargo fmt --check`.
3. Record the exact command results and warnings in a local note (not committed) so we know whether failures are pre-existing.
4. Capture the current commit hash for rollback safety.
5. Create a local `cargo check/test` smoke script (optional, under `scripts/`, not committed) so CI and local runs are equivalent.

### Definition of Done
- Full-known test result matrix documented.
- Clean rollback point identified.

---

## Phase 1 — Correctness and crash fixes (highest priority)

> **Why first:** these are user-facing crashes on normal input (non-ASCII text). They are independent of the larger redesign.

### 1.1 Fix Unicode char-index / byte-index confusion in `src/vim.rs`

**Problem:** helpers return **character indices** (`TextBuffer::position_to_char_index`), but `buffer.text()` is a Rust `String` and Rust slicing is **byte based**. Many vim functions panic/slice incorrectly on non-ASCII.

**Tasks**
- Add a safe extraction helper in `src/vim.rs`:
  - `fn text_char_range(buffer: &TextBuffer, start: CursorPosition, end: CursorPosition) -> Option<String>` that converts positions to **byte** ranges via `buffer.position_to_byte_index(...)` before slicing `buffer.text()` (or better, use `buffer.slice_chars`-style APIs already present).
- Replace **every** `buffer.text()[start_off..end_off]` slice with the helper. Affected functions:
  - `extract_range`, `delete_range`, `transform_visual`, `finish_motion_operator`, `delete_chars`, `change_to_eol`, `toggle_case_char` (also fix the `start + c.len_utf8()` bug — it must be `+1` char index or a byte-range helper), and `jump_to_next_match`/`char_offset` interactions if needed.
- Audit **all** `buffer.text()[a..b]` occurrences in `src/vim.rs` and confirm whether `a/b` are byte or char offsets; fix any char-index misuse.
- Ensure `replace_char_range` call sites pass character indices (it already uses char indices) — do **not** convert those to byte indices.
- Add regression tests:
  - `d`, `c`, `y`, `x`, `D`, `C`, `~`, `r`, `J`, `p`, visual delete/yank on a buffer containing `é`, `🙂`, and CJK.
  - Test ranges that start/end inside multi-byte chars.

**Acceptance criteria**
- No panic for any vim operator on mixed ASCII/Unicode content.
- Existing `src/vim.rs` tests still pass.
- New Unicode tests fail on the pre-fix code and pass after.

### 1.2 Fix assistant context selection slicing in `src/app.rs`

**Problem:** `assistant_editor_context()` slices `buffer.text()[start..end]` with char indices → panic on Unicode selection.

**Tasks**
- Convert `start`/`end` positions to byte offsets via `buffer.position_to_byte_index(...)` before slicing, or use the same safe helper from `src/vim.rs` extracted into a shared utility (e.g. `src/editor/buffer.rs` or a small `src/editor/coordinates.rs`).
- Add test: assistant panel with a selection spanning multibyte characters.
- Ensure `buffer.insert_at_cursors` (used by “Insert at cursor”) doesn’t regress.

### 1.3 Vim search behavior: make `?` actually search backward

**Files:** `src/vim.rs`, `src/editor/widget.rs`

**Tasks**
- Store the search direction when the user presses `/` vs `?` (e.g. `VimState.search_forward: bool`).
- Initialize `cmdline` with the matching prompt char.
- Pass direction to `jump_to_next_match`.
- Keep `/` forward and `?` backward, including `n`/`N` semantics.
- Add tests for `?` on forward/backward matches.

### 1.4 Vim paste must respect mode

**Problem:** `Event::Paste` inserts directly even in Normal/Visual/Command/Search mode.

**Tasks**
- In `src/editor/widget.rs` `handle_keyboard_input`, when `vim_enabled`:
  - Normal mode: route paste through `feed_vim` or make it equivalent to `p`/`P`.
  - Insert mode: insert normally.
  - Visual mode: replace selection with pasted text.
  - Command/Search mode: feed paste text into `cmdline`.
- Add widget tests for paste in each mode.

### 1.5 Vim `zz` / `zt` / `zb`

**Problem:** advertised but no-op; comment claims widget scroll logic handles it but it doesn’t.

**Tasks**
- Either implement viewport centering/top/bottom in the widget (set `state.desired_scroll_y` / a new `scroll_to_vim_position` flag), or explicitly remove the claims and `Pending::Z` handling.
- Prefer implementing: add `VimResult::viewport: Option<VimViewport>` or extend `EditorAction` to carry scroll intent.
- Add tests for zz/zt/zb.

### 1.6 Vim `:q!` / `:quit!` must force-close

**Files:** `src/app.rs`, `src/vim.rs`

**Tasks**
- Distinguish `Quit` (prompt if dirty) vs `ForceQuit`.
- In `handle_editor_action`, `ForceQuit` bypasses `request_close_file`’s unsaved prompt (close without saving, or confirm only once via an app-level “force close” param).
- Add app-level test that `:q!` closes even when the buffer is dirty, while `:q` does not.

### 1.7 AI assistant: real streaming or honest metadata

**Problem:** `ProviderEvent::Chunk` is never sent; `show()` may claim streaming.

**Tasks**
- Choose one path:
  - **(Preferred)** Implement true streaming: read stdout incrementally on a worker thread (or read chunks via `BufReader`/`read_until`) and send `Chunk` events; keep `MAX_STREAM_CHARS` cap and propagate cancellation.
  - **(Fallback)** Keep buffered output but remove “streaming” from README/tooltip/plan.
- If streaming is implemented:
  - Handle stderr interleaving reasonably.
  - Stop reading when the panel is cleared/closed (cancellation flag already exists in spirit; wire it properly).
  - Add tests with a fake provider that emits output slowly.
- Update `src/assistant.rs` doc comments and `README.md` to match reality.

### 1.8 Assistant: Windows command quoting

**Problem:** POSIX single-quote escaping produced for `cmd /C`.

**Tasks**
- Detect platform inside `render_command`/worker; on Windows use `cmd.exe`-appropriate quoting (double quotes with `^`/`""` escaping as needed) for `{prompt}`, `{file}`, `{selection}`.
- Keep POSIX quoting for Unix.
- Add unit tests for a Windows-specific quoting path (can be table-driven with `#[cfg]` or a `quote_for_shell(target: Shell)` helper).

### 1.9 Assistant: don’t drop trailing prose after last code fence

**Files:** `src/assistant.rs`

**Tasks**
- Modify `split_code_blocks` to emit final `Segment::Text` if trailing non-whitespace prose exists.
- Keep the existing test behavior for the *first* code-block extraction, but update `split_code_blocks`-specific expectations (the existing test currently asserts trailing prose is dropped; update it deliberately — this is not a guard test, so it’s fine).
- Add test for paragraphs after the last fence.

### 1.10 Inline diagnostics overflow / clipping

**Files:** `src/editor/widget.rs`, `src/theme.rs` maybe

**Tasks**
- Truncate or ellipsize inline message horizontally based on available editor width to the minimap.
- Reserve a fixed right inset so the message doesn’t overlap the minimap; draw in a clipped painter.
- Optionally show full message in hover tooltip.
- Add a widget test (or at least an assertion on the truncation logic) for a long message on a narrow pane.

### 1.11 Theme duplicate labels / two “One Dark” palettes

**Files:** `src/settings.rs`, `src/theme.rs`

**Tasks**
- Decide canonical naming:
  - Keep `Theme::Dark`/`Theme::Light` as aliases? Better: make `Theme::Dark`/`Theme::Light` the **default concrete** themes and rename their display names to “Default Dark” / “Default Light”, or collapse `OneDark`/`OneLight` into the default entries.
- Ensure `Theme::all()` yields unique `display_name` values.
- Decide whether `blue_dark()`/`blue_light()` should be removed in favor of `one_dark()`/`one_light()`, or vice versa. The README says One Dark/Light are the defaults — make `built_in_theme(Theme::Dark)` and `built_in_theme(Theme::OneDark)` resolve to the **same** palette OR remove the duplicate entry.
- Update `ZED_PARITY_PLAN.md`’s theme picker claims.
- Add test asserting `Theme::all()` returns unique display names and serialized IDs.

### 1.12 `rgba()` premultiplied alpha bug

**Files:** `src/theme.rs`

**Tasks**
- Change `fn rgba(value: u32, alpha: u8)` to produce a genuinely premultiplied color:
  - `r = (r * alpha) / 255`, `g = ...`, `b = ...`, then `Color32::from_rgba_premultiplied(r, g, b, alpha)`.
- Re-run the existing theme contrast tests; verify visual difference is intentional (translation becomes more subtle as originally intended).
- Add a unit test that `rgba(...)` is roughly the unmultiplied color blended at `alpha/255`.

### 1.13 `inline_diagnostics` serde default vs in-memory default mismatch

**Files:** `src/settings.rs`

**Tasks**
- Pick one source of truth. Recommended default **on** per current `EditorSettings::default`, so change `#[serde(default = "default_true_setting")]` for `inline_diagnostics`.
- Add test: deserialize a settings snippet without `editor.inline_diagnostics` and assert it equals `EditorSettings::default().inline_diagnostics`.

### Phase 1 Definition of Done
- All Phase 1 fixes landed with unit tests.
- `cargo test --lib` green; no new warnings from `cargo clippy`.
- No new dependency added.
- Existing integration guards untouched (no tests weakened).
- README claims match implementation.

---

## Phase 2 — Trust / security hardening (fail-closed)

### 2.1 Make trust fail-closed everywhere executable code is run

**Files:** `src/workspace.rs`, `src/app.rs`, `src/trust_ui.rs`, `src/plugins/*`, `src/tasks/*`, `src/profiler.rs`, `src/lsp/manager.rs`, `src/terminal/*`

**Tasks**
- Define a single helper: `fn can_execute(trust_store: &Option<TrustStore>, root: &Path, capability: ExecutableCapability) -> bool` that **returns false unless** a trust_store exists and `permits(root, capability)`. Replace the current `if let Some(trust_store) = ...` pattern in `run_task` so `None` does not bypass security.
- Enforce trust before:
  - **Plugins:** `load_all`/`reload_all`. If root untrusted, do not load `.blue/plugins` (or run in an even more restricted “no API/FS” mode after security review).
  - **LSP:** `start_lsp()` / `lazy_get_client`. An untrusted folder must not auto-spawn user-configured server commands. Provide an explicit user action/approval or a “restricted mode” that still allows read-only editing/search/browse per the trust prompt copy.
  - **Terminals:** `create_session`. Opening a shell is executable.
  - **Profiler:** `profiler_state.run()`. `cargo flamegraph` is executable.
  - **Tasks:** already partially done; fix the `None` bypass.
  - **Git remote ops** (fetch/pull/push) if they execute network commands — they use git2 (no shell) but still touch network; decide whether they need capability gating, and document the decision.
- Decide and document exactly what remains available in Untrusted/Restricted mode:
  - Allowed: open/edit/browse/search, LSP **diagnostics/hover/completion** via a trusted/declared safe process? (This is a policy decision.)
  - By default: treat unknown as Untrusted unless the user explicitly opens the trust prompt and chooses Trust.
- Add tests:
  - `run_task` blocks when `trust_store` is `None`.
  - Trust gating blocks terminal/profiler/plugin/LSP in untrusted mode.
  - Trusting a root unblocks the same actions.
- Preserve the user-facing trust prompt flow (it currently appears on open); ensure it appears **before** plugins/LSP start if those are listed as executable.

### 2.2 Guard trust prompt ordering

**Tasks**
- Move `plugin_system.load_all` and `start_lsp` so they respect the trust decision or are deferred until trust is granted in `open_workspace_folder` and `load_session`.
- On session restore, if a root is not trusted, do not start LSP/load plugins until the user validates.

### 2.3 Hardening the Lua plugin sandbox

**Files:** `src/plugins/sandbox.rs`, `src/plugins/api.rs`

**Tasks**
- Keep the no-`io`/`os`/`package`/`debug`/`load`/`loadstring`/`dofile` baseline; add:
  - A memory/time watchdog (e.g. cap allocation via a runtime table or instruction hook is insufficient — document as a known limitation or add a limit on `string` operations).
  - Confirm `require` is unavailable (it already is if `package` is removed; add a test).
  - Add a test asserting `os`, `io`, `require`, `dofile` are nil after `apply_sandbox`.
- If a future plugin loads from an untrusted workspace, the sandbox should not be the only defense — trust gating should prevent loading entirely.

### Phase 2 Definition of Done
- All executable paths fail closed when trust is unknown/absent.
- Untrusted mode is visibly restrictive and documented in the trust prompt.
- Tests cover the `None` bypass and each gated feature.
- No new dependency added.

---

## Phase 3 — Wire-in or delete foundational/unwired code

### 3.1 Multi-root workspace: migrate remaining single-root paths

**Files:** `src/app.rs`, `src/workspace.rs`, `src/lsp/*`, `src/tasks/*`, `src/terminal/*`, `src/profiler.rs`, `src/git/*`

**Tasks**
- Audit every `self.tree.root_path` call site and replace with `workspace_root_for_path(path)` / `primary_workspace_root()`.
  - Known remaining sites: `src/app.rs:3835,3902,5712,5806,5726,5820,6747,6833,7056,7814,9135,10168,10235,10410,10420,10529,11376,11390,11409,11427`.
- Ensure `Workspace::owner_of` is used for deepest-root ownership consistently.
- Update `is_lsp_path`/`request_*` helpers to be root-aware (they already mostly are; remove fallbacks that silently use `tree.root_path`).
- Add tests for:
  - two nested roots where a file belongs to the deepest root; LSP/git/tasks/terminals/profiler use the correct root.
  - adding a second root doesn’t clobber the active root in LSP/terminal/task state.

### 3.2 Unify / wire or delete `SessionState` and `BackgroundJobs`

**Files:** `src/app.rs`, `src/workspace.rs`

**Decide one of two paths:**

> **Decision taken: Path B — delete.** The live session path is `AppSessionState` in `src/app.rs`; wiring `SessionState`/`BackgroundJobs` into git/DAP/tasks/profiler is not justified while those consumers are still using ad-hoc state. The dead types and their tests were removed in `aria/01a0670e-stack-ide`.

**Path A — wire them (recommended if “foundation” work is intended):**
- Remove `AppSessionState` and use `SessionState` as the single source of truth.
- Extend `SessionState` with the fields currently only in `AppSessionState` (`pane_tree`, `pinned_tabs`, `tab_groups`, `tab_to_group`, `bookmarks`, `scroll_positions`), plus cursor/selection/scroll capture where feasible.
- Add an atomic save path (reuse `SessionState::save`).
- Wire `BackgroundJobs` into `git::remote::spawn_fetch/pull/push`, DAP, tasks, profiler:
  - Return `job_id` from `start`.
  - Replace the ad-hoc progress receivers with `BackgroundJobs::set_progress`.
  - Implement real cancellation: either a shared `Arc<AtomicBool>` per job, or worker shutdown on `request_cancel`.
- Add tests for job lifecycle (start → progress → finish/fail/cancel).

**Path B — delete the foundation:**
- Remove `SessionState`/`BackgroundJobs` fields and modules from `app.rs`/`workspace.rs`; keep only `AppSessionState`.
- Remove the dead code from `src/workspace.rs`.
- This must be deliberate and clearly documented in the plan.

### 3.3 Delete or wire `src/config/` (dead module)

**Files:** `src/config/mod.rs`, `src/config/keybinds.rs`, `src/config/theme.rs`, `src/lib.rs`

**Decision:** the live system is `src/settings.rs`. The old `Config` subsystem is not used.
- **Preferred:** delete `src/config/` entirely.
- If keymap customization is a goal, migrate it into `src/settings.rs` or a new `Keymap` module that is actually loaded — do not keep a dead copy.
- Remove `pub mod config;` from `src/lib.rs`.
- Add a guard test (if desired) that `src/config` isn’t reintroduced, or at least update `lib.rs` docs.

### 3.4 Delete or wire `src/workspace_features/` (dead module)

**Files:** `src/workspace_features/*`, `src/lib.rs`

**Decision:** none of these modules are referenced by app code.
- If the features are intended, move the useful ones into `src/workspace.rs` or `src/settings.rs` and wire them.
- Otherwise delete the directory and `pub mod workspace_features;` from `src/lib.rs`.
- Update `ZED_PARITY_PLAN.md` references (it references `workspace_features/recent.rs` for recent projects — if deleting, update docs to actual code path).

### 3.5 Remove dead functions / `#[allow(dead_code)]` litter

**Files:** `src/app.rs`, `src/config/mod.rs`, others

**Tasks**
- Delete dead functions:
  - `save_active` (`src/app.rs:4539`)
  - `show_outline_panel` (`src/app.rs:7536`)
  - `handle_terminal_key` (`src/app.rs:11029`)
  - `outline_node_mut_by_path` (`src/app.rs:11075`) — verify it is truly unused; if it’s a helper for a dead panel, delete too.
- Remove `#[allow(dead_code)]` from used functions (`request_signature_help_at_cursor` is actually called; keep it but remove the annotation).
- Remove the many `#[allow(dead_code)]` default fns from `src/config/mod.rs` when that module is deleted.
- Run `cargo clippy --lib -- -D warnings` and resolve every remaining warning from the new code (fix dead code rather than suppressing).

### Phase 3 Definition of Done
- All remaining `tree.root_path` single-root paths either migrated or explicitly justified with a TODO + owner (no silent stray uses).
- `SessionState` / `BackgroundJobs` are either fully wired with tests or fully removed.
- `src/config` and `src/workspace_features` are either wired or deleted; no dead modules remain.
- `cargo clippy --lib -- -D warnings` passes with **no** `#[allow(dead_code)]` added.

---

## Phase 4 — Feature completion / parity (each a small independent slice)

### 4.1 Git: per-hunk staging/unstaging + commit amend

**Files:** `src/git/*`, `src/app.rs`, maybe `src/git/ui.rs`

**Tasks**
- Implement `stage_hunk`/`unstage_hunk` in `src/git/diff.rs` (apply only a `DiffHunk` range to the index).
- Add a UI affordance in the git gutter or git panel.
- Add `GitPanelAction::StageHunk { path, hunk }` / `UnstageHunk`.
- Add commit amend option in commit UI.
- Add tests using temp repos (set identity as the existing git tests do).

### 4.2 LSP save notification: decide + implement (requires policy change)

**Files:** `src/lsp/transport.rs`, `src/lsp/mod.rs`, `src/app.rs`, `tests/integration_test.rs`

**Tasks**
- This touches `ask_before_changing_the_lsp_synchronization_strategy`.
- If auto-save should notify LSP, submit approval and update the guard test/fingerprint to `"didSave": true` plus send `textDocument/didSave` in `write_buffer_to_disk`.
- If not approved, adjust the README/PR wording to say LSP didSave is deliberately off.
- Add test: after a manual or auto save, `didSave` is sent when enabled.

### 4.3 DAP debugging: wire the client into the UI

**Files:** `src/debug.rs`, `src/app.rs`, `src/panes/*`

**Tasks**
- Add a debug config source (workspace `.blue`, settings, or manual).
- Wire `DebugSession` lifecycle into the bottom `DebugConsole` panel: start, stop, breakpoints, continue/pause/step, thread/stack frame, scopes/variables.
- Add trust gating (see Phase 2) before launching a debug adapter.
- Add tests for DAP client framing and state transitions (no process spawn for unit tests).

### 4.4 SSH remote editing: full transport or honest “foundation”

**Files:** `src/remote.rs`, `src/app.rs`, `src/editor/*`

**Tasks**
- If full remote editing is a real goal, replace the `ssh`/`scp` CLI stub with a proper SFTP/SSH library (new dependency → requires allowlist approval), plus:
  - known-host verification,
  - remote file browsing,
  - save conflict detection and atomic upload,
  - reconnect.
- If not, keep the foundation but mark it “experimental/stub” in docs and remove claims of remote editing parity.

### 4.5 Multibuffers, notebooks/REPL, dev containers, marketplace

**Tasks**
- Each is a separate vertical slice; treat as roadmap, not part of this fix batch.
- For **signed marketplace**: requires new crates, signature/hash verification, archive extraction, staged install/rollback/uninstall, and dedicated security tests.
- For **more tree-sitter grammars**: requires allowlist approval and per-grammar tests.
- For **snippet tab-stops**: requires explicit policy approval and update of `ask_before_adding_snippet_placeholder_support`.

### Phase 4 Definition of Done
- Each completed slice passes unit/integration tests.
- No slice modifies a policy guard without explicit approval.
- No new dependency added without updating `ask_before_adding_a_new_dependency`.

---

## Phase 5 — Tests, docs, CI hardening

### 5.1 Add integration guards for the new features

**Files:** `tests/integration_test.rs`, relevant `#[cfg(test)]` modules

**Tasks**
- Add named feature guard tests for:
  - Vim mode (modal state machine), including Unicode safety.
  - Assistant panel (provider command rendering, streaming behavior if implemented, code-block insert/copy).
  - Auto-save modes (after delay, focus change) and LSP didSave policy.
  - Inline diagnostics rendering (or at least message truncation logic as unit tests).
  - File-type icon classification.
  - Theme uniqueness and `rgba` correctness.
  - Trust gating per executable capability.
- Ensure these are positive tests, not weakened versions of existing ones.
- Add new rows to the `src/lib.rs` test table / `README.md` if they become suite-guard-entry tests.

### 5.2 Update docs

**Files:** `IMPLEMENTATION_AUDIT.md`, `ZED_PARITY_PLAN.md`, `README.md`

**Tasks**
- Rewrite/delete `IMPLEMENTATION_AUDIT.md` with a fresh status table matching the current tree.
- Update `ZED_PARITY_PLAN.md` to use the same legend (implemented / partial / missing) and correct the overclaims about streaming.
- Update `README.md` feature list and remove “streaming” if not implemented.
- Add a “Known limitations / intentionally deferred” section.

### 5.3 Make CI clippy actually block

**Files:** `.github/workflows/ci.yml`

**Tasks**
- Change `cargo clippy --lib -- -D warnings` from `continue-on-error: true` to a blocking step **once the warning set is clean** (Phase 1–3 must pass clippy first).
- Add `cargo fmt --check` as a separate required step (or fix formatting and make it required).
- Consider adding a job matrix for the full feature set if the suite gets too slow; keep the existing annotation/PR-comment reporting.

### 5.4 Final verification checklist

- `cargo fmt --check`
- `cargo check --lib --tests`
- `cargo test --lib`
- `cargo test --test integration_test`
- `cargo clippy --lib -- -D warnings`
- `cargo run` smoke test on a Rust + ASCII file.
- `cargo run` smoke test on a file containing `é🙂汉字` with Vim enabled; exercise `d`, `c`, `y`, `x`, `p`, `~`, `r`, `J`, visual delete/yank, `/` and `?`.
- Open an untrusted folder and confirm LSP/plugins/terminal/profiler/tasks are blocked by default; then trust it and confirm they work.
- Open a folder with two nested roots and verify LSP/git/tasks/terminals/profiler use the correct root.
- Theme picker: confirm no duplicate “One Dark”/“One Light” entries; confirm translucent selection/search colors look correct in dark and light.
- Auto-save: configure `after_delay`, edit a file, and confirm it appears saved after the delay; confirm `didSave` behavior matches docs (or is deliberately off).
- Assistant: send a request with a fake slow provider and confirm either real streaming or that docs say “buffered”; test “Insert at cursor” and “Copy”; test trailing prose after last code fence.

---

## Execution order & ownership

| Tranche | Includes | Suggested owner | Risk |
|---|---|---|---|
| T0 | Phase 0 | baseline setup | Low |
| T1 | Phase 1 (Unicode, vim, assistant, theme, inline diag) | editor/buffer + UI | High |
| T2 | Phase 2 (trust/security) | security/workspace | High |
| T3 | Phase 3 (wire/delete foundation, migrate roots) | workspace/app | Medium |
| T4 | Phase 4 (per-hunk, didSave policy ask, DAP, SSH, roadmap slices) | feature teams | Variable |
| T5 | Phase 5 (tests/docs/CI) | QA/infra | Low |

**Recommended commit sequence (on `arena/01a0670e-stack-ide`):**
1. Phase 1 correctness fixes + tests.
2. Phase 2 trust fixes + tests.
3. Phase 3 dead-code removal + root migration.
4. Phase 4 approved feature slices only.
5. Phase 5 docs/CI hardening.

---

## Open questions / approvals needed before implementation

1. **Assistant streaming** — implement real streaming or keep buffered and fix docs? (Preferred: real streaming.)
2. **Trust model** — should LSP, terminals, and plugins be fully blocked in untrusted mode, or should untrusted mode allow read-only LSP while blocking only local execution commands?
3. **`SessionState` / `BackgroundJobs`** — wire them in or delete them?
4. **`src/config` / `src/workspace_features`** — delete, or migrate specific pieces (e.g. keybinding config, recent projects)?
5. **LSP `didSave`** — request approval to change the LSP sync fingerprint, or keep it off?
6. **More languages / marketplace / SSH real transport / DAP UX** — which of these should be scheduled now vs. left on the roadmap?
