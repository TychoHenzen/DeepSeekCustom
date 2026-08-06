use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;
use tracing::{debug, info};

use crate::error::{HarnessError, Result};
use crate::tools::{Tool, ToolOutput};

/// File write tool. Resolves a relative path against `working_dir`, read
/// fresh on every call so a change takes effect on the next tool use. There
/// is no path sandbox: `working_dir` may point outside `project_root`, on
/// purpose. See the phase 4 section of
/// `docs/plans/2026-08-04-long-term-roadmap.md`.
pub struct WriteTool {
    working_dir: Arc<Mutex<std::path::PathBuf>>,
}

impl WriteTool {
    pub fn new(working_dir: Arc<Mutex<std::path::PathBuf>>) -> Self {
        Self { working_dir }
    }
}

#[derive(Deserialize)]
struct WriteInput {
    file_path: String,
    content: String,
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Write content to a file in the current working directory. Creates parent directories if needed."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["file_path", "content"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let parsed: WriteInput = serde_json::from_value(input)
            .map_err(|e| HarnessError::Tool(format!("Invalid write input: {e}")))?;

        let path = self.resolve_path(&parsed.file_path);
        debug!(
            "write: path={}, bytes={}",
            path.display(),
            parsed.content.len()
        );

        // Create parent directories
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| HarnessError::Tool(format!("Failed to create parent dirs: {e}")))?;
        }

        std::fs::write(&path, &parsed.content)
            .map_err(|e| HarnessError::Tool(format!("Failed to write {}: {e}", path.display())))?;

        info!(
            "write: wrote {} bytes to {}",
            parsed.content.len(),
            path.display()
        );
        Ok(ToolOutput {
            content: format!("Wrote {} bytes to {}", parsed.content.len(), path.display()),
            is_error: false,
            image: None,
        })
    }
}

impl WriteTool {
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

