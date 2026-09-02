use egui::{Color32, Stroke, Visuals};

use crate::settings::Theme;

/// Creates a high contrast theme Visuals meeting WCAG AA (4.5:1 contrast ratio).
/// 
/// Color requirements:
/// - Background: #000000
/// - Default text: #FFFFFF
/// - Button background: #1A1A1A, text: #FFFF00
/// - Selected/highlighted item background: #005FFF, text: #FFFFFF
/// - Error text: #FF4040
/// - Warning text: #FFA500
pub fn high_contrast_theme() -> Visuals {
    crate::high_contrast::high_contrast_theme()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorScheme {
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxPalette {
    pub default: Color32,
    pub comment: Color32,
    pub string: Color32,
    pub number: Color32,
    pub keyword: Color32,
    pub type_name: Color32,
    pub macro_name: Color32,
    pub lifetime: Color32,
    pub function: Color32,
    pub symbol: Color32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticPalette {
    pub ui_background: Color32,
    pub panel_background: Color32,
    pub elevated_background: Color32,
    pub editor_background: Color32,
    pub primary_text: Color32,
    pub muted_text: Color32,
    pub selection: Color32,
    pub inactive_selection: Color32,
    pub accent: Color32,
    pub border: Color32,
    pub error: Color32,
    pub warning: Color32,
    pub information: Color32,
    pub success: Color32,
    pub search_match: Color32,
    pub active_search_match: Color32,
    pub current_line: Color32,
    pub completion_function: Color32,
    pub completion_type: Color32,
    pub completion_field: Color32,
    pub completion_variable: Color32,
    pub completion_module: Color32,
    pub completion_keyword: Color32,
    // Inlay hint semantic colors
    pub inlay_type_hint_text: Color32,
    pub inlay_type_hint_background: Color32,
    pub inlay_parameter_hint_text: Color32,
    pub inlay_parameter_hint_background: Color32,
    pub inlay_hint_border: Color32,
    // Hover popup semantic colors
    pub hover_code_background: Color32,
    pub hover_link: Color32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemePalette {
    pub semantic: SemanticPalette,
    pub syntax: SyntaxPalette,
}

#[derive(Debug, Clone)]
pub struct BuiltInTheme {
    pub selection: Theme,
    pub id: &'static str,
    pub display_name: &'static str,
    pub scheme: ColorScheme,
    pub visuals: Visuals,
    pub palette: ThemePalette,
}

impl BuiltInTheme {
    pub fn is_dark(&self) -> bool {
        self.scheme == ColorScheme::Dark
    }
}

pub fn resolve_theme(selection: Theme, system_scheme: Option<ColorScheme>) -> Theme {
    match selection {
        Theme::System => match system_scheme {
            Some(ColorScheme::Light) => Theme::Light,
            Some(ColorScheme::Dark) | None => Theme::Dark,
        },
        concrete => concrete,
    }
}

pub fn built_in_theme(selection: Theme, system_scheme: Option<ColorScheme>) -> BuiltInTheme {
    let resolved = resolve_theme(selection, system_scheme);
    let (scheme, palette) = match resolved {
        Theme::Dark => (ColorScheme::Dark, blue_dark()),
        Theme::Light => (ColorScheme::Light, blue_light()),
        Theme::Nord => (ColorScheme::Dark, nord()),
        Theme::Dracula => (ColorScheme::Dark, dracula()),
        Theme::SolarizedDark => (ColorScheme::Dark, solarized_dark()),
        Theme::System => unreachable!("System is always resolved"),
    };
    BuiltInTheme {
        selection: resolved,
        id: resolved.serialized_id(),
        display_name: resolved.display_name(),
        scheme,
        visuals: visuals(scheme, palette.semantic),
        palette,
    }
}

pub fn default_syntax_palette() -> SyntaxPalette {
    blue_dark().syntax
}

/// High-contrast accessibility theme (currently maps to a high-contrast dark palette).
/// Reserved for future WCAG AA+ contrast improvements.
pub fn high_contrast_theme_builtin() -> BuiltInTheme {
    built_in_theme(crate::settings::Theme::Dark, None)
}

fn rgb(value: u32) -> Color32 {
    Color32::from_rgb(
        ((value >> 16) & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        (value & 0xff) as u8,
    )
}

fn rgba(value: u32, alpha: u8) -> Color32 {
    let color = rgb(value);
    Color32::from_rgba_premultiplied(color.r(), color.g(), color.b(), alpha)
}

fn visuals(scheme: ColorScheme, colors: SemanticPalette) -> Visuals {
    let mut visuals = match scheme {
        ColorScheme::Dark => Visuals::dark(),
        ColorScheme::Light => Visuals::light(),
    };
    visuals.dark_mode = scheme == ColorScheme::Dark;
    visuals.override_text_color = Some(colors.primary_text);
    visuals.panel_fill = colors.panel_background;
    visuals.window_fill = colors.elevated_background;
    visuals.extreme_bg_color = colors.editor_background;
    visuals.faint_bg_color = colors.current_line;
    visuals.code_bg_color = colors.editor_background;
    visuals.hyperlink_color = colors.accent;
    visuals.warn_fg_color = colors.warning;
    visuals.error_fg_color = colors.error;
    visuals.selection.bg_fill = colors.selection;
    visuals.selection.stroke = Stroke::NONE;
    visuals.window_stroke = Stroke::new(1.0, colors.border);
    visuals.window_rounding = egui::Rounding::same(8.0);
    visuals.menu_rounding = egui::Rounding::same(6.0);

    // Zed uses neutral, ghost-style widgets: flat text at rest with a subtle
    // rounded background appearing only on hover / press / open. The neutral
    // steps mirror Zed's `element.hover` / `element.active` / `element.selected`.
    let (hover_bg, active_bg, selected_bg) = match scheme {
        ColorScheme::Dark => (rgb(0x363c46), rgb(0x454a56), rgb(0x454a56)),
        ColorScheme::Light => (rgb(0xdfdfe0), rgb(0xcacaca), rgb(0xcacaca)),
    };
    let rounding = egui::Rounding::same(6.0);

    visuals.widgets.noninteractive.bg_fill = colors.panel_background;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, colors.border);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, colors.primary_text);
    visuals.widgets.noninteractive.rounding = rounding;

    visuals.widgets.inactive.bg_fill = Color32::TRANSPARENT;
    visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    visuals.widgets.inactive.bg_stroke = Stroke::NONE;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, colors.primary_text);
    visuals.widgets.inactive.rounding = rounding;

    visuals.widgets.hovered.bg_fill = hover_bg;
    visuals.widgets.hovered.weak_bg_fill = hover_bg;
    visuals.widgets.hovered.bg_stroke = Stroke::NONE;
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, colors.primary_text);
    visuals.widgets.hovered.rounding = rounding;

    visuals.widgets.active.bg_fill = active_bg;
    visuals.widgets.active.weak_bg_fill = active_bg;
    visuals.widgets.active.bg_stroke = Stroke::NONE;
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, colors.primary_text);
    visuals.widgets.active.rounding = rounding;

    visuals.widgets.open.bg_fill = selected_bg;
    visuals.widgets.open.weak_bg_fill = selected_bg;
    visuals.widgets.open.bg_stroke = Stroke::NONE;
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, colors.primary_text);
    visuals.widgets.open.rounding = rounding;
    visuals
}

