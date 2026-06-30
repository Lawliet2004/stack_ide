//! Markdown preview pane — renders `.md` files as formatted visual output
//! using `pulldown-cmark` for parsing and egui widgets for rendering.

use std::path::PathBuf;

use egui::{Color32, RichText, Ui};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// Per-pane state for the markdown preview.
pub struct MarkdownPreviewState {
    pub path: PathBuf,
    pub scroll_y: f32,
    /// Debounce: last time the content changed (monotonic seconds).
    pub last_changed: f64,
    /// Cached content hash to avoid re-parsing identical content.
    _last_content_hash: u64,
}

impl MarkdownPreviewState {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            scroll_y: 0.0,
            last_changed: 0.0,
            _last_content_hash: 0,
        }
    }
}

/// Render the markdown preview into `ui`.
///
/// `content` is the raw markdown text (from the buffer or from disk).
/// Returns `true` if the "Edit source" toggle button is clicked.
pub fn render_markdown(
    ui: &mut egui::Ui,
    state: &mut MarkdownPreviewState,
    content: &str,
    palette: crate::theme::SemanticPalette,
) -> bool {
    let mut toggle_to_editor = false;

    // Top bar with "< >" toggle button
    egui::TopBottomPanel::top("md_preview_topbar")
        .resizable(false)
        .exact_height(28.0)
        .show_inside(ui, |ui| {
            ui.horizontal_centered(|ui| {
                if ui
                    .button("</> Edit source")
                    .on_hover_text("Switch to code editor view")
                    .clicked()
                {
                    toggle_to_editor = true;
                }
                ui.separator();
                ui.label(
                    RichText::new(
                        state
                            .path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    )
                    .small()
                    .color(palette.muted_text),
                );
            });
        });

    if toggle_to_editor {
        return true;
    }

    egui::ScrollArea::vertical()
        .id_source(("md_preview_scroll", &state.path))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_max_width(ui.available_width().min(800.0));

            let options = Options::all();
            let parser = Parser::new_ext(content, options);

            let events: Vec<Event<'_>> = parser.collect();

            render_events(ui, &events, &state.path, palette);
        });

    false
}

// ─── Event rendering ─────────────────────────────────────────────────────────

struct RenderContext {
    /// Nesting depth in lists.
    list_depth: usize,
    /// Ordered list counter at each depth.
    list_counter: Vec<Option<u64>>,
    /// True when inside a blockquote.
    in_blockquote: bool,
    /// True when inside a code block.
    in_code_block: bool,
    /// Language hint for code blocks.
    code_lang: String,
    /// Accumulated code block text.
    code_content: String,
    /// Accumulated inline paragraph text.
    paragraph_text: String,
    /// Accumulated heading text.
    heading_level: u32,
    heading_text: String,
    /// True when inside a table.
    in_table: bool,
    table_header: bool,
    table_rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    /// Base directory of the markdown file (for relative image paths).
    base_dir: PathBuf,
}

impl RenderContext {
    fn new(base_dir: PathBuf) -> Self {
        Self {
            list_depth: 0,
            list_counter: Vec::new(),
            in_blockquote: false,
            in_code_block: false,
            code_lang: String::new(),
            code_content: String::new(),
            paragraph_text: String::new(),
            heading_level: 0,
            heading_text: String::new(),
            in_table: false,
            table_header: false,
            table_rows: Vec::new(),
            current_row: Vec::new(),
            current_cell: String::new(),
            base_dir,
        }
    }
}

