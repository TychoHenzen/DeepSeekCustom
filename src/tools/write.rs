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
        let dir = std::env::temp_dir().join(format!("dsc-write-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn writes_and_verifies_file() {
        let root = std::env::current_dir().unwrap();
        let tool = WriteTool::new(dir_arc(root.clone()));

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
    async fn writes_via_absolute_path() {
        let dir = unique_temp_dir("absolute");
        let file = dir.join("hello.txt");

        let tool = WriteTool::new(dir_arc(std::env::current_dir().unwrap()));
        let input =
            serde_json::json!({"file_path": file.to_string_lossy(), "content": "hello world"});
        let output = tool.execute(input).await.expect("execute");
        assert!(!output.is_error);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn changing_shared_working_dir_moves_where_relative_writes_land() {
        let dir_a = unique_temp_dir("a");
        let dir_b = unique_temp_dir("b");

        let shared = dir_arc(dir_a.clone());
        let tool = WriteTool::new(shared.clone());

        let first = tool
            .execute(serde_json::json!({"file_path": "out.txt", "content": "in_a"}))
            .await
            .expect("execute");
        assert!(!first.is_error);
        assert_eq!(
            std::fs::read_to_string(dir_a.join("out.txt")).unwrap(),
            "in_a"
        );
        assert!(!dir_b.join("out.txt").exists());

        *shared.lock().unwrap() = dir_b.clone();

        let second = tool
            .execute(serde_json::json!({"file_path": "out.txt", "content": "in_b"}))
            .await
            .expect("execute");
        assert!(!second.is_error);
        assert_eq!(
            std::fs::read_to_string(dir_b.join("out.txt")).unwrap(),
            "in_b"
        );

        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }
}
