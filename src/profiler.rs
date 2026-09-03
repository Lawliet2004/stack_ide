//! Performance profiler panel: runs `cargo flamegraph`, parses the SVG, and
//! renders an interactive flamegraph using egui Painter.
//!
//! # Architecture
//! - [`ProfilerState`] — stored on `App`; owns all panel state.
//! - [`FlameFrame`] — one parsed stack frame from flamegraph.svg.
//! - [`parse_flamegraph_svg`] — no XML/regex crates; pure string scanning.
//! - [`render_profiler_panel`] — full rendering entry point.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{bounded, Receiver, Sender};
use egui::{Color32, FontId, Rect, Sense, Stroke};

// ─── ProfilerStatus ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ProfilerStatus {
    Idle,
    Running { started: Instant },
    Done { elapsed: Duration },
    Error(String),
}

impl ProfilerStatus {
    pub fn label(&self) -> String {
        match self {
            Self::Idle => "Idle".into(),
            Self::Running { started } => {
                format!("Running... ({:.1}s)", started.elapsed().as_secs_f64())
            }
            Self::Done { elapsed } => format!("Done ({:.1}s)", elapsed.as_secs_f64()),
            Self::Error(e) => format!("Error: {e}"),
        }
    }
}

// ─── FlameFrame ──────────────────────────────────────────────────────────────

/// One parsed stack frame from flamegraph.svg.
#[derive(Debug, Clone)]
pub struct FlameFrame {
    /// Full function name including module path.
    pub func_name: String,
    /// Last segment only (after last `::`).
    pub short_name: String,
    /// X position as 0..1 fraction of total width.
    pub x_pct: f32,
    /// Stack depth derived from y coordinate / frame height.
    pub y_level: u32,
    /// Width as 0..1 fraction of total width.
    pub width_pct: f32,
    pub samples: u32,
    /// CPU percentage.
    pub pct: f32,
    /// Optional file path hint extracted from func_name.
    pub file_hint: Option<String>,
}

impl FlameFrame {
    /// Deterministic color derived from func_name bytes (HSV → RGB).
    pub fn color(&self) -> Color32 {
        let h: u32 = self
            .func_name
            .bytes()
            .fold(0u32, |acc, b| acc.wrapping_add(acc.wrapping_mul(31).wrapping_add(b as u32)));
        let hue = (h % 360) as f32;
        let sat = 0.6 + (h % 100) as f32 / 333.0; // 0.6..0.9
        let val = 0.7 + (h % 100) as f32 / 500.0; // 0.7..0.9
        hsv_to_rgb(hue, sat, val)
    }
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Color32 {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r1, g1, b1) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    Color32::from_rgb(
        ((r1 + m) * 255.0) as u8,
        ((g1 + m) * 255.0) as u8,
        ((b1 + m) * 255.0) as u8,
    )
}

// ─── SVG parser ───────────────────────────────────────────────────────────────

/// Parse flamegraph.svg without any XML crate. Uses str::find() scanning.
/// The flamegraph SVG format is predictable; this approach is robust enough.
pub fn parse_flamegraph_svg(svg: &str) -> Vec<FlameFrame> {
    // Extract viewBox dimensions for coordinate normalisation.
    let (vb_w, vb_h) = extract_viewbox(svg);
    let frame_height_svg = 16.0f32; // flamegraph default frame height in SVG units

    let mut frames = Vec::new();
    let mut pos = 0usize;

    while let Some(rect_start) = svg[pos..].find("<rect ").map(|i| pos + i) {
        // Find the end of this rect element (could be <rect ... /> or <rect ...>...</rect>).
        let tag_end = match svg[rect_start..].find('>') {
            Some(i) => rect_start + i + 1,
            None => break,
        };

        let tag_src = &svg[rect_start..tag_end];

        // Parse attributes from the rect tag.
        let x = attr_f32(tag_src, "x").unwrap_or(0.0);
        let y = attr_f32(tag_src, "y").unwrap_or(0.0);
        let w = attr_f32(tag_src, "width").unwrap_or(0.0);
        let _h = attr_f32(tag_src, "height").unwrap_or(frame_height_svg);

        // Now look for the <title> element immediately following.
        let search_region_end = svg[tag_end..].find("</g>").map(|i| tag_end + i).unwrap_or(svg.len().min(tag_end + 512));
        let title_text = if let Some(ts) = svg[tag_end..search_region_end].find("<title>") {
            let abs = tag_end + ts + "<title>".len();
            if let Some(te) = svg[abs..].find("</title>") {
                Some(&svg[abs..abs + te])
            } else {
                None
            }
        } else {
            None
        };

        if let Some(title) = title_text {
            if let Some(frame) = parse_title(title, x, y, w, vb_w, vb_h, frame_height_svg) {
                frames.push(frame);
            }
        }

        pos = tag_end;
    }

    frames
}

