use egui::{Color32, Visuals};

#[derive(Debug, Clone, Copy)]
pub struct ThemeColors {
    pub background: Color32,
    pub surface: Color32,
    pub border: Color32,
    pub text_primary: Color32,
    pub text_secondary: Color32,
    pub text_muted: Color32,
    pub keyword: Color32,
    pub string_literal: Color32,
    pub comment: Color32,
    pub type_name: Color32,
    pub number: Color32,
    pub macro_call: Color32,
    pub error: Color32,
    pub warning: Color32,
    pub info: Color32,
    pub hint: Color32,
    pub selection: Color32,
    pub highlight_line: Color32,
    pub git_added: Color32,
    pub git_modified: Color32,
    pub git_removed: Color32,
    pub minimap_viewport: Color32,
}
fn rgb(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}
pub fn built_in(name: &str) -> ThemeColors {
    let (bg, surface, text, secondary, accent) = match name {
        "light" => (0xf7f7f7, 0xffffff, 0x202124, 0x5f6368, 0x0969da),
        "monokai" => (0x272822, 0x30312b, 0xf8f8f2, 0xc5c8c6, 0xf92672),
        "solarized" => (0x002b36, 0x073642, 0xeee8d5, 0x93a1a1, 0x268bd2),
        _ => (0x111827, 0x1f2937, 0xf3f4f6, 0x9ca3af, 0x60a5fa),
    };
    ThemeColors {
        background: rgb(bg),
        surface: rgb(surface),
        border: rgb(secondary),
        text_primary: rgb(text),
        text_secondary: rgb(secondary),
        text_muted: rgb(secondary),
        keyword: rgb(accent),
        string_literal: rgb(0xa6e22e),
        comment: rgb(0x6b7280),
        type_name: rgb(0x66d9ef),
        number: rgb(0xae81ff),
        macro_call: rgb(0xfd971f),
        error: rgb(0xef4444),
        warning: rgb(0xf59e0b),
        info: rgb(0x3b82f6),
        hint: rgb(0x22c55e),
        selection: rgb(accent).gamma_multiply(0.45),
        highlight_line: rgb(surface),
        git_added: rgb(0x22c55e),
        git_modified: rgb(0x3b82f6),
        git_removed: rgb(0xef4444),
        minimap_viewport: rgb(accent).gamma_multiply(0.35),
    }
}
pub fn visuals(theme: &ThemeColors) -> Visuals {
    let mut v = if theme.background == rgb(0xf7f7f7) {
        Visuals::light()
    } else {
        Visuals::dark()
    };
    v.panel_fill = theme.background;
    v.window_fill = theme.surface;
    v.extreme_bg_color = theme.background;
    v.widgets.noninteractive.fg_stroke.color = theme.text_primary;
    v
}
pub fn apply(ctx: &egui::Context, theme: &ThemeColors) {
    ctx.set_visuals(visuals(theme));
}
