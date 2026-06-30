//! Startup time instrumentation, history persistence, and breakdown panel.
//!
//! # Usage
//! ```rust,ignore
//! // In main() — before everything else:
//! let mut timer = StartupTimer::new();
//! // ...init subsystems...
//! timer.begin("Config load");
//! let config = load_config();
//! timer.end("Config load");
//! // ...more events...
//! // When first frame renders:
//! let data = timer.finish();
//! // Store data on App:
//! app.startup_data = Some(data);
//! ```
//!
//! # History persistence
//! Last 10 records are stored in `~/.config/blue-ide/startup_history.json`.
//! Writes are performed on a background thread (fire-and-forget) and never
//! block the first render.

use std::time::{Duration, Instant};

// ─── StartupCategory ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StartupCategory {
    System,
    Config,
    FileTree,
    Git,
    Lsp,
    Plugins,
    Session,
    Rendering,
    Other,
}

impl StartupCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Config => "Config",
            Self::FileTree => "File Tree",
            Self::Git => "Git",
            Self::Lsp => "LSP",
            Self::Plugins => "Plugins",
            Self::Session => "Session",
            Self::Rendering => "Rendering",
            Self::Other => "Other",
        }
    }

    /// Deterministic display color for charts and badges.
    pub fn color(self) -> egui::Color32 {
        match self {
            Self::System => egui::Color32::from_rgb(120, 120, 120),
            Self::Config => egui::Color32::from_rgb(80, 140, 220),
            Self::FileTree => egui::Color32::from_rgb(60, 180, 90),
            Self::Git => egui::Color32::from_rgb(230, 130, 50),
            Self::Lsp => egui::Color32::from_rgb(170, 80, 200),
            Self::Plugins => egui::Color32::from_rgb(220, 200, 50),
            Self::Session => egui::Color32::from_rgb(60, 190, 190),
            Self::Rendering => egui::Color32::from_rgb(220, 80, 80),
            Self::Other => egui::Color32::from_rgb(140, 140, 140),
        }
    }

    /// Infer a category from an event name prefix.
    pub fn from_name(name: &str) -> Self {
        let lower = name.to_lowercase();
        if lower.starts_with("config") || lower.starts_with("theme") {
            Self::Config
        } else if lower.starts_with("file tree") || lower.starts_with("directory") {
            Self::FileTree
        } else if lower.starts_with("git") {
            Self::Git
        } else if lower.starts_with("lsp") || lower.starts_with("rust-analyzer") {
            Self::Lsp
        } else if lower.starts_with("plugin") {
            Self::Plugins
        } else if lower.starts_with("session") || lower.starts_with("buffer") {
            Self::Session
        } else if lower.starts_with("first render") || lower.starts_with("rendering") {
            Self::Rendering
        } else if lower.starts_with("eframe") || lower.starts_with("window") || lower.starts_with("total") {
            Self::System
        } else {
            Self::Other
        }
    }
}

// ─── StartupEvent ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct StartupEvent {
    pub name: String,
    pub elapsed_since_start: Duration,
    pub duration: Duration,
    pub category: StartupCategory,
}

// ─── StartupTimer ────────────────────────────────────────────────────────────

/// Instrumentation timer for the startup sequence.
///
/// Construct at the very first line of `main()`, call `begin`/`end` around each
/// subsystem, and call `finish()` after the first render to produce [`StartupData`].
pub struct StartupTimer {
    start: Instant,
    events: Vec<StartupEvent>,
    /// Pending begin times keyed by event name.
    pending: Vec<(String, Instant)>,
}

