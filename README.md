# Stack IDE

Stack IDE (Blue IDE) is a Rust desktop code editor built with `eframe` and `egui`.

## Version

Version 1 of this repository is published as package version `0.1.0`.

## Development

```powershell
cargo check
cargo test
cargo run
```

Build artifacts, local agent logs, and generated analysis output are intentionally excluded from Git.

## Zed-parity feature highlights

See [`ZED_PARITY_PLAN.md`](ZED_PARITY_PLAN.md) for the full gap analysis and roadmap.

- **Vim mode** — modal editing (normal/insert/visual/visual-line) with motions,
  operators, text objects, `/`/`?` search, `zz/zt/zb` viewport scrolling, and
  a `:` command line. Toggle with Ctrl+Alt+V or `editor.vim_mode` in settings
  (default off).
- **Theme pack + picker** — One Dark/Light, Ayu, Gruvbox, Catppuccin, Nord,
  Dracula, Solarized and more; live-preview picker on Ctrl+Alt+T.
- **AI assistant panel** — Ctrl+Alt+A; pluggable provider via a shell command
  template (works with `ollama`, `llm`, or any CLI model) with incremental
  streaming, insert-at-cursor/copy for code blocks, and cancel-on-clear.
- **Auto save** — off / after delay / on focus change.
- **Inline diagnostics** — Zed-style end-of-line diagnostic messages.
- **File-type icons** — in the project tree, tabs, and quick open.
- Plus: LSP (completion, hover, goto, signature help, code actions, inlay
  hints, semantic tokens), project search/replace, git suite (blame, stash,
  tags, conflicts, log), split terminals with sessions, tasks, Lua plugins,
  editorconfig, zen mode, and accessibility features.

## Known limitations / intentionally deferred

- LSP `textDocument/didSave` remains **off** (pending explicit approval); auto
  save still updates git status and dirty markers.
- Remote/SSH editing is a foundational stub (`src/remote.rs`), not a full SFTP
  transport.
- Signed extension marketplace, multibuffers, notebooks/REPL, devcontainers,
  snippet tab-stops, per-hunk git staging, and the full DAP debugger UI are
  roadmap slices, not yet implemented.
- Executable capabilities (terminals, profiler, tasks, plugins, LSP server
  spawn) are gated by workspace trust; Git remote operations still use `git2`
  and are not trust-gated.
- `cargo fmt --check` currently surfaces pre-existing formatting drift; CI runs
  it non-blocking until a formatting pass lands.
