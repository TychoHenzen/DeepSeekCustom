//! Building an `ImageAttachment` from raw image bytes.
//!
//! Shared by browser uploads, tools, and persistence without a UI dependency.

use base64::Engine;
use image::ImageFormat;

use crate::api::types::ImageAttachment;

/// Check that `bytes` is a real image and wrap it as an
/// `ImageAttachment`. The original bytes are kept, not re-encoded.
///
/// `image::guess_format` only reads magic bytes. So
/// `load_from_memory_with_format` is what proves the file decodes. That
/// catches a truncated or corrupt file before it reaches a backend.
///
/// `label` names the source in an error message. A dropped file uses its
/// path. A test uses whatever it likes.
pub fn attachment_from_image_bytes(bytes: &[u8], label: &str) -> Result<ImageAttachment, String> {
    let format = image::guess_format(bytes)
        .map_err(|_| format!("{label} is not a recognized image format"))?;
    image::load_from_memory_with_format(bytes, format)
        .map_err(|e| format!("{label} could not be decoded: {e}"))?;
    Ok(ImageAttachment {
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
        media_type: mime_for_image_format(format).to_string(),
    })
}

pub fn decode_image_attachment(image: &ImageAttachment) -> Result<Vec<u8>, base64::DecodeError> {
    base64::engine::general_purpose::STANDARD.decode(&image.data)
}

/// The MIME type an `ImageAttachment` carries for a decoded
/// `image::ImageFormat`.
///
/// This build enables a decoder for png, jpeg, and bmp only. See the
/// `image` dependency comment in `Cargo.toml`. `guess_format` recognises
/// other formats by their magic bytes. Those still fail at the
/// `load_from_memory_with_format` step in `attachment_from_image_bytes`.
/// No code round-trips through a separate name for them.
pub fn mime_for_image_format(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Bmp => "image/bmp",
        _ => "application/octet-stream",
    }
}
