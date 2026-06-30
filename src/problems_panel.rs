use std::collections::HashMap;
use std::path::{Path, PathBuf};

use egui::{Align, Color32, Layout, RichText, ScrollArea, Ui};

use crate::lsp::types::{DiagnosticSeverity, LspDiagnostic};
use crate::theme::SemanticPalette;

#[derive(Debug, Clone)]
pub struct DiagnosticRow {
    pub path: PathBuf,
    pub line: u32,
    pub col: u32,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub code: Option<String>,
}

pub struct DiagnosticCounts {
    pub total: usize,
    pub errors: usize,
    pub warnings: usize,
}

#[derive(Debug, Clone)]
pub enum PanelAction {
    Close,
    NavigateTo { row_index: usize },
    NavigateToDiagnostic { path: PathBuf, line: usize, col: usize },
}

pub fn flatten_diagnostics(
    diagnostics: &HashMap<PathBuf, Vec<LspDiagnostic>>,
) -> Vec<DiagnosticRow> {
    let mut rows = Vec::new();
    for (path, diags) in diagnostics {
        for diag in diags {
            rows.push(DiagnosticRow {
                path: path.clone(),
                line: diag.line_start,
                col: diag.col_start,
                severity: diag.severity,
                message: diag.message.clone(),
                code: diag.code.clone(),
            });
        }
    }
    rows.sort_by(|a, b| {
        a.path
            .to_string_lossy()
            .cmp(&b.path.to_string_lossy())
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.col.cmp(&b.col))
            .then_with(|| severity_order(a.severity).cmp(&severity_order(b.severity)))
            .then_with(|| a.message.cmp(&b.message))
    });
    rows
}

fn severity_order(severity: DiagnosticSeverity) -> u8 {
    match severity {
        DiagnosticSeverity::Error => 0,
        DiagnosticSeverity::Warning => 1,
        DiagnosticSeverity::Information => 2,
        DiagnosticSeverity::Hint => 3,
    }
}

pub fn count_diagnostics(rows: &[DiagnosticRow]) -> DiagnosticCounts {
    let mut errors = 0;
    let mut warnings = 0;
    for row in rows {
        match row.severity {
            DiagnosticSeverity::Error => errors += 1,
            DiagnosticSeverity::Warning => warnings += 1,
            _ => {}
        }
    }
    DiagnosticCounts {
        total: rows.len(),
        errors,
        warnings,
    }
}

fn severity_color(severity: DiagnosticSeverity, palette: SemanticPalette) -> Color32 {
    match severity {
        DiagnosticSeverity::Error => palette.error,
        DiagnosticSeverity::Warning => palette.warning,
        DiagnosticSeverity::Information => palette.information,
        DiagnosticSeverity::Hint => palette.muted_text,
    }
}

fn severity_icon(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Error => "✖",
        DiagnosticSeverity::Warning => "⚠",
        DiagnosticSeverity::Information => "ℹ",
        DiagnosticSeverity::Hint => "💡",
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

pub fn show(
    ui: &mut Ui,
    rows: &[DiagnosticRow],
    show_errors: &mut bool,
    show_warnings: &mut bool,
    palette: SemanticPalette,
) -> Option<PanelAction> {
    let mut action = None;
    let counts = count_diagnostics(rows);

    ui.horizontal(|ui| {
        ui.heading("Problems");
        ui.label(format!("{}", counts.total));
        show_filter_chips(ui, &counts, show_errors, show_warnings, palette);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("✕").clicked() {
                action = Some(PanelAction::Close);
            }
        });
    });

    ui.separator();

    if let Some(navigate) = show_body(ui, rows, show_errors, show_warnings, palette) {
        action = Some(navigate);
    }

    action
}

pub fn show_filter_chips(
    ui: &mut Ui,
    counts: &DiagnosticCounts,
    show_errors: &mut bool,
    show_warnings: &mut bool,
    palette: SemanticPalette,
) {
    if counts.errors > 0 {
        let label = RichText::new(format!("✖ {}", counts.errors))
            .color(severity_color(DiagnosticSeverity::Error, palette));
        if ui.selectable_label(*show_errors, label).clicked() {
            *show_errors = !*show_errors;
        }
    }
    if counts.warnings > 0 {
        let label = RichText::new(format!("⚠ {}", counts.warnings))
            .color(severity_color(DiagnosticSeverity::Warning, palette));
        if ui.selectable_label(*show_warnings, label).clicked() {
            *show_warnings = !*show_warnings;
        }
    }
}