fn extract_viewbox(svg: &str) -> (f32, f32) {
    // Find viewBox="0 0 W H"
    if let Some(vb_start) = svg.find("viewBox=\"") {
        let start = vb_start + "viewBox=\"".len();
        if let Some(end) = svg[start..].find('"') {
            let vb = &svg[start..start + end];
            let parts: Vec<&str> = vb.split_whitespace().collect();
            if parts.len() == 4 {
                let w = parts[2].parse().unwrap_or(1200.0);
                let h = parts[3].parse().unwrap_or(600.0);
                return (w, h);
            }
        }
    }
    (1200.0, 600.0)
}

fn attr_f32(tag: &str, name: &str) -> Option<f32> {
    // Find: name="VALUE"
    let needle = format!("{name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = tag[start..].find('"')?;
    tag[start..start + end].parse().ok()
}

fn parse_title(
    title: &str,
    x_svg: f32,
    y_svg: f32,
    w_svg: f32,
    vb_w: f32,
    vb_h: f32,
    frame_h_svg: f32,
) -> Option<FlameFrame> {
    // Format: "function_name (N samples, M.MM%)"
    // Find the last " (" to split func name from stats.
    let sep = title.rfind(" (")?;
    let func_name = title[..sep].trim().to_string();
    let stats = &title[sep + 2..];

    // Parse "N samples, M.MM%)"
    let samples_end = stats.find(" samples")?;
    let samples: u32 = stats[..samples_end].trim().parse().ok()?;

    let pct_start = stats.find(',')? + 1;
    let pct_end = stats.find('%')?;
    let pct: f32 = stats[pct_start..pct_end].trim().parse().ok()?;

    let short_name = func_name
        .rsplit("::")
        .next()
        .unwrap_or(&func_name)
        .to_string();

    // Extract file hint if func_name looks like a path.
    let file_hint = if func_name.contains('/') || func_name.contains('\\') {
        Some(func_name.clone())
    } else {
        None
    };

    let x_pct = x_svg / vb_w;
    let width_pct = w_svg / vb_w;
    let y_level = if vb_h > 0.0 && frame_h_svg > 0.0 {
        let bottom_y = vb_h; // flamegraph puts deepest frames at top visually
        ((bottom_y - y_svg) / frame_h_svg) as u32
    } else {
        0
    };

    Some(FlameFrame {
        func_name,
        short_name,
        x_pct,
        y_level,
        width_pct,
        samples,
        pct,
        file_hint,
    })
}

// ─── Background runner ────────────────────────────────────────────────────────

pub enum ProfilerEvent {
    Line(String),
    Done {
        svg_path: Option<PathBuf>,
        error: Option<String>,
    },
}

fn spawn_flamegraph_process(root: PathBuf, extra_args: String, tx: Sender<ProfilerEvent>) {
    std::thread::spawn(move || {
        let mut cmd = Command::new("cargo");
        cmd.arg("flamegraph")
            .arg("--output")
            .arg("flamegraph.svg");
        // Append extra args if non-empty.
        for arg in extra_args.split_whitespace() {
            cmd.arg(arg);
        }
        let mut child = match cmd
            .current_dir(&root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(ProfilerEvent::Done {
                    svg_path: None,
                    error: Some(format!(
                        "cargo flamegraph not found: {e}\nInstall: cargo install flamegraph"
                    )),
                });
                return;
            }
        };

        // Stream stderr to log.
        if let Some(stderr) = child.stderr.take() {
            use std::io::BufRead;
            let tx2 = tx.clone();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(stderr).lines().flatten() {
                    let _ = tx2.send(ProfilerEvent::Line(line));
                }
            });
        }

        let status = child.wait();
        let svg_path = root.join("flamegraph.svg");
        let svg = svg_path.exists().then_some(svg_path);
        let error = match status {
            Ok(s) if s.success() => None,
            Ok(s) => Some(format!(
                "cargo flamegraph exited with code {}",
                s.code().unwrap_or(-1)
            )),
            Err(e) => Some(e.to_string()),
        };
        let _ = tx.send(ProfilerEvent::Done { svg_path: svg, error });
    });
}

