//! AI assistant panel — Zed-style conversation dock with a pluggable provider.
//!
//! Ships a dependency-free **custom command** provider: the user configures a
//! shell command template (`settings.assistant.command`, e.g.
//! `ollama run llama3.1` or any OpenAI-compatible CLI) and the panel pipes the
//! assembled prompt — with optional active-file/selection context — through
//! it on a background thread, streaming output into the conversation.
//!
//! Design notes:
//! - Runs the provider on a worker thread; the UI thread only polls a
//!   [`crossbeam_channel`] receiver (same non-blocking pattern as the LSP client).
//! - No command runs until the user sends a message, and the command comes
//!   exclusively from the user's own settings file.
//! - Pure helpers (prompt assembly, placeholder substitution, code-block
//!   extraction) are unit-tested without egui.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, TryRecvError};
use egui::{RichText, ScrollArea, TextEdit};

use crate::theme::SemanticPalette;

/// Maximum characters buffered from a provider stream (guards against
/// runaway output filling memory).
const MAX_STREAM_CHARS: usize = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    fn label(self) -> &'static str {
        match self {
            Role::User => "You",
            Role::Assistant => "Assistant",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    /// When the message was appended.
    pub at: Instant,
}

/// Context about the active editor, supplied by the app each frame.
#[derive(Debug, Clone, Default)]
pub struct EditorContext {
    pub file_path: Option<PathBuf>,
    pub language: Option<String>,
    pub file_text: Option<String>,
    pub selection: Option<String>,
}

/// Actions the panel asks the app shell to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssistantEvent {
    /// Insert a code block into the active buffer at the caret.
    InsertCode(String),
    /// Copy the given text (app owns the clipboard).
    Copy(String),
}

enum ProviderEvent {
    Chunk(String),
    Done(String),
    Failed(String),
}

/// The AI assistant side-dock panel.
#[derive(Default)]
pub struct AssistantPanel {
    /// Panel visibility (toggled with Ctrl+Alt+A).
    pub open: bool,
    /// Panel width, persisted across the session in settings.
    pub width: f32,
    messages: Vec<ChatMessage>,
    draft: String,
    include_file: bool,
    include_selection: bool,
    busy: bool,
    /// Partial assistant output for the in-flight request.
    streaming: String,
    output_rx: Option<Receiver<ProviderEvent>>,
    /// Set when the user clears/cancels the in-flight request; the worker
    /// checks it between output reads and kills its child process.
    cancel: Arc<AtomicBool>,
    error: Option<String>,
    scroll_to_bottom: bool,
    request_focus_input: bool,
}

impl AssistantPanel {
    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// Conversation transcript (for tests and the status bar).
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Clear the conversation, cancelling any in-flight provider request.
    pub fn clear(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.busy = false;
        self.messages.clear();
        self.streaming.clear();
        self.output_rx = None;
        self.error = None;
    }

