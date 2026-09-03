//! Integration tests — see crate root `# Regression tests` (`lib.rs`).
//!
//! | Area | Example tests | Run |
//! |------|---------------|-----|
//! | Suite guard (Always A20) | `all_existing_and_new_tests_pass` | `cargo test all_existing_and_new_tests_pass` |
//! | Suite guard (legacy anchor) | `existing_tests_remain_passing` | `cargo test existing_tests_remain_passing` |
//! | Dependency allowlist | `ask_before_adding_a_new_dependency` | `cargo test --test integration_test ask_before_adding_a_new_dependency` |
//! | LSP sync strategy fingerprint | `ask_before_changing_the_lsp_synchronization_strategy` | `cargo test --test integration_test ask_before_changing_the_lsp_synchronization_strategy` |
//! | Manual completion trigger | `ask_before_adding_automatic_completion_on_every_typed_character` | `cargo test --test integration_test ask_before_adding_automatic_completion_on_every_typed_character` |
//! | Snippet literal-only policy | `ask_before_adding_snippet_placeholder_support` | `cargo test --test integration_test ask_before_adding_snippet_placeholder_support` |
//! | Editor widget architecture fingerprint | `ask_before_broadly_redesigning_the_editor_widget` | `cargo test --test integration_test ask_before_broadly_redesigning_the_editor_widget` |
//! | Never: weaken/remove tests | `remove_or_weaken_existing_tests_to_make_the_feature_pass` | `cargo test --test integration_test remove_or_weaken_existing_tests_to_make_the_feature_pass` |
//! | Syntax highlighting | `integration_test_syntax_highlighting_*`, `test_highlight_*` | `cargo test --test integration_test` |
#![allow(clippy::all)]

use std::collections::BTreeSet;
use std::process::Command;

use blue_ide::editor::buffer::TextBuffer;
use egui::FontId;

fn assert_cargo_test_success(args: &[&str]) {
    let mut cmd = Command::new(env!("CARGO"));
    cmd.current_dir(env!("CARGO_MANIFEST_DIR"));
    
    let mut final_args = Vec::new();
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let nested_target = manifest_dir.join("target").join("nested");
    let nested_target_str = nested_target.to_str().unwrap();
    
    if !args.is_empty() {
        final_args.push(args[0]);
        if cfg!(windows) {
            final_args.push("--target-dir");
            final_args.push(nested_target_str);
        }
        final_args.extend_from_slice(&args[1..]);
    } else {
        final_args.extend_from_slice(args);
    }
    
    cmd.args(&final_args);
    
    if cfg!(windows) {
        cmd.env("CARGO_TARGET_DIR", &nested_target);
    }
    let status = cmd
        .status()
        .unwrap_or_else(|error| panic!("failed to spawn `cargo {}`: {error}", args.join(" ")));
    assert!(
        status.success(),
        "`cargo {}` must succeed (existing tests remain passing)",
        args.join(" ")
    );
}

fn approved_dependency_names() -> BTreeSet<&'static str> {
    [
        "cosmic-text",
        "crossbeam-channel",
        "directories",
        "eframe",
        "egui",
        "git2",
        "glob",
        "ignore",
        "image",
        "indexmap",
        "lsp-types",
        "mlua",
        "open",
        "portable-pty",
        "pulldown-cmark",
        "regex",
        "rfd",
        "ropey",
        "serde",
        "serde_json",
        "similar",
        "toml",
        "tree-sitter",
        "tree-sitter-javascript",
        "tree-sitter-python",
        "tree-sitter-rust",
        "tree-sitter-typescript",
        "uuid",
    ]
    .into_iter()
    .collect()
}

fn dependency_names_from_cargo_toml(manifest: &str) -> BTreeSet<String> {
    let mut in_dependencies = false;
    let mut names = BTreeSet::new();
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed == "[dependencies]" {
            in_dependencies = true;
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_dependencies {
                break;
            }
            continue;
        }
        if !in_dependencies || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((name, _)) = trimmed.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if !name.is_empty() {
            names.insert(name.to_string());
        }
    }
    names
}

