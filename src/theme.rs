use egui::{Color32, Rounding, Stroke, Visuals};

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

/// Builds egui's [`Visuals`] from a semantic palette.
///
/// The intent is a shell where surfaces are separated by *layer* rather than by
/// outlines: widgets carry no frame at rest, interaction states are small translucent
/// scrims derived from the surface underneath (see [`crate::chrome`]), and the only
/// lines left in the UI are hairlines that quietly mark structure.
fn visuals(scheme: ColorScheme, colors: SemanticPalette) -> Visuals {
    let mut visuals = match scheme {
        ColorScheme::Dark => Visuals::dark(),
        ColorScheme::Light => Visuals::light(),
    };

    let chrome = crate::chrome::chrome_fill(&colors);
    let divider = crate::chrome::divider(&colors);
    let hovered_fill = crate::chrome::hover_on(chrome, &colors);
    let pressed_fill = crate::chrome::active_on(chrome, &colors);

    visuals.dark_mode = scheme == ColorScheme::Dark;
    visuals.override_text_color = Some(colors.primary_text);

    // Three levels of one neutral ramp: document, chrome, elevation.
    visuals.panel_fill = chrome;
    visuals.window_fill = colors.elevated_background;
    visuals.extreme_bg_color = colors.editor_background;
    visuals.faint_bg_color = colors.current_line;
    visuals.code_bg_color = colors.editor_background;
    visuals.hyperlink_color = colors.accent;
    visuals.warn_fg_color = colors.warning;
    visuals.error_fg_color = colors.error;

    // A 2px accent caret instead of egui's 2px pale bar.
    visuals.text_cursor = Stroke::new(2.0, colors.accent);
    visuals.text_cursor_preview = false;

    // Selection is a tint, not a box. The stroke doubles as egui's focused-field
    // outline (`TextEdit` paints `selection.stroke` while it has focus), so keeping it
    // as a 1px accent hairline is what makes an input "pop" when clicked.
    visuals.selection.bg_fill = colors.selection;
    visuals.selection.stroke = Stroke::new(1.0, colors.accent);

    // Floating containers get a hairline and a soft shadow rather than a heavy frame.
    visuals.window_rounding = Rounding::same(crate::chrome::RADIUS_PANEL);
    visuals.menu_rounding = Rounding::same(crate::chrome::RADIUS_PANEL);
    visuals.window_stroke = Stroke::new(1.0, divider);
    visuals.window_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 2.0),
        blur: 12.0,
        spread: 0.0,
        color: Color32::from_black_alpha(70),
    };
    visuals.popup_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 4.0),
        blur: 16.0,
        spread: 0.0,
        color: Color32::from_black_alpha(84),
    };

    let widgets = &mut visuals.widgets;

    // Labels, separators, and panel rules: the divider is the only stroke the shell
    // draws routinely, and it is tuned to sit just above the panel it is painted on.
    widgets.noninteractive.bg_fill = chrome;
    widgets.noninteractive.weak_bg_fill = chrome;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, divider);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, colors.primary_text);

    // Buttons, tabs, and rows are flat until you point at them. Fields stay legible
    // without a border because their fill is the document step, one level below the
    // panel they sit on.
    widgets.inactive.bg_fill = Color32::TRANSPARENT;
    widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    widgets.inactive.bg_stroke = Stroke::NONE;
    widgets.inactive.fg_stroke = Stroke::new(1.0, colors.muted_text);

    for (state, fill) in [
        (&mut widgets.hovered, hovered_fill),
        (&mut widgets.active, pressed_fill),
        (&mut widgets.open, pressed_fill),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::NONE;
        state.fg_stroke = Stroke::new(1.0, colors.primary_text);
    }

    // One corner radius for controls, and no overhanging hover frames: `expansion`
    // would let egui draw a highlight box larger than the widget it belongs to.
    for widget in [
        &mut widgets.noninteractive,
        &mut widgets.inactive,
        &mut widgets.hovered,
        &mut widgets.active,
        &mut widgets.open,
    ] {
        widget.rounding = Rounding::same(crate::chrome::RADIUS_WIDGET);
        widget.expansion = 0.0;
    }

    visuals.striped = false;
    visuals.collapsing_header_frame = false;
    visuals.indent_has_left_vline = true;
    visuals.slider_trailing_fill = false;
    visuals
}

