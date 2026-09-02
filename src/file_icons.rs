//! Per-filetype painted icons (Zed/VS Code-style) for the project tree and
//! editor tabs.
//!
//! Ships a compact, font-independent icon set: each recognized file type gets
//! a rounded translucent badge tinted with its conventional language brand
//! color plus a short monogram, so no icon font dependency is required.

use egui::{Color32, FontId, Painter, Rect, Align2, vec2};

/// Recognized file kinds with their brand colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileIcon {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    React,
    Json,
    Toml,
    Yaml,
    Markdown,
    Html,
    Css,
    Go,
    C,
    Cpp,
    CSharp,
    Java,
    Ruby,
    Php,
    Shell,
    Sql,
    Image,
    Font,
    Archive,
    Lock,
    Git,
    Docker,
    Text,
}

impl FileIcon {
    /// Conventional brand color for the type.
    pub const fn color(self) -> Color32 {
        match self {
            FileIcon::Rust => Color32::from_rgb(222, 125, 60),
            FileIcon::Python => Color32::from_rgb(83, 176, 222),
            FileIcon::JavaScript => Color32::from_rgb(228, 190, 75),
            FileIcon::TypeScript => Color32::from_rgb(96, 151, 246),
            FileIcon::React => Color32::from_rgb(97, 218, 251),
            FileIcon::Json => Color32::from_rgb(203, 173, 45),
            FileIcon::Toml => Color32::from_rgb(156, 132, 108),
            FileIcon::Yaml => Color32::from_rgb(203, 111, 68),
            FileIcon::Markdown => Color32::from_rgb(120, 158, 230),
            FileIcon::Html => Color32::from_rgb(228, 110, 66),
            FileIcon::Css => Color32::from_rgb(78, 155, 220),
            FileIcon::Go => Color32::from_rgb(93, 178, 216),
            FileIcon::C => Color32::from_rgb(95, 133, 190),
            FileIcon::Cpp => Color32::from_rgb(0, 148, 206),
            FileIcon::CSharp => Color32::from_rgb(114, 158, 62),
            FileIcon::Java => Color32::from_rgb(224, 118, 80),
            FileIcon::Ruby => Color32::from_rgb(204, 70, 66),
            FileIcon::Php => Color32::from_rgb(126, 133, 188),
            FileIcon::Shell => Color32::from_rgb(137, 176, 82),
            FileIcon::Sql => Color32::from_rgb(219, 128, 82),
            FileIcon::Image => Color32::from_rgb(165, 142, 220),
            FileIcon::Font => Color32::from_rgb(226, 106, 106),
            FileIcon::Archive => Color32::from_rgb(218, 176, 96),
            FileIcon::Lock => Color32::from_rgb(160, 160, 160),
            FileIcon::Git => Color32::from_rgb(240, 110, 74),
            FileIcon::Docker => Color32::from_rgb(70, 162, 228),
            FileIcon::Text => Color32::from_rgb(140, 150, 165),
        }
    }

    /// Short monogram painted inside the badge.
    pub const fn monogram(self) -> &'static str {
        match self {
            FileIcon::Rust => "RS",
            FileIcon::Python => "PY",
            FileIcon::JavaScript => "JS",
            FileIcon::TypeScript => "TS",
            FileIcon::React => "RX",
            FileIcon::Json => "{}",
            FileIcon::Toml => "TM",
            FileIcon::Yaml => "YM",
            FileIcon::Markdown => "MD",
            FileIcon::Html => "<>",
            FileIcon::Css => "CS",
            FileIcon::Go => "GO",
            FileIcon::C => "C",
            FileIcon::Cpp => "C+",
            FileIcon::CSharp => "C#",
            FileIcon::Java => "JV",
            FileIcon::Ruby => "RB",
            FileIcon::Php => "PH",
            FileIcon::Shell => "SH",
            FileIcon::Sql => "SQ",
            FileIcon::Image => "IM",
            FileIcon::Font => "FT",
            FileIcon::Archive => "AR",
            FileIcon::Lock => "LK",
            FileIcon::Git => "GT",
            FileIcon::Docker => "DK",
            FileIcon::Text => "TX",
        }
    }
}