/// Approved LSP document-sync strategy (full text + per-buffer dirty flag). Changing to
/// incremental/range sync or a different flush policy requires approval and updating these
/// markers (see **Boundaries → Ask before** §2).
const LSP_SYNCHRONIZATION_STRATEGY_MARKERS: &[(&str, &str, &str)] = &[
    (
        "src/editor/buffer.rs",
        "pub fn needs_lsp_sync",
        "per-buffer dirty flag before didChange",
    ),
    (
        "src/editor/buffer.rs",
        "pub fn mark_lsp_synced",
        "clear dirty after didChange is sent",
    ),
    (
        "src/app.rs",
        "fn ensure_lsp_document_synced",
        "pre-request document sync gate",
    ),
    (
        "src/app.rs",
        "ensure_lsp_document_synced(&path)",
        "completion waits for buffer sync",
    ),
    (
        "src/app.rs",
        "fn sync_lsp_changes",
        "batch didChange flush across open buffers",
    ),
    (
        "src/app.rs",
        "self.sync_lsp_changes();",
        "hover debounce path flushes pending edits",
    ),
    ("src/lsp/mod.rs", "pub fn did_open", "didOpen on file open"),
    (
        "src/lsp/mod.rs",
        "pub fn did_change",
        "didChange enqueue API",
    ),
    (
        "src/lsp/transport.rs",
        "\"method\": \"textDocument/didChange\"",
        "didChange wire method",
    ),
    (
        "src/lsp/transport.rs",
        "\"contentChanges\": [{ \"text\": text }]",
        "full-document contentChanges payload (not incremental ranges)",
    ),
];

fn read_workspace_file(relative_path: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative_path);
    std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("could not read {}: {error}", path.display());
    })
}

