//! Minimap — a rasterized pixel-buffer overview of the entire file.
//!
//! # Architecture
//!
//! `MinimapState` is stored per pane (in `App::minimap_states`).
//! The texture is built once per content change (dirty flag), never on scroll.
//! Viewport overlay, diagnostic lines, diff hunk lines, and cursor indicator are
//! painted as cheap `egui::Painter` calls every frame — they are NOT baked into
//! the texture.
//!
//! # Texture strategy
//! - Width: `MINIMAP_WIDTH` (100 px)
//! - Height: `total_visible_lines * MINIMAP_LINE_HEIGHT` (2 px/line), capped at `MAX_MINIMAP_HEIGHT`
//! - For large files a sample_factor skips lines so the texture stays within the cap.
//! - Each non-whitespace character → a 1×2 px rect coloured with its syntax highlight
//!   colour (alpha reduced to 180) from the cached `LayoutJob`.
//! - Whitespace chars → editor background (transparent gap).
//! - `TextureOptions::NEAREST` — no bilinear blur.

use egui::{pos2, vec2, Color32, ColorImage, Painter, Rect, Sense, Stroke, TextureHandle};

use crate::editor::buffer::TextBuffer;
use crate::git::{DiffHunk, HunkKind};
use crate::lsp::types::{DiagnosticSeverity, LspDiagnostic};
use crate::theme::ThemePalette;

// ─── constants ────────────────────────────────────────────────────────────────

pub const MINIMAP_WIDTH: usize = 100;
pub const MINIMAP_LINE_HEIGHT: f32 = 2.0;
pub const MAX_MINIMAP_HEIGHT: usize = 4096;
/// Pane widths narrower than this auto-hide the minimap.
pub const MINIMAP_AUTO_HIDE_WIDTH: f32 = 300.0;

// ─── MinimapState ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct MinimapState {
    /// User-toggled visibility (Ctrl+Shift+M).  Does not change on auto-hide.
    pub visible: bool,
    pub texture: Option<TextureHandle>,
    pub texture_dirty: bool,
    pub last_line_count: usize,
    /// Last known buffer revision — used to detect in-line edits that don't
    /// change line count (e.g. renaming a variable on a single line).
    last_revision: u64,
    /// Set by click/drag; consumed by `EditorWidget` to drive `ScrollArea`.
    pub desired_scroll_y: Option<f32>,
    pub dragging: bool,
    /// Cached texture height in pixels (set during rebuild).
    pub texture_height: usize,
}

impl MinimapState {
    pub fn new() -> Self {
        Self {
            visible: false,
            texture: None,
            texture_dirty: true,
            last_line_count: 0,
            last_revision: 0,
            desired_scroll_y: None,
            dragging: false,
            texture_height: 0,
        }
    }

    /// Mark texture as needing rebuild (buffer content/fold state changed).
    pub fn invalidate(&mut self) {
        self.texture_dirty = true;
    }

    /// Toggle user-controlled visibility.
    pub fn toggle_visible(&mut self) {
        self.visible = !self.visible;
    }

    /// Rebuild the minimap texture from `buffer` if the dirty flag is set or
    /// the line count or buffer revision changed.  Returns `true` when a
    /// rebuild happened.
    ///
    /// `pane_id` is used to name the GPU texture so each pane has its own slot.
    pub fn rebuild_if_needed(
        &mut self,
        ctx: &egui::Context,
        buffer: &mut TextBuffer,
        palette: ThemePalette,
        pane_id: u64,
    ) -> bool {
        let line_count = buffer.len_lines();
        let revision = buffer.revision();
        if self.last_line_count == line_count
            && self.last_revision == revision
            && !self.texture_dirty
        {
            return false;
        }

        self.texture_dirty = false;
        self.last_line_count = line_count;
        self.last_revision = revision;

        // Build the pixel buffer with syntax colours from the cached LayoutJob.
        let font_id = egui::FontId::monospace(1.0); // tiny — only used to get/cache the layout
        let layout_job = buffer.get_layout_with_palette(font_id, palette.syntax);
        let (pixels, width, height) =
            build_minimap_texture(buffer, &layout_job, palette, line_count);
        self.texture_height = height;

        let texture_name = format!("minimap_{pane_id}");
        // Drop old handle first so GPU memory is freed before re-allocation.
        self.texture = None;

        if width > 0 && height > 0 {
            let texture = ctx.load_texture(
                &texture_name,
                ColorImage {
                    pixels,
                    size: [width, height],
                },
                egui::TextureOptions {
                    magnification: egui::TextureFilter::Nearest,
                    minification: egui::TextureFilter::Nearest,
                    wrap_mode: egui::TextureWrapMode::ClampToEdge,
                },
            );
            self.texture = Some(texture);
        }
        true
    }

