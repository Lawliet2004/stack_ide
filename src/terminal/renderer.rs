use egui::Color32;

#[derive(Debug, Clone)]
pub struct Cell {
    pub ch: char,
    pub fg: egui::Color32,
    pub bg: egui::Color32,
    pub bold: bool,
    pub italic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParserState {
    Normal,
    Escape,
    Csi,
    Osc,
    GCharset,
}

pub struct TerminalBuffer {
    pub lines: Vec<Vec<Cell>>,       // screen lines
    pub scroll_back: Vec<Vec<Cell>>, // scrollback history, max 10000 lines
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub cols: usize,
    pub rows: usize,
    // ANSI parser state
    current_fg: egui::Color32,
    current_bg: egui::Color32,
    current_bold: bool,
    parser_state: ParserState,
    escape_buf: String,
    utf8_buf: Vec<u8>,
}

impl TerminalBuffer {
    pub fn new(rows: usize, cols: usize) -> Self {
        let default_fg = Color32::from_rgb(220, 220, 220);
        let default_bg = Color32::TRANSPARENT;
        let mut lines = Vec::with_capacity(rows);
        for _ in 0..rows {
            lines.push(vec![
                Cell {
                    ch: ' ',
                    fg: default_fg,
                    bg: default_bg,
                    bold: false,
                    italic: false,
                };
                cols
            ]);
        }
        TerminalBuffer {
            lines,
            scroll_back: Vec::new(),
            cursor_row: 0,
            cursor_col: 0,
            cols,
            rows,
            current_fg: default_fg,
            current_bg: default_bg,
            current_bold: false,
            parser_state: ParserState::Normal,
            escape_buf: String::new(),
            utf8_buf: Vec::new(),
        }
    }

    pub fn resize(&mut self, new_rows: usize, new_cols: usize) {
        if new_rows == self.rows && new_cols == self.cols {
            return;
        }

        let default_fg = Color32::from_rgb(220, 220, 220);
        let default_bg = Color32::TRANSPARENT;

        // Adjust each existing line to new_cols
        for line in &mut self.lines {
            if line.len() < new_cols {
                line.resize(
                    new_cols,
                    Cell {
                        ch: ' ',
                        fg: default_fg,
                        bg: default_bg,
                        bold: false,
                        italic: false,
                    },
                );
            } else if line.len() > new_cols {
                line.truncate(new_cols);
            }
        }

        // If new_rows is larger, add new lines
        if self.lines.len() < new_rows {
            while self.lines.len() < new_rows {
                self.lines.push(vec![
                    Cell {
                        ch: ' ',
                        fg: default_fg,
                        bg: default_bg,
                        bold: false,
                        italic: false,
                    };
                    new_cols
                ]);
            }
        } else if self.lines.len() > new_rows {
            // If new_rows is smaller, move excess lines to scrollback!
            let diff = self.lines.len() - new_rows;
            for mut line in self.lines.drain(0..diff) {
                line.truncate(new_cols);
                self.scroll_back.push(line);
            }
        }

        self.cols = new_cols;
        self.rows = new_rows;

        // Clamp cursor within bounds
        if self.cursor_row >= new_rows {
            self.cursor_row = new_rows.saturating_sub(1);
        }
        if self.cursor_col >= new_cols {
            self.cursor_col = new_cols.saturating_sub(1);
        }

        // Enforce scrollback limit
        if self.scroll_back.len() > 10_000 {
            let to_drain = self.scroll_back.len() - 10_000;
            self.scroll_back.drain(0..to_drain);
        }
    }

