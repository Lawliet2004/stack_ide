//! Task configuration: tasks.toml format.
use std::collections::HashMap;
use std::path::Path;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_cwd")]
    pub cwd: String,
    #[serde(default)]
    pub problem_matcher: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}
fn default_cwd() -> String { ".".into() }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TasksFile {
    #[serde(default)]
    pub tasks: HashMap<String, TaskConfig>,
}
impl TasksFile {
    pub fn load(workspace_root: &Path) -> Result<Self, String> {
        let path = workspace_root.join("tasks.toml");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("tasks.toml parse error: {e}"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trips_tasks_toml() {
        use std::time::{SystemTime,UNIX_EPOCH};
        let u = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("blue_cfg_{u}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tasks.toml"),
            "[tasks.build]\ncommand=\"cargo\"\nargs=[\"build\"]\nproblem_matcher=\"rustc\"\n"
        ).unwrap();
        let f = TasksFile::load(&dir).unwrap();
        assert!(f.tasks.contains_key("build"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