    /// Draw the minimap panel inside `minimap_rect`.
    ///
    /// Returns `Some(target_scroll_y)` when the user clicked/dragged and the
    /// editor should scroll to that position.
    ///
    /// # Arguments
    /// - `painter`            – egui painter clipped to the editor panel
    /// - `minimap_rect`       – allocated screen rect for the 100-px strip
    /// - `scroll_y`           – current editor scroll offset in content-space pixels
    /// - `visible_height`     – height of the editor viewport in pixels
    /// - `total_line_count`   – total *visible* (non-folded) lines
    /// - `row_height`         – pixels per line in the editor (with padding)
    /// - `diagnostics`        – current LSP diagnostics for the file
    /// - `diff_hunks`         – git diff hunks for the file
    /// - `cursor_line`        – 0-based line index of the editor cursor
    /// - `ui`                 – used for interaction (`interact`) and tooltip
    /// - `palette`            – active colour theme
    /// - `path`               – buffer path (used to key the interaction id)
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        painter: &Painter,
        minimap_rect: Rect,
        scroll_y: f32,
        visible_height: f32,
        total_line_count: usize,
        row_height: f32,
        diagnostics: &[LspDiagnostic],
        diff_hunks: &[DiffHunk],
        cursor_line: usize,
        ui: &mut egui::Ui,
        palette: ThemePalette,
        path: Option<&std::path::PathBuf>,
    ) -> Option<f32> {
        // ── Background ──────────────────────────────────────────────────────
        painter.rect_filled(minimap_rect, 0.0, palette.semantic.panel_background);

        // Left border
        painter.line_segment(
            [minimap_rect.left_top(), minimap_rect.left_bottom()],
            Stroke::new(1.0, palette.semantic.border),
        );

        let minimap_h = minimap_rect.height();
        let minimap_tex_h = self.texture_height as f32;

        // ── Texture ─────────────────────────────────────────────────────────
        if let Some(texture) = &self.texture {
            painter.image(
                texture.id(),
                minimap_rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }

        // ── Viewport overlay rect ────────────────────────────────────────────
        let (vp_top, vp_h) = calculate_viewport_rect(
            scroll_y,
            visible_height,
            total_line_count,
            minimap_h,
            row_height,
        );
        let vp_rect = Rect::from_min_size(
            pos2(minimap_rect.left(), minimap_rect.top() + vp_top),
            vec2(minimap_rect.width(), vp_h.max(2.0)),
        );
        let vp_rect = clamp_rect_to(vp_rect, minimap_rect);
        // The viewport is a scrim over the minimap's own surface rather than a fixed
        // white overlay, so it stays visible on light themes too.
        let minimap_surface = palette.semantic.panel_background;
        painter.rect_filled(
            vp_rect,
            0.0,
            crate::chrome::scrim_over(minimap_surface, 0.07),
        );
        painter.rect_stroke(
            vp_rect,
            0.0,
            Stroke::new(
                1.0,
                crate::chrome::scrim_over(minimap_surface, 0.26),
            ),
        );

        // ── Cursor indicator line ─────────────────────────────────────────────
        if total_line_count > 0 {
            let cursor_y =
                minimap_rect.top() + (cursor_line as f32 / total_line_count as f32) * minimap_h;
            let cursor_y = cursor_y.clamp(minimap_rect.top(), minimap_rect.bottom() - 1.0);
            painter.line_segment(
                [
                    pos2(minimap_rect.left(), cursor_y),
                    pos2(minimap_rect.right(), cursor_y),
                ],
                Stroke::new(1.0, palette.semantic.primary_text),
            );
        }

        // ── Diagnostic overlays (full-width horizontal lines) ─────────────────
        for diag in diagnostics {
            let line = diag.line_start as usize;
            if line >= total_line_count {
                continue;
            }
            let y = minimap_rect.top() + (line as f32 / total_line_count as f32) * minimap_h;
            let y = y.clamp(minimap_rect.top(), minimap_rect.bottom() - 1.0);
            let color = diagnostic_overlay_color(diag.severity);
            painter.line_segment(
                [pos2(minimap_rect.left(), y), pos2(minimap_rect.right(), y)],
                Stroke::new(1.0, color),
            );
        }

        // ── Git diff hunk overlays (3-px left-edge strip) ─────────────────────
        for hunk in diff_hunks {
            if hunk.line_start >= total_line_count {
                continue;
            }
            let y_top =
                minimap_rect.top() + (hunk.line_start as f32 / total_line_count as f32) * minimap_h;
            let hunk_end = (hunk.line_start + hunk.line_count).min(total_line_count);
            let y_bottom =
                minimap_rect.top() + (hunk_end as f32 / total_line_count as f32) * minimap_h;
            let y_top = y_top.clamp(minimap_rect.top(), minimap_rect.bottom());
            let y_bottom = (y_bottom + 1.0).clamp(y_top + 1.0, minimap_rect.bottom());

            let color = match hunk.kind {
                HunkKind::Added => Color32::from_rgba_unmultiplied(80, 200, 80, 220),
                HunkKind::Modified => Color32::from_rgba_unmultiplied(80, 140, 255, 220),
                HunkKind::Removed => Color32::from_rgba_unmultiplied(255, 80, 80, 220),
            };
            // Paint a 3-px strip on the left edge
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(minimap_rect.left(), y_top),
                    pos2(minimap_rect.left() + 3.0, y_bottom),
                ),
                0.0,
                color,
            );
        }

        // ── Interaction: click / drag ─────────────────────────────────────────
        let interaction_id = ui.make_persistent_id(("minimap", path));
        let minimap_response = ui.interact(minimap_rect, interaction_id, Sense::click_and_drag());

        // Hover tooltip: "Ln NNN"
        if minimap_response.hovered() {
            if let Some(ptr) = ui.input(|i| i.pointer.hover_pos()) {
                let click_y = (ptr.y - minimap_rect.top()).clamp(0.0, minimap_h);
                let hovered_line = if total_line_count > 0 && minimap_h > 0.0 {
                    ((click_y / minimap_h) * total_line_count as f32).floor() as usize + 1
                // 1-based display
                } else {
                    1
                };
                // show tooltip only if not clicking/dragging
                if !minimap_response.dragged() {
                    egui::show_tooltip_at_pointer(
                        ui.ctx(),
                        egui::Id::new(("minimap_tooltip", path)),
                        |tip_ui| {
                            tip_ui.label(format!("Ln {hovered_line}"));
                        },
                    );
                }
            }
        }

        // Update dragging state
        if minimap_response.drag_started() {
            self.dragging = true;
        }
        if minimap_response.drag_stopped() || !minimap_response.dragged() {
            self.dragging = false;
        }

        // Map click/drag Y → scroll offset
        let mut new_scroll = None;
        if minimap_response.clicked() || minimap_response.dragged() {
            if let Some(ptr) = minimap_response.interact_pointer_pos() {
                let click_y = (ptr.y - minimap_rect.top()).clamp(0.0, minimap_h);
                // `minimap_tex_h` maps to `total_lines * MINIMAP_LINE_HEIGHT`.
                // But we display the texture scaled to `minimap_h`.
                // Use the display height directly for the fraction.
                let effective_h = if minimap_tex_h > 0.0 { minimap_h } else { 1.0 };
                let offset =
                    click_to_scroll_offset(click_y, effective_h, total_line_count, row_height);
                new_scroll = Some(offset);
            }
        }

        new_scroll
    }
}

