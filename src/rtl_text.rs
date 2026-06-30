//! Right-to-Left (RTL) Text Support (Feature 2)
//!
//! Provides utilities to check lines and comment tokens for Arabic and Hebrew
//! Unicode characters, and split lines to render LTR code and RTL comments with
//! their correct layout direction.

use egui::{text::LayoutJob, Color32, FontId, TextFormat};

/// Checks if a character belongs to Arabic or Hebrew script ranges.
pub fn is_rtl_char(ch: char) -> bool {
    ('\u{0600}'..='\u{06FF}').contains(&ch) || ('\u{0590}'..='\u{05FF}').contains(&ch)
}

/// Checks if a string contains any RTL characters (Arabic or Hebrew).
pub fn contains_rtl(text: &str) -> bool {
    text.chars().any(is_rtl_char)
}

/// Finds the starting byte position of a comment token (`//` or `/*`) in a line.
pub fn find_comment_start(line: &str) -> Option<usize> {
    if let Some(pos) = line.find("//") {
        return Some(pos);
    }
    if let Some(pos) = line.find("/*") {
        if !line[..pos].ends_with('*') {
            return Some(pos);
        }
    }
    None
}

/// Splits a line into code part and comment part at the comment token, if present.
pub fn split_at_comment(line: &str) -> Option<(&str, &str)> {
    find_comment_start(line).map(|pos| line.split_at(pos))
}

/// Creates a LayoutJob for the LTR code part of a mixed line.
pub fn create_layout_job_for_code(
    code: &str,
    font_id: FontId,
    text_color: Color32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.append(
        code,
        0.0,
        TextFormat {
            font_id,
            color: text_color,
            ..Default::default()
        },
    );
    job
}

/// Creates a LayoutJob for the RTL comment part of a line.
pub fn create_layout_job_for_comment(
    comment: &str,
    font_id: FontId,
    comment_color: Color32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.append(
        comment,
        0.0,
        TextFormat {
            font_id,
            color: comment_color,
            ..Default::default()
        },
    );
    job
}

/// Creates a LayoutJob for a line with LTR code and RTL comment (if comment has RTL characters).
pub fn create_layout_job_for_line(
    line: &str,
    font_id: FontId,
    text_color: Color32,
    weak_text_color: Color32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    
    if let Some((code_part, comment_part)) = split_at_comment(line) {
        if contains_rtl(comment_part) {
            // If it's a comment-only line, lay out the whole line in RTL
            if code_part.trim().is_empty() {
                job.append(
                    line,
                    0.0,
                    TextFormat {
                        font_id,
                        color: weak_text_color,
                        ..Default::default()
                    },
                );
                return job;
            }
            
            // Otherwise, code part is LTR
            job.append(
                code_part,
                0.0,
                TextFormat {
                    font_id: font_id.clone(),
                    color: text_color,
                    ..Default::default()
                },
            );
            
            job.append(
                comment_part,
                0.0,
                TextFormat {
                    font_id,
                    color: weak_text_color,
                    ..Default::default()
                },
            );
            return job;
        }
    }
    
    // Default LTR fallback
    job.append(
        line,
        0.0,
        TextFormat {
            font_id,
            color: text_color,
            ..Default::default()
        },
    );
    job
}
