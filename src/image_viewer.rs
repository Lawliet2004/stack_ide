//! Image preview pane — renders PNG, JPG, GIF (first frame), WEBP, BMP, ICO.
//!
//! Decodes via the `image` crate, uploads as an egui texture, then displays
//! with fit-to-pane scaling by default. Zoom, pan, and checkerboard transparency
//! background are all supported.

use std::path::PathBuf;
use std::time::SystemTime;

use egui::{pos2, vec2, Color32, Rect, Sense, Vec2};

/// Per-pane state for the image viewer.
pub struct ImageViewerState {
    pub path: PathBuf,
    pub texture: Option<egui::TextureHandle>,
    /// Original pixel dimensions of the image.
    pub image_size: Vec2,
    /// Current zoom factor (1.0 = 100%).
    pub zoom: f32,
    /// Pan offset in image pixels.
    pub pan_offset: Vec2,
    /// Error string if loading/decoding failed.
    pub load_error: Option<String>,
    /// Last known modification time (used by FileWatcher).
    pub last_modified: SystemTime,
    /// True when we're dragging to pan.
    dragging: bool,
    drag_start_offset: Vec2,
}

impl ImageViewerState {
    pub fn new(path: PathBuf) -> Self {
        let last_modified = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        Self {
            path,
            texture: None,
            image_size: Vec2::ZERO,
            zoom: 1.0, // will be set to "fit" on first render
            pan_offset: Vec2::ZERO,
            load_error: None,
            last_modified,
            dragging: false,
            drag_start_offset: Vec2::ZERO,
        }
    }

    /// Load (or reload) the image from disk, uploading it as an egui texture.
    pub fn load(&mut self, ctx: &egui::Context) {
        self.load_error = None;
        match std::fs::read(&self.path) {
            Err(e) => {
                self.load_error = Some(format!("{}", e));
            }
            Ok(bytes) => {
                match image::load_from_memory(&bytes) {
                    Err(e) => {
                        self.load_error = Some(format!("Cannot decode image: {}", e));
                    }
                    Ok(img) => {
                        let rgba = img.to_rgba8();
                        let (w, h) = rgba.dimensions();
                        self.image_size = vec2(w as f32, h as f32);

                        let pixels: Vec<Color32> = rgba
                            .chunks_exact(4)
                            .map(|p| Color32::from_rgba_premultiplied(p[0], p[1], p[2], p[3]))
                            .collect();

                        let color_image = egui::ColorImage {
                            size: [w as usize, h as usize],
                            pixels,
                        };

                        let name = format!("image:{}", self.path.to_string_lossy());
                        self.texture = Some(ctx.load_texture(
                            &name,
                            color_image,
                            egui::TextureOptions::LINEAR,
                        ));

                        // Update modification time
                        self.last_modified = std::fs::metadata(&self.path)
                            .and_then(|m| m.modified())
                            .unwrap_or(self.last_modified);
                    }
                }
            }
        }
    }

    /// Reset zoom to fit the image in the given pane size (no upscale beyond 100%).
    pub fn fit_to_pane(&mut self, pane_size: Vec2) {
        if self.image_size.x <= 0.0 || self.image_size.y <= 0.0 {
            self.zoom = 1.0;
            return;
        }
        let scale_x = pane_size.x / self.image_size.x;
        let scale_y = pane_size.y / self.image_size.y;
        let fit_scale = scale_x.min(scale_y).min(1.0); // never scale up beyond 100%
        self.zoom = fit_scale;
        self.pan_offset = Vec2::ZERO;
    }
}