fn render_events(
    ui: &mut Ui,
    events: &[Event<'_>],
    md_path: &PathBuf,
    palette: crate::theme::SemanticPalette,
) {
    let base_dir = md_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();

    let mut ctx = RenderContext::new(base_dir);

    for event in events {
        match event {
            Event::Start(tag) => handle_start(ui, &mut ctx, tag, palette),
            Event::End(tag) => handle_end(ui, &mut ctx, tag, palette),
            Event::Text(text) => handle_text(&mut ctx, text.as_ref()),
            Event::Code(code) => {
                // Inline code
                if ctx.in_code_block {
                    ctx.code_content.push_str(code.as_ref());
                } else {
                    render_inline_code(ui, code.as_ref(), palette);
                }
            }
            Event::SoftBreak => {
                if !ctx.in_code_block {
                    ctx.paragraph_text.push(' ');
                }
            }
            Event::HardBreak => {
                if !ctx.in_code_block {
                    flush_paragraph(ui, &mut ctx, palette);
                }
            }
            Event::Rule => {
                flush_paragraph(ui, &mut ctx, palette);
                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);
            }
            Event::Html(_) => {
                // Strip HTML, render inner text
            }
            Event::FootnoteReference(_) => {}
            Event::TaskListMarker(checked) => {
                ui.checkbox(&mut (*checked).clone(), "");
            }
            Event::InlineHtml(_) => {}
        }
    }

    // Flush any remaining content
    flush_paragraph(ui, &mut ctx, palette);
}

fn handle_start(
    ui: &mut Ui,
    ctx: &mut RenderContext,
    tag: &Tag<'_>,
    palette: crate::theme::SemanticPalette,
) {
    match tag {
        Tag::Heading { level, .. } => {
            flush_paragraph(ui, ctx, palette);
            ctx.heading_level = *level as u32;
            ctx.heading_text.clear();
        }
        Tag::Paragraph => {
            ctx.paragraph_text.clear();
        }
        Tag::BlockQuote => {
            flush_paragraph(ui, ctx, palette);
            ctx.in_blockquote = true;
        }
        Tag::CodeBlock(kind) => {
            flush_paragraph(ui, ctx, palette);
            ctx.in_code_block = true;
            ctx.code_content.clear();
            ctx.code_lang = match kind {
                pulldown_cmark::CodeBlockKind::Fenced(lang) => lang.to_string(),
                pulldown_cmark::CodeBlockKind::Indented => String::new(),
            };
        }
        Tag::List(start) => {
            flush_paragraph(ui, ctx, palette);
            ctx.list_depth += 1;
            ctx.list_counter.push(*start);
        }
        Tag::Item => {
            ctx.paragraph_text.clear();
        }
        Tag::Table(_) => {
            flush_paragraph(ui, ctx, palette);
            ctx.in_table = true;
            ctx.table_rows.clear();
        }
        Tag::TableHead => {
            ctx.table_header = true;
        }
        Tag::TableRow => {
            ctx.current_row.clear();
        }
        Tag::TableCell => {
            ctx.current_cell.clear();
        }
        Tag::Emphasis => {} // italic — will be handled at text level
        Tag::Strong => {}   // bold
        Tag::Strikethrough => {}
        Tag::Link { dest_url: _, .. } => {
            // Store URL for rendering when End(Link) fires
        }
        Tag::Image { dest_url, .. } => {
            flush_paragraph(ui, ctx, palette);
            render_image(ui, dest_url.as_ref(), "", &ctx.base_dir, palette);
        }
        _ => {}
    }
}