// ─── ProfilerState ───────────────────────────────────────────────────────────

/// All profiler panel state, stored on `BlueIdeApp`.
pub struct ProfilerState {
    pub status: ProfilerStatus,
    pub flamegraph_path: Option<PathBuf>,
    pub last_svg_modified: Option<SystemTime>,
    pub frames: Vec<FlameFrame>,
    pub total_samples: u32,
    // Zoom / pan.
    pub zoom_scale: f32,
    pub pan_offset_x: f32,
    // Interaction.
    pub hovered_frame: Option<usize>,
    pub selected_frame: Option<usize>,
    // Configuration.
    pub extra_args: String,
    // Log output from cargo flamegraph process.
    pub log_output: String,
    // Internal.
    receiver: Option<Receiver<ProfilerEvent>>,
    poll_timer: Instant,
    // Drag-pan state.
    drag_start: Option<(f32, f32)>,
    pan_at_drag_start: f32,
}

impl Default for ProfilerState {
    fn default() -> Self {
        Self {
            status: ProfilerStatus::Idle,
            flamegraph_path: None,
            last_svg_modified: None,
            frames: Vec::new(),
            total_samples: 0,
            zoom_scale: 1.0,
            pan_offset_x: 0.0,
            hovered_frame: None,
            selected_frame: None,
            extra_args: String::new(),
            log_output: String::new(),
            receiver: None,
            poll_timer: Instant::now(),
            drag_start: None,
            pan_at_drag_start: 0.0,
        }
    }
}

impl ProfilerState {
    const TIMEOUT_SECS: u64 = 60;
    const POLL_INTERVAL_MS: u64 = 500;

    pub fn run(&mut self, workspace_root: &Path) {
        if matches!(self.status, ProfilerStatus::Running { .. }) {
            return;
        }
        self.log_output.clear();
        self.frames.clear();
        self.total_samples = 0;
        self.hovered_frame = None;
        self.selected_frame = None;
        self.status = ProfilerStatus::Running { started: Instant::now() };
        self.poll_timer = Instant::now();

        let (tx, rx) = bounded(256);
        self.receiver = Some(rx);
        spawn_flamegraph_process(workspace_root.to_path_buf(), self.extra_args.clone(), tx);
    }

    pub fn reset_zoom(&mut self) {
        self.zoom_scale = 1.0;
        self.pan_offset_x = 0.0;
    }

    /// Called each frame; drains the process receiver and polls for SVG changes.
    pub fn poll(&mut self) {
        // Drain process events.
        if let Some(rx) = self.receiver.clone() {
            while let Ok(event) = rx.try_recv() {
                match event {
                    ProfilerEvent::Line(line) => {
                        self.log_output.push_str(&line);
                        self.log_output.push('\n');
                    }
                    ProfilerEvent::Done { svg_path, error } => {
                        self.receiver = None;
                        let start_time = if let ProfilerStatus::Running { started } = self.status {
                            started
                        } else {
                            Instant::now()
                        };
                        if let Some(err) = error {
                            // If SVG still produced despite error code, try parsing it.
                            if let Some(path) = svg_path {
                                self.try_load_svg(&path);
                                self.status = ProfilerStatus::Done {
                                    elapsed: start_time.elapsed(),
                                };
                            } else {
                                self.status = ProfilerStatus::Error(err);
                            }
                        } else if let Some(path) = svg_path {
                            self.try_load_svg(&path);
                            self.status = ProfilerStatus::Done {
                                elapsed: start_time.elapsed(),
                            };
                        } else {
                            self.status =
                                ProfilerStatus::Error("flamegraph.svg not produced".into());
                        }
                    }
                }
            }
        }

        // Timeout check.
        if let ProfilerStatus::Running { started } = &self.status {
            if started.elapsed().as_secs() > Self::TIMEOUT_SECS {
                self.status = ProfilerStatus::Error("Timed out after 60s".into());
                self.receiver = None;
            }
        }

        // Poll SVG modification time every 500ms while running.
        if matches!(self.status, ProfilerStatus::Running { .. }) {
            if self.poll_timer.elapsed().as_millis() >= Self::POLL_INTERVAL_MS as u128 {
                self.poll_timer = Instant::now();
                if let Some(path) = &self.flamegraph_path.clone() {
                    self.check_svg_changed(path.clone());
                }
            }
        }
    }