/// Render the image viewer pane.
///
/// Returns `true` if the user pressed the "Open as text" fallback (only shown on error).
pub fn render_image_viewer(
    ui: &mut egui::Ui,
    state: &mut ImageViewerState,
    palette: crate::theme::SemanticPalette,
) -> bool {
    // --- Load on first render if no texture yet ---
    if state.texture.is_none() && state.load_error.is_none() {
        state.load(ui.ctx());
        // Set initial zoom to fit
        let pane_size = ui.available_size();
        state.fit_to_pane(pane_size);
    }

    // --- Error state ---
    if let Some(ref err) = state.load_error.clone() {
        let filename = state
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let size_str = std::fs::metadata(&state.path)
            .map(|m| format!("File size: {} bytes", m.len()))
            .unwrap_or_default();
        return crate::content_error::render_content_error(
            ui,
            &format!("Cannot decode image: {}", filename),
            &format!("{}\n{}", err, size_str),
        );
    }

    let Some(ref texture) = state.texture.clone() else {
        return false;
    };

    // --- Keyboard zoom shortcuts (Ctrl+0 / Ctrl+= / Ctrl+-) ---
    let pane_size = ui.available_size();
    ui.input(|i| {
        if i.modifiers.ctrl || i.modifiers.command {
            if i.key_pressed(egui::Key::Num0) {
                state.fit_to_pane(pane_size);
            }
            if i.key_pressed(egui::Key::Equals) || i.key_pressed(egui::Key::Plus) {
                state.zoom = (state.zoom * 1.1).clamp(0.1, 32.0);
            }
            if i.key_pressed(egui::Key::Minus) {
                state.zoom = (state.zoom / 1.1).clamp(0.1, 32.0);
            }
        }
    });

    // --- Scroll wheel zoom ---
    let scroll_delta = ui.input(|i| i.raw_scroll_delta.y);
    if scroll_delta != 0.0 {
        let factor = if scroll_delta > 0.0 { 1.1 } else { 1.0 / 1.1 };
        state.zoom = (state.zoom * factor).clamp(0.1, 32.0);
    }

    // Content area with the image
    let avail = ui.available_rect_before_wrap();

    // --- Checkerboard background (for transparent images) ---
    draw_checkerboard(ui.painter(), avail);

    // --- Image display ---
    let display_w = state.image_size.x * state.zoom;
    let display_h = state.image_size.y * state.zoom;

    // Center the image in the pane
    let image_rect = if display_w <= avail.width() && display_h <= avail.height() {
        // Fits — center it
        let x = avail.left() + (avail.width() - display_w) * 0.5 + state.pan_offset.x;
        let y = avail.top() + (avail.height() - display_h) * 0.5 + state.pan_offset.y;
        Rect::from_min_size(pos2(x, y), vec2(display_w, display_h))
    } else {
        // Larger than pane — apply pan
        let x = avail.left() + state.pan_offset.x;
        let y = avail.top() + state.pan_offset.y;
        Rect::from_min_size(pos2(x, y), vec2(display_w, display_h))
    };

    let painter = ui.painter_at(avail);
    painter.image(
        texture.id(),
        image_rect,
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );

    // --- Pan interaction ---
    let interact_id = ui.make_persistent_id(("image_viewer_pan", &state.path));
    let response = ui.interact(avail, interact_id, Sense::click_and_drag());

    if response.drag_started() {
        state.dragging = true;
        state.drag_start_offset = state.pan_offset;
    }
    if response.dragged() && state.dragging {
        state.pan_offset = state.drag_start_offset + response.drag_delta();
    }
    if response.drag_stopped() {
        state.dragging = false;
    }

    // --- Status bar ---
    let zoom_pct = (state.zoom * 100.0).round() as i32;
    let file_size = std::fs::metadata(&state.path)
        .map(|m| format_bytes(m.len()))
        .unwrap_or_default();
    let filename = state
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let status = format!(
        "{} | {}×{} | {} | {}%",
        filename, state.image_size.x as u32, state.image_size.y as u32, file_size, zoom_pct
    );
    egui::TopBottomPanel::bottom("image_status_bar")
        .resizable(false)
        .exact_height(22.0)
        .show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(&status)
                        .size(11.0)
                        .color(palette.muted_text),
                );
            });
        });

    false
}

/// Draw an 8×8 px checkerboard pattern in `rect` to indicate transparency.
fn draw_checkerboard(painter: &egui::Painter, rect: Rect) {
    let tile = 8.0_f32;
    let light = Color32::from_rgb(220, 220, 220);
    let dark = Color32::from_rgb(180, 180, 180);

    let cols = ((rect.width() / tile).ceil() as i32).max(1);
    let rows = ((rect.height() / tile).ceil() as i32).max(1);

    for row in 0..rows {
        for col in 0..cols {
            let color = if (row + col) % 2 == 0 { light } else { dark };
            let x = rect.left() + col as f32 * tile;
            let y = rect.top() + row as f32 * tile;
            let w = tile.min(rect.right() - x);
            let h = tile.min(rect.bottom() - y);
            if w > 0.0 && h > 0.0 {
                painter.rect_filled(Rect::from_min_size(pos2(x, y), vec2(w, h)), 0.0, color);
            }
        }
    }
}

fn format_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{} B", n)
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    }
}