fn blend(a: Color32, b: Color32, amount: f32) -> Color32 {
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * amount).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

fn palette(semantic: SemanticPalette, syntax: SyntaxPalette) -> ThemePalette {
    ThemePalette { semantic, syntax }
}

/// Zed's dark shell: one warm-neutral ramp ("sand") for every surface, plus a single
/// blue for accent, focus, and links. Surfaces differ by *step*, not by hue, which is
/// what keeps the dock, tab bar, and editor legible as separate layers without the
/// borders a VS Code-style theme needs.
pub fn blue_dark() -> ThemePalette {
    let document = rgb(0x111110); // step 1 - editor, text fields
    let chrome = rgb(0x191918); // step 2 - dock, tab bar, status bar, title bar
    let elevated = rgb(0x222221); // step 3 - popups, current line, insets
    let raised = rgb(0x2a2a28); // step 4 - selected-but-unfocused
    let matched = rgb(0x31312e); // step 5 - find matches
    let rule = rgb(0x3b3a37); // step 6 - the only border color
    let placeholder = rgb(0x7c7b74); // step 10 - line numbers, dim labels
    let muted = rgb(0xb5b3ad); // step 11 - secondary text, icons
    let text = rgb(0xeeeeec); // step 12 - primary text
    let accent = rgb(0x70b8ff); // blue step 11

    palette(
        SemanticPalette {
            ui_background: elevated,
            panel_background: chrome,
            elevated_background: elevated,
            editor_background: document,
            primary_text: text,
            muted_text: muted,
            selection: blend(document, accent, 0.30),
            inactive_selection: raised,
            accent,
            border: rule,
            error: rgb(0xff9592),
            warning: rgb(0xf5e147),
            information: accent,
            success: rgb(0x3dd68c),
            search_match: matched,
            active_search_match: rgba(0xf5e147, 110),
            current_line: elevated,
            completion_function: rgb(0x8cc4ff),
            completion_type: rgb(0x5cc8c0),
            completion_field: rgb(0x9fce9f),
            completion_variable: muted,
            completion_module: rgb(0xe6c07b),
            completion_keyword: rgb(0xc193e8),
            // Inlay hint semantic colors
            inlay_type_hint_text: placeholder,
            inlay_type_hint_background: rgba(0x2a2a28, 190),
            inlay_parameter_hint_text: placeholder,
            inlay_parameter_hint_background: rgba(0x2a2a28, 150),
            inlay_hint_border: rgba(0x3b3a37, 120),
            // Hover popup semantic colors
            hover_code_background: document,
            hover_link: accent,
        },
        SyntaxPalette {
            default: text,
            comment: placeholder,
            string: rgb(0x9fce9f),
            number: rgb(0xd9a373),
            keyword: rgb(0xc193e8),
            type_name: rgb(0x5cc8c0),
            macro_name: rgb(0xe6c07b),
            lifetime: rgb(0x8cc4ff),
            function: rgb(0x8cc4ff),
            symbol: muted,
        },
    )
}