fn handle_end(
    ui: &mut Ui,
    ctx: &mut RenderContext,
    tag: &TagEnd,
    palette: crate::theme::SemanticPalette,
) {
    match tag {
        TagEnd::Heading(_) => {
            flush_heading(ui, ctx, palette);
            ui.add_space(4.0);
        }
        TagEnd::Paragraph => {
            flush_paragraph(ui, ctx, palette);
            ui.add_space(8.0);
        }
        TagEnd::BlockQuote => {
            flush_paragraph(ui, ctx, palette);
            ctx.in_blockquote = false;
            ui.add_space(4.0);
        }
        TagEnd::CodeBlock => {
            flush_code_block(ui, ctx, palette);
            ctx.in_code_block = false;
            ui.add_space(4.0);
        }
        TagEnd::List(_) => {
            if ctx.list_depth > 0 {
                ctx.list_depth -= 1;
            }
            ctx.list_counter.pop();
            ui.add_space(4.0);
        }
        TagEnd::Item => {
            let indent = ctx.list_depth.saturating_sub(1) as f32 * 16.0;
            let is_ordered = ctx.list_counter.last().copied().flatten().is_some();

            let text = std::mem::take(&mut ctx.paragraph_text);
            if !text.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(indent);
                    let bullet = if is_ordered {
                        if let Some(Some(n)) = ctx.list_counter.last_mut() {
                            let s = format!("{}.", n);
                            *n += 1;
                            s
                        } else {
                            "•".to_owned()
                        }
                    } else {
                        "•".to_owned()
                    };
                    ui.label(&bullet);
                    ui.label(&text);
                });
            }
        }
        TagEnd::Table => {
            flush_table(ui, ctx, palette);
            ctx.in_table = false;
        }
        TagEnd::TableHead => {
            ctx.table_rows.push(ctx.current_row.clone());
            ctx.table_header = false;
        }
        TagEnd::TableRow => {
            if !ctx.table_header {
                ctx.table_rows.push(ctx.current_row.clone());
            }
        }
        TagEnd::TableCell => {
            let cell = std::mem::take(&mut ctx.current_cell);
            ctx.current_row.push(cell);
        }
        TagEnd::Link => {
            // Link end — the text was accumulated, render it
            let text = std::mem::take(&mut ctx.paragraph_text);
            if !text.is_empty() {
                ui.label(RichText::new(&text).color(palette.hover_link).underline());
            }
        }
        _ => {}
    }
}

fn handle_text(ctx: &mut RenderContext, text: &str) {
    if ctx.in_code_block {
        ctx.code_content.push_str(text);
    } else if ctx.heading_level > 0 {
        ctx.heading_text.push_str(text);
    } else if ctx.in_table {
        ctx.current_cell.push_str(text);
    } else {
        ctx.paragraph_text.push_str(text);
    }
}

fn flush_paragraph(ui: &mut Ui, ctx: &mut RenderContext, palette: crate::theme::SemanticPalette) {
    let text = std::mem::take(&mut ctx.paragraph_text);
    if text.is_empty() {
        return;
    }

    if ctx.in_blockquote {
        // Blockquote: left border + indented text
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(3.0, 18.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect.expand2(egui::vec2(0.0, 2.0)), 0.0, palette.accent);
            ui.add_space(8.0);
            ui.label(RichText::new(&text).color(palette.muted_text));
        });
    } else {
        ui.label(&text);
    }
}

fn flush_heading(ui: &mut Ui, ctx: &mut RenderContext, palette: crate::theme::SemanticPalette) {
    let text = std::mem::take(&mut ctx.heading_text);
    if text.is_empty() {
        ctx.heading_level = 0;
        return;
    }

    let level = ctx.heading_level;
    ctx.heading_level = 0;

    let (size, color) = match level {
        1 => (28.0, palette.primary_text),
        2 => (22.0, palette.primary_text),
        3 => (18.0, palette.primary_text),
        4 => (14.0, palette.muted_text),
        5 => (13.0, palette.muted_text),
        _ => (12.0, palette.muted_text),
    };

    let label = ui.label(RichText::new(&text).size(size).strong().color(color));

    if level <= 2 {
        // Bottom border line under H1/H2
        let rect = label.rect;
        let y = rect.bottom() + 2.0;
        ui.painter().line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(1.0, palette.border),
        );
        ui.add_space(4.0);
    }
}