    pub fn feed(&mut self, data: &[u8]) {
        let mut temp_buf = std::mem::take(&mut self.utf8_buf);
        temp_buf.extend_from_slice(data);

        let mut i = 0;
        while i < temp_buf.len() {
            let byte = temp_buf[i];

            match self.parser_state {
                ParserState::Escape => {
                    match byte {
                        b'[' => {
                            self.parser_state = ParserState::Csi;
                            self.escape_buf.clear();
                            self.escape_buf.push('[');
                        }
                        b']' => {
                            self.parser_state = ParserState::Osc;
                            self.escape_buf.clear();
                        }
                        b'(' | b')' => {
                            self.parser_state = ParserState::GCharset;
                        }
                        _ => {
                            self.parser_state = ParserState::Normal;
                        }
                    }
                    i += 1;
                }
                ParserState::Csi => {
                    self.escape_buf.push(byte as char);
                    if (0x40..=0x7E).contains(&byte) {
                        self.process_csi_sequence();
                        self.parser_state = ParserState::Normal;
                    }
                    i += 1;
                }
                ParserState::Osc => {
                    self.escape_buf.push(byte as char);
                    if byte == 0x07 || self.escape_buf.ends_with("\x1b\\") {
                        self.parser_state = ParserState::Normal;
                    }
                    i += 1;
                }
                ParserState::GCharset => {
                    self.parser_state = ParserState::Normal;
                    i += 1;
                }
                ParserState::Normal => {
                    match byte {
                        b'\x1b' => {
                            self.parser_state = ParserState::Escape;
                        }
                        b'\n' => {
                            self.newline();
                        }
                        b'\r' => {
                            self.cursor_col = 0;
                        }
                        b'\x08' => {
                            self.cursor_col = self.cursor_col.saturating_sub(1);
                        }
                        b'\x07' => {
                            // Ignore bell
                        }
                        b'\t' => {
                            let tab_size = 8;
                            self.cursor_col = ((self.cursor_col / tab_size) + 1) * tab_size;
                            if self.cursor_col >= self.cols {
                                self.newline();
                            }
                        }
                        _ => {
                            let len = if byte < 0x80 {
                                1
                            } else if (byte & 0xE0) == 0xC0 {
                                2
                            } else if (byte & 0xF0) == 0xE0 {
                                3
                            } else if (byte & 0xF8) == 0xF0 {
                                4
                            } else {
                                1
                            };

                            if i + len > temp_buf.len() {
                                break;
                            }

                            let ch = if len == 1 {
                                byte as char
                            } else {
                                match std::str::from_utf8(&temp_buf[i..i + len]) {
                                    Ok(s) => s.chars().next().unwrap_or('?'),
                                    Err(_) => '?',
                                }
                            };

                            i += len - 1;

                            if !ch.is_ascii_control() {
                                self.write_char(ch);
                            }
                        }
                    }
                    i += 1;
                }
            }
        }

        if i < temp_buf.len() {
            self.utf8_buf = temp_buf[i..].to_vec();
        } else {
            self.utf8_buf = Vec::new();
        }
    }

    pub fn feed_str(&mut self, s: &str) {
        self.feed(s.as_bytes());
    }

    fn newline(&mut self) {
        if self.cursor_row + 1 >= self.rows {
            let first_line = self.lines.remove(0);
            self.scroll_back.push(first_line);

            if self.scroll_back.len() > 10_000 {
                let to_drain = self.scroll_back.len() - 10_000;
                self.scroll_back.drain(0..to_drain);
            }

            let default_fg = Color32::from_rgb(220, 220, 220);
            let default_bg = Color32::TRANSPARENT;
            self.lines.push(vec![
                Cell {
                    ch: ' ',
                    fg: default_fg,
                    bg: default_bg,
                    bold: false,
                    italic: false,
                };
                self.cols
            ]);
        } else {
            self.cursor_row += 1;
        }
    }

    fn write_char(&mut self, ch: char) {
        if self.cursor_col >= self.cols {
            self.cursor_col = 0;
            self.newline();
        }

        if self.cursor_row < self.lines.len() && self.cursor_col < self.cols {
            self.lines[self.cursor_row][self.cursor_col] = Cell {
                ch,
                fg: self.current_fg,
                bg: self.current_bg,
                bold: self.current_bold,
                italic: false,
            };
            self.cursor_col += 1;
        }
    }

