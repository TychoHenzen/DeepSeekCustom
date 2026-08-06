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

