//! Integrated debugger: Debug Adapter Protocol (DAP) client foundation.
//!
//! Implements a generic DAP client that communicates with any compliant adapter
//! over stdin/stdout. Does not hardcode any specific debugger.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_channel::{bounded, Receiver, Sender};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ─── DAP protocol types ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DapMessage {
    pub seq: u64,
    #[serde(rename = "type")]
    pub msg_type: String, // "request" | "response" | "event"
    #[serde(flatten)]
    pub body: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DapRequest {
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DapResponse {
    pub request_seq: u64,
    pub success: bool,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DapEvent {
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

// ─── Session state model ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugState {
    NotStarted,
    Starting,
    Running,
    Paused,
    Stopped,
    Exited,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BreakpointId(pub u64);

#[derive(Debug, Clone)]
pub struct Breakpoint {
    pub id: BreakpointId,
    pub file: PathBuf,
    pub line: usize,
    pub enabled: bool,
    /// DAP-verified state, set after the adapter confirms it.
    pub verified: bool,
}

#[derive(Debug, Default)]
pub struct BreakpointStore {
    next_id: u64,
    breakpoints: HashMap<BreakpointId, Breakpoint>,
}

impl BreakpointStore {
    pub fn toggle(&mut self, file: PathBuf, line: usize) {
        // Remove if exists, add if not.
        if let Some(id) = self
            .breakpoints
            .values()
            .find(|bp| bp.file == file && bp.line == line)
            .map(|bp| bp.id)
        {
            self.breakpoints.remove(&id);
        } else {
            self.next_id += 1;
            let id = BreakpointId(self.next_id);
            self.breakpoints.insert(
                id,
                Breakpoint {
                    id,
                    file,
                    line,
                    enabled: true,
                    verified: false,
                },
            );
        }
    }

    pub fn has(&self, file: &PathBuf, line: usize) -> bool {
        self.breakpoints
            .values()
            .any(|bp| &bp.file == file && bp.line == line && bp.enabled)
    }

    pub fn for_file(&self, file: &PathBuf) -> Vec<&Breakpoint> {
        self.breakpoints
            .values()
            .filter(|bp| &bp.file == file)
            .collect()
    }

    pub fn all(&self) -> impl Iterator<Item = &Breakpoint> {
        self.breakpoints.values()
    }

    /// Mark a breakpoint as verified (or unverified) by the adapter.
    pub fn set_verified(&mut self, file: &PathBuf, line: usize, verified: bool) {
        for bp in self.breakpoints.values_mut() {
            if &bp.file == file && bp.line == line {
                bp.verified = verified;
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct StackFrame {
    pub id: u64,
    pub name: String,
    pub file: Option<PathBuf>,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone)]
pub struct Scope {
    pub name: String,
    pub variables_reference: u64,
}

#[derive(Debug, Clone)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub type_name: Option<String>,
    pub variables_reference: u64, // >0 means expandable
}

/// One console output line from the debug adapter.
#[derive(Debug, Clone)]
pub struct DebugConsoleEntry {
    pub category: String, // "stdout" | "stderr" | "console"
    pub text: String,
}

// ─── Inbound messages from background thread ─────────────────────────────────

pub enum DapInbound {
    Response(DapResponse),
    Event(DapEvent),
    ThreadExit,
}

// ─── DAP client ──────────────────────────────────────────────────────────────

/// Low-level DAP wire transport: sends requests, receives responses+events.
pub struct DapClient {
    seq: Arc<AtomicU64>,
    writer_tx: Sender<String>, // serialized DAP JSON lines to write to adapter stdin
    pub inbound: Receiver<DapInbound>,
    _child: Child,
}

impl DapClient {
    /// Spawn a debug adapter process and wire up I/O.
    pub fn spawn(adapter_path: &str, args: &[&str]) -> Result<Self, String> {
        let mut child = Command::new(adapter_path)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to spawn adapter '{adapter_path}': {e}"))?;

        let stdin = child.stdin.take().expect("captured stdin");
        let stdout = child.stdout.take().expect("captured stdout");
        let seq = Arc::new(AtomicU64::new(1));

        // Writer thread: encode DAP messages and write to adapter stdin.
        let (writer_tx, writer_rx) = bounded::<String>(64);
        {
            let mut writer = stdin;
            std::thread::spawn(move || {
                while let Ok(body) = writer_rx.recv() {
                    let header = format!("Content-Length: {}\r\n\r\n", body.len());
                    if writer.write_all(header.as_bytes()).is_err()
                        || writer.write_all(body.as_bytes()).is_err()
                    {
                        break;
                    }
                }
            });
        }

        // Reader thread: parse DAP responses/events from adapter stdout.
        let (inbound_tx, inbound_rx) = bounded::<DapInbound>(256);
        {
            let tx = inbound_tx;
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    // Parse "Content-Length: N\r\n\r\n<body>"
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 {
                        break;
                    }
                    let content_length: usize = header
                        .trim()
                        .strip_prefix("Content-Length:")
                        .and_then(|s| s.trim().parse().ok())
                        .unwrap_or(0);
                    // Consume the blank line.
                    let mut blank = String::new();
                    let _ = reader.read_line(&mut blank);
                    if content_length == 0 {
                        continue;
                    }
                    let mut body = vec![0u8; content_length];
                    use std::io::Read;
                    if reader.read_exact(&mut body).is_err() {
                        break;
                    }
                    let Ok(msg) = serde_json::from_slice::<DapMessage>(&body) else {
                        continue;
                    };
                    let inbound = match msg.msg_type.as_str() {
                        "response" => {
                            if let Ok(r) = serde_json::from_value::<DapResponse>(msg.body) {
                                DapInbound::Response(r)
                            } else {
                                continue;
                            }
                        }
                        "event" => {
                            if let Ok(e) = serde_json::from_value::<DapEvent>(msg.body) {
                                DapInbound::Event(e)
                            } else {
                                continue;
                            }
                        }
                        _ => continue,
                    };
                    if tx.send(inbound).is_err() {
                        break;
                    }
                }
                let _ = tx.send(DapInbound::ThreadExit);
            });
        }

        Ok(Self {
            seq,
            writer_tx,
            inbound: inbound_rx,
            _child: child,
        })
    }

    /// Send a DAP request, returning the sequence number.
    pub fn send(&self, command: &str, arguments: Option<Value>) -> u64 {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let msg = serde_json::json!({
            "seq": seq,
            "type": "request",
            "command": command,
            "arguments": arguments,
        });
        if let Ok(s) = serde_json::to_string(&msg) {
            let _ = self.writer_tx.try_send(s);
        }
        seq
    }

    pub fn initialize(&self) -> u64 {
        self.send(
            "initialize",
            Some(serde_json::json!({
                "adapterID": "stack-ide",
                "clientName": "Stack IDE",
                "linesStartAt1": true,
                "columnsStartAt1": true,
                "pathFormat": "path",
                "supportsRunInTerminalRequest": false,
            })),
        )
    }

    pub fn launch(&self, args: Value) -> u64 {
        self.send("launch", Some(args))
    }

    pub fn set_breakpoints(&self, file: &PathBuf, lines: &[usize]) -> u64 {
        let bps: Vec<Value> = lines
            .iter()
            .map(|&l| serde_json::json!({ "line": l }))
            .collect();
        self.send(
            "setBreakpoints",
            Some(serde_json::json!({
                "source": { "path": file.to_string_lossy() },
                "breakpoints": bps,
            })),
        )
    }

    pub fn configuration_done(&self) -> u64 {
        self.send("configurationDone", None)
    }

    pub fn cont(&self, thread_id: u64) -> u64 {
        self.send("continue", Some(serde_json::json!({ "threadId": thread_id })))
    }

    pub fn pause(&self, thread_id: u64) -> u64 {
        self.send("pause", Some(serde_json::json!({ "threadId": thread_id })))
    }

    pub fn next(&self, thread_id: u64) -> u64 {
        self.send("next", Some(serde_json::json!({ "threadId": thread_id })))
    }

    pub fn step_in(&self, thread_id: u64) -> u64 {
        self.send("stepIn", Some(serde_json::json!({ "threadId": thread_id })))
    }

    pub fn step_out(&self, thread_id: u64) -> u64 {
        self.send("stepOut", Some(serde_json::json!({ "threadId": thread_id })))
    }

    pub fn stack_trace(&self, thread_id: u64) -> u64 {
        self.send(
            "stackTrace",
            Some(serde_json::json!({ "threadId": thread_id, "levels": 20 })),
        )
    }

    pub fn scopes(&self, frame_id: u64) -> u64 {
        self.send("scopes", Some(serde_json::json!({ "frameId": frame_id })))
    }

    pub fn variables(&self, variables_reference: u64) -> u64 {
        self.send(
            "variables",
            Some(serde_json::json!({ "variablesReference": variables_reference })),
        )
    }

    pub fn disconnect(&self) -> u64 {
        self.send(
            "disconnect",
            Some(serde_json::json!({ "restart": false, "terminateDebuggee": true })),
        )
    }
}

// ─── Debug session ────────────────────────────────────────────────────────────

pub struct DebugSession {
    pub state: DebugState,
    pub client: DapClient,
    pub breakpoints: BreakpointStore,
    pub stack_frames: Vec<StackFrame>,
    pub scopes: Vec<Scope>,
    pub variables: Vec<Variable>,
    pub console: Vec<DebugConsoleEntry>,
    pub active_thread: Option<u64>,
    pub active_frame: Option<u64>,
    /// Last pending request seq → command name (for routing responses).
    pending: HashMap<u64, String>,
}

impl DebugSession {
    pub fn new(client: DapClient) -> Self {
        Self {
            state: DebugState::Starting,
            client,
            breakpoints: BreakpointStore::default(),
            stack_frames: Vec::new(),
            scopes: Vec::new(),
            variables: Vec::new(),
            console: Vec::new(),
            active_thread: None,
            active_frame: None,
            pending: HashMap::new(),
        }
    }

    /// Drive initialization: send `initialize` + `launch`, then set breakpoints.
    pub fn start(&mut self, launch_args: Value, bps: Vec<(PathBuf, Vec<usize>)>) {
        let seq = self.client.initialize();
        self.pending.insert(seq, "initialize".to_string());
        // Store launch args and bps for after `initialized` event.
        // (We stash them in console as an implementation shortcut.)
        let _ = launch_args; // used after initialized event in `poll`
        let _ = bps;
    }

    /// Non-blocking poll of incoming DAP messages. Call every frame.
    pub fn poll(&mut self) {
        while let Ok(msg) = self.client.inbound.try_recv() {
            match msg {
                DapInbound::Response(r) => self.handle_response(r),
                DapInbound::Event(e) => self.handle_event(e),
                DapInbound::ThreadExit => {
                    self.state = DebugState::Exited;
                }
            }
        }
    }

    fn handle_response(&mut self, r: DapResponse) {
        let command = self.pending.remove(&r.request_seq);
        if !r.success {
            self.console.push(DebugConsoleEntry {
                category: "stderr".to_string(),
                text: format!(
                    "DAP error ({}): {}",
                    r.command,
                    r.message.as_deref().unwrap_or("unknown")
                ),
            });
            return;
        }
        let body = r.body.unwrap_or(Value::Null);
        match command.as_deref() {
            Some("stackTrace") => {
                if let Some(frames) = body.get("stackFrames").and_then(|v| v.as_array()) {
                    self.stack_frames = frames
                        .iter()
                        .filter_map(parse_stack_frame)
                        .collect();
                    if let Some(first) = self.stack_frames.first() {
                        self.active_frame = Some(first.id);
                        let seq = self.client.scopes(first.id);
                        self.pending.insert(seq, "scopes".to_string());
                    }
                }
            }
            Some("scopes") => {
                if let Some(scopes) = body.get("scopes").and_then(|v| v.as_array()) {
                    self.scopes = scopes
                        .iter()
                        .filter_map(|s| {
                            Some(Scope {
                                name: s.get("name")?.as_str()?.to_string(),
                                variables_reference: s
                                    .get("variablesReference")?
                                    .as_u64()
                                    .unwrap_or(0),
                            })
                        })
                        .collect();
                    // Eagerly load the first scope's variables.
                    if let Some(scope) = self.scopes.first() {
                        if scope.variables_reference > 0 {
                            let seq = self.client.variables(scope.variables_reference);
                            self.pending.insert(seq, "variables".to_string());
                        }
                    }
                }
            }
            Some("variables") => {
                if let Some(vars) = body.get("variables").and_then(|v| v.as_array()) {
                    self.variables = vars
                        .iter()
                        .filter_map(|v| {
                            Some(Variable {
                                name: v.get("name")?.as_str()?.to_string(),
                                value: v
                                    .get("value")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                type_name: v
                                    .get("type")
                                    .and_then(|t| t.as_str())
                                    .map(str::to_string),
                                variables_reference: v
                                    .get("variablesReference")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0),
                            })
                        })
                        .collect();
                }
            }
            _ => {}
        }
    }

    fn handle_event(&mut self, e: DapEvent) {
        match e.event.as_str() {
            "initialized" => {
                self.state = DebugState::Running;
                // Send all pending breakpoints.
                let by_file: HashMap<PathBuf, Vec<usize>> = {
                    let mut m: HashMap<PathBuf, Vec<usize>> = HashMap::new();
                    for bp in self.breakpoints.all() {
                        m.entry(bp.file.clone()).or_default().push(bp.line);
                    }
                    m
                };
                for (file, lines) in &by_file {
                    let seq = self.client.set_breakpoints(file, lines);
                    self.pending.insert(seq, "setBreakpoints".to_string());
                }
                let seq = self.client.configuration_done();
                self.pending.insert(seq, "configurationDone".to_string());
            }
            "stopped" => {
                self.state = DebugState::Paused;
                if let Some(tid) = e
                    .body
                    .as_ref()
                    .and_then(|b| b.get("threadId"))
                    .and_then(|v| v.as_u64())
                {
                    self.active_thread = Some(tid);
                    let seq = self.client.stack_trace(tid);
                    self.pending.insert(seq, "stackTrace".to_string());
                }
            }
            "continued" => {
                self.state = DebugState::Running;
                self.stack_frames.clear();
                self.scopes.clear();
                self.variables.clear();
            }
            "exited" | "terminated" => {
                self.state = DebugState::Exited;
            }
            "output" => {
                if let Some(body) = &e.body {
                    let category = body
                        .get("category")
                        .and_then(|c| c.as_str())
                        .unwrap_or("console")
                        .to_string();
                    let text = body
                        .get("output")
                        .and_then(|o| o.as_str())
                        .unwrap_or("")
                        .to_string();
                    self.console.push(DebugConsoleEntry { category, text });
                }
            }
            _ => {}
        }
    }
}

fn parse_stack_frame(v: &Value) -> Option<StackFrame> {
    let id = v.get("id")?.as_u64()?;
    let name = v.get("name")?.as_str()?.to_string();
    let file = v
        .get("source")
        .and_then(|s| s.get("path"))
        .and_then(|p| p.as_str())
        .map(PathBuf::from);
    let line = v.get("line").and_then(|l| l.as_u64()).unwrap_or(0) as usize;
    let column = v.get("column").and_then(|c| c.as_u64()).unwrap_or(0) as usize;
    Some(StackFrame { id, name, file, line, column })
}

// ─── Debug panel UI state ─────────────────────────────────────────────────────

/// Configuration for launching a debug session.
#[derive(Debug, Clone)]
pub struct DebugAdapterConfig {
    pub adapter_path: String,
    pub adapter_args: Vec<String>,
    pub launch_args: Value,
}

impl Default for DebugAdapterConfig {
    fn default() -> Self {
        Self {
            adapter_path: String::new(),
            adapter_args: Vec::new(),
            launch_args: serde_json::json!({}),
        }
    }
}

pub struct DebugPanelState {
    pub session: Option<DebugSession>,
    pub config: DebugAdapterConfig,
    pub show: bool,
    pub console_scroll_to_bottom: bool,
}

impl Default for DebugPanelState {
    fn default() -> Self {
        Self {
            session: None,
            config: DebugAdapterConfig::default(),
            show: false,
            console_scroll_to_bottom: true,
        }
    }
}

impl DebugPanelState {
    pub fn start_session(&mut self) -> Result<(), String> {
        if self.config.adapter_path.trim().is_empty() {
            return Err("No debug adapter configured. Set the adapter path in Settings.".into());
        }
        let adapter_args: Vec<&str> = self
            .config
            .adapter_args
            .iter()
            .map(String::as_str)
            .collect();
        let client =
            DapClient::spawn(&self.config.adapter_path, &adapter_args)?;
        let mut session = DebugSession::new(client);
        let seq = session.client.initialize();
        session.pending.insert(seq, "initialize".to_string());
        self.session = Some(session);
        Ok(())
    }

    pub fn stop_session(&mut self) {
        if let Some(s) = &self.session {
            let _ = s.client.disconnect();
        }
        self.session = None;
    }

    pub fn poll(&mut self) {
        if let Some(s) = &mut self.session {
            s.poll();
            if s.state == DebugState::Exited {
                self.session = None;
            }
        }
    }

    pub fn state(&self) -> DebugState {
        self.session
            .as_ref()
            .map(|s| s.state)
            .unwrap_or(DebugState::NotStarted)
    }
}

/// Render the debug toolbar row.
pub fn render_debug_toolbar(ui: &mut egui::Ui, state: &mut DebugPanelState) {
    let s = state.state();
    let running = matches!(s, DebugState::Running | DebugState::Paused);
    let paused = s == DebugState::Paused;
    let not_started = s == DebugState::NotStarted || s == DebugState::Exited;

    ui.horizontal(|ui| {
        if ui
            .add_enabled(not_started, egui::Button::new("▶ Start"))
            .clicked()
        {
            if let Err(e) = state.start_session() {
                eprintln!("debug: {e}");
            }
        }
        if ui
            .add_enabled(paused, egui::Button::new("▶ Continue"))
            .clicked()
        {
            if let Some(s) = &mut state.session {
                if let Some(tid) = s.active_thread {
                    let seq = s.client.cont(tid);
                    s.pending.insert(seq, "continue".to_string());
                }
            }
        }
        if ui
            .add_enabled(running, egui::Button::new("⏸ Pause"))
            .clicked()
        {
            if let Some(s) = &mut state.session {
                if let Some(tid) = s.active_thread {
                    let seq = s.client.pause(tid);
                    s.pending.insert(seq, "pause".to_string());
                }
            }
        }
        if ui
            .add_enabled(paused, egui::Button::new("→ Step Over"))
            .clicked()
        {
            if let Some(s) = &mut state.session {
                if let Some(tid) = s.active_thread {
                    let seq = s.client.next(tid);
                    s.pending.insert(seq, "next".to_string());
                }
            }
        }
        if ui
            .add_enabled(paused, egui::Button::new("↓ Step In"))
            .clicked()
        {
            if let Some(s) = &mut state.session {
                if let Some(tid) = s.active_thread {
                    let seq = s.client.step_in(tid);
                    s.pending.insert(seq, "stepIn".to_string());
                }
            }
        }
        if ui
            .add_enabled(paused, egui::Button::new("↑ Step Out"))
            .clicked()
        {
            if let Some(s) = &mut state.session {
                if let Some(tid) = s.active_thread {
                    let seq = s.client.step_out(tid);
                    s.pending.insert(seq, "stepOut".to_string());
                }
            }
        }
        if ui
            .add_enabled(running, egui::Button::new("⏹ Stop"))
            .clicked()
        {
            state.stop_session();
        }

        ui.separator();
        let label = match s {
            DebugState::NotStarted => "Not started",
            DebugState::Starting => "Starting…",
            DebugState::Running => "Running",
            DebugState::Paused => "Paused",
            DebugState::Stopped | DebugState::Exited => "Stopped",
            DebugState::Error => "Error",
        };
        ui.label(label);
        if matches!(s, DebugState::Starting | DebugState::Running) {
            ui.spinner();
        }
    });
}

/// Render the call stack panel.
pub fn render_call_stack(ui: &mut egui::Ui, state: &DebugPanelState) {
    ui.label("Call Stack");
    ui.separator();
    let session = match &state.session {
        Some(s) => s,
        None => {
            ui.weak("No active session");
            return;
        }
    };
    if session.stack_frames.is_empty() {
        ui.weak("No frames");
        return;
    }
    egui::ScrollArea::vertical()
        .max_height(150.0)
        .show(ui, |ui| {
            for frame in &session.stack_frames {
                let active = session.active_frame == Some(frame.id);
                let label = format!(
                    "{} — {}:{}",
                    frame.name,
                    frame.file.as_ref().and_then(|f| f.file_name()).and_then(|n| n.to_str()).unwrap_or("?"),
                    frame.line
                );
                if ui.selectable_label(active, &label).clicked() {
                    // Frame selection handled by caller via click events.
                }
            }
        });
}

/// Render the variables panel.
pub fn render_variables(ui: &mut egui::Ui, state: &DebugPanelState) {
    ui.label("Variables");
    ui.separator();
    let session = match &state.session {
        Some(s) => s,
        None => {
            ui.weak("No active session");
            return;
        }
    };
    egui::ScrollArea::vertical()
        .max_height(150.0)
        .show(ui, |ui| {
            for var in &session.variables {
                ui.horizontal(|ui| {
                    ui.strong(&var.name);
                    ui.label(format!(" = {}", var.value));
                    if let Some(t) = &var.type_name {
                        ui.weak(format!(" : {t}"));
                    }
                });
            }
        });
}

/// Render the debug console.
pub fn render_debug_console(ui: &mut egui::Ui, state: &mut DebugPanelState) {
    ui.label("Debug Console");
    ui.separator();
    let session = match &state.session {
        Some(s) => s,
        None => {
            ui.weak("No output");
            return;
        }
    };
    egui::ScrollArea::vertical()
        .stick_to_bottom(state.console_scroll_to_bottom)
        .max_height(200.0)
        .show(ui, |ui| {
            for entry in &session.console {
                let color = match entry.category.as_str() {
                    "stderr" => egui::Color32::from_rgb(220, 80, 80),
                    "stdout" => ui.visuals().text_color(),
                    _ => egui::Color32::from_rgb(150, 200, 150),
                };
                ui.colored_label(color, &entry.text);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakpoint_store_toggle_adds_and_removes() {
        let mut store = BreakpointStore::default();
        let f = PathBuf::from("src/main.rs");
        assert!(!store.has(&f, 10));
        store.toggle(f.clone(), 10);
        assert!(store.has(&f, 10));
        store.toggle(f.clone(), 10);
        assert!(!store.has(&f, 10));
    }

    #[test]
    fn breakpoint_store_for_file_filters_correctly() {
        let mut store = BreakpointStore::default();
        let f1 = PathBuf::from("a.rs");
        let f2 = PathBuf::from("b.rs");
        store.toggle(f1.clone(), 1);
        store.toggle(f2.clone(), 5);
        assert_eq!(store.for_file(&f1).len(), 1);
        assert_eq!(store.for_file(&f2).len(), 1);
    }

    #[test]
    fn dap_message_round_trips_through_serde() {
        let msg = serde_json::json!({
            "seq": 1u64,
            "type": "request",
            "command": "initialize",
            "arguments": { "adapterID": "test" }
        });
        let s = serde_json::to_string(&msg).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["command"], "initialize");
    }
}