    /// Append a user message and start the configured provider, if any.
    /// Returns `Err(reason)` when nothing is configured.
    pub fn send(
        &mut self,
        settings_command: &str,
        context: &EditorContext,
    ) -> Result<(), String> {
        if self.busy {
            return Err("Assistant is still responding".to_owned());
        }
        let prompt = self.draft.trim().to_owned();
        if prompt.is_empty() {
            return Err("Type a question first".to_owned());
        }
        let command = settings_command.trim();
        if command.is_empty() {
            return Err(
                "No assistant provider configured — set `assistant.command` in Settings"
                    .to_owned(),
            );
        }
        let rendered = if cfg!(target_os = "windows") {
            render_command_windows(
                command,
                &prompt,
                context,
                self.include_file,
                self.include_selection,
            )
        } else {
            render_command(
                command,
                &prompt,
                context,
                self.include_file,
                self.include_selection,
            )
        };
        self.messages.push(ChatMessage {
            role: Role::User,
            content: prompt,
            at: Instant::now(),
        });
        self.draft.clear();
        self.error = None;
        self.streaming.clear();
        self.busy = true;
        self.cancel.store(false, Ordering::SeqCst);

        let (tx, rx) = crossbeam_channel::bounded::<ProviderEvent>(64);
        self.output_rx = Some(rx);
        let cancel_worker = self.cancel.clone();
        let spawned_worker = std::thread::Builder::new()
            .name("assistant-provider".to_owned())
            .spawn(move || {
                let shell = if cfg!(target_os = "windows") {
                    ("cmd".to_owned(), vec!["/C".to_owned()])
                } else {
                    ("sh".to_owned(), vec!["-c".to_owned()])
                };
                let mut cmd = std::process::Command::new(&shell.0);
                cmd.args(&shell.1).arg(&rendered.command);
                cmd.stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());
                let spawned = cmd.spawn();
                let mut child = match spawned {
                    Ok(child) => child,
                    Err(error) => {
                        let _ = tx.send(ProviderEvent::Failed(format!(
                            "Failed to start provider: {error}"
                        )));
                        return;
                    }
                };
                // Feed the full prompt over stdin so commands that read stdin
                // (e.g. `llm`, `aichat`) receive it verbatim.
                if let Some(stdin) = child.stdin.as_mut() {
                    use std::io::Write;
                    let _ = stdin.write_all(rendered.stdin_prompt.as_bytes());
                }
                child.stdin = None;

                // Stream stdout incrementally. stderr is captured so that a
                // provider that only writes to stderr still surfaces something.
                use std::io::{BufRead, BufReader, Read};
                let Some(stdout_stream) = child.stdout.take() else {
                    let _ = child.kill();
                    let _ = tx.send(ProviderEvent::Failed(
                        "Provider did not expose stdout".to_owned(),
                    ));
                    return;
                };
                let mut stdout = BufReader::new(stdout_stream);
                let mut stderr = String::new();
                let mut streamed = String::new();
                loop {
                    if cancel_worker.load(Ordering::SeqCst) {
                        let _ = child.kill();
                        let _ = tx.send(ProviderEvent::Failed("Cancelled".to_owned()));
                        break;
                    }
                    let mut line = Vec::new();
                    match stdout.read_until(b'\n', &mut line) {
                        Ok(0) => break,
                        Ok(_) => {
                            let text = String::from_utf8_lossy(&line).into_owned();
                            streamed.push_str(&text);
                            if streamed.chars().count() > MAX_STREAM_CHARS {
                                streamed = streamed
                                    .chars()
                                    .take(MAX_STREAM_CHARS)
                                    .collect();
                            }
                            let _ = tx.send(ProviderEvent::Chunk(text));
                        }
                        Err(_) => break,
                    }
                }
                let _ = child
                    .stderr
                    .take()
                    .map(|mut s| s.read_to_string(&mut stderr));
                match child.wait() {
                    Ok(status) => {
                        if streamed.trim().is_empty() && !stderr.trim().is_empty() {
                            streamed = stderr;
                        }
                        if status.success() || !streamed.trim().is_empty() {
                            let _ = tx.send(ProviderEvent::Done(streamed));
                        } else {
                            let _ = tx.send(ProviderEvent::Failed(format!(
                                "Provider exited with {status}"
                            )));
                        }
                    }
                    Err(error) => {
                        let _ =
                            tx.send(ProviderEvent::Failed(format!("Provider failed: {error}")));
                    }
                }
            });
        match spawned_worker {
            Ok(_handle) => Ok(()),
            Err(error) => {
                self.busy = false;
                self.output_rx = None;
                Err(format!("Failed to spawn assistant worker: {error}"))
            }
        }
    }

    /// Drain completed provider output; call once per frame.
    pub fn poll(&mut self) {
        let mut close = false;
        let Some(rx) = self.output_rx.as_mut() else {
            return;
        };
        loop {
            match rx.try_recv() {
                Ok(ProviderEvent::Chunk(chunk)) => {
                    if self.streaming.chars().count() < MAX_STREAM_CHARS {
                        self.streaming.push_str(&chunk);
                    }
                }
                Ok(ProviderEvent::Done(text)) => {
                    if !text.trim().is_empty() {
                        self.streaming.push_str(&text);
                    }
                    let content = std::mem::take(&mut self.streaming);
                    self.messages.push(ChatMessage {
                        role: Role::Assistant,
                        content,
                        at: Instant::now(),
                    });
                    self.busy = false;
                    self.scroll_to_bottom = true;
                    close = true;
                    break;
                }
                Ok(ProviderEvent::Failed(message)) => {
                    let mut content = std::mem::take(&mut self.streaming);
                    if !content.trim().is_empty() {
                        self.messages.push(ChatMessage {
                            role: Role::Assistant,
                            content,
                            at: Instant::now(),
                        });
                    } else {
                        content = message;
                        self.messages.push(ChatMessage {
                            role: Role::Assistant,
                            content,
                            at: Instant::now(),
                        });
                    }
                    self.busy = false;
                    self.scroll_to_bottom = true;
                    close = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    close = true;
                    if self.busy {
                        // Worker exited without a terminal event (should not
                        // happen, but never wedge the panel).
                        self.busy = false;
                    }
                    break;
                }
            }
        }
        if close {
            self.output_rx = None;
        }
    }

    /// Render the panel into a right dock `ui`. Returns the first action the
    /// user requested this frame.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        palette: &SemanticPalette,
        settings_command: &str,
        context: &EditorContext,
    ) -> Option<AssistantEvent> {
        self.poll();
        let mut event = None;
        ui.set_min_width(ui.available_width());

        // ── Header ────────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            let status = if self.busy {
                (palette.warning, "●")
            } else if settings_command.trim().is_empty() {
                (palette.muted_text, "○")
            } else {
                (palette.success, "●")
            };
            ui.label(
                RichText::new("Assistant")
                    .strong()
                    .color(palette.primary_text),
            );
            ui.label(RichText::new(status.1).color(status.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(RichText::new("Clear").size(11.0)))
                    .clicked()
                {
                    self.clear();
                }
            });
        });
        ui.separator();

        // ── Transcript ────────────────────────────────────────────────────
        ScrollArea::vertical()
            .id_source("assistant_transcript")
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if self.messages.is_empty() && self.streaming.is_empty() {
                    ui.add_space(12.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("Ask about the open file, a selection, or anything else.")
                                .color(palette.muted_text),
                        );
                        if settings_command.trim().is_empty() {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(
                                    "No provider configured.\nSet Preferences → AI Assistant\n\
                                     to a command such as `ollama run codellama`.",
                                )
                                .color(palette.muted_text)
                                .size(11.0),
                            );
                        }
                    });
                    ui.add_space(12.0);
                }
                for message in &self.messages {
                    render_message(ui, palette, message, &mut event);
                }
                if self.busy && !self.streaming.is_empty() {
                    render_message(
                        ui,
                        palette,
                        &ChatMessage {
                            role: Role::Assistant,
                            content: self.streaming.clone(),
                            at: Instant::now(),
                        },
                        &mut event,
                    );
                }
                if self.busy && self.streaming.is_empty() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new("Thinking…").color(palette.muted_text).italics(),
                        );
                    });
                }
            });

        // ── Error ─────────────────────────────────────────────────────────
        if let Some(error) = &self.error {
            ui.label(RichText::new(error).color(palette.error).size(11.0));
        }

        ui.separator();

        // ── Context chips ─────────────────────────────────────────────────
        ui.horizontal_wrapped(|ui| {
            ui.toggle_value(&mut self.include_file, RichText::new("File").size(11.0));
            ui.toggle_value(&mut self.include_selection, RichText::new("Selection").size(11.0));
            let file_label = context
                .file_path
                .as_ref()
                .map(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string())
                })
                .unwrap_or_else(|| "no file".to_owned());
            ui.label(
                RichText::new(file_label)
                    .color(palette.muted_text)
                    .size(10.0),
            );
        });

        // ── Input ─────────────────────────────────────────────────────────
        let input = ui.add_sized(
            [ui.available_width(), 64.0],
            TextEdit::multiline(&mut self.draft)
                .id(egui::Id::new("assistant_draft"))
                .hint_text("Ask anything…  (Enter to send, Shift+Enter for newline)")
                .desired_rows(3),
        );
        if self.request_focus_input {
            input.request_focus();
            self.request_focus_input = false;
        }
        let send_clicked = ui
            .add_enabled(
                !self.busy && !self.draft.trim().is_empty(),
                egui::Button::new(RichText::new("Send").strong()),
            )
            .clicked();
        let enter_sent = input.lost_focus()
            && ui.input(|input| input.key_pressed(egui::Key::Enter))
            && !ui.input(|input| input.modifiers.shift);
        if send_clicked || enter_sent {
            if let Err(message) = self.send(settings_command, context) {
                self.error = Some(message);
            }
        }

        if self.scroll_to_bottom {
            self.scroll_to_bottom = false;
        }
        event
    }
}

