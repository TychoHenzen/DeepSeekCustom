//! Unit tests for `deepseek_custom::gui::attachment` (`src/gui/attachment.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use base64::Engine;
use image::ImageFormat;

use deepseek_custom::api::types::ImageAttachment;
use deepseek_custom::gui::attachment::{
    AttachmentSlot, attachment_from_clipboard_image, decode_image_bytes, edge_trigger,
};
use deepseek_custom::gui::transcript::{BlockKind, Transcript};

/// A 1x1 PNG, the smallest real image a test can attach.
fn test_png_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::new(1, 1))
        .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
        .expect("a 1x1 image must encode as PNG");
    bytes
}

fn test_image_attachment() -> ImageAttachment {
    ImageAttachment {
        data: base64::engine::general_purpose::STANDARD.encode(test_png_bytes()),
        media_type: "image/png".into(),
    }
}

#[test]
fn a_new_slot_is_empty() {
    assert!(AttachmentSlot::new().is_empty());
}

#[test]
fn set_fills_the_slot_without_a_notice() {
    let mut slot = AttachmentSlot::new();
    let mut transcript = Transcript::default();
    slot.set(test_image_attachment(), &mut transcript);
    assert!(!slot.is_empty());
    assert!(
        transcript.blocks().is_empty(),
        "a first attachment replaces nothing, so it must not post a notice"
    );
}

#[test]
fn set_caps_at_one_and_notices_the_replacement() {
    let mut slot = AttachmentSlot::new();
    let mut transcript = Transcript::default();
    slot.set(test_image_attachment(), &mut transcript);
    slot.set(test_image_attachment(), &mut transcript);
    assert!(!slot.is_empty(), "the replacement must still be pending");
    let notices = transcript
        .blocks()
        .iter()
        .filter(|block| matches!(block.kind, BlockKind::Notice { .. }))
        .count();
    assert_eq!(notices, 1, "replacing must not be silent");
}

#[test]
fn take_hands_over_the_image_and_empties_the_slot() {
    let mut slot = AttachmentSlot::new();
    let mut transcript = Transcript::default();
    slot.set(test_image_attachment(), &mut transcript);
    assert!(slot.take().is_some());
    assert!(slot.is_empty(), "the strip must clear on send");
}

#[test]
fn take_on_an_empty_slot_yields_nothing() {
    assert!(AttachmentSlot::new().take().is_none());
}

#[test]
fn clear_drops_the_pending_image() {
    let mut slot = AttachmentSlot::new();
    let mut transcript = Transcript::default();
    slot.set(test_image_attachment(), &mut transcript);
    slot.clear();
    assert!(slot.is_empty());
}

#[test]
fn clearing_an_empty_slot_is_a_no_op() {
    let mut slot = AttachmentSlot::new();
    slot.clear();
    assert!(slot.is_empty());
}

#[test]
fn attachment_from_clipboard_image_encodes_rgba_pixels_as_png() {
    let image = arboard::ImageData {
        width: 2,
        height: 2,
        bytes: vec![0u8; 2 * 2 * 4].into(),
    };
    let attachment = attachment_from_clipboard_image(&image).expect("valid buffer must encode");
    assert_eq!(attachment.media_type, "image/png");
    let decoded = decode_image_bytes(&attachment).expect("payload must be base64");
    assert_eq!(
        image::guess_format(&decoded).expect("must sniff"),
        ImageFormat::Png
    );
}

#[test]
fn attachment_from_clipboard_image_rejects_a_mismatched_buffer() {
    let image = arboard::ImageData {
        width: 2,
        height: 2,
        bytes: vec![0u8; 3].into(),
    };
    assert!(attachment_from_clipboard_image(&image).is_none());
}

#[test]
fn edge_trigger_fires_once_per_press() {
    let mut prev = false;
    assert!(edge_trigger(true, &mut prev), "the press frame fires");
    assert!(!edge_trigger(true, &mut prev), "holding does not refire");
    assert!(!edge_trigger(false, &mut prev), "release does not fire");
    assert!(edge_trigger(true, &mut prev), "the next press fires again");
}
