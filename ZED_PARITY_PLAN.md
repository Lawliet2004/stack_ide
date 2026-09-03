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
| **Vim / modal editing** | ✅ | `vim.rs` — modal state machine, motions/operators/text objects, `/` `?`, viewport scrolls, ex line. |
| **Multibuffers** (excerpts of many files in one buffer; used by project search results) | ❌ | Roadmap Phase 3. |
| Edit predictions / inline ghost-text completions | ❌ | Requires a model backend; roadmap Phase 4. |
| Auto save (off / after delay / on focus change) | ✅ | `app.rs` `poll_auto_save`; LSP `didSave` off pending approval. |

### B. Appearance & UI polish

| Zed capability | Status | Notes |
|---|---|---|
| Theme system with live-preview theme selector | ✅ | Built-ins include Default Dark/Light, One Dark/Light, Ayu, Gruvbox, Catppuccin; picker has unique display names. |
| Per-filetype icons in project panel & tabs (icon themes) | 🟡 | Tree has color tints only. Roadmap. |
| Zen mode / distraction free | ✅ | `zen_mode.rs` |
| Inline diagnostics (message rendered at end of line) | ✅ | Square/underline + end-of-line message; editor clips to visible width. |
| Notification center (dismissable, typed) | 🟡 | Plugin toasts only; roadmap Phase 3. |
| Pane zoom, drag tabs between panes | 🟡 | Split/focus exists (`panes/`); zoom & DnD roadmap Phase 3. |
| Welcome screen, recent projects | ✅ | `app.rs` recent workspaces / recent files. |
| Settings UI (live-preview) | ✅ | `app.rs` settings modal |
| Keymap customization (TOML) | ✅ | Command specs in `app.rs`; legacy `src/config/keybinds.rs` removed. |

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
cherry-pick, diff-vs-HEAD viewer, status coloring, **per-hunk stage/unstage**,
and **commit amend** — ✅ broad parity (`git/`). Remaining polish: side-by-side
diff view, sources/synchronized scrolling, intraline highlighting.

### E. Terminal & processes

PTY terminals with splits, search, links, env editor, history — ✅ (`terminal/`).

### F. Collaboration (Zed channels, calls, follow mode, channel notes)

❌ Server-product feature. Out of scope for a local IDE; a future **LAN/relay
collab** slice is listed in Phase 4 of the roadmap, non-goal for now.

### G. AI

| Zed capability | Status | Notes |
|---|---|---|
| Assistant panel (conversation, context of file/selection, insert code) | ✅ | `assistant.rs` — provider-pluggable panel with a dependency-free custom-command provider (`ollama`, `llm`, CLI), context chips, insert/copy. |
| Inline assist (prompt-to-edit in buffer) | ❌ | Phase 4. |
| Edit predictions (Copilot-style) | ❌ | Phase 4. |

### H. Extensibility

Lua plugin sandbox with menus/commands/notifications ✅ (`plugins/`).
Zed uses WASM extensions + signed marketplace — roadmap Phase 3/4 (marketplace
with signature verification is a standalone security-sensitive slice).

### I. Remote development (SSH projects, dev containers)

🟡 `DocumentUri`/`remote.rs` foundation + per-root trust exist; SSH transport,
browse, conflict-safe save, devcontainers — roadmap (needs dependency approval
for real SFTP).

### J. Debugger / profiler

🟡 DAP client (`debug.rs`) wired into the bottom **Debug Console** panel:
start/stop, continue/pause/step, breakpoint toggle at the active cursor,
call-stack selection, scopes/variables, output. Remaining: gutter breakpoint
rendering, richer adapter config forms. Rust profiler plumbing is trust-gated.

### K. Accessibility

Screen-reader annotations, high contrast, keyboard nav, RTL, ligatures — ✅
stronger than Zed's baseline today.

---

## 2. Phase 1 — implemented (landed on `arena/01a060ca-stack-ide`, CI green)

1. **Vim mode** (`src/vim.rs`) — Normal/Insert/Visual/Visual-Line, counts,
   motions (`h j k l w W b B e 0 ^ $ gg G f F t T % { } n N ; ,`), operators
   (`d c y > < gu gU` with motions and doubling), text objects (`iw aw i" a"
   i' a' i( a( i{ a{ i[ a[`), `x X s S D C o O p P r J u Ctrl+r ~`, visual ops
   (`d x y c p ~ u U > < o J`), `/` search with `n`/`N` (mirrored into the
   search panel for highlighting), `:` ex line (`:w :q :q! :wq :x :noh`), block
   cursor in normal/visual, bar cursor in insert, cmdline overlay, status-bar
   mode badge, persisted `editor.vim_mode` (default **off**), Ctrl+Alt+V toggle.
2. **Theme pack + theme selector** — One Dark & One Light (Zed's defaults), Ayu
   Dark/Mirage/Light, Gruvbox Dark/Light, Catppuccin Mocha/Latte added to the
   existing five; live-preview theme picker (Ctrl+Alt+T, also View menu and
   palette) with type-to-filter, hover preview, Enter commit, Esc revert.
3. **File-type icons** (`src/file_icons.rs`) — painted brand-colored monogram
   badges for 27 file kinds in the project tree, editor tabs, and quick-open.
4. **Auto save** — `off | after_delay | focus_change` with configurable delay,
   routed through `write_buffer_to_disk` so git status stays correct. LSP
   `didSave` remains deliberately off pending explicit approval.
5. **Inline diagnostics** — severity-colored end-of-line messages (Zed-style),
   `editor.inline_diagnostics` setting.
6. **Assistant panel** (`src/assistant.rs`) — right-dock conversation UI with
   active-file/selection context chips, background worker, *Insert at cursor* /
   *Copy* actions on fenced code blocks, dependency-free **custom command**
   provider (`assistant.command` template with `{prompt} {file} {selection}
   {language}`; prompt piped to stdin when `{prompt}` is absent), status dot,
   Ctrl+Alt+A toggle, settings UI section.
7. **CI** (`.github/workflows/ci.yml`) — check + full lib suite + integration
   guards + clippy on every push; failure names surfaced as GitHub annotations.

All of it verified by CI on GitHub Actions (compile + ~470 lib tests +
integration guards passing).

## 3. Later phases (roadmap, each an independent slice)

- **Phase 2:** more tree-sitter grammars (needs allowlisted deps), LS
  auto-install, snippet tab-stops (policy ask), per-language settings.
- **Phase 3:** multibuffers (project-search-as-buffer), notification center,
  pane zoom + tab DnD, SSH remote editing, signed extension marketplace.
- **Phase 4:** edit predictions & inline assist (needs model backend),
  collaboration (relay), notebooks/REPL, dev containers.

Already landed on this branch: git per-hunk stage/unstage + commit amend, and
the DAP debugger bottom-panel UX. They are reflected in the gap-analysis tables
above.

## 4. Guardrails honored

No new dependencies were added (repo dependency-allowlist test untouched); the
LSP synchronization strategy, completion trigger policy, snippet policy, and the
stateless editor-widget architecture are unchanged — vim mode is an additive
key-interception layer behind a default-off setting.