fn render_message(
    ui: &mut egui::Ui,
    palette: &SemanticPalette,
    message: &ChatMessage,
    event: &mut Option<AssistantEvent>,
) {
    let (role_color, body_color) = match message.role {
        Role::User => (palette.accent, palette.primary_text),
        Role::Assistant => (palette.success, palette.primary_text),
    };
    ui.add_space(4.0);
    ui.label(RichText::new(message.role.label()).strong().color(role_color).size(11.0));
    // Render fenced code blocks separately with Insert/Copy actions.
    for segment in split_code_blocks(&message.content) {
        match segment {
            Segment::Text(text) => {
                ui.label(RichText::new(text.trim()).color(body_color));
            }
            Segment::Code(code) => {
                egui::Frame::group(ui.style())
                    .fill(palette.editor_background)
                    .show(ui, |ui| {
                        let mut code_text = code.clone();
                        ui.add(
                            TextEdit::multiline(&mut code_text)
                                .id(egui::Id::new("assistant_code_block"))
                                .font(egui::TextStyle::Monospace)
                                .desired_rows(2)
                                .desired_width(ui.available_width().max(120.0))
                                .code_editor(),
                        );
                        ui.horizontal(|ui| {
                            if ui
                                .add(egui::Button::new(
                                    RichText::new("Insert at cursor").size(10.0),
                                ))
                                .clicked()
                            {
                                *event = Some(AssistantEvent::InsertCode(code.clone()));
                            }
                            if ui
                                .add(egui::Button::new(RichText::new("Copy").size(10.0)))
                                .clicked()
                            {
                                *event = Some(AssistantEvent::Copy(code.clone()));
                            }
                        });
                    });
            }
        }
        ui.add_space(2.0);
    }
    ui.add_space(4.0);
}

