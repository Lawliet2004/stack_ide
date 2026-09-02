# Zed Parity Plan — Blue IDE

**Goal:** bring Blue IDE's UI, UX, and engine capabilities to parity with
[Zed](https://zed.dev) (zed.dev), the Rust editor Blue IDE uses as its product
benchmark, and polish the overall look & feel to match.

**Date:** 2026-09-02 · **Baseline:** `1641eef` ("chore: publish version 1 baseline")

> Note: an older `IMPLEMENTATION_AUDIT.md` (2026-06-21) listed many capabilities
> as missing; nearly all of them (goto-line UI, workspace symbols, signature
> help, code actions, rename-adjacent flows, inline blame, stash/tag/conflict
> tooling, session persistence, trust gating) have since been implemented. This
> document is the **current** gap analysis and the roadmap going forward.

---

## 1. Gap analysis — Zed vs. Blue IDE today

Legend: ✅ parity or equivalent · 🟡 partial (exists, needs polish) · ❌ missing

### A. Editing core

| Zed capability | Status | Notes |
|---|---|---|
| Tree-sitter syntax highlighting, folds, sticky headers, indent guides, bracket colorization | ✅ | `editor/highlight.rs`, `editor/folding.rs`, `editor/widget.rs` |
| Multi-cursor (Alt+click, Ctrl+D next-occurrence, Ctrl+Shift+L select-all, column select) | ✅ | `editor/buffer.rs` cursor sets |
| Command palette (fuzzy) | ✅ | `launcher.rs` |
| File finder / quick open (fuzzy, workspace-indexed) | ✅ | `launcher.rs` |
| Project search & replace (regex, case, whole word, per-file apply) | ✅ | `search.rs`, `search_panel.rs` |
| Go to line/column (Ctrl+G), go to symbol (Ctrl+Shift+O), workspace symbols (Ctrl+T) | ✅ | `app.rs` modals |
| Outline panel, breadcrumbs | ✅ | `outline.rs`, `UiSettings::show_breadcrumbs` |
| Minimap | ✅ | `editor/minimap.rs` |
| Signature help, hover docs, completions, code actions, inlay hints, code lens, semantic tokens | ✅ | `lsp/*`, `editor/hover.rs`, `editor/completion.rs` |
| Line ops: move/duplicate/sort/join, case transforms, undo history panel | ✅ | `editor/buffer.rs` |
| Bookmarks | ✅ | F2/F9 + gutter |
| **Vim / modal editing** | ❌ | Zed's #1 community feature. **Phase 1 of this plan.** |
| **Multibuffers** (excerpts of many files in one buffer; used by project search results) | ❌ | Roadmap Phase 3. |
| Edit predictions / inline ghost-text completions | ❌ | Requires a model backend; roadmap Phase 4. |
| Auto save (off / after delay / on focus change) | ❌ | **Phase 1.** |

### B. Appearance & UI polish

| Zed capability | Status | Notes |
|---|---|---|
| Theme system with live-preview theme selector | 🟡 | 5 built-ins; Zed defaults are **One Dark / One Light** — not present. **Phase 1 adds One Dark/Light, Ayu, Gruvbox, Catppuccin + Ctrl+K Ctrl+T style picker.** |
| Per-filetype icons in project panel & tabs (icon themes) | 🟡 | Tree has color tints only. **Phase 1 adds a painted icon set.** |
| Zen mode / distraction free | ✅ | `zen_mode.rs` |
| Inline diagnostics (message rendered at end of line) | ❌ | Squiggle+tooltip only today. **Phase 1.** |
| Notification center (dismissable, typed) | 🟡 | Plugin toasts only; roadmap Phase 3. |
| Pane zoom, drag tabs between panes | 🟡 | Split/focus exists (`panes/`); zoom & DnD roadmap Phase 3. |
| Welcome screen, recent projects | ✅ | `workspace_features/recent.rs` |
| Settings UI (live-preview) | ✅ | `app.rs` settings modal |
| Keymap customization (TOML) | ✅ | `config/keybinds.rs` (Zed uses JSON keymaps; equivalent) |

### C. Language intelligence / backend

| Zed capability | Status | Notes |
|---|---|---|
| LSP: completion, hover, goto-def, diagnostics, formatting, symbols, rename-adjacent code actions, signature help | ✅ | `lsp/*` with stale-response gating |
| EditorConfig | ✅ | `editorconfig.rs` |
| Tasks (tasks.toml + problem matchers) | ✅ | `tasks/` |
| Language grammars beyond Rust/Python/JS/TS (Go, C, C++, JSON, TOML, YAML, MD, HTML/CSS…) | ❌ | Needs new tree-sitter crates → dependency allowlist approval required (repo policy test). Roadmap Phase 2 — **asked, not unilaterally added.** |
| Language-server auto-download/install | 🟡 | Server commands configurable; auto-install roadmap. |
| Snippets with tab-stops | 🟡 | Deliberately literal-only per repo policy ("ask before" test). |

### D. Git

Blame gutter, fetch/pull/push, log viewer, tags, stash, conflict resolver,
cherry-pick, diff-vs-HEAD viewer, status coloring — ✅ broad parity (`git/`).
Roadmap polish: per-hunk stage/unstage buttons, commit amend.

### E. Terminal & processes

PTY terminals with splits, search, links, env editor, history — ✅ (`terminal/`).

### F. Collaboration (Zed channels, calls, follow mode, channel notes)

❌ Server-product feature. Out of scope for a local IDE; a future **LAN/relay
collab** slice is listed in Phase 4 of the roadmap, non-goal for now.

### G. AI

| Zed capability | Status | Notes |
|---|---|---|
| Assistant panel (conversation, context of file/selection, insert code) | ❌ | **Phase 1** — provider-pluggable panel; ships a dependency-free "custom command" provider (works with `ollama`, `llm`, `aichat`, any CLI) plus an OpenAI-compatible provider seam. |
| Inline assist (prompt-to-edit in buffer) | ❌ | Phase 4. |
| Edit predictions (Copilot-style) | ❌ | Phase 4. |

### H. Extensibility

Lua plugin sandbox with menus/commands/notifications ✅ (`plugins/`).
Zed uses WASM extensions + signed marketplace — roadmap Phase 3/4 (marketplace
with signature verification is a standalone security-sensitive slice).

### I. Remote development (SSH projects, dev containers)

🟡 `DocumentUri`/`remote.rs` foundation + per-root trust exist; SSH transport,
browse, conflict-safe save — Phase 3.

### J. Debugger / profiler

DAP client foundation (`debug.rs`) + Rust profiler plumbing (`profiler.rs`);
breakpoint/variable/stack UX — Phase 3.

### K. Accessibility

Screen-reader annotations, high contrast, keyboard nav, RTL, ligatures — ✅
stronger than Zed's baseline today.

---

## 2. Phase 1 — implemented in this change (no new dependencies)

1. **Vim mode** (`src/vim.rs`) — Normal/Insert/Visual/Visual-Line, counts,
   motions (`h j k l w W b B e 0 ^ $ gg G f F t T %`), operators (`d c y > < gu
   gU ~`), text objects (`iw aw i" a" i' a' ip ap`), `x X s S D C o O p P r J u
   Ctrl+r`, visual ops, `/` search + `n/N`, `zz` scroll, ex line (`:w :q :wq :x
   :q! :noh :sort`), mode indicator in the status bar, persisted
   `editor.vim_mode` setting, command-palette toggle, policy-safe (default **off**).
2. **Theme pack + theme selector** — One Dark & One Light (Zed defaults), Ayu
   Dark/Mirage/Light, Gruvbox Dark/Light, Catppuccin Mocha/Latte + a
   live-preview theme picker modal (Zed's Ctrl+K Ctrl+T) and palette commands.
3. **File-type icons** (`src/file_icons.rs`) — painted brand-colored icons
   (Rust gear, Python, JS/TS, JSON/TOML/YAML, MD, images, lockfile, git…) in the
   project tree and editor tabs.
4. **Auto save** — `off | after_delay | focus_change` with configurable delay;
   wired through the existing save path so LSP `didSave` stays correct.
5. **Inline diagnostics** — severity-colored end-of-line messages (Zed-style),
   toggle in settings.
6. **Assistant panel** (`src/assistant.rs`) — right-dock conversation UI with
   active-file/selection context, background job execution, *Insert code block*
   into the buffer, provider abstraction with a dependency-free **custom
   command** provider (configurable template `{prompt} {file} {selection}
   {language}`) so any local CLI model works.
7. **CI** (`.github/workflows/ci.yml`) — fmt + clippy + check + full test suite
   on every push, used to verify this work.

## 3. Later phases (roadmap, each an independent slice)

- **Phase 2:** more tree-sitter grammars (needs allowlisted deps), LS
  auto-install, snippet tab-stops (policy ask), per-language settings.
- **Phase 3:** multibuffers (project-search-as-buffer), notification center,
  pane zoom + tab DnD, git hunk staging UI, SSH remote editing,
  DAP debugging UI, extension marketplace (signed).
- **Phase 4:** edit predictions & inline assist (needs model backend),
  collaboration (relay), notebooks/REPL, dev containers.

## 4. Guardrails honored

No new dependencies were added (repo dependency-allowlist test untouched); the
LSP synchronization strategy, completion trigger policy, snippet policy, and the
stateless editor-widget architecture are unchanged — vim mode is an additive
key-interception layer behind a default-off setting.
