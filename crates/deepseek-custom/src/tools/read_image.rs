use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::Engine;
use image::ImageFormat;
use serde::Deserialize;
use tracing::debug;

use crate::api::types::ImageAttachment;
use crate::error::{HarnessError, Result};
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
    working_dir: Arc<Mutex<std::path::PathBuf>>,
}

impl ReadImageTool {
    pub fn new(working_dir: Arc<Mutex<std::path::PathBuf>>) -> Self {
        Self { working_dir }
    }

    /// Resolve a file path against the current working directory, read
    /// fresh from the shared flag. An absolute path is used as given.
    /// Mirrors `ReadTool::resolve_path` exactly; kept separate rather than
    /// shared, since the two tools have no other coupling and a shared
    /// helper would be the only thing tying them together.
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

        let path = self.resolve_path(&parsed.file_path);
        debug!("read_image: path={}", path.display());

        if path.is_dir() {
            return Ok(ToolOutput {
                content: format!("{} is a directory, not an image file", path.display()),
                is_error: true,
                image: None,
            });
        }

        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolOutput {
                    content: format!("Failed to read {}: {e}", path.display()),
                    is_error: true,
                    image: None,
                });
            }
        };

        match attachment_from_image_bytes(&bytes) {
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
            Err(reason) => Ok(ToolOutput {
                content: format!("{} is not a readable image: {reason}", path.display()),
                is_error: true,
                image: None,
            }),
        }
    }
}

/// Validate that `bytes` is a real, decodable image and wrap it as an
/// `ImageAttachment`, keeping the original bytes rather than re-encoding.
/// `image::guess_format` only sniffs magic bytes; `load_from_memory_with_format`
/// is what actually proves the file decodes, catching a truncated or
/// corrupt file. Mirrors `attachment_from_image_bytes` in `src/gui/mod.rs`,
/// which does the same job for a pasted or dropped image; kept as its own
/// copy here since a tool must not depend on the GUI crate module.
fn attachment_from_image_bytes(bytes: &[u8]) -> std::result::Result<ImageAttachment, String> {
    let format =
        image::guess_format(bytes).map_err(|_| "not a recognized image format".to_string())?;
    image::load_from_memory_with_format(bytes, format)
        .map_err(|e| format!("could not be decoded: {e}"))?;
    Ok(ImageAttachment {
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
        media_type: mime_for_image_format(format).to_string(),
    })
}

/// The MIME type an `ImageAttachment` carries for a decoded
/// `image::ImageFormat`. Only png, jpeg, and bmp are backed by an enabled
/// decoder in this build (see the `image` dependency comment in
/// `Cargo.toml`); any other format `guess_format` recognises by its magic
/// bytes still fails at the `load_from_memory_with_format` step above.
fn mime_for_image_format(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Bmp => "image/bmp",
        _ => "application/octet-stream",
    }
}