impl StartupTimer {
    /// Create at the very first line of `main()`.
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            events: Vec::new(),
            pending: Vec::new(),
        }
    }

    /// Mark the start of a named event.
    pub fn begin(&mut self, name: impl Into<String>) {
        let name = name.into();
        self.pending.push((name, Instant::now()));
    }

    /// Mark the end of the most-recently-started event with `name`.
    pub fn end(&mut self, name: &str) {
        let now = Instant::now();
        let pos = self.pending.iter().rposition(|(n, _)| n == name);
        if let Some(idx) = pos {
            let (n, start) = self.pending.remove(idx);
            let duration = now.duration_since(start);
            let elapsed_since_start = start.duration_since(self.start);
            let category = StartupCategory::from_name(&n);
            self.events.push(StartupEvent {
                name: n,
                elapsed_since_start,
                duration,
                category,
            });
        }
    }

    /// Record a single instant event (duration = 0).
    pub fn record(&mut self, name: impl Into<String>) {
        let name = name.into();
        let now = Instant::now();
        let elapsed_since_start = now.duration_since(self.start);
        let category = StartupCategory::from_name(&name);
        self.events.push(StartupEvent {
            name,
            elapsed_since_start,
            duration: Duration::ZERO,
            category,
        });
    }

    /// Notify the timer that the LSP handshake is complete. This completes any
    /// pending "LSP handshake" event that was started earlier.
    pub fn complete_lsp_handshake(&mut self) {
        self.end("LSP handshake");
    }

    /// Finalise the timer and produce [`StartupData`]. Call after the first render.
    pub fn finish(mut self) -> StartupData {
        let total_duration = self.start.elapsed();
        // Close any events left open.
        let open_names: Vec<String> = self.pending.iter().map(|(n, _)| n.clone()).collect();
        for name in open_names {
            self.end(&name);
        }
        // Sort chronologically.
        self.events.sort_by_key(|e| e.elapsed_since_start);

        StartupData {
            events: self.events,
            total_duration,
        }
    }
}

impl Default for StartupTimer {
    fn default() -> Self {
        Self::new()
    }
}

// ─── StartupData ─────────────────────────────────────────────────────────────

/// Finalised startup timing data stored on the App after first render.
#[derive(Debug, Clone)]
pub struct StartupData {
    pub events: Vec<StartupEvent>,
    pub total_duration: Duration,
}

impl StartupData {
    /// Generate a plain-text report for clipboard export.
    pub fn text_report(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let now = chrono_naive_now();
        let _ = writeln!(&mut out, "Blue IDE Startup Report");
        let _ = writeln!(&mut out, "Generated: {now}");
        let _ = writeln!(&mut out, "Total: {:.3}s", self.total_duration.as_secs_f64());
        let _ = writeln!(&mut out, "---");
        for ev in &self.events {
            let _ = writeln!(&mut out, "{}: {}ms", ev.name, ev.duration.as_millis());
        }
        out
    }

    /// Build a [`StartupHistoryEntry`] for persistence.
    pub fn to_history_entry(&self) -> StartupHistoryEntry {
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let sum_category = |cat: StartupCategory| -> u64 {
            self.events
                .iter()
                .filter(|e| e.category == cat)
                .map(|e| e.duration.as_millis() as u64)
                .sum()
        };

        StartupHistoryEntry {
            timestamp: now_unix,
            total_ms: self.total_duration.as_millis() as u64,
            lsp_ms: sum_category(StartupCategory::Lsp),
            plugins_ms: sum_category(StartupCategory::Plugins),
            session_ms: sum_category(StartupCategory::Session),
        }
    }
}

fn chrono_naive_now() -> String {
    // We avoid pulling in chrono just for formatting.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Rough calendar format: not perfectly accurate but fine for a report.
    let secs_per_day = 86400u64;
    let days_since_epoch = secs / secs_per_day;
    let time_of_day = secs % secs_per_day;
    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;
    // Approximate date from days_since_epoch (good enough for human-readable report).
    let years_since_1970 = days_since_epoch / 365;
    let year = 1970 + years_since_1970;
    let day_of_year = days_since_epoch % 365;
    let month = (day_of_year / 30).min(11) + 1;
    let day = (day_of_year % 30) + 1;
    format!("{year}-{month:02}-{day:02} {h:02}:{m:02}:{s:02}")
}

// ─── StartupHistoryEntry ─────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StartupHistoryEntry {
    pub timestamp: i64,
    pub total_ms: u64,
    pub lsp_ms: u64,
    pub plugins_ms: u64,
    pub session_ms: u64,
}

/// Load the last N startup history records. Returns an empty vec on any error.
pub fn load_startup_history() -> Vec<StartupHistoryEntry> {
    let Some(path) = history_path() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    serde_json::from_str::<Vec<StartupHistoryEntry>>(&text).unwrap_or_default()
}

/// Append one entry and save (max 10 records). Non-blocking — spawns a background thread.
pub fn save_startup_history(new_entry: StartupHistoryEntry) {
    let Some(path) = history_path() else { return };
    std::thread::spawn(move || {
        let mut history = load_startup_history();
        history.push(new_entry);
        // Keep last 10.
        if history.len() > 10 {
            let drop = history.len() - 10;
            history.drain(..drop);
        }
        let Ok(json) = serde_json::to_string_pretty(&history) else { return };
        // Atomic write: write to temp then rename.
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    });
}