fn flush_code_block(ui: &mut Ui, ctx: &mut RenderContext, palette: crate::theme::SemanticPalette) {
    let code = std::mem::take(&mut ctx.code_content);
    if code.is_empty() {
        return;
    }

    let lang = std::mem::take(&mut ctx.code_lang);

    egui::Frame::none()
        .fill(palette.editor_background)
        .inner_margin(egui::Margin::same(8.0))
        .rounding(egui::Rounding::same(4.0))
        .show(ui, |ui| {
            if !lang.is_empty() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                    ui.label(RichText::new(&lang).small().color(palette.muted_text));
                });
            }
            egui::ScrollArea::horizontal().show(ui, |ui| {
                ui.label(
                    RichText::new(&code)
                        .monospace()
                        .size(12.0)
                        .color(palette.primary_text),
                );
            });
        });
}

fn flush_table(ui: &mut Ui, ctx: &mut RenderContext, _palette: crate::theme::SemanticPalette) {
    if ctx.table_rows.is_empty() {
        return;
    }

    let rows = std::mem::take(&mut ctx.table_rows);
    let col_count = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if col_count == 0 {
        return;
    }

    egui::ScrollArea::horizontal().show(ui, |ui| {
        egui::Grid::new(ui.make_persistent_id("md_table"))
            .striped(true)
            .show(ui, |ui| {
                for (row_idx, row) in rows.iter().enumerate() {
                    for col in 0..col_count {
                        let cell = row.get(col).map(|s| s.as_str()).unwrap_or("");
                        if row_idx == 0 {
                            ui.label(RichText::new(cell).strong());
                        } else {
                            ui.label(cell);
                        }
                    }
                    ui.end_row();
                }
            });
    });

    ui.add_space(8.0);
}

fn render_inline_code(ui: &mut Ui, code: &str, palette: crate::theme::SemanticPalette) {
    // Inline code with a subtle background
    ui.label(
        RichText::new(code)
            .monospace()
            .background_color(Color32::from_rgba_unmultiplied(80, 80, 80, 120))
            .color(palette.primary_text),
    );
}

fn render_image(
    ui: &mut Ui,
    url: &str,
    alt: &str,
    base_dir: &PathBuf,
    palette: crate::theme::SemanticPalette,
) {
    // Only load relative file paths; skip http/https
    if url.starts_with("http://") || url.starts_with("https://") {
        ui.label(
            RichText::new(format!("[image: {}]", alt))
                .color(palette.muted_text)
                .italics(),
        );
        return;
    }

    let path = base_dir.join(url);
    if !path.exists() {
        ui.label(
            RichText::new(format!("[image not found: {}]", url))
                .color(palette.error)
                .small(),
        );
        return;
    }

    // Load synchronously (files < 2 MB)
    let metadata_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if metadata_size > 2 * 1024 * 1024 {
        // Large image: show placeholder
        ui.label(
            RichText::new(format!("[large image: {}]", url))
                .color(palette.muted_text)
                .italics(),
        );
        return;
    }

    match std::fs::read(&path) {
        Err(_) => {
            ui.label(
                RichText::new(format!("[cannot read: {}]", url))
                    .color(palette.error)
                    .small(),
            );
        }
        Ok(bytes) => match image::load_from_memory(&bytes) {
            Err(_) => {
                ui.label(
                    RichText::new(format!("[cannot decode: {}]", url))
                        .color(palette.error)
                        .small(),
                );
            }
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                let pixels: Vec<Color32> = rgba
                    .chunks_exact(4)
                    .map(|p| Color32::from_rgba_premultiplied(p[0], p[1], p[2], p[3]))
                    .collect();
                let color_image = egui::ColorImage {
                    size: [w as usize, h as usize],
                    pixels,
                };
                let texture = ui.ctx().load_texture(
                    format!("md_img:{}", path.to_string_lossy()),
                    color_image,
                    egui::TextureOptions::LINEAR,
                );
                let max_w = ui.available_width();
                let scale = (max_w / w as f32).min(1.0);
                let display_w = w as f32 * scale;
                let display_h = h as f32 * scale;
                ui.image(egui::load::SizedTexture::new(
                    texture.id(),
                    egui::vec2(display_w, display_h),
                ));
            }
        },
    }
}