pub fn show_body(
    ui: &mut Ui,
    rows: &[DiagnosticRow],
    show_errors: &bool,
    show_warnings: &bool,
    palette: SemanticPalette,
) -> Option<PanelAction> {
    if rows.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label("No problems detected");
        });
        return None;
    }

    let mut action = None;
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (index, row) in rows.iter().enumerate() {
                if row.severity == DiagnosticSeverity::Error && !*show_errors {
                    continue;
                }
                if row.severity == DiagnosticSeverity::Warning && !*show_warnings {
                    continue;
                }

                let _row_id = ui.make_persistent_id(("problem_row", index));
                let color = severity_color(row.severity, palette);
                let icon = severity_icon(row.severity);
                let filename = file_name(&row.path);
                let location = format!("{}:{}:{}", filename, row.line + 1, row.col + 1);
                let message = &row.message;
                let code_suffix = row
                    .code
                    .as_ref()
                    .map(|c| format!(" [{}]", c))
                    .unwrap_or_default();
                let full_text = format!("{} {} {}{}", icon, message, location, code_suffix);

                let response = ui
                    .horizontal(|ui| {
                        ui.colored_label(color, icon);
                        ui.label(message);
                        if let Some(code) = &row.code {
                            ui.weak(format!("[{}]", code));
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.weak(location);
                        });
                    })
                    .response
                    .interact(egui::Sense::click())
                    .on_hover_text(full_text);

                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, &row.message)
                });

                if response.clicked() {
                    action = Some(PanelAction::NavigateTo { row_index: index });
                }
            }
        });

    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_diagnostic(
        line: u32,
        col: u32,
        severity: DiagnosticSeverity,
        message: &str,
    ) -> LspDiagnostic {
        LspDiagnostic {
            line_start: line,
            col_start: col,
            line_end: line,
            col_end: col,
            severity,
            message: message.to_owned(),
            code: None,
        }
    }

    #[test]
    fn flatten_produces_sorted_rows() {
        let mut diagnostics = HashMap::new();
        diagnostics.insert(
            PathBuf::from("b.rs"),
            vec![make_diagnostic(5, 10, DiagnosticSeverity::Error, "error b")],
        );
        diagnostics.insert(
            PathBuf::from("a.rs"),
            vec![
                make_diagnostic(10, 0, DiagnosticSeverity::Warning, "warning a"),
                make_diagnostic(5, 0, DiagnosticSeverity::Error, "error a"),
            ],
        );

        let rows = flatten_diagnostics(&diagnostics);

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].path, PathBuf::from("a.rs"));
        assert_eq!(rows[0].line, 5);
        assert_eq!(rows[0].severity, DiagnosticSeverity::Error);
        assert_eq!(rows[1].path, PathBuf::from("a.rs"));
        assert_eq!(rows[1].line, 10);
        assert_eq!(rows[1].severity, DiagnosticSeverity::Warning);
        assert_eq!(rows[2].path, PathBuf::from("b.rs"));
        assert_eq!(rows[2].line, 5);
    }

    #[test]
    fn count_diagnostics_returns_correct_totals() {
        let rows = vec![
            DiagnosticRow {
                path: PathBuf::from("a.rs"),
                line: 0,
                col: 0,
                severity: DiagnosticSeverity::Error,
                message: "e1".to_owned(),
                code: None,
            },
            DiagnosticRow {
                path: PathBuf::from("a.rs"),
                line: 1,
                col: 0,
                severity: DiagnosticSeverity::Warning,
                message: "w1".to_owned(),
                code: None,
            },
            DiagnosticRow {
                path: PathBuf::from("a.rs"),
                line: 2,
                col: 0,
                severity: DiagnosticSeverity::Hint,
                message: "h1".to_owned(),
                code: None,
            },
        ];

        let counts = count_diagnostics(&rows);

        assert_eq!(counts.total, 3);
        assert_eq!(counts.errors, 1);
        assert_eq!(counts.warnings, 1);
    }

    #[test]
    fn empty_diagnostics_returns_empty_rows() {
        let diagnostics = HashMap::new();
        let rows = flatten_diagnostics(&diagnostics);
        assert!(rows.is_empty());
    }

    #[test]
    fn diagnostic_code_is_preserved() {
        let mut diagnostics = HashMap::new();
        let mut diag = make_diagnostic(0, 0, DiagnosticSeverity::Error, "unused");
        diag.code = Some("E0001".to_owned());
        diagnostics.insert(PathBuf::from("main.rs"), vec![diag]);

        let rows = flatten_diagnostics(&diagnostics);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].code, Some("E0001".to_owned()));
    }

    #[test]
    fn utf16_navigation_ascii() {
        use crate::editor::position::decode_utf16_column;
        let line = "hello world";
        assert_eq!(decode_utf16_column(line, 0), 0);
        assert_eq!(decode_utf16_column(line, 5), 5);
        assert_eq!(decode_utf16_column(line, 11), 11);
    }

    #[test]
    fn utf16_navigation_multibyte_bmp() {
        use crate::editor::position::decode_utf16_column;
        let line = "文字列";
        assert_eq!(decode_utf16_column(line, 0), 0);
        assert_eq!(decode_utf16_column(line, 1), 1);
        assert_eq!(decode_utf16_column(line, 2), 2);
        assert_eq!(decode_utf16_column(line, 3), 3);
    }

    #[test]
    fn utf16_navigation_end_of_line_and_clamped() {
        use crate::editor::position::decode_utf16_column;
        let ascii = "hello world";
        let ascii_eol = ascii.chars().count();
        assert_eq!(decode_utf16_column(ascii, ascii_eol as u32), ascii_eol);
        assert_eq!(decode_utf16_column(ascii, 99), ascii_eol);

        let emoji = "a😀b";
        assert_eq!(decode_utf16_column(emoji, 4), 3);
        assert_eq!(decode_utf16_column(emoji, 99), 3);
    }

    #[test]
    fn utf16_navigation_emoji_non_bmp() {
        use crate::editor::position::decode_utf16_column;
        let line = "a😀b";
        assert_eq!(decode_utf16_column(line, 0), 0);
        assert_eq!(decode_utf16_column(line, 1), 1);
        assert_eq!(decode_utf16_column(line, 2), 1);
        assert_eq!(decode_utf16_column(line, 3), 2);
        assert_eq!(decode_utf16_column(line, 4), 3);
    }
}