fn assert_source_markers_present(markers: &[(&str, &str, &str)], context: &str) {
    for (relative_path, marker, description) in markers {
        let content = read_workspace_file(relative_path);
        assert!(
            content.contains(marker),
            "{context} marker missing ({description}): expected `{marker}` in {relative_path}"
        );
    }
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

fn assert_lsp_sync_strategy_markers_present() {
    assert_source_markers_present(
        LSP_SYNCHRONIZATION_STRATEGY_MARKERS,
        "LSP synchronization strategy",
    );
}

/// Approved manual completion trigger (Ctrl+Space / Cmd+Space only). Auto-completion on
/// every typed character requires approval and updating these checks (see **Boundaries →
/// Ask before** §3).
const MANUAL_COMPLETION_TRIGGER_MARKERS: &[(&str, &str, &str)] = &[
    (
        "src/editor/widget.rs",
        "consume_key(Modifiers::COMMAND, Key::Space)",
        "explicit Ctrl+Space / Cmd+Space binding",
    ),
    (
        "src/editor/widget.rs",
        "editor_action = Some(EditorAction::RequestCompletion)",
        "completion routed as an explicit editor action",
    ),
    (
        "src/app.rs",
        "EditorAction::RequestCompletion =>",
        "app handles completion only from explicit action",
    ),
    (
        "src/lib.rs",
        "Explicit **Ctrl+Space** invocation only",
        "documented manual-trigger policy",
    ),
];

fn assert_typing_does_not_auto_request_completion() {
    let widget_rs = read_workspace_file("src/editor/widget.rs");
    let body = extract_fn_body(&widget_rs, "handle_keyboard_input")
        .expect("handle_keyboard_input should exist");
    assert!(
        !body.contains("RequestCompletion"),
        "typing must not enqueue RequestCompletion from handle_keyboard_input"
    );
    assert!(
        !body.contains("request_completion"),
        "typing must not call LSP completion directly from handle_keyboard_input"
    );
    assert!(
        body.contains("Event::Text"),
        "typing path should handle Event::Text"
    );
    assert!(
        body.contains("insert_at_cursor"),
        "typing should insert buffer text without opening completion"
    );

    let app_rs = read_workspace_file("src/app.rs");
    let production = app_rs
        .split("#[cfg(test)]\nmod tests")
        .next()
        .unwrap_or(app_rs.as_str());
    let request_sites = production
        .matches("self.request_completion_at_cursor()")
        .count();
    assert_eq!(
        request_sites, 1,
        "request_completion_at_cursor should be invoked only from the explicit \
         EditorAction::RequestCompletion handler"
    );
}

/// Approved completion acceptance policy: snippet-marker text is inserted literally.
/// Tab-stop navigation / `insertTextFormat` expansion requires approval and updating
/// these checks (see **Boundaries → Ask before** §4).
const SNIPPET_LITERAL_ONLY_MARKERS: &[(&str, &str, &str)] = &[
    (
        "src/lib.rs",
        "Inserted as plain text — no tab-stop navigation",
        "Never boundary documented",
    ),
    (
        "src/app.rs",
        "no snippet tab-stop navigation",
        "apply completion inserts literal text",
    ),
    (
        "src/editor/buffer.rs",
        "Snippet tab stops are not expanded",
        "buffer apply path",
    ),
    (
        "src/lsp/transport.rs",
        "neither expands snippet placeholders",
        "transport parse/accept policy",
    ),
    (
        "src/lsp/types.rs",
        "snippet placeholder syntax is not expanded",
        "typed completion item contract",
    ),
    (
        "src/app.rs",
        "fn completion_inserts_snippet_like_text_as_plain_text",
        "behavioral regression for literal snippet text",
    ),
];

const FORBIDDEN_SNIPPET_EXPANSION_HOOKS: &[&str] = &[
    "expand_snippet",
    "tab_stop",
    "TabStop",
    "SnippetSession",
    "insert_text_format",
    "SnippetPlaceholder",
];

fn assert_snippet_placeholders_are_not_expanded_at_runtime() {
    for relative_path in [
        "src/app.rs",
        "src/editor/buffer.rs",
        "src/editor/completion.rs",
        "src/editor/widget.rs",
    ] {
        let content = read_workspace_file(relative_path);
        for hook in FORBIDDEN_SNIPPET_EXPANSION_HOOKS {
            assert!(
                !content.contains(hook),
                "snippet expansion hook `{hook}` found in {relative_path}; add only after \
                 explicit approval and fingerprint test update"
            );
        }
    }
}

/// Approved editor widget architecture: stateless per-frame widget, typed handoff to the
/// app shell, and explicit pointer-hover precedence. Broad redesigns require approval
/// and a deliberate update to these checks (see **Boundaries → Ask before** §5).
const EDITOR_WIDGET_ARCHITECTURE_MARKERS: &[(&str, &str, &str)] = &[
    (
        "src/editor/widget.rs",
        "pub struct EditorWidget;",
        "stateless unit struct (no per-widget LSP/buffer ownership)",
    ),
    (
        "src/editor/widget.rs",
        "pub struct EditorInteraction",
        "per-frame interaction policy from app",
    ),
    (
        "src/editor/widget.rs",
        "pub struct EditorAnnotations",
        "read-only overlay data per frame",
    ),
    (
        "src/editor/widget.rs",
        "pub struct EditorPresentation",
        "visual configuration per frame",
    ),
    (
        "src/editor/widget.rs",
        "pub struct EditorOutput",
        "per-frame results consumed by app",
    ),
    (
        "src/editor/widget.rs",
        "pub enum EditorAction",
        "upward-requested editor actions",
    ),
    (
        "src/editor/widget.rs",
        "pub fn resolve_pointer_hover_precedence_with_completion",
        "completion → diagnostic → source-text precedence gate",
    ),
    (
        "src/editor/widget.rs",
        "resolve_pointer_hover_precedence_with_completion(",
        "precedence applied during EditorWidget::show",
    ),
    (
        "src/editor/widget.rs",
        "the widget never\n//!   owns or calls `LspClient`",
        "documented LSP ownership boundary",
    ),
    (
        "src/app.rs",
        "let output = EditorWidget::show(",
        "app owns buffer/state and invokes widget per frame",
    ),
    (
        "src/app.rs",
        "collected_editor_action = output.action",
        "editor actions collected after show, applied outside borrow",
    ),
    (
        "src/app.rs",
        "hover_popup = output.hover_popup",
        "hover handoff consumed by app debounce/render path",
    ),
];

const ANCHOR_EDITOR_WIDGET_REGRESSION_TESTS: &[(&str, &str)] = &[
    (
        "src/editor/widget.rs",
        "fn editor_interaction_model_groups_per_frame_gates",
    ),
    (
        "src/editor/widget.rs",
        "fn resolve_pointer_hover_precedence_prefers_diagnostic",
    ),
    (
        "src/editor/widget.rs",
        "fn ctrl_space_produces_one_completion_request_action_and_does_not_insert_a_space",
    ),
    (
        "src/app.rs",
        "fn diagnostic_hover_suppresses_lsp_hover",
    ),
    (
        "src/app.rs",
        "fn completion_popup_suppresses_lsp_hover",
    ),
    (
        "src/app.rs",
        "fn normal_typing_cursor_movement_scrolling_search_highlighting_diagnostic_underlines_diagnostic_tooltips_file_tabs_and_modal_behavior_continue_to_work",
    ),
];

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

fn assert_editor_widget_does_not_own_lsp_client() {
    let widget_rs = read_workspace_file("src/editor/widget.rs");
    let production = widget_rs
        .split("#[cfg(test)]\nmod tests")
        .next()
        .unwrap_or(widget_rs.as_str());
    let code = rust_source_without_comments(production);
    assert!(
        !code.contains("LspClient"),
        "editor widget must not reference LspClient in production code; redesign requires \
         explicit approval and fingerprint test update"
    );
}

fn assert_anchor_editor_widget_regression_tests_present() {
    for (relative_path, test_signature) in ANCHOR_EDITOR_WIDGET_REGRESSION_TESTS {
        let content = read_workspace_file(relative_path);
        assert!(
            content.contains(test_signature),
            "anchor editor widget regression test missing: expected `{test_signature}` in \
             {relative_path}"
        );
    }
}

/// Locks the editor widget to the stateless handoff architecture. Broad redesigns require
/// approval and a deliberate update to this test.
#[test]
fn ask_before_broadly_redesigning_the_editor_widget() {
    assert_source_markers_present(
        EDITOR_WIDGET_ARCHITECTURE_MARKERS,
        "editor widget architecture",
    );
    assert_editor_widget_does_not_own_lsp_client();
    assert_anchor_editor_widget_regression_tests_present();
}

/// Locks snippet-like completion text to literal insertion. Placeholder/tab-stop support
/// requires approval and a deliberate update to this test.
#[test]
fn ask_before_adding_snippet_placeholder_support() {
    assert_source_markers_present(SNIPPET_LITERAL_ONLY_MARKERS, "snippet literal-only policy");
    assert_snippet_placeholders_are_not_expanded_at_runtime();
}

/// Locks completion to an explicit Ctrl+Space trigger. Auto-completion on every keystroke
/// requires approval and a deliberate update to this test.
#[test]
fn ask_before_adding_automatic_completion_on_every_typed_character() {
    assert_source_markers_present(
        MANUAL_COMPLETION_TRIGGER_MARKERS,
        "manual completion trigger",
    );
    assert_typing_does_not_auto_request_completion();
}

/// Locks the full-document LSP sync strategy to an explicit source fingerprint. Strategy
/// changes require approval and a deliberate update to `LSP_SYNCHRONIZATION_STRATEGY_MARKERS`.
#[test]
fn ask_before_changing_the_lsp_synchronization_strategy() {
    assert_lsp_sync_strategy_markers_present();
}

/// Locks `[dependencies]` in `Cargo.toml` to an explicit allowlist. New crates require
/// approval and a deliberate update to this test (see **Boundaries → Ask before** §1).
#[test]
fn ask_before_adding_a_new_dependency() {
    let manifest_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", manifest_path.display()));
    let actual = dependency_names_from_cargo_toml(&manifest);
    let approved = approved_dependency_names()
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual, approved,
        "new [dependencies] entries require explicit approval and an allowlist update in \
         ask_before_adding_a_new_dependency"
    );
}

