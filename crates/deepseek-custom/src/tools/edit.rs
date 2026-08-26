//! The `edit` tool: exact string replacement inside one file.
//!
//! Without this tool the model has `read` and `write` and nothing between
//! them, so changing three lines of a 400-line file means echoing the whole
//! file back through `write`. A real autopilot run instead worked around the
//! gap by writing PowerShell scripts that did the replacement and running
//! them through `bash`, which left five `fix_*.ps1` files in the project
//! root and one broken script that read the same file twice.
//!
//! The contract matches Claude Code's own `Edit` tool, so a skill or a
//! prompt written for that harness means the same thing here: `old_string`
//! must appear exactly once unless `replace_all` is set, and a match that
//! is missing or ambiguous is a tool error rather than a guess.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;
use tracing::{debug, info};

use crate::error::{HarnessError, Result};
use crate::tools::line_endings::{has_crlf, to_crlf, to_lf};
use crate::tools::{Tool, ToolOutput};

/// Exact string replacement in a file. Resolves a relative path against
/// `working_dir`, read fresh on every call, the same way `read` and
/// `write` do. There is no path sandbox, on purpose. See the working
/// directory section of `CLAUDE.md`.
pub struct EditTool {
    working_dir: Arc<Mutex<PathBuf>>,
}

impl EditTool {
    pub fn new(working_dir: Arc<Mutex<PathBuf>>) -> Self {
        Self { working_dir }
    }

    /// Resolve a file path against the current working directory, read
    /// fresh from the shared handle. An absolute path is used as given.
    fn resolve_path(&self, file_path: &str) -> PathBuf {
        super::resolve_against(&self.working_dir, file_path)
    }
}

#[derive(Deserialize)]
struct EditInput {
    file_path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

/// What one edit did, before it is turned into tool output text.
struct EditOutcome {
    replacements: usize,
    content: String,
}

/// Apply the replacement to `source`, or say why it cannot be applied.
///
/// Pure over its inputs, so the whole decision table (missing match,
/// ambiguous match, no-op edit, single, all) is testable without a file.
///
/// Matching happens on LF text, and the result carries back whichever line
/// endings `source` already used. The model never sees a carriage return,
/// because `read` strips it, so a byte-exact match against a CRLF file
/// would reject every needle spanning more than one line. See
/// `tools/line_endings.rs`.
pub fn apply_edit(
    source: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> std::result::Result<(String, usize), String> {
    let crlf = has_crlf(source);
    let (source, old_string, new_string) = (to_lf(source), to_lf(old_string), to_lf(new_string));
    let (edited, replacements) = replace_in_lf(&source, &old_string, &new_string, replace_all)?;
    let edited = if crlf { to_crlf(&edited) } else { edited };
    Ok((edited, replacements))
}

/// The replacement itself, over text whose newlines are already bare LF.
fn replace_in_lf(
    source: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> std::result::Result<(String, usize), String> {
    if old_string.is_empty() {
        return Err("old_string is empty. Use the write tool to create a file.".to_string());
    }
    if old_string == new_string {
        return Err(
            "old_string and new_string are identical, so this edit changes nothing.".to_string(),
        );
    }

    let matches = source.matches(old_string).count();
    if matches == 0 {
        return Err(
            "old_string was not found in the file. Read the file and copy the text exactly, \
             including indentation."
                .to_string(),
        );
    }
    if matches > 1 && !replace_all {
        return Err(format!(
            "old_string appears {matches} times. Add surrounding lines to make it unique, \
             or set replace_all to true."
        ));
    }

    if replace_all {
        return Ok((source.replace(old_string, new_string), matches));
    }
    Ok((source.replacen(old_string, new_string, 1), 1))
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace an exact string in a file. old_string must match the file byte for byte, \
         including indentation, and must appear exactly once unless replace_all is true. \
         Prefer this over rewriting a whole file with the write tool."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file to edit"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact text to replace, copied from the file"
                },
                "new_string": {
                    "type": "string",
                    "description": "The text to put in its place. Must differ from old_string."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace every occurrence instead of requiring exactly one. Defaults to false."
                }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let parsed: EditInput = serde_json::from_value(input)
            .map_err(|e| HarnessError::Tool(format!("Invalid edit input: {e}")))?;

        let path = self.resolve_path(&parsed.file_path);
        debug!("edit: path={}", path.display());

        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(e) => {
                return Ok(ToolOutput::error(format!(
                    "Failed to read {}: {e}",
                    path.display()
                )));
            }
        };

        let outcome = match apply_edit(
            &source,
            &parsed.old_string,
            &parsed.new_string,
            parsed.replace_all,
        ) {
            Ok((content, replacements)) => EditOutcome {
                replacements,
                content,
            },
            Err(reason) => return Ok(ToolOutput::error(format!("{}: {reason}", path.display()))),
        };

        if let Err(e) = std::fs::write(&path, &outcome.content) {
            return Ok(ToolOutput::error(format!(
                "Failed to write {}: {e}",
                path.display()
            )));
        }

        info!(
            "edit: {} replacement(s) in {}",
            outcome.replacements,
            path.display()
        );
        Ok(ToolOutput {
            content: format!(
                "Made {} replacement(s) in {}",
                outcome.replacements,
                path.display()
            ),
            is_error: false,
            image: None,
        })
    }
}