    fn process_csi_sequence(&mut self) {
        if self.escape_buf.len() < 2 {
            return;
        }
        let final_char = self.escape_buf.chars().last().unwrap();
        let mut chars = self.escape_buf.chars();
        chars.next(); // Skip '['
        chars.next_back(); // Skip final_char
        let params_str: String = chars.collect();

        let mut params = Vec::new();
        if !params_str.is_empty() {
            for part in params_str.split(';') {
                if let Ok(val) = part.parse::<u8>() {
                    params.push(val);
                } else {
                    params.push(0);
                }
            }
        } else {
            params.push(0);
        }

        match final_char {
            'm' => {
                let mut i = 0;
                while i < params.len() {
                    let code = params[i];
                    match code {
                        0 => {
                            self.current_fg = Color32::from_rgb(220, 220, 220);
                            self.current_bg = Color32::TRANSPARENT;
                            self.current_bold = false;
                        }
                        1 => {
                            self.current_bold = true;
                        }
                        22 => {
                            self.current_bold = false;
                        }
                        30..=37 => {
                            self.current_fg = ansi_color(code, self.current_bold);
                        }
                        39 => {
                            self.current_fg = Color32::from_rgb(220, 220, 220);
                        }
                        40..=47 => {
                            self.current_bg = ansi_color(code - 10, self.current_bold);
                        }
                        49 => {
                            self.current_bg = Color32::TRANSPARENT;
                        }
                        90..=97 => {
                            self.current_fg = ansi_color(code, true);
                        }
                        100..=107 => {
                            self.current_bg = ansi_color(code - 10, true);
                        }
                        38 | 48 => {
                            if i + 1 < params.len() {
                                let mode = params[i + 1];
                                if mode == 5 {
                                    if i + 2 < params.len() {
                                        let idx = params[i + 2];
                                        let color = parse_256_color(idx);
                                        if code == 38 {
                                            self.current_fg = color;
                                        } else {
                                            self.current_bg = color;
                                        }
                                        i += 2;
                                    }
                                } else if mode == 2 {
                                    if i + 4 < params.len() {
                                        let r = params[i + 2];
                                        let g = params[i + 3];
                                        let b = params[i + 4];
                                        let color = Color32::from_rgb(r, g, b);
                                        if code == 38 {
                                            self.current_fg = color;
                                        } else {
                                            self.current_bg = color;
                                        }
                                        i += 4;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
            }
            'A' => {
                let n = params.first().copied().unwrap_or(1) as usize;
                self.cursor_row = self.cursor_row.saturating_sub(n);
            }
            'B' => {
                let n = params.first().copied().unwrap_or(1) as usize;
                self.cursor_row = std::cmp::min(self.cursor_row + n, self.rows.saturating_sub(1));
            }
            'C' => {
                let n = params.first().copied().unwrap_or(1) as usize;
                self.cursor_col = std::cmp::min(self.cursor_col + n, self.cols.saturating_sub(1));
            }
            'D' => {
                let n = params.first().copied().unwrap_or(1) as usize;
                self.cursor_col = self.cursor_col.saturating_sub(n);
            }
            'H' | 'f' => {
                let r = params.first().copied().unwrap_or(1).saturating_sub(1) as usize;
                let c = params.get(1).copied().unwrap_or(1).saturating_sub(1) as usize;
                self.cursor_row = std::cmp::min(r, self.rows.saturating_sub(1));
                self.cursor_col = std::cmp::min(c, self.cols.saturating_sub(1));
            }
            'J' => {
                let mode = params.first().copied().unwrap_or(0);
                let default_fg = Color32::from_rgb(220, 220, 220);
                let default_bg = Color32::TRANSPARENT;
                let cell = Cell {
                    ch: ' ',
                    fg: default_fg,
                    bg: default_bg,
                    bold: false,
                    italic: false,
                };
                match mode {
                    0 => {
                        for col in self.cursor_col..self.cols {
                            self.lines[self.cursor_row][col] = cell.clone();
                        }
                        for row in (self.cursor_row + 1)..self.rows {
                            for col in 0..self.cols {
                                self.lines[row][col] = cell.clone();
                            }
                        }
                    }
                    1 => {
                        for row in 0..self.cursor_row {
                            for col in 0..self.cols {
                                self.lines[row][col] = cell.clone();
                            }
                        }
                        for col in 0..=self.cursor_col {
                            if col < self.cols {
                                self.lines[self.cursor_row][col] = cell.clone();
                            }
                        }
                    }
                    2 | 3 => {
                        for row in 0..self.rows {
                            for col in 0..self.cols {
                                self.lines[row][col] = cell.clone();
                            }
                        }
                        self.cursor_row = 0;
                        self.cursor_col = 0;
                    }
                    _ => {}
                }
            }
            'K' => {
                let mode = params.first().copied().unwrap_or(0);
                let default_fg = Color32::from_rgb(220, 220, 220);
                let default_bg = Color32::TRANSPARENT;
                let cell = Cell {
                    ch: ' ',
                    fg: default_fg,
                    bg: default_bg,
                    bold: false,
                    italic: false,
                };
                match mode {
                    0 => {
                        for col in self.cursor_col..self.cols {
                            self.lines[self.cursor_row][col] = cell.clone();
                        }
                    }
                    1 => {
                        for col in 0..=self.cursor_col {
                            if col < self.cols {
                                self.lines[self.cursor_row][col] = cell.clone();
                            }
                        }
                    }
                    2 => {
                        for col in 0..self.cols {
                            self.lines[self.cursor_row][col] = cell.clone();
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

fn ansi_color(code: u8, bright: bool) -> egui::Color32 {
    match code {
        30 | 90 => {
            let v = if bright { 128 } else { 0 };
            Color32::from_rgb(v, v, v)
        }
        31 | 91 => Color32::from_rgb(if bright { 255 } else { 205 }, 49, 49),
        32 | 92 => Color32::from_rgb(if bright { 115 } else { 13 }, 188, 121),
        33 | 93 => Color32::from_rgb(if bright { 229 } else { 229 }, 229, 16),
        34 | 94 => Color32::from_rgb(if bright { 36 } else { 36 }, 114, 200),
        35 | 95 => Color32::from_rgb(if bright { 188 } else { 188 }, 63, 188),
        36 | 96 => Color32::from_rgb(if bright { 17 } else { 17 }, 168, 205),
        37 | 97 => Color32::from_rgb(if bright { 255 } else { 229 }, 229, 229),
        _ => Color32::from_rgb(204, 204, 204),
    }
}

fn parse_256_color(idx: u8) -> Color32 {
    if idx < 8 {
        ansi_color(idx + 30, false)
    } else if idx < 16 {
        ansi_color(idx - 8 + 30, true)
    } else if idx < 232 {
        let val = idx - 16;
        let r = (val / 36) * 51;
        let g = ((val / 6) % 6) * 51;
        let b = (val % 6) * 51;
        Color32::from_rgb(r, g, b)
    } else {
        let val = idx - 232;
        let g = 8 + val * 10;
        Color32::from_rgb(g, g, g)
    }
}

/// Background color of the terminal surface (VS Code "dark+" terminal bg).
pub const TERMINAL_BG: Color32 = Color32::from_rgb(24, 24, 24);
/// Inner padding around the terminal text, in points.
const TERMINAL_PADDING: f32 = 8.0;

pub fn render_terminal(
    ui: &mut egui::Ui,
    buffer: &mut TerminalBuffer,
    font_id: egui::FontId,
    ligatures_enabled: bool,
    mut ligature_renderer: Option<&mut crate::text::ligature::LigatureRenderer>,
) -> egui::Response {
    let char_size = ui.fonts(|f| f.glyph_width(&font_id, 'M'));
    // Derive the line height from the actual terminal font so glyphs and the
    // cursor line up exactly, rather than from the editor's monospace style.
    let row_height = ui.fonts(|f| f.row_height(&font_id));

    let char_width = if char_size > 0.0 { char_size } else { 8.0 };
    let line_height = if row_height > 0.0 { row_height } else { 15.0 };

    // Paint the terminal background so it reads as a distinct surface like the
    // integrated terminal in VS Code.
    let full_rect = ui.available_rect_before_wrap();
    ui.painter().rect_filled(full_rect, 0.0, TERMINAL_BG);

    let total_rows = buffer.scroll_back.len() + buffer.lines.len();

    let mut frame = egui::Frame::none().inner_margin(egui::Margin::same(TERMINAL_PADDING));
    frame.fill = Color32::TRANSPARENT;

    let scroll_output = frame
        .show(ui, |ui| {
            // Terminals are dense grids: kill inter-line spacing so rows are
            // flush against each other.
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);

            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show_rows(ui, line_height, total_rows, |ui, row_range| {
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                    let range_start = row_range.start;
                    let content_top = ui.min_rect().min;

                    for row_idx in row_range {
                        let line = if row_idx < buffer.scroll_back.len() {
                            &buffer.scroll_back[row_idx]
                        } else {
                            &buffer.lines[row_idx - buffer.scroll_back.len()]
                        };

                        let row_y = content_top.y + (row_idx - range_start) as f32 * line_height;

                        if ligatures_enabled && ligature_renderer.is_some() {
                            render_terminal_row_with_ligatures(
                                ui,
                                ligature_renderer.as_deref_mut().unwrap(),
                                line,
                                font_id.clone(),
                                line_height,
                                char_width,
                                content_top.x,
                                row_y,
                            );
                        } else {
                            let mut job = egui::text::LayoutJob::default();
                            job.wrap.max_width = f32::INFINITY;
                            for cell in line {
                                job.append(
                                    &cell.ch.to_string(),
                                    0.0,
                                    egui::text::TextFormat {
                                        font_id: font_id.clone(),
                                        color: cell.fg,
                                        background: cell.bg,
                                        ..Default::default()
                                    },
                                );
                            }
                            ui.label(job);
                        }
                    }

                    // Draw the cursor as a filled block, positioned relative to
                    // the first row currently rendered by `show_rows`.
                    let absolute_cursor_row = buffer.scroll_back.len() + buffer.cursor_row;
                    if absolute_cursor_row >= range_start {
                        let cursor_x = buffer.cursor_col as f32 * char_width;
                        let cursor_y = (absolute_cursor_row - range_start) as f32 * line_height;
                        let rect = egui::Rect::from_min_size(
                            content_top + egui::vec2(cursor_x, cursor_y),
                            egui::vec2(char_width, line_height),
                        );
                        ui.painter().rect_filled(
                            rect,
                            0.0,
                            Color32::from_rgba_unmultiplied(220, 220, 220, 160),
                        );
                    }
                })
        })
        .inner;

    let scroll_id = scroll_output.id;
    let response = ui.interact(scroll_output.inner_rect, scroll_id, egui::Sense::click());
    if response.clicked() {
        ui.memory_mut(|m| m.request_focus(scroll_id));
    }

    response
}

fn render_terminal_row_with_ligatures(
    ui: &mut egui::Ui,
    ligature_renderer: &mut crate::text::ligature::LigatureRenderer,
    line: &[Cell],
    font_id: egui::FontId,
    line_height: f32,
    char_width: f32,
    left: f32,
    row_y: f32,
) {
    let painter = ui.painter();
    let baseline_y = row_y + line_height * 0.75;

    // Draw cell backgrounds.
    for (col, cell) in line.iter().enumerate() {
        if cell.bg != Color32::TRANSPARENT {
            let rect = egui::Rect::from_min_size(
                egui::pos2(left + col as f32 * char_width, row_y),
                egui::vec2(char_width, line_height),
            );
            painter.rect_filled(rect, 0.0, cell.bg);
        }
    }

    // Group consecutive cells by foreground color and render each run with
    // ligature shaping.
    let mut run_start = 0;
    while run_start < line.len() {
        let run_color = line[run_start].fg;
        let mut run_end = run_start;
        while run_end < line.len() && line[run_end].fg == run_color {
            run_end += 1;
        }

        let run_text: String = line[run_start..run_end].iter().map(|c| c.ch).collect();
        if !run_text.trim().is_empty() {
            let run_x = left + run_start as f32 * char_width;
            ligature_renderer.render_run(
                ui.ctx(),
                painter,
                &run_text,
                font_id.size,
                line_height,
                run_color,
                egui::pos2(run_x, baseline_y),
                Some(char_width),
            );
        }

        run_start = run_end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ansi_color_gray() {
        // Bright black should map to gray (128, 128, 128)
        let color = ansi_color(90, true);
        assert_eq!(color, Color32::from_rgb(128, 128, 128));

        let normal_black = ansi_color(30, false);
        assert_eq!(normal_black, Color32::from_rgb(0, 0, 0));
    }

    #[test]
    fn test_utf8_csi_panic_safety() {
        let mut buffer = TerminalBuffer::new(24, 80);
        // Feed a CSI sequence with a multi-byte UTF-8 character as the final byte or parameter.
        // E.g., ESC [ 1 ; 3 🌈
        // While this is not a valid final character for standard CSI, it tests that slicing does not crash.
        buffer.feed(b"\x1b[1;3\xf0\x9f\x8c\x88m");
        // Ensure no panic, and it resets to normal state.
        assert_eq!(buffer.parser_state, ParserState::Normal);
    }

    #[test]
    fn test_osc_skipping() {
        let mut buffer = TerminalBuffer::new(2, 10);
        // Feed OSC title setting sequence
        buffer.feed(b"\x1b]0;Hello World\x07");
        assert_eq!(buffer.parser_state, ParserState::Normal);

        // Confirm the text "0;Hello World" was NOT written to the screen
        for row in &buffer.lines {
            for cell in row {
                assert_eq!(cell.ch, ' ');
            }
        }
    }

    #[test]
    fn test_gcharset_skipping() {
        let mut buffer = TerminalBuffer::new(2, 10);
        // Feed G0 charset transition sequence (ESC ( B)
        buffer.feed(b"\x1b(B");
        assert_eq!(buffer.parser_state, ParserState::Normal);

        // Confirm that the 'B' was NOT written to the screen
        for row in &buffer.lines {
            for cell in row {
                assert_eq!(cell.ch, ' ');
            }
        }
    }

    #[test]
    fn test_scrollback_limit() {
        let mut buffer = TerminalBuffer::new(2, 10);
        // Feed 10005 newlines to exceed the 10000 limit
        for _ in 0..10005 {
            buffer.feed(b"\n");
        }
        assert!(buffer.scroll_back.len() <= 10000);
    }
}