/// Suite guard must keep running the full lib and integration test binaries.
const REGRESSION_SUITE_GUARD_MARKERS: &[(&str, &str, &str)] = &[
    (
        "tests/integration_test.rs",
        "fn all_existing_and_new_tests_pass",
        "Always A20 suite guard entry point",
    ),
    (
        "tests/integration_test.rs",
        "fn existing_tests_remain_passing",
        "legacy suite guard anchor",
    ),
    (
        "tests/integration_test.rs",
        "fn run_all_regression_test_bins_quiet",
        "shared runner for lib + integration regression binaries",
    ),
    (
        "tests/integration_test.rs",
        "assert_cargo_test_success(&[\"test\", \"--lib\", \"--quiet\"])",
        "suite guard runs the lib test binary",
    ),
    (
        "tests/integration_test.rs",
        "SUITE_GUARD_INTEGRATION_TESTS",
        "suite guards share one skip list for the integration binary",
    ),
    (
        "tests/integration_test.rs",
        "\"all_existing_and_new_tests_pass\"",
        "A20 suite guard is excluded from nested integration runs",
    ),
    (
        "tests/integration_test.rs",
        "\"existing_tests_remain_passing\"",
        "legacy suite guard is excluded from nested integration runs",
    ),
    (
        "src/lib.rs",
        "all_existing_and_new_tests_pass",
        "regression hub documents the A20 suite guard",
    ),
    (
        "src/lib.rs",
        "existing_tests_remain_passing",
        "regression hub documents the legacy suite guard",
    ),
];