fn palette(semantic: SemanticPalette, syntax: SyntaxPalette) -> ThemePalette {
    ThemePalette { semantic, syntax }
}

/// The default dark theme: a faithful port of Zed's built-in "One Dark" theme.
///
/// Color model (from Zed's `assets/themes/one/one.json`):
/// - Editor + active tab + toolbar: `editor.background` `#282c33`
/// - Title bar + status bar: `background` `#3b414d`
/// - Tab bar + side panels + popovers: `panel.background` `#2f343e`
/// - Elements (inputs, menu rows): `element.background` `#2e343e`
/// - Accent: `#74ade8` (matches Zed's One Dark `text.accent`).
pub fn blue_dark() -> ThemePalette {
    palette(
        SemanticPalette {
            ui_background: rgb(0x3b414d),
            panel_background: rgb(0x2f343e),
            elevated_background: rgb(0x2e343e),
            editor_background: rgb(0x282c33),
            primary_text: rgb(0xdce0e5),
            muted_text: rgb(0xa9afbc),
            selection: rgba(0x74ade8, 64),
            inactive_selection: rgb(0x454a56),
            accent: rgb(0x74ade8),
            border: rgb(0x464b57),
            error: rgb(0xd07277),
            warning: rgb(0xdec184),
            information: rgb(0x74ade8),
            success: rgb(0xa1c181),
            search_match: rgba(0x74ade8, 102),
            active_search_match: rgba(0xe8af74, 102),
            current_line: rgb(0x2c3138),
            completion_function: rgb(0x73ade9),
            completion_type: rgb(0x6eb4bf),
            completion_field: rgb(0xd07277),
            completion_variable: rgb(0xacb2be),
            completion_module: rgb(0x6eb4bf),
            completion_keyword: rgb(0xb477cf),
            inlay_type_hint_text: rgb(0x788ca6),
            inlay_type_hint_background: rgba(0x5a6f89, 26),
            inlay_parameter_hint_text: rgb(0xa9afbc),
            inlay_parameter_hint_background: rgba(0x74ade8, 26),
            inlay_hint_border: rgba(0x464b57, 180),
            hover_code_background: rgb(0x2e343e),
            hover_link: rgb(0x74ade8),
        },
        SyntaxPalette {
            default: rgb(0xacb2be),
            comment: rgb(0x5d636f),
            string: rgb(0xa1c181),
            number: rgb(0xbf956a),
            keyword: rgb(0xb477cf),
            type_name: rgb(0x6eb4bf),
            macro_name: rgb(0xb477cf),
            lifetime: rgb(0x74ade8),
            function: rgb(0x73ade9),
            symbol: rgb(0xacb2be),
        },
    )
}