    fn try_load_svg(&mut self, path: &Path) {
        self.flamegraph_path = Some(path.to_path_buf());
        if let Ok(text) = std::fs::read_to_string(path) {
            if text.contains("<rect ") {
                self.frames = parse_flamegraph_svg(&text);
                self.total_samples = self.frames.iter().map(|f| f.samples).max().unwrap_or(0);
            } else {
                self.status = ProfilerStatus::Error("No profiling data in SVG".into());
                return;
            }
        }
        if let Ok(meta) = std::fs::metadata(path) {
            self.last_svg_modified = meta.modified().ok();
        }
    }

    fn check_svg_changed(&mut self, path: PathBuf) {
        if let Ok(meta) = std::fs::metadata(&path) {
            if let Ok(modified) = meta.modified() {
                if Some(modified) != self.last_svg_modified {
                    self.try_load_svg(&path.clone());
                }
            }
        }
    }
}

// ─── Rendering ───────────────────────────────────────────────────────────────

const FRAME_HEIGHT: f32 = 20.0;
const TOOLBAR_HEIGHT: f32 = 36.0;
const DETAIL_PANEL_WIDTH: f32 = 220.0;

/// Main entry point — render the profiler panel inside the bottom panel area.
pub fn render_profiler_panel(
    ui: &mut egui::Ui,
    state: &mut ProfilerState,
    workspace_root: Option<&PathBuf>,
    palette: crate::theme::SemanticPalette,
    trusted: bool,
) -> Option<PathBuf> {
    state.poll();

    let mut lsp_jump_request: Option<PathBuf> = None;

    // ── Toolbar ──────────────────────────────────────────────────────────────
    ui.allocate_ui(egui::vec2(ui.available_width(), TOOLBAR_HEIGHT), |ui| {
        ui.horizontal_centered(|ui| {
            let can_run = trusted
                && workspace_root.is_some()
                && !matches!(state.status, ProfilerStatus::Running { .. });

            if ui.add_enabled(can_run, egui::Button::new("▶ Run Profile")).clicked() {
                if let Some(root) = workspace_root {
                    state.run(root);
                }
            }

            ui.label("Args:");
            ui.add(
                egui::TextEdit::singleline(&mut state.extra_args)
                    .hint_text("--bin mybin -- --arg")
                    .desired_width(180.0),
            );

            if let ProfilerStatus::Done { .. } = &state.status {
                if let Some(path) = &state.flamegraph_path.clone() {
                    if ui.button("Open SVG").clicked() {
                        let _ = open_external(path);
                    }
                }
            }

            if ui.button("⟳").on_hover_text("Ctrl+0: Reset zoom").clicked() {
                state.reset_zoom();
            }

            // Status label.
            let status_text = state.status.label();
            match &state.status {
                ProfilerStatus::Error(_) => {
                    ui.colored_label(palette.error, &status_text);
                }
                ProfilerStatus::Running { .. } => {
                    ui.spinner();
                    ui.label(&status_text);
                }
                _ => {
                    ui.weak(&status_text);
                }
            }
        });
    });

    ui.separator();

    // ── Canvas + detail panel ────────────────────────────────────────────────
    if state.frames.is_empty() {
        match &state.status {
            ProfilerStatus::Idle => {
                ui.centered_and_justified(|ui| {
                    ui.weak("Click ▶ Run Profile to generate a flamegraph");
                });
            }
            ProfilerStatus::Running { .. } => {
                ui.centered_and_justified(|ui| {
                    ui.label("Profiling... (this may take a minute)");
                });
                // Show log output.
                let log = state.log_output.clone();
                if !log.is_empty() {
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .stick_to_bottom(true)
                        .max_height(200.0)
                        .show(ui, |ui| {
                            ui.monospace(&log);
                        });
                }
            }
            ProfilerStatus::Error(e) => {
                ui.colored_label(palette.error, e);
                let log = state.log_output.clone();
                if !log.is_empty() {
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .max_height(200.0)
                        .show(ui, |ui| {
                            ui.monospace(&log);
                        });
                }
            }
            ProfilerStatus::Done { .. } => {
                ui.weak("No profiling data (SVG contained no frames)");
            }
        }
        return lsp_jump_request;
    }

    // Show the flamegraph canvas + optional detail panel.
    let selected = state.selected_frame;
    let has_detail = selected.is_some();
    let avail = ui.available_size();
    let canvas_w = if has_detail {
        avail.x - DETAIL_PANEL_WIDTH - 4.0
    } else {
        avail.x
    };

    ui.horizontal(|ui| {
        // Flamegraph canvas.
        ui.allocate_ui(egui::vec2(canvas_w, avail.y), |ui| {
            lsp_jump_request = render_flamegraph_canvas(ui, state, canvas_w, palette);
        });

        if has_detail {
            ui.separator();
            if let Some(idx) = selected {
                ui.allocate_ui(egui::vec2(DETAIL_PANEL_WIDTH, avail.y), |ui| {
                    render_detail_panel(ui, state, idx, palette);
                });
            }
        }
    });

    lsp_jump_request
}