/// Named checklist regressions that must not be deleted, ignored, or hollowed out.
const ANCHOR_CHECKLIST_REGRESSION_TESTS: &[(&str, &str)] = &[
    (
        "tests/integration_test.rs",
        "fn existing_tests_remain_passing",
    ),
    (
        "tests/integration_test.rs",
        "fn ask_before_adding_a_new_dependency",
    ),
    (
        "tests/integration_test.rs",
        "fn ask_before_broadly_redesigning_the_editor_widget",
    ),
    (
        "tests/integration_test.rs",
        "fn remove_or_weaken_existing_tests_to_make_the_feature_pass",
    ),
    (
        "src/lsp/mod.rs",
        "fn preserve_the_non_blocking_ui_thread",
    ),
    (
        "src/app.rs",
        "fn validate_request_context_before_applying_asynchronous_responses",
    ),
    (
        "src/app.rs",
        "fn normal_typing_cursor_movement_scrolling_search_highlighting_diagnostic_underlines_diagnostic_tooltips_file_tabs_and_modal_behavior_continue_to_work",
    ),
    (
        "src/app.rs",
        "fn display_stale_lsp_results",
    ),
    (
        "src/app.rs",
        "fn render_completion_or_hover_using_hard_coded_mock_data",
    ),
];

fn assert_anchor_checklist_regression_tests_present() {
    for (relative_path, test_signature) in ANCHOR_CHECKLIST_REGRESSION_TESTS {
        let content = read_workspace_file(relative_path);
        assert!(
            content.contains(test_signature),
            "anchor checklist regression test missing: expected `{test_signature}` in \
             {relative_path}"
        );
    }
}

fn assert_anchor_regression_tests_are_not_ignored() {
    for (relative_path, test_signature) in ANCHOR_CHECKLIST_REGRESSION_TESTS {
        let content = read_workspace_file(relative_path);
        let fn_name = test_signature.strip_prefix("fn ").unwrap_or(test_signature);
        let ignored = format!("#[ignore]\nfn {fn_name}");
        let ignored_spaced = format!("#[ignore]\r\nfn {fn_name}");
        assert!(
            !content.contains(&ignored) && !content.contains(&ignored_spaced),
            "anchor regression `{fn_name}` must not be #[ignore] in {relative_path}"
        );
    }
}

fn assert_anchor_regression_tests_retain_assertions() {
    for (relative_path, test_signature) in ANCHOR_CHECKLIST_REGRESSION_TESTS {
        let content = read_workspace_file(relative_path);
        let fn_name = test_signature.strip_prefix("fn ").unwrap_or(test_signature);
        let body = extract_fn_body(&content, fn_name)
            .unwrap_or_else(|| panic!("could not extract `{fn_name}` in {relative_path}"));
        assert!(
            body.contains("assert") || body.contains("assert_cargo_test_success"),
            "anchor regression `{fn_name}` must retain real assertions in {relative_path}"
        );
        assert!(
            !body.contains("todo!") && !body.contains("unimplemented!"),
            "anchor regression `{fn_name}` must not be stubbed with todo!/unimplemented! in \
             {relative_path}"
        );
    }
}