fn history_path() -> Option<std::path::PathBuf> {
    directories::BaseDirs::new()
        .map(|d| d.config_dir().join("blue-ide").join("startup_history.json"))
}

// ─── Startup breakdown panel ─────────────────────────────────────────────────

/// State for the startup breakdown floating window.
#[derive(Debug, Default)]
pub struct StartupBreakdownState {
    pub open: bool,
    pub history: Vec<StartupHistoryEntry>,
    pub history_loaded: bool,
    /// Which event row is expanded (index into StartupData.events).
    pub expanded_row: Option<usize>,
}

impl StartupBreakdownState {
    pub fn open_panel(&mut self) {
        self.open = true;
        if !self.history_loaded {
            self.history = load_startup_history();
            self.history_loaded = true;
        }
    }
}

/// Render the startup breakdown floating window.
///
/// Returns `true` if clipboard copy was requested (caller should set clipboard text).
pub fn show_startup_breakdown(
    ctx: &egui::Context,
    state: &mut StartupBreakdownState,
    data: &StartupData,
    palette: crate::theme::SemanticPalette,
) -> Option<String> {
    if !state.open {
        return None;
    }

    let mut copy_text: Option<String> = None;

    let mut is_open = state.open;
    egui::Window::new("Startup Time Breakdown")
        .id(egui::Id::new("startup_breakdown"))
        .default_size([640.0, 520.0])
        .resizable(true)
        .collapsible(false)
        .open(&mut is_open)
        .show(ctx, |ui| {
            // ── Warning banner ───────────────────────────────────────────────
            if data.total_duration.as_secs_f64() > 3.0 {
                let advice = slowest_category_advice(data);
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(80, 70, 0))
                    .inner_margin(egui::Margin::same(8.0))
                    .show(ui, |ui| {
                        ui.colored_label(
                            egui::Color32::from_rgb(255, 220, 60),
                            format!(
                                "⚠ Startup took {:.1}s. {}",
                                data.total_duration.as_secs_f64(),
                                advice
                            ),
                        );
                    });
                ui.add_space(4.0);
            }

            // Title row with total duration and copy button.
            ui.horizontal(|ui| {
                ui.heading(format!(
                    "Total: {:.3}s",
                    data.total_duration.as_secs_f64()
                ));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("📋 Copy Report").clicked() {
                        copy_text = Some(data.text_report());
                    }
                });
            });
            ui.separator();

            // ── Two-column layout: event list + donut summary ────────────────
            ui.horizontal(|ui| {
                // Left: event timeline list (350 px).
                ui.allocate_ui(egui::vec2(390.0, 360.0), |ui| {
                    show_event_list(ui, data, state, palette);
                });

                ui.separator();

                // Right: category summary.
                ui.allocate_ui(egui::vec2(200.0, 360.0), |ui| {
                    show_category_summary(ui, data, palette);
                });
            });

            // ── Startup history bar chart ────────────────────────────────────
            if !state.history.is_empty() {
                ui.separator();
                ui.collapsing("Startup History (last 10 runs)", |ui| {
                    show_history_chart(ui, &state.history, palette);
                });
            }
        });
    state.open = is_open;
 
    copy_text
}

