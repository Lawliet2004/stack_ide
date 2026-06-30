# Blue IDE Plugins

Place Lua scripts in this directory to extend Blue IDE with custom functionality.

Each `.lua` file is loaded as an independent plugin. Plugins run in isolated Lua states and cannot access each other's state directly.

## Plugin API

Every plugin has access to a global `blue` table with the following functions:

### Buffer Access

- `blue.current_file()` → `string` | `nil`
  - Returns the absolute path of the active file, or `nil` if no file is open.

- `blue.get_text()` → `string`
  - Returns the full content of the active buffer.

- `blue.get_line(n: number)` → `string` | `nil`
  - Returns the text of line `n` (1-indexed). Returns `nil` if out of range.

- `blue.get_cursor()` → `(number, number)`
  - Returns the current cursor position as `(line, col)`, both 1-indexed.

- `blue.set_cursor(line: number, col: number)`
  - Moves the cursor to the given position. Clamps silently to valid range.

- `blue.insert_text(text: string)`
  - Inserts text at the current cursor position. Moves cursor to end of insertion.

- `blue.replace_text(new_text: string)`
  - Replaces the entire buffer content with `new_text`. Marks buffer as modified.

- `blue.get_selection()` → `string` | `nil`
  - Returns selected text if any, `nil` if nothing selected.

- `blue.set_selection(start_line, start_col, end_line, end_col)`
  - Sets the selection range. All values are 1-indexed.

### File Operations

- `blue.open_file(path: string)`
  - Opens the file at `path` in the focused pane.

- `blue.save_file()`
  - Saves the active buffer to disk.

- `blue.get_workspace_root()` → `string` | `nil`
  - Returns the folder opened via File > Open Folder, or `nil` if none.

- `blue.read_file(path: string)` → `string` | `nil`
  - Reads the contents of the file at `path` and returns it as a string.
  - Returns `nil` if the file does not exist, cannot be read, or is outside the workspace root.
  - Restricted to the workspace root and its subdirectories.

- `blue.list_files(dir: string)` → `table`
  - Lists files in `dir` (non-recursive). Returns an array of absolute paths.
  - Restricted to the workspace root and its subdirectories.

### UI

- `blue.notify(message: string, level: string)`
  - Shows a notification in the status bar.
  - `level` is `"info"`, `"warning"`, or `"error"`.
  - Notifications fade after 4 seconds.

- `blue.show_input_dialog(prompt: string)` → `string` | `nil`
  - Shows a modal text input dialog.
  - Returns entered text or `nil` if cancelled.

- `blue.show_menu_item(label: string, callback_name: string)`
  - Adds an item to the Plugins menu.
  - When clicked, calls the Lua function named `callback_name`.

### Terminal

- `blue.run_command(cmd: string)` → `string`
  - Runs `cmd` in a shell (not in the IDE terminal panel).
  - Returns stdout as a string. Blocks until completion (max 10 seconds).
  - Timeout returns an empty string.

### Editor State

- `blue.get_language()` → `string`
  - Returns the language of the active file (`"rust"`, `"python"`, etc.).

- `blue.get_diagnostics()` → `table`
  - Returns an array of diagnostic tables with fields: `{ line, col, severity, message }`.

### Event Hooks

- `blue.on_save(fn)`
  - Registers `fn` to be called whenever any file is saved.
  - `fn(path: string)` receives the file path.

- `blue.on_open(fn)`
  - Registers `fn` to be called whenever a file is opened.
  - `fn(path: string)` receives the file path.

- `blue.on_cursor_move(fn)`
  - Registers `fn` to be called when the cursor moves.
  - `fn(line: number, col: number)` receives cursor coordinates (1-indexed).
  - Rate-limited: fires at most once per 200ms.

- `blue.on_text_change(fn)`
  - Registers `fn` to be called when buffer content changes.
  - `fn(path: string)` receives the file path.
  - Rate-limited: fires at most once per 500ms.

## Sandbox Restrictions

For security, the following Lua standard libraries are removed:
- `io` (use `blue.*` API instead)
- `os` (use `blue.run_command` instead)
- `package` (no arbitrary module loading)
- `debug` (introspection disabled)

File access is restricted to the workspace root. Attempts to escape the workspace will be logged and denied.

Each plugin is limited to 1,000,000 Lua instructions per call. Infinite loops will be interrupted and the error logged.

## Example: Word Count Plugin

Create `word_count.lua`:

```lua
blue.show_menu_item("Word Count", "show_word_count")

function show_word_count()
    local text = blue.get_text()
    if text == nil then
        blue.notify("No file open", "warning")
        return
    end
    
    local count = 0
    for word in text:gmatch("%S+") do
        count = count + 1
    end
    
    blue.notify("Word count: " .. count, "info")
end
```

Then click Plugins > Word Count to see the word count in a notification.

## Reloading Plugins

Press `Ctrl+Shift+R` to reload all plugins. Previously broken plugins get another chance to load.

## Tips

- Keep plugins fast. The IDE will pause while plugins execute.
- Use `blue.notify()` for user feedback.
- Check `blue.current_file()` before accessing buffer content.
- Log to stderr with `print()` — output is prefixed with your plugin name.