/// The light sibling of [`blue_dark`]: the same ramp read in the opposite direction,
/// where a *whiter* surface means "closer to the reader". Popups and text fields take
/// the brightest step and the dock steps down from them, so elevation still reads as
/// elevation without shadows doing all the work.
fn blue_light() -> ThemePalette {
    let document = rgb(0xfdfdfc); // step 1 - editor, text fields, popups
    let chrome = rgb(0xf9f9f8); // step 2 - dock, tab bar, status bar
    let elevated = rgb(0xf1f0ef); // step 3 - insets, current line
    let raised = rgb(0xe9e8e6); // step 4 - selected-but-unfocused
    let matched = rgb(0xe2e1de); // step 5 - find matches
    let rule = rgb(0xdad9d6); // step 6 - borders and separators
    let dim = rgb(0x8d8d86); // step 9 - line numbers, dim labels
    let placeholder = rgb(0x82827c); // step 10 - muted glyphs
    let muted = rgb(0x63635e); // step 11 - secondary text
    let text = rgb(0x21201c); // step 12 - primary text
    let accent = rgb(0x0d74ce); // blue step 11

    palette(
        SemanticPalette {
            ui_background: elevated,
            panel_background: chrome,
            elevated_background: document,
            editor_background: document,
            primary_text: text,
            muted_text: muted,
            selection: blend(document, accent, 0.22),
            inactive_selection: raised,
            accent,
            border: rule,
            error: rgb(0xce2c31),
            warning: rgb(0x9e6c00),
            information: accent,
            success: rgb(0x218358),
            search_match: rgb(0xd5d3c8),
            active_search_match: rgba(0xffdc00, 130),
            current_line: elevated,
            completion_function: rgb(0x0b62c4),
            completion_type: rgb(0x0d6b7a),
            completion_field: rgb(0x0a6b3d),
            completion_variable: rgb(0x4a4a45),
            completion_module: rgb(0x8a4a00),
            completion_keyword: rgb(0x9a3f9e),
            // Inlay hint semantic colors
            inlay_type_hint_text: dim,
            inlay_type_hint_background: rgba(0xe9e8e6, 200),
            inlay_parameter_hint_text: dim,
            inlay_parameter_hint_background: rgba(0xe9e8e6, 160),
            inlay_hint_border: rgba(0xdad9d6, 160),
            // Hover popup semantic colors
            hover_code_background: elevated,
            hover_link: accent,
        },
        SyntaxPalette {
            default: text,
            comment: dim,
            string: rgb(0x0a6b3d),
            number: rgb(0x8a4a00),
            keyword: rgb(0x9a3f9e),
            type_name: rgb(0x0d6b7a),
            macro_name: rgb(0x7a5c00),
            lifetime: rgb(0x0b62c4),
            function: rgb(0x0b62c4),
            symbol: rgb(0x4a4a45),
        },
    )
}

fn nord() -> ThemePalette {
    palette(
        SemanticPalette {
            ui_background: rgb(0x434c5e),
            panel_background: rgb(0x3b4252),
            elevated_background: rgb(0x434c5e),
            editor_background: rgb(0x2e3440),
            primary_text: rgb(0xeceff4),
            muted_text: rgb(0xd8dee9),
            selection: rgb(0x4c566a),
            inactive_selection: rgb(0x3b4252),
            accent: rgb(0x88c0d0),
            border: blend(rgb(0x3b4252), rgb(0x4c566a), 0.55),
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
            ui_background: rgb(0x343746),
            panel_background: rgb(0x21222c),
            elevated_background: rgb(0x343746),
            editor_background: rgb(0x282a36),
            primary_text: rgb(0xf8f8f2),
            muted_text: rgb(0xb9b9b3),
            selection: rgb(0x44475a),
            inactive_selection: rgb(0x383a4a),
            accent: rgb(0xbd93f9),
            border: blend(rgb(0x21222c), rgb(0x44475a), 0.6),
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
            ui_background: rgb(0x0b3b46),
            panel_background: rgb(0x073642),
            elevated_background: rgb(0x0b3b46),
            editor_background: rgb(0x002b36),
            primary_text: rgb(0xeee8d5),
            muted_text: rgb(0x93a1a1),
            selection: rgb(0x07586b),
            inactive_selection: rgb(0x073642),
            accent: rgb(0x268bd2),
            border: blend(rgb(0x073642), rgb(0x586e75), 0.5),
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
