//! One image waits here for the next turn. Four input paths fill it.
//!
//! This owns every field that exists only because a turn may carry an
//! image. Those are the slot itself, the OS clipboard handle, and the
//! previous frame's Ctrl+V key state. All three were split out of
//! `DeepSeekGui`, which held them alongside forty other fields.
//!
//! The slot holds at most one image. `AgentCommand::UserTurn` carries an
//! `Option<ImageAttachment>`, and that is the whole reason for the cap.
//! So this type uses an `Option` directly. It does not use a collection
//! that never reaches length two.

use std::path::Path;

use base64::Engine;
use eframe::egui;
use image::ImageFormat;
use tracing::warn;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_V};

use super::transcript::{BlockKind, Severity, Transcript};
use crate::api::types::ImageAttachment;

/// Largest edge, in points, of a thumbnail in the pending strip.
const PENDING_ATTACHMENT_THUMBNAIL_MAX: f32 = 80.0;

/// The pending image and the input sources that fill it.
///
/// Every method that can reject an image takes the transcript. A
/// rejection only helps if the user sees it. A silent drop leaves
/// somebody watching the strip with no idea why nothing appeared.
pub struct AttachmentSlot {
    /// The image the next turn will carry, if any.
    pending: Option<ImageAttachment>,
    /// Direct connection to the OS clipboard for Ctrl+V image paste.
    /// Distinct from egui-winit's own internal clipboard, which only reads
    /// and writes text. `None` when `arboard::Clipboard::new` fails, which
    /// disables image paste without affecting anything else in the GUI.
    clipboard: Option<arboard::Clipboard>,
    /// Previous frame's raw Ctrl+V key state, for edge-triggering
    /// `poll_ctrl_v_paste`. See that method for why this polls
    /// `GetAsyncKeyState` directly instead of an egui key event.
    ctrl_v_prev_down: bool,
}

impl AttachmentSlot {
    /// An empty slot, with the clipboard opened if the OS allows it.
    /// A clipboard that will not open disables image paste and nothing
    /// else. So it is logged, not treated as a startup failure.
    pub fn new() -> Self {
        Self {
            pending: None,
            clipboard: arboard::Clipboard::new()
                .inspect_err(
                    |e| warn!(error = %e, "could not open OS clipboard; Ctrl+V image paste disabled"),
                )
                .ok(),
            ctrl_v_prev_down: false,
        }
    }

    /// True while no image is waiting to be sent.
    pub fn is_empty(&self) -> bool {
        self.pending.is_none()
    }

    /// Hand over the pending image and leave the slot empty. This is what
    /// a submitted turn calls, so the strip clears exactly when the image
    /// goes out.
    pub fn take(&mut self) -> Option<ImageAttachment> {
        self.pending.take()
    }

    /// Discard the pending image without sending it, e.g. from the strip's
    /// own remove button. Clearing an already-empty slot does nothing.
    pub fn clear(&mut self) {
        self.pending = None;
    }

    /// Put `attachment` in the slot, replacing whatever was there.
    ///
    /// A turn can carry only one image. So a second paste or drop
    /// replaces the first rather than queuing beside it. The replacement
    /// is never silent. A `Notice` says so. A user watching only the strip
    /// could otherwise lose track of which image is about to be sent.
    pub fn set(&mut self, attachment: ImageAttachment, transcript: &mut Transcript) {
        if self.pending.is_some() {
            transcript.push(BlockKind::Notice {
                text: "Only one image can be attached per turn; replacing the pending image."
                    .into(),
                severity: Severity::Info,
            });
        }
        self.pending = Some(attachment);
    }

    /// Poll the raw Windows key state for a Ctrl+V edge and, on one, try to
    /// pull an image off the OS clipboard.
    ///
    /// This skips egui's own event system on purpose. It also has no
    /// choice. `egui-winit-0.31.1` turns a winit key event into an egui one
    /// in `State::on_keyboard_input`. Reading that function shows
    /// `is_paste_command` catches Ctrl+V first. egui's caller never sees a
    /// `Key` event for `V`.
    ///
    /// A clipboard holding text becomes a text-only
    /// `egui::Event::Paste(String)`, with no image bytes. A clipboard
    /// holding only an image is the common case for a screenshot tool.
    /// There, `clipboard.get()` inside egui-winit returns `None`, and the
    /// function pushes no event at all. Neither case leaves this app
    /// anything to read. `eframe` 0.31 has no `raw_input_hook` to catch the
    /// winit event first. `GetAsyncKeyState` is the only signal left.
    ///
    /// The paste is gated on the focus flag from `ctx`. A Ctrl+V typed into
    /// another window never attaches an image here. The key state itself is
    /// tracked whether this window has focus or not. That way a press that
    /// started earlier does not fire the instant focus arrives.
    pub(crate) fn poll_ctrl_v_paste(&mut self, ctx: &egui::Context, transcript: &mut Transcript) {
        let just_pressed = ctrl_v_edge_triggered(&mut self.ctrl_v_prev_down);
        if !just_pressed || !ctx.input(|i| i.focused) {
            return;
        }
        let Some(clipboard) = self.clipboard.as_mut() else {
            return;
        };
        // An `Err` here almost always means the clipboard holds text
        // rather than an image. A plain-text Ctrl+V must keep working as
        // before. So this is not logged as a failure.
        let Ok(image) = clipboard.get_image() else {
            return;
        };
        match attachment_from_clipboard_image(&image) {
            Some(attachment) => self.set(attachment, transcript),
            None => warn!("clipboard image could not be encoded as PNG"),
        }
    }

