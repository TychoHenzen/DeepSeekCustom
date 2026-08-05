use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;
use tracing::info;

use crate::error::Result;
use crate::tools::{Tool, ToolOutput};

/// Changes the shared working directory. `working_dir` is the same
/// `Arc<Mutex<PathBuf>>` `BashTool`, `ReadTool`, and `WriteTool` read fresh
/// on every call, so writing it here moves all three at once. It also
/// changes what the next turn's system prompt reports, and where a
/// `ClaudeCli` backend's child process spawns. See the phase 4 section of
/// `docs/plans/2026-08-04-long-term-roadmap.md`.
///
/// This never calls `std::env::set_current_dir`. The directory is a value
/// this harness threads through its own tools, not the OS process's actual
/// current directory.
///
/// There is no path sandbox: the target may point outside `project_root`,
/// on purpose, the same as `BashTool`, `ReadTool`, and `WriteTool`.
pub struct CdTool {
    working_dir: Arc<Mutex<std::path::PathBuf>>,
}

impl CdTool {
    pub fn new(working_dir: Arc<Mutex<std::path::PathBuf>>) -> Self {
        Self { working_dir }
    }
}

#[derive(Deserialize)]
struct CdInput {
    path: String,
}

#[async_trait]
impl Tool for CdTool {
    fn name(&self) -> &str {
        "cd"
    }

    fn description(&self) -> &str {
        "Change the current working directory for the rest of this session. \
         Affects every later bash, read, and write call, and is reported in \
         the system prompt on the next turn. A relative path resolves \
         against the current working directory, so two cd calls in a row \
         compose. The target must already exist and be a directory."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to change into"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let parsed: CdInput = match serde_json::from_value(input) {
            Ok(parsed) => parsed,
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!("Invalid cd input: {e}"),
                    is_error: true,
                    image: None,
                });
            }
        };

        let resolved = self.resolve_path(&parsed.path);

        if !resolved.exists() {
            return Ok(ToolOutput {
                content: format!("cd: no such path: {}", resolved.display()),
                is_error: true,
                image: None,
            });
        }

        if !resolved.is_dir() {
            return Ok(ToolOutput {
                content: format!("cd: not a directory: {}", resolved.display()),
                is_error: true,
                image: None,
            });
        }

        let canonical = match std::fs::canonicalize(&resolved) {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!("cd: cannot read {}: {e}", resolved.display()),
                    is_error: true,
                    image: None,
                });
            }
        };

        {
            let mut working_dir = self.working_dir.lock().expect("working_dir mutex poisoned");
            *working_dir = canonical.clone();
        }

        info!("cd: working_dir now {}", canonical.display());

        Ok(ToolOutput {
            content: format!("working directory is now {}", canonical.display()),
            is_error: false,
            image: None,
        })
    }
}

impl CdTool {
    /// Resolve a target path against the current working directory, read
    /// fresh from the shared value. An absolute path is used as given.
    fn resolve_path(&self, path: &str) -> std::path::PathBuf {
        let target = std::path::Path::new(path);
        if target.is_absolute() {
            return target.to_path_buf();
        }
        let working_dir = self
            .working_dir
            .lock()
            .expect("working_dir mutex poisoned")
            .clone();
        working_dir.join(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::bash::BashTool;

    fn dir_arc(p: std::path::PathBuf) -> Arc<Mutex<std::path::PathBuf>> {
        Arc::new(Mutex::new(p))
    }

    /// Create a uniquely named directory under the system temp dir.
    fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("dsc-cd-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn successful_change_moves_the_shared_value() {
        let start = unique_temp_dir("start");
        let target = unique_temp_dir("target");
        let shared = dir_arc(start.clone());
        let tool = CdTool::new(shared.clone());

        let output = tool
            .execute(serde_json::json!({"path": target.to_string_lossy()}))
            .await
            .expect("execute");
        assert!(!output.is_error, "unexpected error: {}", output.content);

        let expected = std::fs::canonicalize(&target).unwrap();
        assert_eq!(*shared.lock().unwrap(), expected);

        let _ = std::fs::remove_dir_all(&start);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[tokio::test]
    async fn following_bash_call_runs_in_the_new_directory() {
        let start = unique_temp_dir("bash-start");
        let target = unique_temp_dir("bash-target");
        let shared = dir_arc(start.clone());
        let cd = CdTool::new(shared.clone());
        let bash = BashTool::new(shared.clone());

        let cd_output = cd
            .execute(serde_json::json!({"path": target.to_string_lossy()}))
            .await
            .expect("execute");
        assert!(!cd_output.is_error);

        let bash_output = bash
            .execute(serde_json::json!({"command": "cd"}))
            .await
            .expect("execute");
        assert!(!bash_output.is_error);
        let canonical_target = std::fs::canonicalize(&target).unwrap();
        // cmd's own `cd` builtin prints the current directory; strip any
        // Windows extended-length prefix quirk by just checking containment
        // of the target's file name, which is unique to this test run.
        let target_name = canonical_target.file_name().unwrap().to_string_lossy();
        assert!(
            bash_output.content.contains(target_name.as_ref()),
            "expected bash cwd to contain {}, got: {}",
            target_name,
            bash_output.content
        );

        let _ = std::fs::remove_dir_all(&start);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[tokio::test]
    async fn two_relative_changes_compose() {
        let root = unique_temp_dir("compose-root");
        let inner_a = root.join("a");
        let inner_b = inner_a.join("b");
        std::fs::create_dir_all(&inner_b).unwrap();

        let shared = dir_arc(root.clone());
        let tool = CdTool::new(shared.clone());

        let first = tool
            .execute(serde_json::json!({"path": "a"}))
            .await
            .expect("execute");
        assert!(!first.is_error, "unexpected error: {}", first.content);

        let second = tool
            .execute(serde_json::json!({"path": "b"}))
            .await
            .expect("execute");
        assert!(!second.is_error, "unexpected error: {}", second.content);

        let expected = std::fs::canonicalize(&inner_b).unwrap();
        assert_eq!(*shared.lock().unwrap(), expected);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn missing_path_is_a_tool_error_naming_the_path() {
        let start = unique_temp_dir("missing-start");
        let shared = dir_arc(start.clone());
        let tool = CdTool::new(shared.clone());

        let missing = start.join("does-not-exist");
        let output = tool
            .execute(serde_json::json!({"path": missing.to_string_lossy()}))
            .await
            .expect("execute");
        assert!(output.is_error);
        assert!(output.content.contains("no such path"));
        assert_eq!(*shared.lock().unwrap(), start);

        let _ = std::fs::remove_dir_all(&start);
    }

    #[tokio::test]
    async fn path_that_is_a_file_is_a_tool_error() {
        let start = unique_temp_dir("file-start");
        let file = start.join("not_a_dir.txt");
        std::fs::write(&file, "hello").unwrap();
        let shared = dir_arc(start.clone());
        let tool = CdTool::new(shared.clone());

        let output = tool
            .execute(serde_json::json!({"path": file.to_string_lossy()}))
            .await
            .expect("execute");
        assert!(output.is_error);
        assert!(output.content.contains("not a directory"));
        assert_eq!(*shared.lock().unwrap(), start);

        let _ = std::fs::remove_dir_all(&start);
    }

    #[tokio::test]
    async fn invalid_input_is_a_tool_error_not_a_hard_err() {
        let shared = dir_arc(std::env::current_dir().unwrap());
        let tool = CdTool::new(shared);

        let output = tool
            .execute(serde_json::json!({"not_path": "whatever"}))
            .await
            .expect("execute");
        assert!(output.is_error);
    }
}