fn show_event_list(
    ui: &mut egui::Ui,
    data: &StartupData,
    state: &mut StartupBreakdownState,
    _palette: crate::theme::SemanticPalette,
) {
    let max_dur_ms = data
        .events
        .iter()
        .map(|e| e.duration.as_millis())
        .max()
        .unwrap_or(1)
        .max(1) as f32;

    egui::ScrollArea::vertical()
        .id_source("startup_event_list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (idx, event) in data.events.iter().enumerate() {
                let expanded = state.expanded_row == Some(idx);
                let cat_color = event.category.color();
                let dur_ms = event.duration.as_millis();
                let dur_text = if event.duration.as_secs() >= 1 {
                    format!("{:.2}s", event.duration.as_secs_f64())
                } else {
                    format!("{}ms", dur_ms)
                };

                let row_resp = ui
                    .horizontal(|ui| {
                        // Category dot.
                        let (dot_rect, _) =
                            ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot_rect.center(), 4.0, cat_color);

                        // Event name.
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&event.name)
                                    .monospace()
                                    .size(11.0),
                            )
                            .wrap(false),
                        );

                        // Duration bar.
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(&dur_text)
                                    .monospace()
                                    .size(11.0)
                                    .color(egui::Color32::GRAY),
                            );
                            let bar_w = (dur_ms as f32 / max_dur_ms * 120.0).max(1.0);
                            let (bar_rect, _) = ui.allocate_exact_size(
                                egui::vec2(bar_w, 10.0),
                                egui::Sense::hover(),
                            );
                            ui.painter().rect_filled(bar_rect, 2.0, cat_color.gamma_multiply(0.7));
                        });
                    })
                    .response;

                if row_resp.interact(egui::Sense::click()).clicked() {
                    state.expanded_row = if expanded { None } else { Some(idx) };
                }

                if expanded {
                    egui::Frame::none()
                        .inner_margin(egui::Margin::symmetric(20.0, 2.0))
                        .fill(egui::Color32::from_black_alpha(30))
                        .show(ui, |ui| {
                            ui.label(format!("Category: {}", event.category.label()));
                            ui.label(format!(
                                "Start: +{:.1}ms from main()",
                                event.elapsed_since_start.as_secs_f64() * 1000.0
                            ));
                            ui.label(format!("Duration: {}ms", dur_ms));
                        });
                }
            }
        });
}

fn show_category_summary(
    ui: &mut egui::Ui,
    data: &StartupData,
    _palette: crate::theme::SemanticPalette,
) {
    use std::collections::HashMap;
    let mut totals: HashMap<StartupCategory, Duration> = HashMap::new();
    for event in &data.events {
        *totals.entry(event.category).or_insert(Duration::ZERO) += event.duration;
    }

    let mut cats: Vec<(StartupCategory, Duration)> = totals.into_iter().collect();
    cats.sort_by(|a, b| b.1.cmp(&a.1));

    ui.label(egui::RichText::new("By Category").strong());
    ui.add_space(4.0);

    // Simple donut chart using painter.
    let size = 120.0f32;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let center = rect.center();
    let outer_r = size * 0.45;
    let inner_r = size * 0.25;

    let total_secs: f64 = cats.iter().map(|(_, d)| d.as_secs_f64()).sum();
    if total_secs > 0.0 {
        let mut angle = -std::f32::consts::FRAC_PI_2; // Start at top.
        for (cat, dur) in &cats {
            let sweep = (dur.as_secs_f64() / total_secs) as f32 * std::f32::consts::TAU;
            // Draw arc as a fan of thin lines.
            let steps = ((sweep * outer_r) as usize).max(4);
            let color = cat.color();
            let step = sweep / steps as f32;
            for i in 0..steps {
                let a0 = angle + i as f32 * step;
                let a1 = a0 + step;
                let p0 = center + outer_r * egui::vec2(a0.cos(), a0.sin());
                let p1 = center + outer_r * egui::vec2(a1.cos(), a1.sin());
                let p2 = center + inner_r * egui::vec2(a1.cos(), a1.sin());
                let p3 = center + inner_r * egui::vec2(a0.cos(), a0.sin());
                ui.painter().add(egui::Shape::convex_polygon(
                    vec![p0, p1, p2, p3],
                    color,
                    egui::Stroke::NONE,
                ));
            }
            angle += sweep;
        }
    }

    // Center text.
    ui.painter().text(
        center,
        egui::Align2::CENTER_CENTER,
        format!("{:.1}s", data.total_duration.as_secs_f64()),
        egui::FontId::proportional(11.0),
        egui::Color32::WHITE,
    );

    // Legend.
    ui.add_space(4.0);
    for (cat, dur) in &cats {
        ui.horizontal(|ui| {
            let (dot_rect, _) =
                ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(dot_rect.center(), 4.0, cat.color());
            ui.label(
                egui::RichText::new(format!(
                    "{}: {:.0}ms",
                    cat.label(),
                    dur.as_secs_f64() * 1000.0
                ))
                .size(11.0),
            );
        });
    }
}