/// The default light theme: a faithful port of Zed's built-in "One Light" theme.
fn blue_light() -> ThemePalette {
    palette(
        SemanticPalette {
            ui_background: rgb(0xdcdcdd),
            panel_background: rgb(0xebebec),
            elevated_background: rgb(0xebebec),
            editor_background: rgb(0xfafafa),
            primary_text: rgb(0x242529),
            muted_text: rgb(0x58585a),
            selection: rgba(0x5c78e2, 64),
            inactive_selection: rgb(0xcacaca),
            accent: rgb(0x5c78e2),
            border: rgb(0xc9c9ca),
            error: rgb(0xd36151),
            warning: rgb(0xa48819),
            information: rgb(0x5c78e2),
            success: rgb(0x669f59),
            search_match: rgba(0x5c79e2, 102),
            active_search_match: rgba(0xd0a923, 102),
            current_line: rgb(0xf1f1f2),
            completion_function: rgb(0x5b79e3),
            completion_type: rgb(0x3882b7),
            completion_field: rgb(0xd3604f),
            completion_variable: rgb(0x242529),
            completion_module: rgb(0x3882b7),
            completion_keyword: rgb(0xa449ab),
            inlay_type_hint_text: rgb(0x7274a7),
            inlay_type_hint_background: rgba(0x7274a7, 26),
            inlay_parameter_hint_text: rgb(0x58585a),
            inlay_parameter_hint_background: rgba(0x5c78e2, 26),
            inlay_hint_border: rgba(0xc9c9ca, 200),
            hover_code_background: rgb(0xebebec),
            hover_link: rgb(0x5c78e2),
        },
        SyntaxPalette {
            default: rgb(0x242529),
            comment: rgb(0xa2a3a7),
            string: rgb(0x649f57),
            number: rgb(0xad6e25),
            keyword: rgb(0xa449ab),
            type_name: rgb(0x3882b7),
            macro_name: rgb(0xa449ab),
            lifetime: rgb(0x5c78e2),
            function: rgb(0x5b79e3),
            symbol: rgb(0x242529),
        },
    )
}

