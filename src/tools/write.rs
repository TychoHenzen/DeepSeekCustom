use async_trait::async_trait;
use serde::Deserialize;
use tracing::{debug, info};

use crate::error::{HarnessError, Result};
use crate::tools::{Tool, ToolOutput};

/// File write tool with path safety.
pub struct WriteTool {
    project_root: std::path::PathBuf,
}

impl WriteTool {
    pub fn new(project_root: std::path::PathBuf) -> Self {
        Self { project_root }
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
        "Write content to a file within the project. Creates parent directories if needed."
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

        let path = self.resolve_path(&parsed.file_path)?;
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
        })
    }
}

impl WriteTool {
    /// Resolve a file path relative to project root. Rejects `..` escape attempts.
    fn resolve_path(&self, file_path: &str) -> Result<std::path::PathBuf> {
        let path = std::path::Path::new(file_path);

        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.project_root.join(path)
        };

        let canonical_root = self
            .project_root
            .canonicalize()
            .unwrap_or_else(|_| self.project_root.clone());

        // For new files (don't exist yet), canonicalize the parent
        let canonical = if resolved.exists() {
            resolved.canonicalize()
        } else {
            resolved
                .parent()
                .map(|p| p.canonicalize())
                .unwrap_or_else(|| {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "no parent",
                    ))
                })
        }
        .map_err(|e| HarnessError::Tool(format!("Invalid path '{}': {e}", file_path)))?;

        if !canonical.starts_with(&canonical_root) {
            return Err(HarnessError::Tool(format!(
                "Path traversal detected: '{}' is outside project root",
                file_path
            )));
        }

        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writes_and_verifies_file() {
        let root = std::env::current_dir().unwrap();
        let tool = WriteTool::new(root.clone());

        let test_path = "target/test_write_output.txt";
        let content = "hello from write tool";
        let input = serde_json::json!({"file_path": test_path, "content": content});
        let output = tool.execute(input).await.expect("execute");
        assert!(!output.is_error);
        assert!(output.content.contains("Wrote"));

        // Verify file was written
        let written = std::fs::read_to_string(root.join(test_path)).unwrap();
        assert_eq!(written, content);

        // Cleanup
        let _ = std::fs::remove_file(root.join(test_path));
    }

    #[tokio::test]
    async fn rejects_paths_outside_project_root() {
        let root = std::env::current_dir().unwrap();
        let tool = WriteTool::new(root);

        let input =
            serde_json::json!({"file_path": "../../Windows/System32/hack.exe", "content": "bad"});
        let result = tool.execute(input).await;
        assert!(result.is_err());
    }
}
