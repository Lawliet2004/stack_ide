//! Color picker overlay — detects color literals in the active buffer's
//! visible lines and renders inline swatches in the gutter. Clicking a swatch
//! opens egui's built-in color picker; edits apply to the rope in real time.

use egui::Color32;
use regex::Regex;
use std::sync::OnceLock;

/// The format of a detected color literal. Used when writing the edited
/// color back to the source in the same notation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorFormat {
    /// `#RGB`, `#RRGGBB`, or `#RRGGBBAA`
    Hex,
    /// `rgb(r, g, b)`
    RgbFunc,
    /// `rgba(r, g, b, a)`
    RgbaFunc,
    /// `Color32::from_rgb(r, g, b)` or `Color32::from_rgba_premultiplied(r,g,b,a)`
    Color32Func,
    /// A CSS named color like `"red"`, `"blue"`, etc.
    Named,
}

/// One color span found in a source line.
#[derive(Debug, Clone)]
pub struct ColorSpan {
    pub line: usize,
    pub start_col: usize,
    /// Exclusive end column (in Rust chars).
    pub end_col: usize,
    pub color: Color32,
    pub format: ColorFormat,
}

// ─── Detection ───────────────────────────────────────────────────────────────

fn hex_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"#([0-9A-Fa-f]{8}|[0-9A-Fa-f]{6}|[0-9A-Fa-f]{3})\b").unwrap()
    })
}

fn rgb_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*(?:,\s*[\d.]+\s*)?\)").unwrap()
    })
}

fn color32_rgb_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"Color32::from_rgb\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*\)").unwrap()
    })
}

fn color32_rgba_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"Color32::from_rgba_premultiplied\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*\)",
        )
        .unwrap()
    })
}

/// Detect all color spans in `line` at the given `line_index`.
/// Only the visible portion of the buffer is scanned (call per visible line).
pub fn detect_colors_in_line(line_index: usize, line: &str) -> Vec<ColorSpan> {
    let mut spans: Vec<ColorSpan> = Vec::new();

    // Hex colors
    for cap in hex_regex().captures_iter(line) {
        let full = cap.get(0).unwrap();
        let hex_str = cap.get(1).unwrap().as_str();
        if let Some(color) = parse_hex_color(hex_str) {
            let start_col = line[..full.start()].chars().count();
            let end_col = line[..full.end()].chars().count();
            spans.push(ColorSpan {
                line: line_index,
                start_col,
                end_col,
                color,
                format: ColorFormat::Hex,
            });
        }
    }

    // rgb() / rgba()
    for cap in rgb_regex().captures_iter(line) {
        let full = cap.get(0).unwrap();
        let r: u8 = cap[1].parse().unwrap_or(0);
        let g: u8 = cap[2].parse().unwrap_or(0);
        let b: u8 = cap[3].parse().unwrap_or(0);
        let start_col = line[..full.start()].chars().count();
        let end_col = line[..full.end()].chars().count();
        let format = if full.as_str().starts_with("rgba") {
            ColorFormat::RgbaFunc
        } else {
            ColorFormat::RgbFunc
        };
        spans.push(ColorSpan {
            line: line_index,
            start_col,
            end_col,
            color: Color32::from_rgb(r, g, b),
            format,
        });
    }

    // Color32::from_rgb(...)
    for cap in color32_rgb_regex().captures_iter(line) {
        let full = cap.get(0).unwrap();
        let r: u8 = cap[1].parse().unwrap_or(0);
        let g: u8 = cap[2].parse().unwrap_or(0);
        let b: u8 = cap[3].parse().unwrap_or(0);
        let start_col = line[..full.start()].chars().count();
        let end_col = line[..full.end()].chars().count();
        spans.push(ColorSpan {
            line: line_index,
            start_col,
            end_col,
            color: Color32::from_rgb(r, g, b),
            format: ColorFormat::Color32Func,
        });
    }

    // Color32::from_rgba_premultiplied(...)
    for cap in color32_rgba_regex().captures_iter(line) {
        let full = cap.get(0).unwrap();
        let r: u8 = cap[1].parse().unwrap_or(0);
        let g: u8 = cap[2].parse().unwrap_or(0);
        let b: u8 = cap[3].parse().unwrap_or(0);
        let _a: u8 = cap[4].parse().unwrap_or(255);
        let start_col = line[..full.start()].chars().count();
        let end_col = line[..full.end()].chars().count();
        spans.push(ColorSpan {
            line: line_index,
            start_col,
            end_col,
            color: Color32::from_rgb(r, g, b),
            format: ColorFormat::Color32Func,
        });
    }

    // Deduplicate overlapping spans (keep first found)
    spans.sort_by_key(|s| s.start_col);
    let mut deduped: Vec<ColorSpan> = Vec::new();
    for span in spans {
        if let Some(last) = deduped.last() {
            if span.start_col < last.end_col {
                continue; // overlaps — skip
            }
        }
        deduped.push(span);
    }

    deduped
}

/// Format a `Color32` back to a source string in the given format.
pub fn format_color(color: Color32, format: ColorFormat) -> String {
    match format {
        ColorFormat::Hex => {
            if color.a() == 255 {
                format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b())
            } else {
                format!(
                    "#{:02X}{:02X}{:02X}{:02X}",
                    color.r(),
                    color.g(),
                    color.b(),
                    color.a()
                )
            }
        }
        ColorFormat::RgbFunc => {
            format!("rgb({}, {}, {})", color.r(), color.g(), color.b())
        }
        ColorFormat::RgbaFunc => {
            let a = color.a() as f32 / 255.0;
            format!(
                "rgba({}, {}, {}, {:.2})",
                color.r(),
                color.g(),
                color.b(),
                a
            )
        }
        ColorFormat::Color32Func => {
            if color.a() == 255 {
                format!(
                    "Color32::from_rgb({}, {}, {})",
                    color.r(),
                    color.g(),
                    color.b()
                )
            } else {
                format!(
                    "Color32::from_rgba_premultiplied({}, {}, {}, {})",
                    color.r(),
                    color.g(),
                    color.b(),
                    color.a()
                )
            }
        }
        ColorFormat::Named => {
            // Fall back to hex for edited named colors
            format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b())
        }
    }
}

// ─── Color picker popup state ─────────────────────────────────────────────────

/// Active color picker popup. Stored on `BlueIdeApp`.
#[derive(Debug, Clone)]
pub struct ColorPickerState {
    pub span: ColorSpan,
    pub current_color: Color32,
    pub pane_id: crate::panes::PaneId,
    /// The formatted string at picker open time — used to detect if the color
    /// actually changed when writing back.
    pub original_text: String,
    /// Last formatted string written (to avoid spamming rope edits on no-change).
    pub last_written: String,
}

impl ColorPickerState {
    pub fn new(span: ColorSpan, pane_id: crate::panes::PaneId) -> Self {
        let formatted = format_color(span.color, span.format);
        Self {
            current_color: span.color,
            original_text: formatted.clone(),
            last_written: formatted,
            span,
            pane_id,
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn parse_hex_color(hex: &str) -> Option<Color32> {
    match hex.len() {
        3 => {
            // #RGB → #RRGGBB
            let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
            Some(Color32::from_rgb(r, g, b))
        }
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Color32::from_rgb(r, g, b))
        }
        8 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            let a = u8::from_str_radix(&hex[6..8], 16).ok()?;
            Some(Color32::from_rgba_premultiplied(r, g, b, a))
        }
        _ => None,
    }
}