fn render_flamegraph_canvas(
    ui: &mut egui::Ui,
    state: &mut ProfilerState,
    canvas_w: f32,
    _palette: crate::theme::SemanticPalette,
) -> Option<PathBuf> {
    let max_level = state.frames.iter().map(|f| f.y_level).max().unwrap_or(0);
    let _canvas_h = (max_level + 2) as f32 * FRAME_HEIGHT;

    let (response, painter) = ui.allocate_painter(
        egui::vec2(canvas_w, ui.available_height()),
        Sense::click_and_drag(),
    );
    let visible_rect = response.rect;

    // ── Keyboard shortcuts (Ctrl+0 zoom reset) ──────────────────────────────
    if response.has_focus() {
        ui.input(|i| {
            if i.modifiers.ctrl && i.key_pressed(egui::Key::Num0) {
                state.reset_zoom();
            }
        });
    }

    // ── Scroll wheel zoom ────────────────────────────────────────────────────
    if response.hovered() {
        let scroll_delta = ui.input(|i| i.raw_scroll_delta.y);
        if scroll_delta.abs() > 0.1 {
            let mouse_pos = ui.input(|i| i.pointer.hover_pos()).unwrap_or(visible_rect.center());
            let mouse_x_in_canvas = mouse_pos.x - visible_rect.left();
            let old_zoom = state.zoom_scale;
            let new_zoom = (old_zoom * 1.15f32.powf(scroll_delta / 30.0)).clamp(1.0, 50.0);
            // Adjust pan so the point under mouse stays fixed.
            let canvas_frac = (mouse_x_in_canvas - state.pan_offset_x) / (canvas_w * old_zoom);
            state.zoom_scale = new_zoom;
            state.pan_offset_x = mouse_x_in_canvas - canvas_frac * canvas_w * new_zoom;
            let max_pan = (canvas_w * state.zoom_scale - canvas_w).max(0.0);
            state.pan_offset_x = state.pan_offset_x.clamp(-max_pan, 0.0);
            ui.ctx().request_repaint();
        }
    }

    // ── Drag to pan ──────────────────────────────────────────────────────────
    if response.drag_started() {
        let mouse_x = ui.input(|i| i.pointer.hover_pos())
            .map(|p| p.x)
            .unwrap_or(0.0);
        state.drag_start = Some((mouse_x, state.pan_offset_x));
        state.pan_at_drag_start = state.pan_offset_x;
    }
    if response.dragged() {
        let mouse_x = ui.input(|i| i.pointer.hover_pos())
            .map(|p| p.x)
            .unwrap_or(0.0);
        if let Some((start_x, pan_at_start)) = state.drag_start {
            let delta = mouse_x - start_x;
            let new_pan = pan_at_start + delta;
            let max_pan = (canvas_w * state.zoom_scale - canvas_w).max(0.0);
            state.pan_offset_x = new_pan.clamp(-max_pan, 0.0);
            ui.ctx().request_repaint();
        }
    }
    if response.drag_stopped() {
        state.drag_start = None;
    }

    let zoomed_w = canvas_w * state.zoom_scale;
    let pan = state.pan_offset_x;
    let effective_w = canvas_w;

    // Background.
    painter.rect_filled(visible_rect, 0.0, Color32::from_rgb(20, 20, 25));

    // ── Frame rendering with off-screen culling ──────────────────────────────
    let mouse_pos = ui.input(|i| i.pointer.hover_pos());
    let mut new_hover: Option<usize> = None;
    let mut double_click_frame: Option<usize> = None;
    let double_clicked = response.double_clicked();
    let single_clicked = response.clicked();

    for (idx, frame) in state.frames.iter().enumerate() {
        let fx = frame.x_pct * zoomed_w + pan + visible_rect.left();
        let fw = (frame.width_pct * zoomed_w).max(1.0);
        let fy = visible_rect.top() + (max_level as f32 - frame.y_level as f32) * FRAME_HEIGHT;
        let fh = FRAME_HEIGHT - 1.0;

        // Cull off-screen.
        if fx + fw < visible_rect.left() || fx > visible_rect.right() {
            continue;
        }
        if fy + fh < visible_rect.top() || fy > visible_rect.bottom() {
            continue;
        }

        let frame_rect = Rect::from_min_size(
            egui::pos2(fx.max(visible_rect.left()), fy),
            egui::vec2(fw.min(fx + fw - visible_rect.left()).min(visible_rect.right() - fx.max(visible_rect.left())).max(1.0), fh),
        );

        let is_hovered = mouse_pos.map_or(false, |mp| frame_rect.contains(mp));
        let is_selected = state.selected_frame == Some(idx);

        if is_hovered {
            new_hover = Some(idx);
        }

        let base_color = frame.color();
        let fill_color = if is_hovered {
            Color32::from_rgb(
                (base_color.r() as u32 + 40).min(255) as u8,
                (base_color.g() as u32 + 40).min(255) as u8,
                (base_color.b() as u32 + 40).min(255) as u8,
            )
        } else {
            base_color
        };

        painter.rect_filled(frame_rect, 0.0, fill_color);
        let stroke_width = if is_selected { 2.0 } else { 1.0 };
        let stroke_color = if is_selected {
            Color32::WHITE
        } else {
            Color32::from_rgba_premultiplied(0, 0, 0, 60)
        };
        painter.rect_stroke(frame_rect, 0.0, Stroke::new(stroke_width, stroke_color));

        // Label if wide enough.
        if fw > 40.0 {
            let label = if fw > 120.0 { &frame.func_name } else { &frame.short_name };
            let clip_rect = frame_rect;
            painter.with_clip_rect(clip_rect).text(
                frame_rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                FontId::monospace(11.0),
                Color32::from_rgb(220, 220, 220),
            );
        }

        // Hover tooltip.
        if is_hovered {
            let _tooltip = format!(
                "{}\nSamples: {} ({:.2}%)",
                frame.func_name, frame.samples, frame.pct
            );
            if let Some(mp) = mouse_pos {
                egui::show_tooltip_at(ui.ctx(), egui::Id::new("flame_tooltip"), Some(mp), |ui| {
                    ui.monospace(&frame.func_name);
                    ui.label(format!("Samples: {} ({:.2}%)", frame.samples, frame.pct));
                    if let Some(hint) = &frame.file_hint {
                        ui.weak(format!("Source: {hint}"));
                    }
                });
            }
        }

        if is_hovered && double_clicked {
            double_click_frame = Some(idx);
        }
        if is_hovered && single_clicked && !double_clicked {
            state.selected_frame = Some(idx);
        }
    }

    state.hovered_frame = new_hover;

    // Double-click: zoom to fill that frame.
    if let Some(idx) = double_click_frame {
        let frame = &state.frames[idx];
        if frame.width_pct > 0.0 {
            state.zoom_scale = (1.0 / frame.width_pct).clamp(1.0, 50.0);
            state.pan_offset_x = -(frame.x_pct * canvas_w * state.zoom_scale);
            let max_pan = (canvas_w * state.zoom_scale - canvas_w).max(0.0);
            state.pan_offset_x = state.pan_offset_x.clamp(-max_pan, 0.0);
        }
    }

    // Horizontal scrollbar at bottom when zoomed.
    if state.zoom_scale > 1.01 {
        let sb_h = 8.0;
        let sb_rect = Rect::from_min_size(
            egui::pos2(visible_rect.left(), visible_rect.bottom() - sb_h - 2.0),
            egui::vec2(effective_w, sb_h),
        );
        painter.rect_filled(sb_rect, 3.0, Color32::from_gray(50));
        let thumb_w = effective_w / state.zoom_scale;
        let thumb_x = (-state.pan_offset_x / (zoomed_w - effective_w)) * (effective_w - thumb_w);
        let thumb_rect = Rect::from_min_size(
            egui::pos2(sb_rect.left() + thumb_x, sb_rect.top()),
            egui::vec2(thumb_w.max(8.0), sb_h),
        );
        painter.rect_filled(thumb_rect, 3.0, Color32::from_gray(120));
    }

    None
}

