//! Unit tests for `deepseek_custom::image_bytes` (`src/image_bytes.rs`).
//! These contracts remain presentation-neutral after native clipboard removal.

use image::ImageFormat;

use deepseek_custom::image_bytes::{
    attachment_from_image_bytes, decode_image_attachment, mime_for_image_format,
};

/// A 1x1 PNG, the smallest real image a test can attach.
fn test_png_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::new(1, 1))
        .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
        .expect("a 1x1 image must encode as PNG");
    bytes
}

#[test]
fn attachment_from_image_bytes_accepts_a_real_png_and_keeps_its_bytes() {
    let bytes = test_png_bytes();
    let attachment =
        attachment_from_image_bytes(&bytes, "test.png").expect("a real PNG must be accepted");
    assert_eq!(attachment.media_type, "image/png");
    let decoded = decode_image_attachment(&attachment).expect("payload must be base64");
    assert_eq!(decoded, bytes, "the original bytes must not be re-encoded");
}

#[test]
fn attachment_from_image_bytes_accepts_a_real_jpeg() {
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb8(image::RgbImage::new(1, 1))
        .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Jpeg)
        .expect("a 1x1 image must encode as JPEG");
    let attachment =
        attachment_from_image_bytes(&bytes, "test.jpg").expect("a real JPEG must be accepted");
    assert_eq!(attachment.media_type, "image/jpeg");
}

#[test]
fn attachment_from_image_bytes_rejects_garbage() {
    let error = attachment_from_image_bytes(b"not an image at all", "junk.bin")
        .expect_err("garbage must be rejected");
    assert!(error.contains("junk.bin"), "the error must name the source");
}

#[test]
fn mime_for_image_format_names_each_enabled_decoder() {
    assert_eq!(mime_for_image_format(ImageFormat::Png), "image/png");
    assert_eq!(mime_for_image_format(ImageFormat::Jpeg), "image/jpeg");
    assert_eq!(mime_for_image_format(ImageFormat::Bmp), "image/bmp");
    assert_eq!(
        mime_for_image_format(ImageFormat::Gif),
        "application/octet-stream"
    );
}
