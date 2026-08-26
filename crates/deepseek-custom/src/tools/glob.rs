//! The `glob` tool: find files by path pattern, newest first.
//!
//! Claude Code offers this and the harness did not, so a real autopilot run
//! reached for `powershell Get-ChildItem -Filter` through `bash` instead.
//! That works and it is slow, platform-locked, and it puts shell quoting
//! between the model and a list of paths.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use async_trait::async_trait;
use serde::Deserialize;
use tracing::{debug, info};

use crate::error::{HarnessError, Result};
use crate::tools::{Tool, ToolOutput};

/// The most paths one call reports. A wide pattern over this tree matches
/// thousands of files, and a tool result that large costs more context than
/// the answer is worth.
const MAX_RESULTS: usize = 200;

/// File search by glob pattern, rooted at `working_dir` unless the call
/// names its own `path`. Reads the shared working directory fresh on every
/// call, the same way `read`, `write`, and `edit` do.
pub struct GlobTool {
    working_dir: Arc<Mutex<PathBuf>>,
}

impl GlobTool {
    pub fn new(working_dir: Arc<Mutex<PathBuf>>) -> Self {
        Self { working_dir }
    }

    /// The directory this call searches under. An absolute `path` is used
    /// as given. A relative one joins onto the working directory. No path
    /// at all means the working directory itself.
    fn search_root(&self, path: Option<&str>) -> PathBuf {
        let working_dir = self
            .working_dir
            .lock()
            .expect("working_dir mutex poisoned")
            .clone();
        match path {
            Some(path) => {
                let path = Path::new(path);
                if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    working_dir.join(path)
                }
            }
            None => working_dir,
        }
    }
}

#[derive(Deserialize)]
struct GlobInput {
    pattern: String,
    path: Option<String>,
}

/// Sort key for one hit: its modification time, newest first. A time that
/// cannot be read counts as the epoch, so that file sorts last instead of
/// failing the whole search.
fn modified_at(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Expand `pattern` under `root` and return matching files, newest first,
/// capped at `MAX_RESULTS`. A directory that matches is skipped: the model
/// asked for files.
fn matching_files(root: &Path, pattern: &str) -> std::result::Result<Vec<PathBuf>, String> {
    let joined = root.join(pattern);
    let joined = joined.to_string_lossy().replace('\\', "/");
    let paths = glob::glob(&joined).map_err(|e| format!("Invalid glob pattern: {e}"))?;

    let mut hits: Vec<PathBuf> = paths.flatten().filter(|path| path.is_file()).collect();
    hits.sort_by_key(|path| std::cmp::Reverse(modified_at(path)));
    hits.truncate(MAX_RESULTS);
    Ok(hits)
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files by glob pattern, such as **/*.rs or src/**/mod.rs. Returns matching file \
         paths sorted by modification time, newest first. Searches the current working \
         directory unless a path is given."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match against, for example **/*.rs"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in. Defaults to the current working directory."
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let parsed: GlobInput = serde_json::from_value(input)
            .map_err(|e| HarnessError::Tool(format!("Invalid glob input: {e}")))?;

        let root = self.search_root(parsed.path.as_deref());
        debug!("glob: pattern={} root={}", parsed.pattern, root.display());

        let hits = match matching_files(&root, &parsed.pattern) {
            Ok(hits) => hits,
            Err(reason) => return Ok(ToolOutput::error(reason)),
        };

        info!("glob: {} match(es) for {}", hits.len(), parsed.pattern);
        if hits.is_empty() {
            return Ok(ToolOutput::ok(format!(
                "No files match {} under {}",
                parsed.pattern,
                root.display()
            )));
        }

        let listing: Vec<String> = hits.iter().map(|path| path.display().to_string()).collect();
        let capped = if listing.len() == MAX_RESULTS {
            format!("\n(capped at {MAX_RESULTS} results)")
        } else {
            String::new()
        };
        Ok(ToolOutput::ok(format!("{}{capped}", listing.join("\n"))))
    }
}