// ─── Pixel-buffer builder ────────────────────────────────────────────────────

/// Build the minimap pixel buffer.
///
/// Walks the `LayoutJob` sections (each with a byte range and colour) to assign
/// per-character syntax colours.  Only non-whitespace chars are painted;
/// whitespace falls back to the editor background colour.
fn build_minimap_texture(
    buffer: &TextBuffer,
    layout_job: &egui::text::LayoutJob,
    palette: ThemePalette,
    line_count: usize,
) -> (Vec<Color32>, usize, usize) {
    let line_count = line_count.max(1);

    // Count visible (non-folded) lines
    let visible_count = (0..line_count)
        .filter(|&l| buffer.is_line_visible(l))
        .count()
        .max(1);

    // How many px-rows the texture would need without capping
    let raw_h = visible_count as f32 * MINIMAP_LINE_HEIGHT;
    // Sample factor: 1 means no skipping, 2 means every other line, etc.
    let sample_factor = if raw_h > MAX_MINIMAP_HEIGHT as f32 {
        (raw_h / MAX_MINIMAP_HEIGHT as f32).ceil() as usize
    } else {
        1
    };

    let sampled_lines = (visible_count + sample_factor - 1) / sample_factor;
    let tex_h = ((sampled_lines as f32 * MINIMAP_LINE_HEIGHT).ceil() as usize).max(1);
    let tex_w = MINIMAP_WIDTH;

    let bg = palette.semantic.editor_background;
    let mut pixels = vec![bg; tex_w * tex_h];

    // Build a per-byte colour lookup from the LayoutJob.
    // For large files this stays fast because sections are contiguous and few.
    let text_len = layout_job
        .sections
        .last()
        .map(|s| s.byte_range.end)
        .unwrap_or(0);
    let mut byte_color: Vec<Color32> = vec![palette.syntax.default; text_len.max(1)];
    for section in &layout_job.sections {
        let color = section.format.color;
        let start = section.byte_range.start.min(text_len);
        let end = section.byte_range.end.min(text_len);
        for slot in &mut byte_color[start..end] {
            *slot = color;
        }
    }

    // Walk visible lines with sampling
    let mut visual_row: usize = 0;
    let mut visible_idx: usize = 0;
    for line_idx in 0..line_count {
        if !buffer.is_line_visible(line_idx) {
            continue;
        }
        // Sampling: only keep every sample_factor-th visible line
        let should_paint = visible_idx % sample_factor == 0;
        visible_idx += 1;
        if !should_paint {
            // Still advance the visual row counter so spacing stays correct
            // (don't advance — sampled lines collapse height proportionally)
            continue;
        }

        let py_start = (visual_row as f32 * MINIMAP_LINE_HEIGHT) as usize;
        visual_row += 1;

        let line_text = match buffer.line_text(line_idx) {
            Some(t) => t,
            None => continue,
        };

        // Byte offset of line start in the full source string
        let line_start_byte = buffer
            .position_to_byte_index(crate::editor::buffer::CursorPosition {
                line: line_idx,
                col: 0,
            })
            .unwrap_or(0);

        let mut px_x: usize = 0;
        for (char_byte_offset, ch) in line_text.char_indices() {
            if px_x >= tex_w {
                break;
            }
            if ch.is_whitespace() {
                px_x += 1;
                continue;
            }

            let abs_byte = line_start_byte + char_byte_offset;
            let color = if abs_byte < byte_color.len() {
                let c = byte_color[abs_byte];
                Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 180)
            } else {
                Color32::from_rgba_unmultiplied(
                    palette.syntax.default.r(),
                    palette.syntax.default.g(),
                    palette.syntax.default.b(),
                    180,
                )
            };

            for py in 0..(MINIMAP_LINE_HEIGHT.ceil() as usize) {
                let py_abs = py_start + py;
                if py_abs < tex_h {
                    let idx = py_abs * tex_w + px_x;
                    if idx < pixels.len() {
                        pixels[idx] = color;
                    }
                }
            }
            px_x += 1;
        }
    }

    (pixels, tex_w, tex_h)
}

