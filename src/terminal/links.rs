//! Feature 3 — Terminal Link Detection (Ctrl+Click).
//!
//! Scans terminal output lines for URLs and file paths, renders them in teal
//! with an underline, and opens them on Ctrl+Click.

use std::path::PathBuf;

use regex::Regex;

/// A detected link (URL or file path) inside a single terminal line.
#[derive(Debug, Clone)]
pub struct DetectedLink {
    /// Byte offset of the match start within the line string.
    pub byte_start: usize,
    /// Byte offset one past the end.
    pub byte_end: usize,
    /// What kind of target this is.
    pub kind: LinkKind,
}

#[derive(Debug, Clone)]
pub enum LinkKind {
    Url(String),
    FilePath {
        path: PathBuf,
        line: Option<u32>,
        col: Option<u32>,
    },
}

// ─── Regex patterns (compiled once) ─────────────────────────────────────────

thread_local! {
    static URL_RE: Regex = Regex::new(
        r#"https?://[^\s\])\>"']+"#
    ).expect("URL regex is valid");

    static FILE_PATH_RE: Regex = {
        #[cfg(windows)]
        {
            // Windows: C:\... or .\... or ..\...
            Regex::new(
                r"(?:[A-Za-z]:\\[^\s]+|\.{1,2}[/\\][^\s]+)"
            ).expect("file path regex is valid")
        }
        #[cfg(not(windows))]
        {
            // Unix: /... or ./... or ../...
            Regex::new(
                r"(?:/[^\s]+|\.{1,2}/[^\s]+)"
            ).expect("file path regex is valid")
        }
    };
}

/// Scan a plain-text line and return all detected links, in left-to-right order.
pub fn detect_links(line: &str) -> Vec<DetectedLink> {
    let mut links: Vec<DetectedLink> = Vec::new();
    let mut covered: Vec<(usize, usize)> = Vec::new(); // already matched ranges

    // URLs first (they take priority over path matches)
    URL_RE.with(|re| {
        for m in re.find_iter(line) {
            let url = m.as_str().to_owned();
            links.push(DetectedLink {
                byte_start: m.start(),
                byte_end: m.end(),
                kind: LinkKind::Url(url),
            });
            covered.push((m.start(), m.end()));
        }
    });

    // File paths — skip regions already matched as URLs
    FILE_PATH_RE.with(|re| {
        for m in re.find_iter(line) {
            // Skip if overlapping with an already-covered URL
            if covered.iter().any(|(s, e)| m.start() < *e && m.end() > *s) {
                continue;
            }

            let raw = m.as_str();
            // Parse optional :LINE:COL suffix
            let (path_str, line_no, col_no) = parse_path_with_location(raw);
            let path = PathBuf::from(path_str);

            links.push(DetectedLink {
                byte_start: m.start(),
                byte_end: m.end(),
                kind: LinkKind::FilePath {
                    path,
                    line: line_no,
                    col: col_no,
                },
            });
        }
    });

    links.sort_by_key(|l| l.byte_start);
    links
}

/// Strip a trailing `:LINE` or `:LINE:COL` suffix and return (clean path, line, col).
fn parse_path_with_location(raw: &str) -> (&str, Option<u32>, Option<u32>) {
    // Try :LINE:COL first, then :LINE
    let mut s = raw;
    let mut line_no: Option<u32> = None;
    let mut col_no: Option<u32> = None;

    // Find the last two colon-separated numeric suffixes
    if let Some(last_colon) = s.rfind(':') {
        let suffix = &s[last_colon + 1..];
        if let Ok(col) = suffix.parse::<u32>() {
            col_no = Some(col);
            s = &s[..last_colon];
            // Now check for a line suffix
            if let Some(lc) = s.rfind(':') {
                let lsuffix = &s[lc + 1..];
                if let Ok(ln) = lsuffix.parse::<u32>() {
                    line_no = Some(ln);
                    s = &s[..lc];
                } else {
                    // col was actually a line
                    line_no = col_no.take();
                    s = &s[..last_colon];
                }
            } else {
                // Only one colon found — it was a line number
                line_no = col_no.take();
                s = &s[..last_colon];
            }
        }
    }

    (s, line_no, col_no)
}

/// The teal colour used to render link text.
pub const LINK_COLOR: egui::Color32 = egui::Color32::from_rgb(78, 201, 176);
/// Red used for "file not found" toast.
pub const FILE_NOT_FOUND_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 64, 64);

/// Render one terminal line with link highlighting and Ctrl+Click handling.
///
/// * `line_text` — plain text of the line.
/// * `line_rect` — the bounding rect of the rendered line in screen coords.
/// * `char_width` — monospace cell width in points.
/// * `line_height` — row height in points.
/// * Returns a list of `LinkAction` describing what the user triggered.
pub enum LinkAction {
    OpenUrl(String),
    OpenFile {
        path: PathBuf,
        line: Option<u32>,
        col: Option<u32>,
    },
    FileNotFound(String),
}

pub fn render_line_links(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    line_text: &str,
    line_top_left: egui::Pos2,
    char_width: f32,
    line_height: f32,
) -> Vec<LinkAction> {
    let mut actions = Vec::new();
    let is_ctrl = ctx.input(|i| i.modifiers.ctrl);

    for link in detect_links(line_text) {
        // Compute character column range (byte offset → char index for monospace)
        let char_start = line_text[..link.byte_start].chars().count();
        let char_end = line_text[..link.byte_end].chars().count();

        let x0 = line_top_left.x + char_start as f32 * char_width;
        let x1 = line_top_left.x + char_end as f32 * char_width;
        let y0 = line_top_left.y;
        let y1 = y0 + line_height;

        let link_rect =
            egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1));

        // Draw teal text colour overlay — allocate an invisible widget so egui can
        // track hover/click on the region.
        let resp = ui.allocate_rect(link_rect, egui::Sense::click());

        // Colour the matched characters teal by painting a coloured text label
        // directly via the painter.
        let matched_text: String = line_text[link.byte_start..link.byte_end].to_owned();
        let font_id = egui::FontId::monospace(13.0);
        let painter = ui.painter();
        painter.text(
            egui::pos2(x0, y0),
            egui::Align2::LEFT_TOP,
            &matched_text,
            font_id,
            LINK_COLOR,
        );

        // Underline
        painter.line_segment(
            [egui::pos2(x0, y1 - 1.0), egui::pos2(x1, y1 - 1.0)],
            egui::Stroke::new(1.0, LINK_COLOR),
        );

        // Tooltip
        if resp.hovered() {
            egui::show_tooltip_at_pointer(ctx, egui::Id::new("link_tooltip"), |ui| {
                ui.label("Ctrl+Click to open");
            });
        }

        // Handle Ctrl+Click
        if resp.clicked() && is_ctrl {
            match &link.kind {
                LinkKind::Url(url) => {
                    actions.push(LinkAction::OpenUrl(url.clone()));
                }
                LinkKind::FilePath { path, line, col } => {
                    if path.exists() {
                        actions.push(LinkAction::OpenFile {
                            path: path.clone(),
                            line: *line,
                            col: *col,
                        });
                    } else {
                        actions.push(LinkAction::FileNotFound(
                            path.to_string_lossy().into_owned(),
                        ));
                    }
                }
            }
        }
    }

    actions
}

/// Open a URL using the system default browser.
pub fn open_url(url: &str) {
    if let Err(e) = open::that(url) {
        eprintln!("[links] Failed to open URL '{}': {}", url, e);
    }
}
