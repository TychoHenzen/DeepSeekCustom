use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;
use tracing::debug;

use crate::error::{HarnessError, Result};
use crate::image_bytes::attachment_from_image_bytes;
use crate::tools::{Tool, ToolOutput};

/// Reads an image file off disk and hands it back as an `ImageAttachment`
/// on `ToolOutput::image`, so the model can look at a screenshot or a
/// diagram itself instead of relying on a human to paste one in. See
/// `ToolOutput::image`'s doc comment in `src/tools/mod.rs` for how that
/// field actually reaches the model on the turn after this tool runs.
///
/// A separate tool from `ReadTool`, not an image-aware branch on it.
/// `ReadTool` reads text with `offset`/`limit` and hands back a string; an
/// image needs a binary read, a decode-and-validate step, and a different
/// output shape entirely (`ToolOutput::image`, not more of `content`).
/// Overloading one schema with both shapes would make every call ambiguous
/// about which behavior it gets. The model instead picks the right tool by
/// name, the same way it already picks `bash` over `read` over `write`.
pub struct ReadImageTool {
    root: super::FileToolRoot,
}

impl ReadImageTool {
    pub fn new(working_dir: Arc<Mutex<PathBuf>>) -> Self {
        Self {
            root: super::FileToolRoot::working_directory(working_dir),
        }
    }

    pub(crate) fn rooted(root: PathBuf) -> std::result::Result<Self, String> {
        Ok(Self {
            root: super::FileToolRoot::fixed(root)?,
        })
    }

    /// Resolve a file path against the current working directory, read
    /// fresh from the shared flag. An absolute path is used as given.
    fn resolve_path(&self, file_path: &str) -> std::result::Result<PathBuf, String> {
        self.root.resolve(file_path)
    }
}

#[derive(Deserialize)]
struct ReadImageInput {
    file_path: String,
}

#[async_trait]
impl Tool for ReadImageTool {
    fn name(&self) -> &str {
        "read_image"
    }

    fn description(&self) -> &str {
        "Read an image file (PNG, JPEG, or BMP) from the current working directory and view it. Use this to look at a screenshot, diagram, or photo on disk."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute or relative path to the image file"
                }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput> {
        let parsed: ReadImageInput = serde_json::from_value(input)
            .map_err(|e| HarnessError::Tool(format!("Invalid read_image input: {e}")))?;

        let path = match self.resolve_path(&parsed.file_path) {
            Ok(path) => path,
            Err(reason) => return Ok(ToolOutput::error(reason)),
        };
        debug!("read_image: path={}", path.display());

        if path.is_dir() {
            return Ok(ToolOutput::error(format!(
                "{} is a directory, not an image file",
                path.display()
            )));
        }

        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolOutput::error(format!(
                    "Failed to read {}: {e}",
                    path.display()
                )));
            }
        };

        // The label handed to `attachment_from_image_bytes` is this path, so
        // the reason it reports already names the file. Wrapping it again
        // would print the path twice.
        match attachment_from_image_bytes(&bytes, &path.display().to_string()) {
            Ok(attachment) => Ok(ToolOutput {
                content: format!(
                    "Read image {} ({}, {} bytes)",
                    path.display(),
                    attachment.media_type,
                    bytes.len()
                ),
                is_error: false,
                image: Some(attachment),
            }),
            Err(reason) => Ok(ToolOutput::error(reason)),
        }
    }
}