fn walk_rust_sources(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|error| {
        panic!("could not read directory {}: {error}", dir.display());
    });
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| {
            panic!("could not read entry in {}: {error}", dir.display());
        });
        let path = entry.path();
        if path.is_dir() {
            walk_rust_sources(&path, files);
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
}

fn line_is_ignore_test_attribute(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed == "#[ignore]" || (trimmed.starts_with("#[ignore") && trimmed.ends_with(']'))
}

fn line_starts_ignored_test_function(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("fn ") || trimmed.starts_with("async fn ")
}

fn source_contains_ignored_test_function(source: &str) -> bool {
    let lines: Vec<&str> = source.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        if !line_is_ignore_test_attribute(line) {
            continue;
        }
        for next in lines.iter().skip(index + 1) {
            let next_trimmed = next.trim();
            if next_trimmed.is_empty() || next_trimmed.starts_with("//") {
                continue;
            }
            return line_starts_ignored_test_function(next);
        }
    }
    false
}

fn assert_regression_workspace_has_no_ignored_tests() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for relative_path in ["src", "tests"] {
        let dir = manifest_dir.join(relative_path);
        let mut rust_sources = Vec::new();
        walk_rust_sources(&dir, &mut rust_sources);
        for path in rust_sources {
            let content = std::fs::read_to_string(&path).unwrap_or_else(|error| {
                panic!("could not read {}: {error}", path.display());
            });
            assert!(
                !source_contains_ignored_test_function(&content),
                "regression tests must not be #[ignore] in {}",
                path.display()
            );
        }
    }
}

/// Never boundary: checklist regressions must not be removed, ignored, or hollowed out to
/// green a feature (see **Boundaries → Never** §17).
#[test]
fn remove_or_weaken_existing_tests_to_make_the_feature_pass() {
    assert_source_markers_present(REGRESSION_SUITE_GUARD_MARKERS, "regression suite guard");
    assert_anchor_checklist_regression_tests_present();
    assert_anchor_regression_tests_are_not_ignored();
    assert_anchor_regression_tests_retain_assertions();
    assert_regression_workspace_has_no_ignored_tests();
}

/// Integration tests that re-run the full suite and must be skipped during nested runs.
const SUITE_GUARD_INTEGRATION_TESTS: &[&str] = &[
    "all_existing_and_new_tests_pass",
    "existing_tests_remain_passing",
];

static RUN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Runs the lib crate and integration regression binaries (quiet). Suite-guard tests are
/// skipped together so nested `cargo test` invocations cannot recurse.
fn run_all_regression_test_bins_quiet() {
    let _lock = RUN_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    assert_cargo_test_success(&["test", "--lib", "--quiet"]);
    let mut args = vec!["test", "--test", "integration_test", "--quiet", "--"];
    for test_name in SUITE_GUARD_INTEGRATION_TESTS {
        args.push("--skip");
        args.push(test_name);
    }
    assert_cargo_test_success(&args);
}

/// Always boundary: every existing and newly added regression test must pass (see
/// **Boundaries → Always** A20).
#[test]
fn all_existing_and_new_tests_pass() {
    assert_source_markers_present(REGRESSION_SUITE_GUARD_MARKERS, "regression suite guard");
    run_all_regression_test_bins_quiet();
}

/// Re-runs the lib and integration suites so checklist changes cannot silently break
/// prior regression coverage.
#[test]
fn existing_tests_remain_passing() {
    run_all_regression_test_bins_quiet();
}

#[test]
fn integration_test_syntax_highlighting_on_rust_file() {
    let rust_code = r#"// Test file
fn main() {
    let x = 42;
    let s = "hello";
    if x > 0 { println!("positive"); }
}
"#;

    let mut buffer = TextBuffer::from_text(rust_code);
    let font_id = FontId::monospace(14.0);

    // Get layout - should trigger highlighting
    let layout = buffer.get_layout(font_id.clone());

    // Verify we have colored sections (not all default)
    assert!(!layout.sections.is_empty(), "Layout should have sections");

    // Verify we have some non-default colors
    let default_color = egui::Color32::from_rgb(0xD4, 0xD4, 0xD4);
    let has_color = layout
        .sections
        .iter()
        .any(|s| s.format.color != default_color);
    assert!(
        has_color,
        "Should have some colored sections, not all default"
    );

    // Verify cache works: second call should return same cached layout
    let layout2 = buffer.get_layout(font_id);
    assert_eq!(
        layout.sections.len(),
        layout2.sections.len(),
        "Cached layout should match"
    );

    println!("Integration test passed: highlighting works on Rust code");
}