fn nord() -> ThemePalette {
    palette(
        SemanticPalette {
            ui_background: rgb(0x2e3440),
            panel_background: rgb(0x3b4252),
            elevated_background: rgb(0x434c5e),
            editor_background: rgb(0x2e3440),
            primary_text: rgb(0xeceff4),
            muted_text: rgb(0xd8dee9),
            selection: rgb(0x4c566a),
            inactive_selection: rgb(0x3b4252),
            accent: rgb(0x88c0d0),
            border: rgb(0x4c566a),
            error: rgb(0xbf616a),
            warning: rgb(0xebcb8b),
            information: rgb(0x81a1c1),
            success: rgb(0xa3be8c),
            search_match: rgba(0x81a1c1, 75),
            active_search_match: rgba(0xebcb8b, 120),
            current_line: rgb(0x353c4a),
            completion_function: rgb(0xb48ead),
            completion_type: rgb(0x8fbcbb),
            completion_field: rgb(0xa3be8c),
            completion_variable: rgb(0x88c0d0),
            completion_module: rgb(0xd08770),
            completion_keyword: rgb(0x81a1c1),
            inlay_type_hint_text: rgb(0xa3be8c),
            inlay_type_hint_background: rgba(0xa3be8c, 28),
            inlay_parameter_hint_text: rgb(0xd8dee9),
            inlay_parameter_hint_background: rgba(0x88c0d0, 22),
            inlay_hint_border: rgba(0x4c566a, 180),
            hover_code_background: rgb(0x2e3440),
            hover_link: rgb(0x88c0d0),
        },
        SyntaxPalette {
            default: rgb(0xd8dee9),
            comment: rgb(0x7b88a1),
            string: rgb(0xa3be8c),
            number: rgb(0xb48ead),
            keyword: rgb(0x81a1c1),
            type_name: rgb(0x8fbcbb),
            macro_name: rgb(0xebcb8b),
            lifetime: rgb(0x88c0d0),
            function: rgb(0x88c0d0),
            symbol: rgb(0xeceff4),
        },
    )
}

fn dracula() -> ThemePalette {
    palette(
        SemanticPalette {
            ui_background: rgb(0x282a36),
            panel_background: rgb(0x21222c),
            elevated_background: rgb(0x343746),
            editor_background: rgb(0x282a36),
            primary_text: rgb(0xf8f8f2),
            muted_text: rgb(0xb9b9b3),
            selection: rgb(0x44475a),
            inactive_selection: rgb(0x383a4a),
            accent: rgb(0xbd93f9),
            border: rgb(0x44475a),
            error: rgb(0xff5555),
            warning: rgb(0xf1fa8c),
            information: rgb(0x8be9fd),
            success: rgb(0x50fa7b),
            search_match: rgba(0x8be9fd, 65),
            active_search_match: rgba(0xffb86c, 120),
            current_line: rgb(0x30323f),
            completion_function: rgb(0x50fa7b),
            completion_type: rgb(0x8be9fd),
            completion_field: rgb(0xf1fa8c),
            completion_variable: rgb(0xf8f8f2),
            completion_module: rgb(0xffb86c),
            completion_keyword: rgb(0xff79c6),
            inlay_type_hint_text: rgb(0x50fa7b),
            inlay_type_hint_background: rgba(0x50fa7b, 22),
            inlay_parameter_hint_text: rgb(0xb9b9b3),
            inlay_parameter_hint_background: rgba(0xbd93f9, 22),
            inlay_hint_border: rgba(0x44475a, 180),
            hover_code_background: rgb(0x21222c),
            hover_link: rgb(0xbd93f9),
        },
        SyntaxPalette {
            default: rgb(0xf8f8f2),
            comment: rgb(0x8b92a8),
            string: rgb(0xf1fa8c),
            number: rgb(0xbd93f9),
            keyword: rgb(0xff79c6),
            type_name: rgb(0x8be9fd),
            macro_name: rgb(0x50fa7b),
            lifetime: rgb(0xffb86c),
            function: rgb(0x50fa7b),
            symbol: rgb(0xf8f8f2),
        },
    )
}