enum Segment {
    Text(String),
    Code(String),
}

/// Split `text` into prose and fenced code segments (``` fences).
fn split_code_blocks(text: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("```") {
        let before = &rest[..start];
        if !before.trim().is_empty() {
            segments.push(Segment::Text(before.to_owned()));
        }
        let after = &rest[start + 3..];
        // Skip an optional language tag on the same line, then capture to the
        // closing fence (or end of text for an unterminated block).
        let body = match after.find('\n') {
            Some(newline) => {
                let after_first_line = &after[newline + 1..];
                match after_first_line.find("```") {
                    Some(end) => {
                        let code = &after_first_line[..end];
                        rest = &after_first_line[end + 3..];
                        code
                    }
                    None => {
                        rest = "";
                        after_first_line
                    }
                }
            }
            None => {
                rest = "";
                after
            }
        };
        segments.push(Segment::Code(body.trim_end().to_owned()));
    }
    // Preserve trailing prose after the last fence (and text-only messages).
    if !rest.trim().is_empty() {
        let trailing = rest.to_owned();
        if !trailing.trim().is_empty() {
            segments.push(Segment::Text(trailing));
        }
    }
    segments
}

/// Result of rendering a command template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedCommand {
    /// The shell command line to spawn.
    pub command: String,
    /// Text to write to the process stdin (may be empty).
    pub stdin_prompt: String,
}

