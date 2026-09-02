//! Chrome: the surfaces, density, and shared paint primitives that give the shell
//! its visual identity.
//!
//! The layering model is borrowed from Zed's theme system, where every surface is a
//! step on one neutral ramp rather than an independently picked color:
//!
//! | Layer                              | Zed token                     |
//! |------------------------------------|-------------------------------|
//! | editor / document                  | `background` (deepest step)   |
//! | title bar, tab bar, status bar, tool panels | `*_background` (one step up) |
//! | popups, menus, inputs              | `elevated_surface`            |
//! | hover / active / selected row tint | `*_alpha` steps (translucent) |
//!
//! Two consequences drive the code in this module:
//!
//! 1. **State colors are derived, never picked.** A hover is a small alpha scrim over
//!    whatever is beneath it (`hover`, `active`, `selected`), so the same code is
//!    correct on a dark *and* a light palette. Hard-coded `from_white_alpha` values,
//!    by contrast, silently disappear on light themes.
//! 2. **Density is a multiplier, not a scattering of magic numbers.** `apply_density`
//!    scales egui's spacing once, globally, so rows, buttons, and menus stay in
//!    proportion when that changes.
//!
//! Colors themselves live in [`crate::theme`]; this module only reads
//! [`SemanticPalette`] fields and derives from them, so every built-in theme (and any
//! future one) inherits the chrome treatment without per-theme work.

use crate::theme::SemanticPalette;
use egui::{Color32, Context, FontId, Margin, Stroke, TextStyle};

// ─── Design constants ──────────────────────────────────────────────────────

/// Corner radius for windows, popups, and menus.
pub const RADIUS_PANEL: f32 = 6.0;
/// Corner radius for buttons, inputs, tabs, and list rows.
pub const RADIUS_WIDGET: f32 = 4.0;
/// Corner radius for hairline chrome such as badges and swatches.
pub const RADIUS_CHIP: f32 = 3.0;

/// UI body font size. Matches Zed's default `ui_font_size`.
pub const UI_FONT: f32 = 12.0;
/// Secondary UI text: status bar, panel headers, tab labels, hints.
pub const UI_FONT_SMALL: f32 = 11.0;
/// Popup and window titles.
pub const UI_FONT_HEADING: f32 = 15.0;
/// Fixed-width UI text (terminal-ish labels in the chrome, never the editor buffer).
pub const UI_FONT_MONO: f32 = 11.5;

/// Vertical breathing room for a single interactive row (tab, list item, status item).
pub const ROW_HEIGHT: f32 = 22.0;
/// Hairline used for dividers and input outlines.
pub const HAIRLINE: f32 = 1.0;

/// Relative alpha of a hover scrim over a surface (`element_hover`).
const HOVER_ALPHA: f32 = 0.062;
/// Relative alpha of a pressed/active scrim (`element_active`).
const ACTIVE_ALPHA: f32 = 0.095;
/// Relative alpha of a selected scrim (`element_selected`).
const SELECTED_ALPHA: f32 = 0.135;
/// Share of the theme border color that survives when painted over a panel.
/// Full-strength borders read as "boxes everywhere"; Zed separates surfaces with
/// the background ramp and keeps strokes barely visible.
const DIVIDER_BLEND: f32 = 0.42;

// ─── Color math ────────────────────────────────────────────────────────────