fn render_detail_panel(
    ui: &mut egui::Ui,
    state: &mut ProfilerState,
    idx: usize,
    _palette: crate::theme::SemanticPalette,
) {
    let Some(frame) = state.frames.get(idx) else { return };
    let frame = frame.clone();

    ui.heading("Frame Details");
    ui.separator();

    ui.label(egui::RichText::new("Function:").strong());
    ui.label(&frame.func_name);

    // Module path (everything before last ::).
    if let Some(sep) = frame.func_name.rfind("::") {
        ui.label(egui::RichText::new("Module:").strong());
        ui.label(&frame.func_name[..sep]);
    }

    ui.add_space(4.0);
    ui.label(egui::RichText::new(format!("% of total: {:.2}%", frame.pct)).strong());
    ui.label(format!("Samples: {}", frame.samples));
    ui.label(format!("Call depth: {}", frame.y_level));

    if let Some(hint) = &frame.file_hint {
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Source hint:").strong());
        ui.label(hint);
    }

    ui.add_space(8.0);
    if ui.button("✕ Deselect").clicked() {
        state.selected_frame = None;
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn open_external(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::process::Command::new("explorer").arg(path).spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(path).spawn()?;
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(path).spawn()?;
    }
    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_flamegraph_svg_extracts_frames() {
        let svg = r##"<svg viewBox="0 0 1200 600">
<g><rect x="0" y="540" width="600" height="16" fill="#e05"/>
<title>my_crate::some_fn (200 samples, 40.00%)</title></g>
<g><rect x="0" y="520" width="300" height="16" fill="#e05"/>
<title>std::rt::lang_start (100 samples, 20.00%)</title></g>
</svg>"##;
        let frames = parse_flamegraph_svg(svg);
        assert_eq!(frames.len(), 2);
        let f = &frames[0];
        assert_eq!(f.func_name, "my_crate::some_fn");
        assert_eq!(f.short_name, "some_fn");
        assert_eq!(f.samples, 200);
        assert!((f.pct - 40.0).abs() < 0.01);
        assert!((f.x_pct - 0.0).abs() < 0.001);
        assert!((f.width_pct - 0.5).abs() < 0.001);
    }

    #[test]
    fn parse_flamegraph_svg_empty_svg_returns_empty() {
        assert!(parse_flamegraph_svg("<svg></svg>").is_empty());
    }

    #[test]
    fn parse_flamegraph_svg_malformed_title_skipped() {
        let svg = r#"<svg viewBox="0 0 1200 600">
<g><rect x="0" y="540" width="600" height="16"/>
<title>bad title no numbers</title></g>
</svg>"#;
        // Should not crash; malformed entries are skipped.
        let frames = parse_flamegraph_svg(svg);
        assert!(frames.is_empty());
    }

    #[test]
    fn frame_color_is_deterministic() {
        let frame = FlameFrame {
            func_name: "my_crate::foo".into(),
            short_name: "foo".into(),
            x_pct: 0.0,
            y_level: 1,
            width_pct: 0.5,
            samples: 100,
            pct: 50.0,
            file_hint: None,
        };
        let c1 = frame.color();
        let c2 = frame.color();
        assert_eq!(c1, c2);
    }

    #[test]
    fn hsv_to_rgb_pure_red() {
        let c = hsv_to_rgb(0.0, 1.0, 1.0);
        assert_eq!(c.r(), 255);
        assert!(c.g() < 5);
        assert!(c.b() < 5);
    }

    #[test]
    fn profiler_state_default_is_idle() {
        let s = ProfilerState::default();
        assert_eq!(s.status, ProfilerStatus::Idle);
        assert!(s.frames.is_empty());
        assert!((s.zoom_scale - 1.0).abs() < 0.001);
    }
}
