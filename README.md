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
  operators, text objects, `/` search, and a `:` command line. Toggle with
  Ctrl+Alt+V or `editor.vim_mode` in settings (default off).
- **Theme pack + picker** — One Dark/Light, Ayu, Gruvbox, Catppuccin, Nord,
  Dracula, Solarized and more; live-preview picker on Ctrl+Alt+T.
- **AI assistant panel** — Ctrl+Alt+A; pluggable provider via a shell command
  template (works with `ollama`, `llm`, or any CLI model).
- **Auto save** — off / after delay / on focus change.
- **Inline diagnostics** — Zed-style end-of-line diagnostic messages.
- **File-type icons** — in the project tree, tabs, and quick open.
- Plus: LSP (completion, hover, goto, signature help, code actions, inlay
  hints, semantic tokens), project search/replace, git suite (blame, stash,
  tags, conflicts, log), split terminals with sessions, tasks, Lua plugins,
  DAP foundation, editorconfig, zen mode, and accessibility features.
