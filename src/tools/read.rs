use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;
use tracing::debug;

use crate::error::{HarnessError, Result};
use crate::tools::{Tool, ToolOutput};

/// File read tool with line numbers (cat -n format). Resolves a relative
/// path against `working_dir`, read fresh on every call so a change takes
/// effect on the next tool use. There is no path sandbox: `working_dir` may
/// point outside `project_root`, on purpose. See the phase 4 section of
/// `docs/plans/2026-08-04-long-term-roadmap.md`.
pub struct ReadTool {
    working_dir: Arc<Mutex<std::path::PathBuf>>,
}

impl ReadTool {
    pub fn new(working_dir: Arc<Mutex<std::path::PathBuf>>) -> Self {
        Self { working_dir }
    }
}

#[derive(Deserialize)]
struct ReadInput {
    file_path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read a file from the current working directory. Returns content with line numbers. Supports offset/limit for large files."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (1-based, optional)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum lines to read (optional)"
                }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let parsed: ReadInput = serde_json::from_value(input)
            .map_err(|e| HarnessError::Tool(format!("Invalid read input: {e}")))?;

        let path = self.resolve_path(&parsed.file_path);
        debug!(
            "read: path={}, offset={:?}, limit={:?}",
            path.display(),
            parsed.offset,
            parsed.limit
        );

        let contents = std::fs::read_to_string(&path)
            .map_err(|e| HarnessError::Tool(format!("Failed to read {}: {e}", path.display())))?;

        let lines: Vec<&str> = contents.lines().collect();
        let total_lines = lines.len();

        let start = parsed.offset.map(|o| o.saturating_sub(1)).unwrap_or(0);
        let end = match parsed.limit {
            Some(n) => (start + n).min(total_lines),
            None => total_lines,
        };

        let output = if start < total_lines {
            let selected = &lines[start..end];
            format_with_line_numbers(selected, start + 1)
        } else {
            String::new()
        };

        Ok(ToolOutput {
            content: if output.is_empty() {
                format!(
                    "(empty - {} total lines, requested offset={})",
                    total_lines,
                    start + 1
                )
            } else {
                format!("{output}\n[lines {}-{} of {total_lines}]", start + 1, end)
            },
            is_error: false,
            image: None,
        })
    }
}

impl ReadTool {
    /// Resolve a file path against the current working directory, read
    /// fresh from the shared flag. An absolute path is used as given.
    fn resolve_path(&self, file_path: &str) -> std::path::PathBuf {
        let path = std::path::Path::new(file_path);
        if path.is_absolute() {
            return path.to_path_buf();
        }
        let working_dir = self
            .working_dir
            .lock()
            .expect("working_dir mutex poisoned")
            .clone();
        working_dir.join(path)
    }
}

fn format_with_line_numbers(lines: &[&str], start_num: usize) -> String {
    let width = (start_num + lines.len()).to_string().len();
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let num = start_num + i;
            format!("{:>width$}\t{}", num, line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_arc(p: std::path::PathBuf) -> Arc<Mutex<std::path::PathBuf>> {
        Arc::new(Mutex::new(p))
    }

    /// Create a uniquely named directory under the system temp dir.
    fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("dsc-read-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn reads_known_file_correctly() {
        let root = std::env::current_dir().unwrap();
        let tool = ReadTool::new(dir_arc(root));

        // Read this test file itself
        let input = serde_json::json!({"file_path": "src/tools/read.rs", "limit": 5});
        let output = tool.execute(input).await.expect("execute");
        assert!(!output.is_error);
        assert!(output.content.contains("use async_trait::async_trait;"));
    }

    #[tokio::test]
    async fn reads_via_absolute_path() {
        let dir = unique_temp_dir("absolute");
        let file = dir.join("hello.txt");
        std::fs::write(&file, "hello world").unwrap();

        let tool = ReadTool::new(dir_arc(std::env::current_dir().unwrap()));
        let input = serde_json::json!({"file_path": file.to_string_lossy()});
        let output = tool.execute(input).await.expect("execute");
        assert!(!output.is_error);
        assert!(output.content.contains("hello world"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn changing_shared_working_dir_moves_where_relative_reads_resolve() {
        let dir_a = unique_temp_dir("a");
        let dir_b = unique_temp_dir("b");
        std::fs::write(dir_a.join("marker.txt"), "in_a").unwrap();
        std::fs::write(dir_b.join("marker.txt"), "in_b").unwrap();

        let shared = dir_arc(dir_a.clone());
        let tool = ReadTool::new(shared.clone());

        let first = tool
            .execute(serde_json::json!({"file_path": "marker.txt"}))
            .await
            .expect("execute");
        assert!(first.content.contains("in_a"), "got: {}", first.content);

        *shared.lock().unwrap() = dir_b.clone();

        let second = tool
            .execute(serde_json::json!({"file_path": "marker.txt"}))
            .await
            .expect("execute");
        assert!(second.content.contains("in_b"), "got: {}", second.content);

        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn format_line_numbers_correctly() {
        let lines = vec!["line one", "line two", "line three"];
        let result = format_with_line_numbers(&lines, 10);
        // Should have line numbers 10, 11, 12
        assert!(result.starts_with("10"));
        assert!(result.contains("11\tline two"));
        assert!(result.contains("12\tline three"));
    }
}