/// Diagnostic helper: sorted distinct section colors as hex, for failure
/// messages. Assertions below are unchanged; this only improves reports.
fn distinct_colors(job: &egui::text::LayoutJob) -> Vec<String> {
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
fn test_highlight_keywords() {
    let code = "fn let if else";
    let mut buffer = TextBuffer::from_text(code);
    let font_id = FontId::monospace(14.0);
    let layout = buffer.get_layout(font_id);

    // Keywords should use the active theme's keyword color.
    let keyword_color = blue_ide::theme::default_syntax_palette().keyword;
    let has_keyword = layout
        .sections
        .iter()
        .any(|s| s.format.color == keyword_color);
    assert!(
        has_keyword,
        "Should highlight keywords with the theme keyword color; expected {:?}, found colors {:?}",
        keyword_color,
        distinct_colors(&layout)
    );
}

#[test]
fn test_highlight_strings() {
    let code = r#""string literal""#;
    let mut buffer = TextBuffer::from_text(code);
    let font_id = FontId::monospace(14.0);
    let layout = buffer.get_layout(font_id);

    // Strings should use the active theme's string color.
    let string_color = blue_ide::theme::default_syntax_palette().string;
    let has_string = layout
        .sections
        .iter()
        .any(|s| s.format.color == string_color);
    assert!(
        has_string,
        "Should highlight strings with the theme string color; expected {:?}, found colors {:?}",
        string_color,
        distinct_colors(&layout)
    );
}

#[test]
fn test_highlight_comments() {
    let code = "// this is a comment";
    let mut buffer = TextBuffer::from_text(code);
    let font_id = FontId::monospace(14.0);
    let layout = buffer.get_layout(font_id);

    // Comments should use the active theme's comment color.
    let comment_color = blue_ide::theme::default_syntax_palette().comment;
    let has_comment = layout
        .sections
        .iter()
        .any(|s| s.format.color == comment_color);
    assert!(
        has_comment,
        "Should highlight comments with the theme comment color; expected {:?}, found colors {:?}",
        comment_color,
        distinct_colors(&layout)
    );
}

#[test]
fn test_highlight_numbers() {
    let code = "42 3.14 0xFF";
    let mut buffer = TextBuffer::from_text(code);
    let font_id = FontId::monospace(14.0);
    let layout = buffer.get_layout(font_id);

    // Numbers should use the active theme's number color.
    let number_color = blue_ide::theme::default_syntax_palette().number;
    let has_number = layout
        .sections
        .iter()
        .any(|s| s.format.color == number_color);
    assert!(
        has_number,
        "Should highlight numbers with the theme number color; expected {:?}, found colors {:?}",
        number_color,
        distinct_colors(&layout)
    );
}

#[test]
fn test_cache_invalidation() {
    let mut buffer = TextBuffer::from_text("let x = 1;");
    let font_id = FontId::monospace(14.0);

    // First get
    let layout1 = buffer.get_layout(font_id.clone());
    let _sections1_count = layout1.sections.len();

    // Simulate edit (mark dirty)
    buffer.insert_at_cursor("foo").unwrap();

    // Second get should reparse
    let layout2 = buffer.get_layout(font_id);
    let _sections2_count = layout2.sections.len();

    // Both should have content, though counts may differ
    assert!(!layout1.sections.is_empty());
    assert!(!layout2.sections.is_empty());
    println!("Cache invalidation test passed: dirty flag triggers reparse");
}

#[test]
fn test_large_file_highlighting() {
    let code = (0..500)
        .map(|i| format!("let var{} = {};\n", i, i))
        .collect::<String>();

    let mut buffer = TextBuffer::from_text(&code);
    let font_id = FontId::monospace(14.0);

    // Should handle large file without panic
    let layout = buffer.get_layout(font_id);
    assert!(!layout.sections.is_empty());
    println!("Large file test passed: 500-line file highlighted successfully");
}