#[derive(Debug, Clone)]
pub struct ProblemsPanel {
    pub visible: bool,
    pub show_errors: bool,
    pub show_warnings: bool,
    pub show_info: bool,
    pub show_hints: bool,
    pub filter_text: String,
    pub collapsed_files: std::collections::HashSet<PathBuf>,
    pub selected: Option<(PathBuf, usize)>,
}

impl Default for ProblemsPanel {
    fn default() -> Self {
        Self {
            visible: false,
            show_errors: true,
            show_warnings: true,
            show_info: true,
            show_hints: true,
            filter_text: String::new(),
            collapsed_files: std::collections::HashSet::new(),
            selected: None,
        }
    }
}

pub fn count_all_diagnostics(diagnostics: &HashMap<PathBuf, Vec<LspDiagnostic>>) -> (usize, usize) {
    let mut errors = 0;
    let mut warnings = 0;
    for diags in diagnostics.values() {
        for d in diags {
            match d.severity {
                DiagnosticSeverity::Error => errors += 1,
                DiagnosticSeverity::Warning => warnings += 1,
                _ => {}
            }
        }
    }
    (errors, warnings)
}

pub fn show_problems_panel(
    ui: &mut Ui,
    diagnostics: &HashMap<PathBuf, Vec<LspDiagnostic>>,
    state: &mut ProblemsPanel,
    palette: SemanticPalette,
) -> Option<PanelAction> {
    let mut action = None;

    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            let (errors, warnings) = count_all_diagnostics(diagnostics);
            ui.label(RichText::new("PROBLEMS").strong());
            ui.colored_label(palette.error, format!("{} errors", errors));
            ui.colored_label(palette.warning, format!("{} warnings", warnings));
            
            ui.separator();
            
            ui.checkbox(&mut state.show_errors, "Errors");
            ui.checkbox(&mut state.show_warnings, "Warnings");
            ui.checkbox(&mut state.show_info, "Info");
            ui.checkbox(&mut state.show_hints, "Hints");

            ui.separator();
            ui.label("Filter:");
            ui.text_edit_singleline(&mut state.filter_text);
        });

        ui.separator();

        let mut grouped: Vec<(PathBuf, Vec<LspDiagnostic>)> = Vec::new();
        let filter_lower = state.filter_text.to_lowercase();

        for (path, diags) in diagnostics {
            let mut filtered = Vec::new();
            for d in diags {
                let severity_ok = match d.severity {
                    DiagnosticSeverity::Error => state.show_errors,
                    DiagnosticSeverity::Warning => state.show_warnings,
                    DiagnosticSeverity::Information => state.show_info,
                    DiagnosticSeverity::Hint => state.show_hints,
                };
                if !severity_ok {
                    continue;
                }

                if !filter_lower.is_empty() {
                    let filename = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
                    let message = d.message.to_lowercase();
                    if !filename.contains(&filter_lower) && !message.contains(&filter_lower) {
                        continue;
                    }
                }

                filtered.push(d.clone());
            }

            if !filtered.is_empty() {
                filtered.sort_by(|a, b| {
                    severity_order(a.severity).cmp(&severity_order(b.severity))
                        .then_with(|| a.line_start.cmp(&b.line_start))
                        .then_with(|| a.col_start.cmp(&b.col_start))
                });
                grouped.push((path.clone(), filtered));
            }
        }

        grouped.sort_by(|a, b| a.0.cmp(&b.0));

        if grouped.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label("No problems found");
            });
        } else {
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (path, diags) in grouped {
                        let filename = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
                        let error_count = diags.iter().filter(|d| d.severity == DiagnosticSeverity::Error).count();
                        let warning_count = diags.iter().filter(|d| d.severity == DiagnosticSeverity::Warning).count();
                        
                        let header_text = format!("{} ({} errors, {} warnings)", 
                            filename, error_count, warning_count
                        );

                        let collapsed = state.collapsed_files.contains(&path);
                        let arrow = if collapsed { "▶" } else { "▼" };

                        let resp = ui.horizontal(|ui| {
                            let r = ui.selectable_label(false, RichText::new(format!("{arrow} {header_text}")).strong());
                            if r.clicked() {
                                if collapsed {
                                    state.collapsed_files.remove(&path);
                                } else {
                                    state.collapsed_files.insert(path.clone());
                                }
                            }
                            r
                        }).inner;

                        if resp.double_clicked() {
                            state.selected = Some((path.clone(), 0));
                            action = Some(PanelAction::NavigateToDiagnostic {
                                path: path.clone(),
                                line: 0,
                                col: 0,
                            });
                        }

                        if !collapsed {
                            for d in diags {
                                let icon = severity_icon(d.severity);
                                let color = severity_color(d.severity, palette);
                                let loc_text = format!("{}:{}", d.line_start + 1, d.col_start + 1);
                                
                                ui.horizontal(|ui| {
                                    ui.add_space(16.0);
                                    ui.colored_label(color, icon);
                                    ui.label(RichText::new(&loc_text).weak());
                                    ui.add_space(4.0);
                                    
                                    let is_sel = state.selected.as_ref().map_or(false, |(sp, sl)| *sp == path && *sl == d.line_start as usize);
                                    let item_resp = ui.selectable_label(
                                        is_sel,
                                        &d.message
                                    );
                                    
                                    if let Some(code) = &d.code {
                                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                            ui.weak(code);
                                        });
                                    }

                                    if item_resp.clicked() {
                                        state.selected = Some((path.clone(), d.line_start as usize));
                                        action = Some(PanelAction::NavigateToDiagnostic {
                                            path: path.clone(),
                                            line: d.line_start as usize,
                                            col: d.col_start as usize,
                                        });
                                    }
                                });
                            }
                        }
                    }
                });
        }
    });

    action
}
