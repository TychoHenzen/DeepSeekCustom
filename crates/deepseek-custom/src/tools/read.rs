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
        super::resolve_against(&self.working_dir, file_path)
    }
}

/// `pub`: its own test checks the exact numbering and width of a formatted
/// block of lines directly against a literal `&[&str]`, with no file on
/// disk. Driving that same check through `Tool::execute` would need a real
/// file just to reach this formatting step, adding filesystem I/O to a
/// check that is otherwise pure.
pub fn format_with_line_numbers(lines: &[&str], start_num: usize) -> String {
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