/// WCAG 2.1 relative luminance of `color`, in `0.0..=1.0`.
pub fn relative_luminance(color: Color32) -> f32 {
    let channel = |value: u8| {
        let value = value as f32 / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
}

/// Whether `color` should be treated as a dark surface, i.e. whether scrims over it
/// should be light. Uses a luminance midpoint rather than `Visuals::dark_mode` so the
/// answer stays correct for themes painted onto an unexpected background.
pub fn is_dark(color: Color32) -> bool {
    relative_luminance(color) < 0.36
}

/// Linear interpolation between two opaque colors, `t = 0.0` yielding `from`.
pub fn mix(from: Color32, to: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgb(
        channel(from.r(), to.r()),
        channel(from.g(), to.g()),
        channel(from.b(), to.b()),
    )
}

/// The scrim color used for state changes on `surface`: light on dark surfaces,
/// dark on light ones, and always slightly tinted toward the theme accent so hovers
/// read as intentional rather than as a gray flash.
fn scrim(surface: Color32, palette: &SemanticPalette) -> Color32 {
    let neutral = if is_dark(surface) {
        Color32::from_rgb(0xff, 0xff, 0xff)
    } else {
        Color32::from_rgb(0x00, 0x00, 0x00)
    };
    mix(neutral, palette.accent, 0.16)
}

/// `top` composited over `base` at fraction `alpha` (source-over, opaque result).
pub fn alpha_over(base: Color32, top: Color32, alpha: f32) -> Color32 {
    mix(base, top, alpha.clamp(0.0, 1.0))
}

/// A theme-agnostic interaction tint: light on dark surfaces, dark on light ones.
/// Modules that only have a `&Ui` (and therefore only the active [`egui::Visuals`]) use
/// this instead of [`hover_on`], so their feedback stays correct on every theme.
pub fn scrim_over(surface: Color32, amount: f32) -> Color32 {
    let target = if is_dark(surface) {
        Color32::from_rgb(0xff, 0xff, 0xff)
    } else {
        Color32::from_rgb(0x00, 0x00, 0x00)
    };
    mix(surface, target, amount.clamp(0.0, 1.0))
}

/// Hover tint for a list row, derived from the surface it is painted on.
pub fn row_hover(surface: Color32) -> Color32 {
    scrim_over(surface, HOVER_ALPHA)
}

/// Selection tint for a list row that does not have focus.
pub fn row_selected(surface: Color32) -> Color32 {
    scrim_over(surface, SELECTED_ALPHA)
}

/// Hover feedback painted over an arbitrary surface (tab bars and status bars sit on
/// their own fills, so the scrim has to be composited against those).
pub fn hover_on(surface: Color32, palette: &SemanticPalette) -> Color32 {
    alpha_over(surface, scrim(surface, palette), HOVER_ALPHA)
}

/// Pressed / keyboard-active tint: a stronger scrim than [`hover_on`], still not an
/// outline.
pub fn active_on(surface: Color32, palette: &SemanticPalette) -> Color32 {
    alpha_over(surface, scrim(surface, palette), ACTIVE_ALPHA)
}

/// Selected-but-unfocused tint, e.g. the tab that owns a pane group while another group
/// has focus.
pub fn selected_on(surface: Color32, palette: &SemanticPalette) -> Color32 {
    alpha_over(surface, scrim(surface, palette), SELECTED_ALPHA)
}

/// The quietest line that still reads as a divider on `surface`.
pub fn divider_on(surface: Color32, palette: &SemanticPalette) -> Color32 {
    mix(surface, palette.border, DIVIDER_BLEND)
}

/// Divider for the common case of a line drawn on a dock panel.
pub fn divider(palette: &SemanticPalette) -> Color32 {
    divider_on(palette.panel_background, palette)
}

/// Outline color for a focused input or popup.
pub fn focus_ring(palette: &SemanticPalette) -> Color32 {
    palette.accent
}

// ─── Surfaces ──────────────────────────────────────────────────────────────

/// Fill shared by the title bar, tab bar, status bar, and dock panels. Deliberately
/// one step away from the editor fill, which is what lets the active tab appear to be
/// a hole cut into the editor rather than a highlighted row.
pub fn chrome_fill(palette: &SemanticPalette) -> Color32 {
    palette.panel_background
}

// ─── Paint primitives ──────────────────────────────────────────────────────

/// A 1px vertical rule centered at `x` between `y0` and `y1`.
pub fn vrule(painter: &egui::Painter, x: f32, y0: f32, y1: f32, color: Color32) {
    painter.line_segment(
        [egui::pos2(x, y0), egui::pos2(x, y1)],
        Stroke::new(HAIRLINE, color),
    );
}

/// The unsaved-changes dot used by tabs, title bars, and the status bar.
pub fn paint_dirty_dot(painter: &egui::Painter, center: egui::Pos2, color: Color32) {
    painter.circle_filled(center, 3.0, color);
}

/// A monochrome pin glyph, drawn instead of an emoji so it renders identically on
/// every platform regardless of which fallback fonts happen to be installed.
pub fn paint_pin(painter: &egui::Painter, center: egui::Pos2, color: Color32) {
    let stroke = Stroke::new(1.2, color);
    let top = egui::pos2(center.x, center.y - 4.6);
    let head_bottom = egui::pos2(center.x, center.y - 0.4);
    painter.circle_stroke(egui::pos2(center.x, center.y - 3.0), 2.2, stroke);
    painter.line_segment([top, head_bottom], stroke);
    painter.line_segment(
        [
            egui::pos2(center.x - 3.4, center.y - 0.2),
            egui::pos2(center.x + 3.4, center.y - 0.2),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(center.x - 2.6, center.y - 0.2),
            egui::pos2(center.x - 3.2, center.y + 3.4),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(center.x + 2.6, center.y - 0.2),
            egui::pos2(center.x + 3.2, center.y + 3.4),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(center.x, center.y - 0.4),
            egui::pos2(center.x, center.y + 4.6),
        ],
        stroke,
    );
}

/// A monochrome `×` for close affordances, scaled to `size`.
pub fn paint_cross(painter: &egui::Painter, center: egui::Pos2, size: f32, color: Color32) {
    let half = size * 0.5;
    let stroke = Stroke::new(1.2, color);
    painter.line_segment(
        [
            egui::pos2(center.x - half, center.y - half),
            egui::pos2(center.x + half, center.y + half),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(center.x + half, center.y - half),
            egui::pos2(center.x - half, center.y + half),
        ],
        stroke,
    );
}

/// A flat toggle for chrome rows (status bar items, panel headers).
///
/// egui's `selectable_label` fills the selected state with the *text selection* color
/// and outlines it with the selection stroke, which is right inside a document and
/// shouting its way through a toolbar. Zed instead marks the chosen chrome item with a
/// quiet scrim and an accent rule, so selection and focus keep distinct vocabularies.
pub fn chrome_toggle(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
    palette: &SemanticPalette,
) -> egui::Response {
    chrome_item(ui, label, None, selected, palette)
}

/// [`chrome_toggle`] with a leading status dot, for counts that need to carry severity.
///
/// A painted dot stands in for a cross-or-warning glyph because egui's proportional face
/// does not guarantee either, and a missing one turns a status item into an empty box.
pub fn chrome_status_item(
    ui: &mut egui::Ui,
    label: &str,
    tint: Color32,
    selected: bool,
    palette: &SemanticPalette,
) -> egui::Response {
    chrome_item(ui, label, Some(tint), selected, palette)
}

fn chrome_item(
    ui: &mut egui::Ui,
    label: &str,
    tint: Option<Color32>,
    selected: bool,
    palette: &SemanticPalette,
) -> egui::Response {
    const DOT_DIAMETER: f32 = 6.0;
    const DOT_GAP: f32 = 5.0;
    const PAD_X: f32 = 7.0;

    let font = FontId::proportional(UI_FONT_SMALL);
    let text_w = ui.fonts(|f| {
        f.layout_no_wrap(label.to_owned(), font.clone(), palette.primary_text)
            .size()
            .x
    });
    let dot_w = if tint.is_some() {
        DOT_DIAMETER + DOT_GAP
    } else {
        0.0
    };
    let height = ui.available_height().clamp(18.0, ROW_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(text_w + dot_w + PAD_X * 2.0, height),
        egui::Sense::click(),
    );

    let surface = chrome_fill(palette);
    let fill = if response.hovered() {
        hover_on(surface, palette)
    } else if selected {
        selected_on(surface, palette)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter()
        .rect_filled(rect.shrink2(egui::vec2(1.0, 2.0)), RADIUS_WIDGET, fill);

    let text_color = if selected || response.hovered() {
        palette.primary_text
    } else {
        palette.muted_text
    };
    let mut text_x = rect.left() + PAD_X;
    if let Some(tint) = tint {
        ui.painter().circle_filled(
            egui::pos2(text_x + DOT_DIAMETER * 0.5, rect.center().y),
            DOT_DIAMETER * 0.5,
            tint,
        );
        text_x += dot_w;
    }
    ui.painter().text(
        egui::pos2(text_x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        font,
        text_color,
    );

    if selected {
        underline_accent(ui.painter(), rect, PAD_X, palette);
    }
    if response.hovered() {
        ui.ctx()
            .set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// The accent rule that marks the active chrome item. Sits inside the hairline so it
/// reads as part of the row rather than a border on top of it.
fn underline_accent(
    painter: &egui::Painter,
    rect: egui::Rect,
    inset: f32,
    palette: &SemanticPalette,
) {
    let y = rect.bottom() - 1.0;
    painter.line_segment(
        [
            egui::pos2(rect.left() + inset * 0.5, y),
            egui::pos2(rect.right() - inset * 0.5, y),
        ],
        Stroke::new(1.5, palette.accent),
    );
}

// ─── Density & typography ──────────────────────────────────────────────────

/// Global UI density. The spacing ratios match Zed's `ui_density` setting: compact
/// tightens spacing by a quarter, comfortable loosens it by a quarter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Density {
    Compact,
    Default,
    Comfortable,
}

impl Density {
    /// Multiplier applied to egui's spacing scale.
    pub fn spacing_ratio(self) -> f32 {
        match self {
            Density::Compact => 0.75,
            Density::Default => 1.0,
            Density::Comfortable => 1.25,
        }
    }

    /// The setting string this density serializes to.
    pub fn as_str(self) -> &'static str {
        match self {
            Density::Compact => "compact",
            Density::Default => "default",
            Density::Comfortable => "comfortable",
        }
    }

    /// Parses a persisted value, falling back to [`Density::Default`] for anything
    /// unrecognized (including values written by a future version).
    pub fn from_setting(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "compact" => Density::Compact,
            "comfortable" => Density::Comfortable,
            _ => Density::Default,
        }
    }
}

/// Scale a spacing pair, keeping sub-pixel values from rounding to zero at compact
/// density (a zero item spacing fuses labels into one unclickable run of text).
fn scaled(base: f32, ratio: f32) -> f32 {
    (base * ratio).max(1.0)
}

/// Tightens egui's spacing, typography, and scrollbars into one consistent scale.
///
/// Touches geometry and type only — never colors — so the high-contrast theme can
/// adopt the density without any of its contrast guarantees being re-derived.
pub fn apply_density(ctx: &Context, density: Density) {
    let ratio = density.spacing_ratio();
    ctx.style_mut(|style| {
        style.spacing.item_spacing =
            egui::vec2(scaled(7.0, ratio), scaled(4.0, ratio));
        style.spacing.button_padding = egui::vec2(scaled(7.0, ratio), scaled(3.0, ratio));
        style.spacing.interact_size = egui::vec2(scaled(40.0, ratio), ROW_HEIGHT * ratio);
        style.spacing.indent = scaled(14.0, ratio);
        style.spacing.icon_width = scaled(14.0, ratio);
        style.spacing.icon_width_inner = scaled(8.0, ratio);
        style.spacing.icon_spacing = scaled(5.0, ratio);
        style.spacing.menu_margin = Margin {
            left: 4.0,
            right: 4.0,
            top: scaled(4.0, ratio),
            bottom: scaled(4.0, ratio),
        };
        style.spacing.window_margin = Margin {
            left: scaled(0.0, ratio),
            right: scaled(0.0, ratio),
            top: scaled(0.0, ratio),
            bottom: scaled(0.0, ratio),
        };

        // Overlay scrollbars: thin, translucent, and never stealing layout width, so
        // the editor keeps its full column while the thumb is one gesture away.
        style.spacing.scroll.floating = true;
        style.spacing.scroll.foreground_color = true;
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.bar_inner_margin = 1.5;
        style.spacing.scroll.bar_outer_margin = 0.0;
        style.spacing.scroll.floating_width = 6.0;
        style.spacing.scroll.floating_allocated_width = 0.0;
        style.spacing.scroll.handle_min_length = 24.0;
        style.spacing.scroll.dormant_background_opacity = 0.0;
        style.spacing.scroll.active_background_opacity = 0.0;
        style.spacing.scroll.interact_background_opacity = 0.0;
        style.spacing.scroll.dormant_handle_opacity = 0.30;
        style.spacing.scroll.active_handle_opacity = 0.45;
        style.spacing.scroll.interact_handle_opacity = 0.65;

        // One type scale for the whole shell instead of per-widget sizes.
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(UI_FONT_SMALL)),
            (TextStyle::Body, FontId::proportional(UI_FONT)),
            (TextStyle::Button, FontId::proportional(UI_FONT)),
            (TextStyle::Heading, FontId::proportional(UI_FONT_HEADING)),
            (TextStyle::Monospace, FontId::monospace(UI_FONT_MONO)),
        ]
        .into();

        // Chrome should feel immediate: short state transitions, quick tooltips.
        style.animation_time = 0.1;
        style.interaction.tooltip_delay = 0.2;
        style.interaction.show_tooltips_only_when_still = true;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dark() -> SemanticPalette {
        palette_for(crate::settings::Theme::Dark)
    }

    fn light() -> SemanticPalette {
        palette_for(crate::settings::Theme::Light)
    }

    fn palette_for(theme: crate::settings::Theme) -> SemanticPalette {
        crate::theme::built_in_theme(theme, None).palette.semantic
    }

    #[test]
    fn chrome_sits_one_step_above_the_document() {
        // The whole layering model rests on these two being different colors.
        let palette = dark();
        assert_ne!(chrome_fill(&palette), palette.editor_background);
        assert_ne!(chrome_fill(&palette), palette.elevated_background);
    }

    #[test]
    fn scrims_lighten_dark_surfaces_and_darken_light_ones() {
        let palette = dark();
        let light_palette = light();

        // On a dark chrome, every state step must *raise* luminance; on a light one it
        // must lower it. Otherwise hovers are invisible on one of the two themes.
        let lum = |c: Color32| relative_luminance(c);
        let dark_base = chrome_fill(&palette);
        assert!(lum(hover_on(dark_base, &palette)) > lum(dark_base));
        assert!(lum(active_on(dark_base, &palette)) > lum(hover_on(dark_base, &palette)));
        assert!(lum(selected_on(dark_base, &palette)) > lum(active_on(dark_base, &palette)));

        let light_base = chrome_fill(&light_palette);
        assert!(lum(hover_on(light_base, &light_palette)) < lum(light_base));
        assert!(lum(active_on(light_base, &light_palette)) < lum(hover_on(light_base, &light_palette)));
        assert!(lum(selected_on(light_base, &light_palette)) < lum(active_on(light_base, &light_palette)));
    }

    #[test]
    fn state_colors_stay_within_visible_but_quiet_bounds() {
        let palette = dark();
        // A hover must be perceptible (>1 RGB step) yet stay far below the accent's
        // own contrast against the panel, which is reserved for real emphasis.
        let delta = |a: Color32, b: Color32| -> i32 {
            (a.r() as i32 - b.r() as i32)
                + (a.g() as i32 - b.g() as i32)
                + (a.b() as i32 - b.b() as i32)
        };
        let surface = chrome_fill(&palette);
        let hover_delta = delta(hover_on(surface, &palette), surface).abs();
        let selected_delta = delta(selected_on(surface, &palette), surface).abs();
        let accent_delta = delta(palette.accent, chrome_fill(&palette)).abs();

        assert!(hover_delta >= 4, "hover scrim too faint: {hover_delta}");
        assert!(
            selected_delta < accent_delta,
            "selection scrim must not out-shout the accent"
        );
    }

    #[test]
    fn divider_is_quieter_than_the_raw_border() {
        let palette = dark();
        let border_lum = relative_luminance(palette.border);
        let panel_lum = relative_luminance(chrome_fill(&palette));
        let divider_lum = relative_luminance(divider(&palette));
        assert!(divider_lum > panel_lum && divider_lum < border_lum);
    }

    #[test]
    fn mix_is_linear_at_its_endpoints() {
        let a = Color32::from_rgb(10, 20, 30);
        let b = Color32::from_rgb(20, 40, 60);
        assert_eq!(mix(a, b, 0.0), a);
        assert_eq!(mix(a, b, 1.0), b);
        assert_eq!(mix(a, b, 0.5), Color32::from_rgb(15, 30, 45));
        // Out-of-range t is clamped rather than producing wrapped channels.
        assert_eq!(mix(a, b, 2.0), b);
        assert_eq!(mix(a, b, -1.0), a);
    }

    #[test]
    fn is_dark_tracks_perceived_lightness_not_channel_sum() {
        assert!(is_dark(Color32::BLACK));
        assert!(!is_dark(Color32::WHITE));
        // Mid-gray is the interesting case: light enough to need dark scrims.
        assert!(!is_dark(Color32::from_gray(170)));
        assert!(is_dark(Color32::from_gray(60)));
    }

    #[test]
    fn density_ratios_match_zed_and_scale_monotonically() {
        assert_eq!(Density::Compact.spacing_ratio(), 0.75);
        assert_eq!(Density::Default.spacing_ratio(), 1.0);
        assert_eq!(Density::Comfortable.spacing_ratio(), 1.25);
        assert!(scaled(7.0, 0.75) < scaled(7.0, 1.0));
        assert!(scaled(7.0, 1.0) < scaled(7.0, 1.25));
        assert_eq!(scaled(0.0, 0.75), 1.0, "spacing must never collapse to 0");
    }

    #[test]
    fn density_round_trips_through_its_setting_string() {
        for density in [Density::Compact, Density::Default, Density::Comfortable] {
            assert_eq!(Density::from_setting(density.as_str()), density);
        }
        assert_eq!(Density::from_setting(""), Density::Default);
        assert_eq!(Density::from_setting("  COMPACT "), Density::Compact);
        assert_eq!(Density::from_setting("from-the-future"), Density::Default);
    }

    #[test]
    fn apply_density_rewrites_spacing_and_type_without_touching_colors() {
        let ctx = Context::default();
        let before = ctx.style().visuals.panel_fill;

        apply_density(&ctx, Density::Compact);

        let style = ctx.style();
        assert_eq!(style.spacing.item_spacing.x, scaled(7.0, 0.75));
        assert_eq!(
            style.text_styles[&TextStyle::Body],
            FontId::proportional(UI_FONT)
        );
        assert_eq!(
            style.text_styles[&TextStyle::Small],
            FontId::proportional(UI_FONT_SMALL)
        );
        assert!(style.spacing.scroll.floating);
        assert_eq!(style.spacing.scroll.floating_allocated_width, 0.0);
        assert_eq!(style.visuals.panel_fill, before);
    }

    #[test]
    fn typography_is_monotonic_across_the_scale() {
        assert!(UI_FONT_SMALL < UI_FONT);
        assert_eq!(UI_FONT, UI_FONT);
        assert!(UI_FONT < UI_FONT_MONO.max(UI_FONT) + 4.0);
        assert!(UI_FONT_MONO <= UI_FONT + 0.5);
        assert!(UI_FONT_HEADING > UI_FONT);
    }

    #[test]
    fn chrome_toggle_sizes_to_its_label_and_keeps_both_states_clickable() {
        let ctx = Context::default();
        let palette = dark();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let quiet = chrome_toggle(ui, "Problems", false, &palette);
                let active = chrome_toggle(ui, "Problems", true, &palette);
                // Same label => same hit target, so toggling never reflows the row.
                assert_eq!(quiet.rect.width(), active.rect.width());
                assert!(quiet.rect.width() > 1.0);

                // A longer label claims more room: the target follows the text.
                let wide = chrome_toggle(ui, "Call Hierarchy", false, &palette);
                assert!(wide.rect.width() > quiet.rect.width());
                assert!(!active.clicked());
            });
        });
    }

    #[test]
    fn row_scrims_are_ordered_and_scheme_correct() {
        let palette = dark();
        let base = chrome_fill(&palette);
        let lum = |c: Color32| relative_luminance(c);
        assert!(lum(hover_on(base, &palette)) > lum(base));
        assert!(lum(active_on(base, &palette)) > lum(hover_on(base, &palette)));
        assert!(lum(selected_on(base, &palette)) > lum(active_on(base, &palette)));
        let light_base = light().panel_background;
        assert!(lum(hover_on(light_base, &light())) < lum(light_base));
        assert_eq!(hover_on(base, &palette), hover_on(base, &palette));
    }
}