/// Render the user's command template with prompt/context placeholders.
///
/// Placeholders: `{prompt}`, `{file}`, `{selection}`, `{language}`. When the
/// template does not contain `{prompt}`, the prompt is additionally written to
/// stdin so piped-style CLIs work unchanged.
pub fn render_command(
    template: &str,
    prompt: &str,
    context: &EditorContext,
    include_file: bool,
    include_selection: bool,
) -> RenderedCommand {
    render_command_with_quote(template, prompt, context, include_file, include_selection, shell_quote)
}

/// Windows variant used by the worker when spawning `cmd /C`.
pub fn render_command_windows(
    template: &str,
    prompt: &str,
    context: &EditorContext,
    include_file: bool,
    include_selection: bool,
) -> RenderedCommand {
    render_command_with_quote(template, prompt, context, include_file, include_selection, cmd_quote)
}

fn render_command_with_quote(
    template: &str,
    prompt: &str,
    context: &EditorContext,
    include_file: bool,
    include_selection: bool,
    quote: fn(&str) -> String,
) -> RenderedCommand {
    let file_name = context
        .file_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let language = context.language.clone().unwrap_or_default();
    let mut context_block = String::new();
    if include_file {
        if let Some(text) = &context.file_text {
            context_block.push_str(&format!(
                "\n\nCurrent file `{}`:\n```{}\n{}\n```",
                file_name, language, text
            ));
        }
    }
    if include_selection {
        if let Some(selection) = &context.selection {
            context_block.push_str(&format!(
                "\n\nSelected text:\n```\n{}\n```",
                selection
            ));
        }
    }
    let full_prompt = format!("{prompt}{context_block}");
    let quoted = |value: &str| quote(value);
    let substitute = |template: &str, prompt_value: &str| {
        template
            .replace("'{prompt}'", &quoted(prompt_value))
            .replace("{prompt}", &quote(prompt_value))
            .replace("'{file}'", &quoted(&file_name))
            .replace("{file}", &quote(&file_name))
            .replace(
                "'{selection}'",
                &quoted(context.selection.as_deref().unwrap_or("")),
            )
            .replace(
                "{selection}",
                &quote(context.selection.as_deref().unwrap_or("")),
            )
            .replace("{language}", &language)
    };
    let command = substitute(template, &full_prompt);
    if template.contains("{prompt}") {
        RenderedCommand {
            command,
            stdin_prompt: String::new(),
        }
    } else {
        RenderedCommand {
            command,
            stdin_prompt: full_prompt,
        }
    }
}

/// Single-argument POSIX shell quoting (used with `sh -c`).
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Best-effort `cmd /C` quoting: wrap in double quotes and double any interior
/// double quote (the common escaping convention for a single cmd argument).
fn cmd_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// Extract the first fenced code block (used by tests and future inline assist).
pub fn first_code_block(text: &str) -> Option<String> {
    split_code_blocks(text).into_iter().find_map(|segment| match segment {
        Segment::Code(code) => Some(code),
        Segment::Text(_) => None,
    })
}

