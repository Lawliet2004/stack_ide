//! Problem matcher: parse rustc output into diagnostics.
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchSeverity { Error, Warning, Info }

#[derive(Debug, Clone)]
pub struct ProblemMatch {
    pub file: PathBuf,
    pub line: usize,
    pub column: usize,
    pub severity: MatchSeverity,
    pub code: Option<String>,
    pub message: String,
    pub source: String,
}

pub fn match_rustc(lines: &[&str], workspace_root: &PathBuf) -> Vec<ProblemMatch> {
    use regex::Regex;
    use std::sync::OnceLock;
    static SVRE: OnceLock<Regex> = OnceLock::new();
    let sre = SVRE.get_or_init(|| Regex::new(r"^(error|warning|note|help)(\[([A-Z]\d+)\])?: (.+)$").unwrap());
    static ARRRE: OnceLock<Regex> = OnceLock::new();
    let are = ARRRE.get_or_init(|| Regex::new(r"^\s+--> (.+):(\d+):(\d+)$").unwrap());
    static SHTRE: OnceLock<Regex> = OnceLock::new();
    let shre = SHTRE.get_or_init(|| Regex::new(r"^(.+):(\d+):(\d+): (error|warning|note)(\[([A-Z]\d+)\])?: (.+)$").unwrap());

    let mut out = Vec::new();
    let mut pending: Option<(MatchSeverity, Option<String>, String)> = None;
    for line in lines {
        if let Some(c) = shre.captures(line) {
            out.push(ProblemMatch { file: workspace_root.join(&c[1]),
                line: c[2].parse().unwrap_or(1), column: c[3].parse().unwrap_or(1),
                severity: ps(&c[4]), code: c.get(6).map(|m| m.as_str().to_string()),
                message: c[7].to_string(), source: "rustc".to_string() });
            pending = None; continue;
        }
        if let (Some((sev, code, msg)), Some(c)) = (pending.take(), are.captures(line)) {
            out.push(ProblemMatch { file: workspace_root.join(&c[1]),
                line: c[2].parse().unwrap_or(1), column: c[3].parse().unwrap_or(1),
                severity: sev, code, message: msg, source: "rustc".to_string() });
            continue;
        }
        if let Some(c) = sre.captures(line) {
            let code = c.get(3).map(|m| m.as_str().to_string());
            pending = Some((ps(&c[1]), code, c[4].to_string()));
        } else if !line.trim().starts_with("-->") { pending = None; }
    }
    out
}

fn ps(s: &str) -> MatchSeverity {
    match s { "error" => MatchSeverity::Error, "warning" => MatchSeverity::Warning, _ => MatchSeverity::Info }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_long_form_error() {
        let lines = vec!["error[E0425]: cannot find `x`", "  --> src/main.rs:5:13"];
        let d = match_rustc(&lines, &PathBuf::from("."));
        assert_eq!(d.len(), 1); assert_eq!(d[0].line, 5);
    }
    #[test]
    fn parses_warning() {
        let lines = vec!["warning: unused var", "  --> src/main.rs:10:9"];
        let d = match_rustc(&lines, &PathBuf::from("."));
        assert_eq!(d.len(), 1); assert_eq!(d[0].severity, MatchSeverity::Warning);
    }
}