fn solarized_dark() -> ThemePalette {
    palette(
        SemanticPalette {
            ui_background: rgb(0x002b36),
            panel_background: rgb(0x073642),
            elevated_background: rgb(0x0b3b46),
            editor_background: rgb(0x002b36),
            primary_text: rgb(0xeee8d5),
            muted_text: rgb(0x93a1a1),
            selection: rgb(0x07586b),
            inactive_selection: rgb(0x073642),
            accent: rgb(0x268bd2),
            border: rgb(0x586e75),
            error: rgb(0xdc322f),
            warning: rgb(0xb58900),
            information: rgb(0x268bd2),
            success: rgb(0x859900),
            search_match: rgba(0x268bd2, 75),
            active_search_match: rgba(0xb58900, 125),
            current_line: rgb(0x073642),
            completion_function: rgb(0x2aa198),
            completion_type: rgb(0x268bd2),
            completion_field: rgb(0x859900),
            completion_variable: rgb(0x93a1a1),
            completion_module: rgb(0xcb4b16),
            completion_keyword: rgb(0x6c71c4),
            inlay_type_hint_text: rgb(0x859900),
            inlay_type_hint_background: rgba(0x859900, 25),
            inlay_parameter_hint_text: rgb(0x93a1a1),
            inlay_parameter_hint_background: rgba(0x268bd2, 22),
            inlay_hint_border: rgba(0x586e75, 180),
            hover_code_background: rgb(0x073642),
            hover_link: rgb(0x268bd2),
        },
        SyntaxPalette {
            default: rgb(0xeee8d5),
            comment: rgb(0x93a1a1),
            string: rgb(0x2aa198),
            number: rgb(0xd33682),
            keyword: rgb(0x859900),
            type_name: rgb(0xb58900),
            macro_name: rgb(0xcb4b16),
            lifetime: rgb(0x268bd2),
            function: rgb(0x268bd2),
            symbol: rgb(0xeee8d5),
        },
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn catalog_contains_every_theme_once() {
        let themes = Theme::all();
        let unique: HashSet<_> = themes.iter().copied().collect();
        assert_eq!(themes.len(), 6);
        assert_eq!(unique.len(), themes.len());
    }

    #[test]
    fn metadata_is_non_empty_and_ids_are_unique() {
        let ids: HashSet<_> = Theme::all()
            .iter()
            .map(|theme| theme.serialized_id())
            .collect();
        assert_eq!(ids.len(), Theme::all().len());
        assert!(Theme::all().iter().all(|theme| {
            !theme.serialized_id().is_empty() && !theme.display_name().is_empty()
        }));
    }

    #[test]
    fn system_resolves_from_preference_and_falls_back_dark() {
        assert_eq!(
            resolve_theme(Theme::System, Some(ColorScheme::Light)),
            Theme::Light
        );
        assert_eq!(
            resolve_theme(Theme::System, Some(ColorScheme::Dark)),
            Theme::Dark
        );
        assert_eq!(resolve_theme(Theme::System, None), Theme::Dark);
    }

    #[test]
    fn schemes_are_classified_correctly() {
        assert!(!built_in_theme(Theme::Light, None).is_dark());
        for theme in [
            Theme::Dark,
            Theme::Nord,
            Theme::Dracula,
            Theme::SolarizedDark,
        ] {
            assert!(built_in_theme(theme, None).is_dark());
        }
    }

    #[test]
    fn critical_color_pairs_have_reasonable_contrast() {
        for theme in Theme::all() {
            let built = built_in_theme(*theme, Some(ColorScheme::Dark));
            assert!(
                contrast(
                    built.palette.semantic.primary_text,
                    built.palette.semantic.editor_background
                ) >= 4.0
            );
            assert!(
                contrast(
                    built.palette.syntax.default,
                    built.palette.semantic.editor_background
                ) >= 4.0
            );
        }
    }

    #[test]
    fn light_and_dark_syntax_palettes_are_distinct() {
        assert_ne!(blue_dark().syntax, blue_light().syntax);
        assert_ne!(blue_dark().syntax.default, blue_light().syntax.default);
    }

    #[test]
    fn applying_theme_visuals_replaces_prior_values() {
        let context = egui::Context::default();
        let dark = built_in_theme(Theme::Dark, None);
        let light = built_in_theme(Theme::Light, None);
        context.set_visuals(dark.visuals);
        context.set_visuals(light.visuals.clone());
        assert_eq!(context.style().visuals.panel_fill, light.visuals.panel_fill);
        assert_eq!(context.style().visuals.selection, light.visuals.selection);
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let luminance = |color: Color32| {
            let channel = |value: u8| {
                let value = value as f32 / 255.0;
                if value <= 0.03928 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
        };
        let (lighter, darker) = {
            let x = luminance(a);
            let y = luminance(b);
            if x >= y {
                (x, y)
            } else {
                (y, x)
            }
        };
        (lighter + 0.05) / (darker + 0.05)
    }
}