/// Classify a file by name (special filenames first, then extension).
pub fn icon_for(name: &str) -> FileIcon {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" => {
            return FileIcon::Lock;
        }
        "dockerfile" | "dockerfile.dev" => return FileIcon::Docker,
        ".gitignore" | ".gitattributes" | ".gitmodules" => return FileIcon::Git,
        "makefile" | "cmakelists.txt" => return FileIcon::C,
        _ => {}
    }
    let extension = name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    // Files without a dot are not extension-classified.
    if extension == lower {
        return FileIcon::Text;
    }
    match extension.as_str() {
        "rs" => FileIcon::Rust,
        "py" | "pyi" => FileIcon::Python,
        "js" | "mjs" | "cjs" => FileIcon::JavaScript,
        "ts" | "mts" | "cts" => FileIcon::TypeScript,
        "jsx" | "tsx" => FileIcon::React,
        "json" | "jsonc" | "json5" => FileIcon::Json,
        "toml" => FileIcon::Toml,
        "yaml" | "yml" => FileIcon::Yaml,
        "md" | "markdown" | "mdx" => FileIcon::Markdown,
        "html" | "htm" => FileIcon::Html,
        "css" | "scss" | "sass" | "less" => FileIcon::Css,
        "go" => FileIcon::Go,
        "c" | "h" => FileIcon::C,
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => FileIcon::Cpp,
        "cs" => FileIcon::CSharp,
        "java" => FileIcon::Java,
        "rb" => FileIcon::Ruby,
        "php" => FileIcon::Php,
        "sh" | "bash" | "zsh" | "fish" | "ps1" => FileIcon::Shell,
        "sql" => FileIcon::Sql,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "svg" => FileIcon::Image,
        "ttf" | "otf" | "woff" | "woff2" => FileIcon::Font,
        "zip" | "tar" | "gz" | "7z" | "rar" => FileIcon::Archive,
        "lock" => FileIcon::Lock,
        _ => FileIcon::Text,
    }
}

/// Paint a 14×14 rounded badge with a translucent type-color fill and the
/// type's monogram, centered in `rect`.
pub fn paint(painter: &Painter, rect: Rect, name: &str, default_color: Color32) {
    let icon = icon_for(name);
    let color = if matches!(icon, FileIcon::Text) {
        default_color
    } else {
        icon.color()
    };
    let badge_size = rect.height().min(14.0);
    let badge = Rect::from_center_size(
        rect.center(),
        vec2(badge_size, badge_size),
    );
    painter.rect_filled(badge, 3.0, Color32::from_rgba_unmultiplied(
        color.r(),
        color.g(),
        color.b(),
        38,
    ));
    let font = FontId::monospace(badge_size * 0.62);
    painter.text(
        badge.center(),
        Align2::CENTER_CENTER,
        icon.monogram(),
        font,
        color,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_by_extension() {
        assert_eq!(icon_for("main.rs"), FileIcon::Rust);
        assert_eq!(icon_for("app.tsx"), FileIcon::React);
        assert_eq!(icon_for("config.yaml"), FileIcon::Yaml);
        assert_eq!(icon_for("notes.txt"), FileIcon::Text);
        assert_eq!(icon_for("Cargo.toml"), FileIcon::Toml);
    }

    #[test]
    fn special_filenames_win_over_extensions() {
        assert_eq!(icon_for("Cargo.lock"), FileIcon::Lock);
        assert_eq!(icon_for("Dockerfile"), FileIcon::Docker);
        assert_eq!(icon_for(".gitignore"), FileIcon::Git);
        assert_eq!(icon_for("pnpm-lock.yaml"), FileIcon::Lock);
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(icon_for("PHOTO.PNG"), FileIcon::Image);
        assert_eq!(icon_for("MAIN.RS"), FileIcon::Rust);
    }
}