    /// Attach every dropped file that decodes as an image. eframe already
    /// collects a native drop into `RawInput::dropped_files` with a real
    /// filesystem path. The `bytes` field on `DroppedFile` is web-only. So
    /// this reads each path and checks that it decodes. A file that does
    /// not decode gets a `Notice`, rather than being dropped without a
    /// trace.
    pub(crate) fn handle_dropped_files(
        &mut self,
        ctx: &egui::Context,
        transcript: &mut Transcript,
    ) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for file in dropped {
            let Some(path) = file.path else {
                transcript.push(BlockKind::Notice {
                    text: "A dropped file carried no filesystem path and was ignored.".into(),
                    severity: Severity::Warning,
                });
                continue;
            };
            match attachment_from_file_path(&path) {
                Ok(attachment) => self.set(attachment, transcript),
                Err(message) => {
                    warn!(%message, "dropped file rejected");
                    transcript.push(BlockKind::Notice {
                        text: message,
                        severity: Severity::Warning,
                    });
                }
            }
        }
    }

    /// The pending-attachment strip: the thumbnail and its remove button.
    /// Draws nothing when the slot is empty, so it never reserves space in
    /// the input bar when there is nothing pending.
    pub(crate) fn render_strip(&mut self, ui: &mut egui::Ui) {
        let Some(attachment) = self.pending.as_ref() else {
            return;
        };
        let Some(bytes) = decode_image_bytes(attachment) else {
            return;
        };
        let mut removed = false;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.add(
                    egui::Image::from_bytes("bytes://pending-attachment", bytes).max_size(
                        egui::vec2(
                            PENDING_ATTACHMENT_THUMBNAIL_MAX,
                            PENDING_ATTACHMENT_THUMBNAIL_MAX,
                        ),
                    ),
                );
                removed = ui.small_button("Remove").clicked();
            });
        });
        if removed {
            self.clear();
        }
    }
}

/// The raw base64 bytes of an attachment, decoded for display. `None` when
/// the payload is not valid base64.
pub fn decode_image_bytes(image: &ImageAttachment) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(&image.data)
        .ok()
}

/// True exactly on the frame Ctrl+V transitions from not-held to held,
/// tracked by the caller's own `prev_down` across frames. See
/// `AttachmentSlot::poll_ctrl_v_paste` for why this reads the raw key
/// state instead of an egui event.
///
/// The raw `GetAsyncKeyState` read and the edge-detection bookkeeping are
/// split apart so `edge_trigger` can be unit tested against synthetic key
/// states. A test cannot drive real hardware state.
fn ctrl_v_edge_triggered(prev_down: &mut bool) -> bool {
    // SAFETY: `GetAsyncKeyState` is a plain state query against
    // user32.dll. It takes a virtual-key code by value. It asks only to be
    // called from a thread with a message queue. The GUI thread has one.
    let down = unsafe {
        let ctrl_down = (GetAsyncKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;
        let v_down = (GetAsyncKeyState(VK_V.0 as i32) as u16 & 0x8000) != 0;
        ctrl_down && v_down
    };
    edge_trigger(down, prev_down)
}

/// True exactly once, on the transition from not-`down` to `down`, using
/// and updating `prev_down` as the state carried across calls.
pub fn edge_trigger(down: bool, prev_down: &mut bool) -> bool {
    let just_pressed = down && !*prev_down;
    *prev_down = down;
    just_pressed
}

/// Encode `arboard`'s raw RGBA8 clipboard pixels as PNG bytes for an
/// `ImageAttachment`. `arboard::ImageData` holds raw pixels, not a file
/// format. So the only check worth making is that the buffer's length
/// matches `width * height * 4`.
pub fn attachment_from_clipboard_image(image: &arboard::ImageData) -> Option<ImageAttachment> {
    if image.width == 0 || image.height == 0 || image.bytes.len() != image.width * image.height * 4
    {
        return None;
    }
    let buffer = image::RgbaImage::from_raw(
        image.width as u32,
        image.height as u32,
        image.bytes.to_vec(),
    )?;
    let mut png_bytes = Vec::new();
    image::DynamicImage::ImageRgba8(buffer)
        .write_to(&mut std::io::Cursor::new(&mut png_bytes), ImageFormat::Png)
        .ok()?;
    Some(ImageAttachment {
        data: base64::engine::general_purpose::STANDARD.encode(png_bytes),
        media_type: "image/png".into(),
    })
}

/// Read a dropped file off disk and build an `ImageAttachment` from it.
/// Returns a message describing why not, on failure. This stays separate
/// from `attachment_from_image_bytes`. That way a test can check the
/// decode step on in-memory bytes, without touching the filesystem.
fn attachment_from_file_path(path: &Path) -> Result<ImageAttachment, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    attachment_from_image_bytes(&bytes, &path.display().to_string())
}

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
