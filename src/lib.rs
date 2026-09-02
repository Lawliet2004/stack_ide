//! Blue IDE library crate: editor, LSP client, search, and application shell.
//!
//! # Regression tests
//!
//! Checklist-driven regression coverage is split by responsibility across module
//! `#[cfg(test)]` blocks and `tests/integration_test.rs`. Prefer focused unit tests
//! (`cargo test --lib <filter>`) over manual IDE sessions. Full lib suite:
//! `cargo test --lib`.
//!
//! | Layer | Doc table | Run |
//! |-------|-----------|-----|
//! | UTF-16 / cursor positions | `editor/position.rs` — Position tests | `cargo test --lib editor::position` |
//! | Unicode/LSP position conversion | `editor/position.rs` — `unicode_lsp_position_conversion_is_correct` | `cargo test --lib unicode_lsp_position_conversion_is_correct` |
//! | Buffer edits / coordinates | `editor/buffer.rs` — Buffer tests | `cargo test --lib editor::buffer` |
//! | Editor/UI + `egui::RawInput` | `editor/widget.rs` — Editor/UI state tests | `cargo test --lib editor::widget` |
//! | Completion popup UI | `editor/completion.rs` | `cargo test --lib editor::completion` |
//! | Hover popup / display policy | `editor/hover.rs` | `cargo test --lib editor::hover` |
//! | App orchestration (poll, stale gates) | `app.rs` | `cargo test --lib app::tests` |
//! | Baseline editor regression | `app.rs` — `normal_typing_cursor_movement_*` | `cargo test --lib normal_typing_cursor_movement` |
//! | LSP JSON-RPC wire | `lsp/transport.rs` — LSP transport tests | `cargo test --lib lsp::transport::tests` |
//! | LSP client facade | `lsp/mod.rs` — LSP client tests | `cargo test --lib lsp::tests` |
//! | Problems panel navigation | `problems_panel.rs` | `cargo test --lib problems_panel` |
//! | Search / replace | `search.rs`, `search_panel.rs` | `cargo test --lib search` |
//! | Syntax highlighting | `editor/highlight.rs`, `tests/integration_test.rs` | `cargo test --test integration_test` |
//! | Ctrl+Space completion e2e | `app.rs` — `ctrl_space_sends_a_real_completion_request_and_opens_a_functional_caret_anchored_dropdown` | `cargo test --lib ctrl_space_sends_a_real_completion` |
//! | Completion navigate/accept/click/dismiss | `app.rs` — `completion_can_be_navigated_accepted_clicked_and_dismissed` | `cargo test --lib completion_can_be_navigated` |
//! | Accepted completion prefix edit | `app.rs` — `accepted_completion_edits_the_correct_identifier_prefix` | `cargo test --lib accepted_completion_edits_the_correct_identifier_prefix` |
//! | Pointer hover e2e | `app.rs` — `pointer_hover_sends_a_debounced_real_lsp_hover_request_and_displays_documentation` | `cargo test --lib pointer_hover_sends_a_debounced_real` |
//! | Diagnostic vs LSP hover coexistence | `app.rs` — `diagnostic_tooltips_and_lsp_hover_coexist_according_to_the_specified_precedence` | `cargo test --lib diagnostic_tooltips_and_lsp_hover` |
//! | Stale async responses | `app.rs` — `stale_asynchronous_responses_cannot_affect_the_current_editor_state` | `cargo test --lib stale_asynchronous_responses_cannot` |
//! | Suite guard (all tests passing) | `tests/integration_test.rs` — `all_existing_and_new_tests_pass`, `existing_tests_remain_passing` | `cargo test all_existing_and_new_tests_pass` |
//! | Dependency allowlist | `tests/integration_test.rs` — `ask_before_adding_a_new_dependency` | `cargo test --test integration_test ask_before_adding_a_new_dependency` |
//! | LSP sync strategy fingerprint | `tests/integration_test.rs` — `ask_before_changing_the_lsp_synchronization_strategy` | `cargo test --test integration_test ask_before_changing_the_lsp_synchronization_strategy` |
//! | Manual completion trigger | `tests/integration_test.rs` — `ask_before_adding_automatic_completion_on_every_typed_character` | `cargo test --test integration_test ask_before_adding_automatic_completion_on_every_typed_character` |
//! | Snippet literal-only policy | `tests/integration_test.rs` — `ask_before_adding_snippet_placeholder_support` | `cargo test --test integration_test ask_before_adding_snippet_placeholder_support` |
//! | Editor widget architecture fingerprint | `tests/integration_test.rs` — `ask_before_broadly_redesigning_the_editor_widget` | `cargo test --test integration_test ask_before_broadly_redesigning_the_editor_widget` |
//! | Never: weaken/remove tests | `tests/integration_test.rs` — `remove_or_weaken_existing_tests_to_make_the_feature_pass` | `cargo test --test integration_test remove_or_weaken_existing_tests_to_make_the_feature_pass` |
//!
//! New checklist bullets add a named test plus a row to the relevant module table (one
//! bullet per turn). Wire-shape regressions stay in `lsp/transport.rs`; UI/state
//! regressions use `egui::RawInput` where practical (`editor/widget.rs`, `app.rs`).
//!
//! # Manual acceptance checklist
//!
//! Run after automated verification passes (`cargo test`, `cargo clippy`, `cargo build`).
//! Requires a display (not headless CI).
//!
//! **Prerequisites — Rust workspace with rust-analyzer**
//!
//! Use a folder Blue IDE can treat as a Cargo project root (contains `Cargo.toml` at or
//! above opened files). This repo (`stack_ide`) qualifies. `rust-analyzer` must be on
//! `PATH` (Blue IDE spawns it via settings default `rust-analyzer`).
//!
//! ```powershell
//! where.exe rust-analyzer          # must resolve (e.g. %USERPROFILE%\.cargo\bin\rust-analyzer.exe)
//! Test-Path .\Cargo.toml           # True for this acceptance workspace
//! ```
//!
//! **Launch**
//!
//! ```powershell
//! Set-Location "C:\Users\Papan Ghosh\Desktop\Projects\stack_ide"
//! cargo run
//! ```
//!
//! | # | Step | Expected |
//! |---|------|----------|
//! | 0 | Confirm prerequisites above | `rust-analyzer` found; `Cargo.toml` present in workspace root |
//! | 1 | **File → Open Folder…** — select this repo (`stack_ide`) | File tree appears; status bar shows LSP activity |
//!
//! **Autocomplete**
//!
//! Explicit **Ctrl+Space** invocation only (no automatic trigger while typing). Automated
//! coverage: `cargo test --lib editor::completion`, `cargo test --lib ctrl_space_produces`,
//! `cargo test --lib ctrl_space_sends_a_real_completion`.
//!
//! | # | Step | Expected |
//! |---|------|----------|
//! | 1 | **Open a Rust file** — e.g. click `src/lib.rs` in the file tree | Editor tab opens; Rust syntax highlighting; `.rs` path under the opened folder (LSP-eligible) |
//! | 2 | Place the caret after a partial identifier such as `pri` | Caret immediately after `pri`; editor focused; identifier prefix ready for completion |
//! | 3 | Press **Ctrl+Space** | Completion request sent; no literal space inserted (`cargo test --lib ctrl_space_produces`, `cargo test --lib ctrl_space_sends_a_real_completion`) |
//! | 4 | Confirm a completion dropdown appears at the caret | Popup anchored below (or above) the caret; rows show **label**, optional **detail**, kind-colored dot once rust-analyzer responds (`cargo test --lib ctrl_space_sends_a_real_completion`, `cargo test --lib popup_prefers_below_caret`) |
//! | 5 | Use **ArrowUp** / **ArrowDown** | Selection highlight moves; editor caret stays put (`cargo test --lib completion_can_be_navigated`, `cargo test --lib arrow_keys_change_completion`) |
//! | 6 | Press **Enter** and confirm only the identifier prefix is replaced | `pri` replaced by the selected item; `.`, `::`, and surrounding whitespace preserved; popup closes; caret after insertion (`cargo test --lib accepted_completion_edits_the_correct_identifier_prefix`, `cargo test --lib completion_can_be_navigated`, `cargo test --lib enter_and_tab_accept`, `cargo test --lib accepting_a_completion_replaces`, `cargo test --lib completion_does_not_delete_punctuation`) |
//! | 7 | Repeat steps 2–5 and accept using **Tab** | Same as step 6: prefix-only replacement, popup closes, caret after insertion (`cargo test --lib completion_can_be_navigated`, `cargo test --lib enter_and_tab_accept`) |
//! | 8 | Repeat steps 2–5 and dismiss using **Escape** | Popup closes; buffer unchanged (`cargo test --lib completion_can_be_navigated`, `cargo test --lib escape_dismisses_without_editing`) |
//! | 9 | Repeat steps 2–5, then click a completion row — confirm clicking an item inserts it | Clicked item inserted (prefix replaced when applicable); popup closes (`cargo test --lib completion_can_be_navigated`, `cargo test --lib clicking_completion_row`) |
//! | 10 | Repeat steps 2–5, then type a character **or** click outside the popup — confirm typing or clicking elsewhere closes the list | Popup closes; typing inserts the character; outside click leaves buffer unchanged (`cargo test --lib completion_can_be_navigated`, `cargo test --lib typing_dismisses_completion`, `cargo test --lib clicking_outside_completion`) |
//! | 11 | On a line with Unicode earlier (e.g. `let 🙂pri`), place caret after `pri` and press **Ctrl+Space** — confirm Unicode earlier on the same line does not offset the request | `textDocument/completion` uses UTF-16 column (not raw char index); completions still appear at the caret (`cargo test --lib unicode_earlier_on_the_same_line`) |
//! | 12 | Press **Page Down** / **Page Up** while list is long (optional) | Selection jumps by page; caret still fixed |
//! | 13 | Type `fn ` then **Ctrl+Space** | Keywords / snippets from rust-analyzer |
//! | 14 | With **File → Settings** open, press **Ctrl+Space** | No completion request; modal keeps focus (see **Boundaries → Never** §4) |
//!
//! **Hover**
//!
//! Rest the pointer over **source text** (not gutter) for **~0.35s** (`HOVER_REST_DELAY_SECS`)
//! to send `textDocument/hover`. Diagnostic tooltips and an open completion dropdown
//! take precedence. Automated coverage: `cargo test --lib editor::hover`,
//! `cargo test --lib hover_debounce_produces`, `cargo test --lib diagnostic_hover_suppresses`,
//! `cargo test --lib completion_popup_suppresses`, `cargo test --lib unicode_earlier_on_the_same_line_does_not_offset_the_hover`,
//! `cargo test --lib pointer_hover_sends_a_debounced_real`.
//!
//! | # | Step | Expected |
//! |---|------|----------|
//! | 1 | Rest the pointer over a known function, type, or variable (e.g. `main`, `String`, a `let` binding) | Pointer over rendered source glyphs (not gutter); hover debounce begins (`cargo test --lib hover_request_captures_hovered`) |
//! | 2 | Confirm documentation appears after a short delay (~0.35s) | Brief loading state, then popup with rust-analyzer docs (prose + optional fenced code) near the pointer (`cargo test --lib pointer_hover_sends_a_debounced_real`, `cargo test --lib hover_debounce_produces`, `cargo test --lib hover_request_is_debounced`) |
//! | 3 | Move the pointer and confirm stale documentation disappears | Popup clears when pointer leaves source text or moves to a new symbol; in-flight/stale responses do not repopulate the old popup (`cargo test --lib moving_outside_source_text`, `cargo test --lib hover_rejects_stale_response_when_pointer_moved`) |
//! | 4 | Hover a diagnostic underline and confirm the diagnostic tooltip takes precedence | Diagnostic message (severity, code, text) shown; no LSP documentation popup for that position (`cargo test --lib diagnostic_hover_suppresses`, `cargo test --lib lsp_hover_appears_only_when_no_diagnostic`) |
//! | 5 | Open completion (**Ctrl+Space**) and confirm hover does not overlap it | No LSP hover popup while the completion dropdown is open; pointer over symbols does not trigger hover (`cargo test --lib completion_popup_suppresses`, `cargo test --lib completion_dropdown_excludes`) |
//! | 6 | Confirm long hover documentation wraps or scrolls within the window | Prose wraps at popup max width; tall content scrolls inside the body without spilling past the window (`cargo test --lib long_hover_documentation_wraps_or_scrolls`) |
//! | 7 | On a line with Unicode earlier (e.g. `a🙂z`), rest over `z` — confirm Unicode earlier on the line does not offset the hover request | `textDocument/hover` uses UTF-16 column 3 at char column 2; documentation still appears at the symbol (`cargo test --lib unicode_earlier_on_the_same_line_does_not_offset_the_hover`) |
//! | 8 | Rest on a symbol, then move to a nearby symbol before ~0.35s elapses (optional) | Debounce resets; at most one request per stationary position (`cargo test --lib pointer_movement_resets_hover`) |
//! | 9 | Switch tabs or close the file | Hover state cleared; stale responses do not flash |
//! | 10 | Click outside the hover popup | Popup dismisses (`cargo test --lib clicking_outside_hover`) |
//!
//! **Boundaries**
//!
//! Split into **Always** (invariants every session must satisfy), **Never** (out-of-scope
//! or forbidden behavior), and **Ask before** (contributor scope — get approval first).
//! Confirm **Always** and **Never** during manual acceptance; use **Ask before** in scope reviews.
//!
//! **Always**
//!
//! | # | Always | Expected |
//! |---|--------|----------|
//! | A1 | Buffer synced before completion | Unsaved edits sent via `didChange` before `textDocument/completion` (`cargo test --lib completion_apply_allows_normal_lsp`) |
//! | A2 | LSP wire positions use UTF-16 | Cursor/hover/completion columns encoded — never raw char indices (`cargo test --lib cursor_lsp_position`, `cargo test --lib unicode_earlier_on_the_same_line`) |
//! | A3 | Diagnostic tooltips keep working | Squiggle hover still shows severity/code/message alongside LSP features (`cargo test --lib diagnostic_squiggle`, `cargo test --lib normal_typing_cursor_movement`) |
//! | A4 | Stale LSP responses are dropped | Tab, revision, cursor, or pointer changes reject late results (`cargo test --lib stale_responses_are_ignored`) |
//! | A5 | UI correlation IDs stay monotonic | Each outbound request gets a fresh id tracked in `lsp_pending` (`cargo test --lib ui_correlation_ids_are_monotonically_increasing`) |
//! | A6 | Precedence order is fixed | Completion dropdown → diagnostic tooltip → LSP hover (`cargo test --lib diagnostic_hover_suppresses`, `cargo test --lib completion_popup_suppresses`) |
//! | A7 | Preserve the non-blocking UI thread | LSP I/O on a background thread; each frame `poll_lsp()` drains via non-blocking `try_recv` — never waits on rust-analyzer (`cargo test --lib preserve_the_non_blocking_ui_thread`) |
//! | A8 | Use existing channels and LSP transport | Completion/hover/goto and buffer sync enqueue typed `LspRequest` values on the client channel; inbound traffic is typed `LspResponse` from `spawn_lsp_thread` — no parallel JSON-RPC from UI (`cargo test --lib use_existing_channels_and_lsp_transport`) |
//! | A9 | Validate request context before applying asynchronous responses | `poll_lsp()` routes correlated responses only after session/tab/revision/cursor/pointer context still matches the outbound request (`cargo test --lib validate_request_context_before_applying_asynchronous_responses`) |
//! | A10 | Keep all popup interactions keyboard accessible | Completion (↑/↓/PgUp/PgDn, Enter/Tab, Esc), find/replace (Esc), and modals (Esc) remain operable without pointer (`cargo test --lib keep_all_popup_interactions_keyboard_accessible`) |
//! | A11 | Keep the implementation panic-free for Unicode and empty lines | Cursor, prefix, LSP UTF-16 encode/decode, and edits clamp safely on `""`, empty buffer lines, and supplementary characters (`cargo test --lib keep_the_implementation_panic_free_for_unicode_and_empty_lines`) |
//! | A12 | Add tests for new state transitions and text edits | New popup/buffer paths ship with a named regression test covering session teardown and revision/`needs_lsp_sync` after accept vs dismiss (`cargo test --lib add_tests_for_new_state_transitions_and_text_edits`) |
//! | A13 | Ctrl+Space sends a real completion request and opens a functional caret-anchored dropdown | `show_editor` Ctrl+Space enqueues `LspRequest::Completion`, applies typed results via `poll_lsp()`, and positions a navigable popup at the caret anchor (`cargo test --lib ctrl_space_sends_a_real_completion`, `cargo test --lib ctrl_space_produces`, `cargo test --lib popup_prefers_below_caret`) |
//! | A14 | Completion can be navigated, accepted, clicked, and dismissed | Arrow keys move selection without editing; Enter/Tab accept prefix replacements; row clicks apply items; Escape and outside clicks dismiss without edits (`cargo test --lib completion_can_be_navigated`, `cargo test --lib arrow_keys_change_completion`, `cargo test --lib enter_and_tab_accept`, `cargo test --lib clicking_completion_row`, `cargo test --lib escape_dismisses_without_editing`, `cargo test --lib clicking_outside_completion`) |
//! | A15 | Accepted completion edits the correct identifier prefix | Completion sessions freeze `prefix_char_range` at request time; Enter accept via `show_editor` replaces only that span and preserves `.`, `::`, whitespace, and earlier Unicode on the line (`cargo test --lib accepted_completion_edits_the_correct_identifier_prefix`, `cargo test --lib accepting_a_completion_replaces`, `cargo test --lib completion_replaces_identifier_prefix`, `cargo test --lib completion_does_not_delete_punctuation`) |
//! | A16 | Pointer hover sends a debounced real LSP hover request and displays documentation | Resting pointer over source text debounces for `HOVER_REST_DELAY_SECS`, enqueues `LspRequest::Hover`, then `poll_lsp()` applies typed docs and renders a popup (`cargo test --lib pointer_hover_sends_a_debounced_real`, `cargo test --lib hover_request_is_debounced`, `cargo test --lib hover_debounce_produces`, `cargo test --lib poll_lsp_applies_typed_hover`) |
//! | A17 | Diagnostic tooltips and LSP hover coexist according to the specified precedence | Diagnostic squiggle hover shows editor-owned tooltip and suppresses LSP requests, rendering, and `receive_hover`; clean source text on the same buffer still debounces and shows LSP documentation (`cargo test --lib diagnostic_tooltips_and_lsp_hover`, `cargo test --lib diagnostic_hover_suppresses`, `cargo test --lib resolve_pointer_hover_precedence`, `cargo test --lib lsp_hover_allowed`) |
//! | A18 | Unicode/LSP position conversion is correct | Rust `char` indices encode to UTF-16 for outbound LSP requests and decode for diagnostics/text edits; supplementary characters widen wire columns without using raw char indices (`cargo test --lib unicode_lsp_position_conversion_is_correct`, `cargo test --lib cursor_lsp_position`, `cargo test --lib unicode_earlier_on_the_same_line`, `cargo test --lib use_raw_character_columns_as_lsp_utf16_columns`) |
//! | A19 | Stale asynchronous responses cannot affect the current editor state | Late completion/hover/goto results from `receive_*` and `poll_lsp()` are dropped when tab, revision, cursor, pointer, or correlation context changed; buffer text, caret, and revision stay unchanged (`cargo test --lib stale_asynchronous_responses_cannot`, `cargo test --lib stale_responses_are_ignored`, `cargo test --lib display_stale_lsp_results`, `cargo test --lib validate_request_context_before_applying_asynchronous_responses`) |
//! | A20 | All existing and new tests pass | Full lib and integration regression binaries stay green; checklist additions ship with passing named tests (`cargo test all_existing_and_new_tests_pass`, `cargo test existing_tests_remain_passing`, `cargo test --test integration_test remove_or_weaken_existing_tests_to_make_the_feature_pass`) |
//!
//! **Never**
//!
//! | # | Never | Expected |
//! |---|----------|----------|
//! | 1 | Completion is **Ctrl+Space** only | Typing does not auto-open completion or re-request on every keystroke (`cargo test --lib typing_dismisses_completion`) |
//! | 2 | No project folder or non-`.rs` active file | Status-bar message; no LSP completion/hover (`cargo test --lib hover_not_requested_for_ineligible`) |
//! | 3 | rust-analyzer not running / not ready | No completion/hover requests (`cargo test --lib hover_not_requested_when_lsp_not_running`) |
//! | 4 | Modal open (Settings, unsaved-close, exit confirm) | No new completion/hover; open popups dismissed (`cargo test --lib opening_modal_dismisses`, `cargo test --lib modal_overlays_exclude`) |
//! | 5 | Empty completion response | Popup does not open (`cargo test --lib empty_results_do_not_open`) |
//! | 6 | Snippet-like `insertText` (e.g. `${1:}`) | Inserted as plain text — no tab-stop navigation (`cargo test --lib completion_inserts_snippet_like_text`) |
//! | 7 | Ordinary LSP completion/hover errors | Silent close — no disruptive app-wide error dialog (`cargo test --lib lsp_completion_and_hover_errors_are_silent`) |
//! | 8 | Pointer in gutter or past line end | No hover debounce / documentation popup |
//! | 9 | Undisplayable hover payload (raw JSON/debug) | Popup stays closed (`cargo test --lib hover_undisplayable_content`) |
//! | 10 | Tab switch, buffer edit, or file close while popups open | Stale completion/hover state cleared (`cargo test --lib tab_file_revision_changes`, `cargo test --lib stale_responses_are_ignored`) |
//! | 11 | Block waiting for rust-analyzer on the UI thread | `poll_lsp()` / `LspClient::poll()` drain with `try_recv` only — never block a UI frame on rust-analyzer (`cargo test --lib block_waiting_for_rust_analyzer_on_the_ui_thread`, `cargo test --lib preserve_the_non_blocking_ui_thread`) |
//! | 12 | Display stale LSP results | Late completion/hover responses never populate UI after tab, revision, cursor, pointer, or request-id context changes (`cargo test --lib display_stale_lsp_results`, `cargo test --lib stale_responses_are_ignored`, `cargo test --lib validate_request_context_before_applying_asynchronous_responses`) |
//! | 13 | Use raw character columns as LSP UTF-16 columns | Outbound `textDocument/completion` / `hover` / `goto` and inbound diagnostic ranges encode/decode via UTF-16 helpers — never pass Rust `char` indices on the wire (`cargo test --lib use_raw_character_columns_as_lsp_utf16_columns`, `cargo test --lib cursor_lsp_position`, `cargo test --lib unicode_earlier_on_the_same_line`) |
//! | 14 | Swallow normal editor keystrokes when no popup is open | Typing and arrow/Page keys reach the editor when completion is closed; popup navigation consumes keys only while the dropdown is open (`cargo test --lib swallow_normal_editor_keystrokes_when_no_popup_is_open`, `cargo test --lib normal_typing_cursor_movement`, `cargo test --lib completion_navigation_keys_are_consumed_before_editor_input`) |
//! | 15 | Replace diagnostic tooltips with LSP hover | Diagnostic squiggle tooltips (severity, code, message) stay editor-owned; LSP documentation is suppressed while a diagnostic tooltip is active (`cargo test --lib replace_diagnostic_tooltips_with_lsp_hover`, `cargo test --lib diagnostic_hover_suppresses`, `cargo test --lib widget_renders_diagnostic`) |
//! | 16 | Render completion or hover using hard-coded mock data | Completion items and hover documentation come only from typed `poll_lsp()` responses (`CompletionList` / `HoverResult`) — no production mock item lists or canned docs (`cargo test --lib render_completion_or_hover_using_hard_coded_mock_data`, `cargo test --lib poll_lsp_applies_typed_hover_results_without_wire_parsing`) |
//! | 17 | Remove or weaken existing tests to make the feature pass | Checklist anchor regressions stay implemented, unignored, and covered by `existing_tests_remain_passing` — fix product code instead (`cargo test --test integration_test remove_or_weaken_existing_tests_to_make_the_feature_pass`, `cargo test existing_tests_remain_passing`) |
//!
//! **Ask before**
//!
//! | # | Ask before | Expected |
//! |---|------------|----------|
//! | 1 | Adding a new dependency | Get explicit approval first; then add the crate to `Cargo.toml` **and** update the allowlist in `ask_before_adding_a_new_dependency` (`cargo test --test integration_test ask_before_adding_a_new_dependency`) |
//! | 2 | Changing the LSP synchronization strategy | Get explicit approval first; full-document `didOpen`/`didChange` with per-buffer `needs_lsp_sync` must stay unless the strategy fingerprint test is deliberately updated (`cargo test --test integration_test ask_before_changing_the_lsp_synchronization_strategy`, `cargo test --lib completion_apply_allows_normal_lsp`) |
//! | 3 | Adding automatic completion on every typed character | Get explicit approval first; completion stays **Ctrl+Space**-only unless the manual-trigger fingerprint test is deliberately updated (`cargo test --test integration_test ask_before_adding_automatic_completion_on_every_typed_character`, `cargo test --lib typing_dismisses_completion`) |
//! | 4 | Adding snippet placeholder support | Get explicit approval first; `${…}` / `$0` markers stay literal unless the snippet literal-only fingerprint test is deliberately updated (`cargo test --test integration_test ask_before_adding_snippet_placeholder_support`, `cargo test --lib completion_inserts_snippet_like_text`) |
//! | 5 | Broadly redesigning the editor widget | Get explicit approval first; stateless `EditorWidget`, per-frame handoff types (`EditorInteraction` / `EditorAnnotations` / `EditorPresentation` → `EditorOutput`), upward `EditorAction` flow, and pointer-hover precedence helpers must stay unless the architecture fingerprint test is deliberately updated (`cargo test --test integration_test ask_before_broadly_redesigning_the_editor_widget`, `cargo test --lib editor::widget`, `cargo test --lib normal_typing_cursor_movement`) |
//!
//! **Baseline regression smoke**
//!
//! | # | Step | Expected |
//! |---|------|----------|
//! | B1 | Type, arrow keys, Page Up/Down | Normal cursor movement and scrolling |
//! | B2 | **Ctrl+F** — search for a string | Match highlights; F3 / Shift+F3 navigates |
//! | B3 | Hover a diagnostic underline (if present) | Diagnostic message tooltip (severity, code, text) |
//! | B4 | Open **File → Settings** (or unsaved-close modal) | Modal blocks completion/hover; Escape or Cancel restores editor (see **Boundaries → Never** §4) |

pub mod app;
pub mod assistant;
pub mod color_picker;
pub mod vim;
pub mod config;
pub mod content_error;
pub mod debug;
pub mod diff_viewer;
pub mod editor;
pub mod editorconfig;
pub mod file_watcher;
pub mod filetree;
pub mod git;
pub mod image_viewer;
pub mod language;
pub mod launcher;
pub mod lsp;
pub mod markdown_preview;
pub mod outline;
pub mod pane_content;
pub mod panes;
pub mod perf;
pub mod plugins;
pub mod problems_panel;
pub mod profiler;
pub mod project_template;
pub mod remote;
pub mod search;
pub mod search_panel;
pub mod settings;
pub mod tasks;
pub mod terminal;
pub mod terminal_mux;
pub mod text;
pub mod texture_registry;
pub mod theme;
pub mod trust_ui;
pub mod workspace;

// ─── src/workspace_features/ — Features 1–8 ─────────────────────────────────
/// Workspace feature modules: session, roots, editorconfig, templates, tasks,
/// trust, recent, exclude.
pub mod workspace_features;

pub mod zen_mode;

// New feature modules
pub mod font_ligatures;
pub mod rtl_text;
pub mod high_contrast;
pub mod screen_reader;
pub mod keyboard_nav;