fn show_history_chart(
    ui: &mut egui::Ui,
    history: &[StartupHistoryEntry],
    _palette: crate::theme::SemanticPalette,
) {
    let chart_w = 360.0f32;
    let chart_h = 60.0f32;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(chart_w, chart_h), egui::Sense::hover());

    let max_ms = history.iter().map(|e| e.total_ms).max().unwrap_or(1).max(1) as f32;
    let n = history.len();
    let bar_w = (chart_w / n as f32 - 2.0).max(4.0);

    for (i, entry) in history.iter().enumerate() {
        let bar_h = (entry.total_ms as f32 / max_ms * chart_h).max(2.0);
        let x = rect.left() + i as f32 * (chart_w / n as f32);
        let bar_rect = egui::Rect::from_min_size(
            egui::pos2(x, rect.bottom() - bar_h),
            egui::vec2(bar_w, bar_h),
        );
        let color = if entry.total_ms > 3000 {
            egui::Color32::from_rgb(220, 80, 80)
        } else if entry.total_ms > 1500 {
            egui::Color32::from_rgb(220, 180, 50)
        } else {
            egui::Color32::from_rgb(60, 180, 90)
        };
        ui.painter().rect_filled(bar_rect, 1.0, color);
        ui.painter().text(
            egui::pos2(x + bar_w * 0.5, rect.bottom() + 8.0),
            egui::Align2::CENTER_TOP,
            format!("{:.1}s", entry.total_ms as f32 / 1000.0),
            egui::FontId::proportional(9.0),
            egui::Color32::GRAY,
        );
    }
}

fn slowest_category_advice(data: &StartupData) -> &'static str {
    use std::collections::HashMap;
    let mut totals: HashMap<StartupCategory, Duration> = HashMap::new();
    for ev in &data.events {
        *totals.entry(ev.category).or_insert(Duration::ZERO) += ev.duration;
    }
    let slowest = totals
        .into_iter()
        .max_by_key(|(_, d)| d.as_millis());

    match slowest.map(|(c, _)| c) {
        Some(StartupCategory::Lsp) => {
            "rust-analyzer startup is slow. This is normal for first run or large projects."
        }
        Some(StartupCategory::Plugins) => {
            "Plugins are slow. Check which plugin is slowest and disable it if not needed."
        }
        Some(StartupCategory::Session) => {
            "Restoring session files is slow. Reduce the number of open tabs for faster startup."
        }
        Some(StartupCategory::FileTree) => {
            "Directory scan is slow. Add large directories to the exclude list in Settings."
        }
        _ => "Review the event list below for the bottleneck.",
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_timer_records_events_in_order() {
        let mut timer = StartupTimer::new();
        timer.begin("Config load");
        std::thread::sleep(Duration::from_millis(1));
        timer.end("Config load");

        timer.begin("Git init");
        std::thread::sleep(Duration::from_millis(1));
        timer.end("Git init");

        let data = timer.finish();
        assert_eq!(data.events.len(), 2);
        assert_eq!(data.events[0].name, "Config load");
        assert_eq!(data.events[1].name, "Git init");
        assert!(data.events[0].duration >= Duration::from_millis(1));
    }

    #[test]
    fn startup_timer_infers_correct_categories() {
        assert_eq!(StartupCategory::from_name("Config load"), StartupCategory::Config);
        assert_eq!(StartupCategory::from_name("Git init"), StartupCategory::Git);
        assert_eq!(StartupCategory::from_name("LSP spawn"), StartupCategory::Lsp);
        assert_eq!(StartupCategory::from_name("Plugin discovery"), StartupCategory::Plugins);
        assert_eq!(StartupCategory::from_name("Session restore"), StartupCategory::Session);
        assert_eq!(StartupCategory::from_name("File tree scan"), StartupCategory::FileTree);
        assert_eq!(StartupCategory::from_name("First render"), StartupCategory::Rendering);
        assert_eq!(StartupCategory::from_name("Widget init"), StartupCategory::Other);
    }

    #[test]
    fn startup_data_text_report_contains_event_names() {
        let data = StartupData {
            events: vec![StartupEvent {
                name: "Config load".into(),
                elapsed_since_start: Duration::from_millis(0),
                duration: Duration::from_millis(42),
                category: StartupCategory::Config,
            }],
            total_duration: Duration::from_millis(500),
        };
        let report = data.text_report();
        assert!(report.contains("Config load"));
        assert!(report.contains("42ms"));
        assert!(report.contains("Total:"));
    }

    #[test]
    fn history_entry_round_trips_through_json() {
        let entry = StartupHistoryEntry {
            timestamp: 1_700_000_000,
            total_ms: 1234,
            lsp_ms: 500,
            plugins_ms: 100,
            session_ms: 200,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: StartupHistoryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.total_ms, 1234);
        assert_eq!(back.lsp_ms, 500);
    }
}
