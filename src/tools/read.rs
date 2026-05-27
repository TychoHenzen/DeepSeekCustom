use async_trait::async_trait;
use serde::Deserialize;
use tracing::debug;

use crate::error::{HarnessError, Result};
use crate::tools::{Tool, ToolOutput};

/// File read tool with line numbers (cat -n format).
pub struct ReadTool {
    project_root: std::path::PathBuf,
}

impl ReadTool {
    pub fn new(project_root: std::path::PathBuf) -> Self {
        Self { project_root }
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
        "Read a file from the project. Returns content with line numbers. Supports offset/limit for large files."
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

        let path = self.resolve_path(&parsed.file_path)?;
        debug!("read: path={}, offset={:?}, limit={:?}", path.display(), parsed.offset, parsed.limit);

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
                format!("(empty — {} total lines, requested offset={})", total_lines, start + 1)
            } else {
                format!("{output}\n[lines {}-{} of {total_lines}]", start + 1, end)
            },
            is_error: false,
        })
    }
}

impl ReadTool {
    /// Resolve a file path relative to project root. Rejects `..` escape attempts.
    fn resolve_path(&self, file_path: &str) -> Result<std::path::PathBuf> {
        let path = std::path::Path::new(file_path);

        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.project_root.join(path)
        };

        // Canonicalize both resolved path and project root for comparison
        let canonical = resolved.canonicalize().map_err(|e| {
            HarnessError::Tool(format!("Invalid path '{}': {e}", file_path))
        })?;

        let canonical_root = self.project_root.canonicalize().unwrap_or_else(|_| self.project_root.clone());

        // Check for path traversal
        if !canonical.starts_with(&canonical_root) {
            return Err(HarnessError::Tool(format!(
                "Path traversal detected: '{}' is outside project root",
                file_path
            )));
        }

        Ok(canonical)
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

    #[tokio::test]
    async fn reads_known_file_correctly() {
        let root = std::env::current_dir().unwrap();
        let tool = ReadTool::new(root.clone());

        // Read this test file itself
        let input = serde_json::json!({"file_path": "src/tools/read.rs", "limit": 5});
        let output = tool.execute(input).await.expect("execute");
        assert!(!output.is_error);
        assert!(output.content.contains("use async_trait::async_trait;"));
    }

    #[tokio::test]
    async fn rejects_paths_outside_project_root() {
        let root = std::env::current_dir().unwrap();
        let tool = ReadTool::new(root);

        // Use a path with .. that resolves outside (Windows system dir)
        let input = serde_json::json!({"file_path": "../../Windows/System32/notepad.exe"});
        let result = tool.execute(input).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            HarnessError::Tool(ref msg) => {
                assert!(
                    msg.contains("outside project root") || msg.contains("Invalid path"),
                    "expected traversal or invalid path error, got: {msg}"
                );
            }
            _ => panic!("expected Tool error, got {err:?}"),
        }
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