// ─── Viewport rect helpers ────────────────────────────────────────────────────

/// Calculate the minimap viewport rect `(top_offset, height)` in minimap pixels.
pub fn calculate_viewport_rect(
    scroll_y: f32,
    visible_height: f32,
    total_line_count: usize,
    minimap_height: f32,
    row_height: f32,
) -> (f32, f32) {
    if total_line_count == 0 || minimap_height == 0.0 || row_height == 0.0 {
        return (0.0, minimap_height);
    }
    let total_content_h = total_line_count as f32 * row_height;
    let vp_top = (scroll_y / total_content_h) * minimap_height;
    let vp_h = (visible_height / total_content_h) * minimap_height;
    (vp_top.max(0.0), vp_h.clamp(2.0, minimap_height))
}

/// Map a click Y within minimap display space to a target editor scroll offset.
pub fn click_to_scroll_offset(
    click_y: f32,
    minimap_display_height: f32,
    total_line_count: usize,
    row_height: f32,
) -> f32 {
    if minimap_display_height == 0.0 || total_line_count == 0 {
        return 0.0;
    }
    let fraction = (click_y / minimap_display_height).clamp(0.0, 1.0);
    let target_line = (fraction * total_line_count as f32).floor() as usize;
    let target_line = target_line.min(total_line_count.saturating_sub(1));
    target_line as f32 * row_height
}

// ─── Private helpers ──────────────────────────────────────────────────────────

fn clamp_rect_to(r: Rect, bounds: Rect) -> Rect {
    let top = r.top().max(bounds.top());
    let bottom = r.bottom().min(bounds.bottom());
    Rect::from_min_max(
        pos2(bounds.left(), top),
        pos2(bounds.right(), bottom.max(top + 1.0)),
    )
}

fn diagnostic_overlay_color(severity: DiagnosticSeverity) -> Color32 {
    match severity {
        DiagnosticSeverity::Error => Color32::from_rgba_unmultiplied(255, 80, 80, 200),
        DiagnosticSeverity::Warning => Color32::from_rgba_unmultiplied(255, 200, 50, 200),
        DiagnosticSeverity::Information => Color32::from_rgba_unmultiplied(100, 180, 255, 200),
        DiagnosticSeverity::Hint => Color32::from_rgba_unmultiplied(150, 150, 150, 200),
    }
}
