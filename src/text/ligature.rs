//! Ligature-aware text rendering using `cosmic-text`.
//!
//! This module provides a renderer that shapes text with OpenType features
//! enabled, allowing coding ligature fonts (Fira Code, JetBrains Mono, etc.)
//! to combine sequences like `->`, `!=`, and `>=` into single glyphs.

use cosmic_text::{
    Attrs, Buffer, Color as CosmicColor, Family, FontSystem, Metrics, Shaping, SwashCache,
    SwashContent,
};
use egui::{Color32, Context, Painter, Pos2, Rect, TextureOptions, Vec2};
use std::sync::atomic::{AtomicU64, Ordering};

static TEXTURE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_texture_name() -> String {
    format!(
        "blue-ide-ligature-{}",
        TEXTURE_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn egui_to_cosmic_color(c: Color32) -> CosmicColor {
    CosmicColor::rgba(c.r(), c.g(), c.b(), c.a())
}

/// Renderer that shapes and rasterizes text with coding ligatures enabled.
///
/// The renderer owns a single `FontSystem` and `SwashCache` so that system
/// fonts are loaded once and glyph rasterization is cached across frames.
pub struct LigatureRenderer {
    font_system: FontSystem,
    swash_cache: SwashCache,
}

impl Default for LigatureRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl LigatureRenderer {
    /// Create a new renderer. This loads the system font database, so it
    /// should be created once and reused.
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
        }
    }

    /// Render a single colored text run with ligature shaping.
    ///
    /// `baseline_pos` is the position where the text baseline should be
    /// placed. The returned value is the total advance width of the run in
    /// egui logical pixels.
    ///
    /// When `monospace_width` is provided, glyphs are forced to that advance
    /// so that shaped text stays grid-aligned with the rest of the UI.
    pub fn render_run(
        &mut self,
        ctx: &Context,
        painter: &Painter,
        text: &str,
        font_size: f32,
        line_height: f32,
        color: Color32,
        baseline_pos: Pos2,
        monospace_width: Option<f32>,
    ) -> f32 {
        if text.is_empty() {
            return 0.0;
        }

        let pixels_per_point = ctx.pixels_per_point();
        let physical_font_size = font_size * pixels_per_point;
        let physical_line_height = line_height * pixels_per_point;

        let metrics = Metrics::new(physical_font_size, physical_line_height);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        if let Some(width) = monospace_width {
            buffer.set_monospace_width(&mut self.font_system, Some(width * pixels_per_point));
        }
        let attrs = Attrs::new()
            .family(Family::Monospace)
            .color(egui_to_cosmic_color(color));
        buffer.set_text(&mut self.font_system, text, attrs, Shaping::Advanced);

        let runs: Vec<_> = buffer.layout_runs().collect();
        if runs.is_empty() {
            return 0.0;
        }

        struct GlyphImage {
            x: i32,
            y: i32,
            placement: cosmic_text::Placement,
            data: Vec<u8>,
            content: SwashContent,
        }

        let mut glyph_images = Vec::new();
        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;
        let mut total_advance = 0.0f32;

        for run in runs {
            for glyph in run.glyphs {
                let physical = glyph.physical((0.0, 0.0), 1.0);
                if let Some(image) = self
                    .swash_cache
                    .get_image(&mut self.font_system, physical.cache_key)
                {
                    if image.placement.width > 0 && image.placement.height > 0 {
                        let gx = physical.x + image.placement.left;
                        let gy = physical.y - image.placement.top;
                        min_x = min_x.min(gx);
                        min_y = min_y.min(gy);
                        max_x = max_x.max(gx + image.placement.width as i32);
                        max_y = max_y.max(gy + image.placement.height as i32);
                        glyph_images.push(GlyphImage {
                            x: gx,
                            y: gy,
                            placement: image.placement,
                            data: image.data.clone(),
                            content: image.content,
                        });
                    }
                }
                total_advance = total_advance.max(glyph.x + glyph.w);
            }
        }

        if glyph_images.is_empty() {
            return total_advance / pixels_per_point;
        }

        let width = (max_x - min_x).max(0) as usize;
        let height = (max_y - min_y).max(0) as usize;
        if width == 0 || height == 0 {
            return total_advance / pixels_per_point;
        }

        let mut pixels = vec![Color32::TRANSPARENT; width * height];

        for glyph in glyph_images {
            let offset_x = (glyph.x - min_x) as usize;
            let offset_y = (glyph.y - min_y) as usize;
            match glyph.content {
                SwashContent::Mask => {
                    for y in 0..glyph.placement.height as usize {
                        for x in 0..glyph.placement.width as usize {
                            let alpha = glyph.data[y * glyph.placement.width as usize + x];
                            let dst = &mut pixels[(offset_y + y) * width + (offset_x + x)];
                            *dst = Color32::from_rgba_unmultiplied(
                                color.r(),
                                color.g(),
                                color.b(),
                                alpha,
                            );
                        }
                    }
                }
                SwashContent::Color => {
                    for y in 0..glyph.placement.height as usize {
                        for x in 0..glyph.placement.width as usize {
                            let idx = (y * glyph.placement.width as usize + x) * 4;
                            let r = glyph.data[idx];
                            let g = glyph.data[idx + 1];
                            let b = glyph.data[idx + 2];
                            let a = glyph.data[idx + 3];
                            let dst = &mut pixels[(offset_y + y) * width + (offset_x + x)];
                            *dst = Color32::from_rgba_premultiplied(r, g, b, a);
                        }
                    }
                }
                SwashContent::SubpixelMask => {
                    // Subpixel masks are not implemented here; treat as a
                    // normal alpha mask by averaging the RGB channels.
                    for y in 0..glyph.placement.height as usize {
                        for x in 0..glyph.placement.width as usize {
                            let idx = (y * glyph.placement.width as usize + x) * 3;
                            let a = ((glyph.data[idx] as u16
                                + glyph.data[idx + 1] as u16
                                + glyph.data[idx + 2] as u16)
                                / 3) as u8;
                            let dst = &mut pixels[(offset_y + y) * width + (offset_x + x)];
                            *dst =
                                Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a);
                        }
                    }
                }
            }
        }

        let color_image = egui::ColorImage {
            size: [width, height],
            pixels,
        };

        let texture = ctx.load_texture(unique_texture_name(), color_image, TextureOptions::NEAREST);
        let logical_size = Vec2::new(width as f32 / pixels_per_point, height as f32 / pixels_per_point);
        let logical_origin = Vec2::new(min_x as f32 / pixels_per_point, min_y as f32 / pixels_per_point);
        let screen_rect = Rect::from_min_size(baseline_pos + logical_origin, logical_size);

        painter.image(
            texture.id(),
            screen_rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );

        total_advance / pixels_per_point
    }
}