/// Context bundle for the active buffer, built by the app shell.
pub fn editor_context(
    path: Option<&Path>,
    language: Option<&str>,
    file_text: Option<String>,
    selection: Option<String>,
) -> EditorContext {
    EditorContext {
        file_path: path.map(Path::to_path_buf),
        language: language.map(str::to_owned),
        file_text,
        selection,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> EditorContext {
        EditorContext {
            file_path: Some(PathBuf::from("/tmp/main.rs")),
            language: Some("rust".to_owned()),
            file_text: Some("fn main() {}".to_owned()),
            selection: Some("let x = 1;".to_owned()),
        }
    }

    #[test]
    fn prompt_placeholder_is_shell_quoted() {
        let rendered = render_command("llm '{prompt}'", "what is this?", &ctx(), false, false);
        assert_eq!(rendered.command, "llm 'what is this?'");
        assert!(rendered.stdin_prompt.is_empty());
    }

    #[test]
    fn missing_prompt_placeholder_falls_back_to_stdin() {
        let rendered = render_command("ollama run llama3.1", "hi", &ctx(), false, false);
        assert_eq!(rendered.command, "ollama run llama3.1");
        assert_eq!(rendered.stdin_prompt, "hi");
    }

    #[test]
    fn file_and_selection_context_are_appended() {
        let rendered =
            render_command("cat | llm", "explain", &ctx(), true, true);
        assert!(rendered.stdin_prompt.contains("Current file `/tmp/main.rs`"));
        assert!(rendered.stdin_prompt.contains("fn main() {}"));
        assert!(rendered.stdin_prompt.contains("Selected text:"));
        assert!(rendered.stdin_prompt.contains("let x = 1;"));
        assert!(rendered.stdin_prompt.starts_with("explain"));
    }

    #[test]
    fn quotes_in_prompt_do_not_break_the_command() {
        let rendered = render_command("llm '{prompt}'", "it's a test", &EditorContext::default(), false, false);
        assert_eq!(rendered.command, "llm 'it'\\''s a test'");
    }

    #[test]
    fn windows_command_uses_double_quote_escaping() {
        let rendered = render_command_windows(
            "llm \"{prompt}\"",
            "say \"hi\"",
            &EditorContext::default(),
            false,
            false,
        );
        assert_eq!(rendered.command, "llm \"say \"\"hi\"\"\"");
    }

    #[test]
    fn clear_stops_and_cancels_the_in_flight_request() {
        let mut panel = AssistantPanel::default();
        panel.draft = "hi".to_owned();
        let _ = panel.send("echo hi", &EditorContext::default());
        assert!(panel.is_busy());

        panel.clear();

        assert!(!panel.is_busy(), "clear must immediately unbusy the panel");
        assert!(panel.output_rx.is_none());
        assert!(panel.streaming.is_empty());
        assert!(
            panel.cancel.load(Ordering::Relaxed),
            "clear must set the worker cancellation flag"
        );
    }

    #[test]
    fn code_blocks_are_split_and_extracted() {
        let text = "Here you go:\n```rust\nfn a() {}\nfn b() {}\n```\nDone.";
        let segments = split_code_blocks(text);
        assert_eq!(segments.len(), 3);
        assert!(matches!(&segments[0], Segment::Text(t) if t.trim() == "Here you go:"));
        assert!(matches!(&segments[1], Segment::Code(c) if c.trim() == "fn a() {}\nfn b() {}"));
        assert!(matches!(&segments[2], Segment::Text(t) if t.trim() == "Done."));
        assert_eq!(first_code_block(text).as_deref(), Some("fn a() {}\nfn b() {}"));
    }

    #[test]
    fn trailing_prose_after_last_fence_is_preserved() {
        let text = "```rust\nfn a() {}\n```\nHope this helps.";
        let segments = split_code_blocks(text);
        assert_eq!(segments.len(), 2);
        assert!(matches!(&segments[1], Segment::Text(t) if t.trim() == "Hope this helps."));
    }

    #[test]
    fn plain_text_message_renders_as_text() {
        let text = "Just a normal reply with no code.";
        let segments = split_code_blocks(text);
        assert_eq!(segments.len(), 1);
        assert!(matches!(&segments[0], Segment::Text(t) if t.trim() == text));
    }

    #[test]
    fn unterminated_code_block_is_captured_to_end() {
        let text = "```py\nprint(1)";
        assert_eq!(first_code_block(text).as_deref(), Some("print(1)"));
    }
}
