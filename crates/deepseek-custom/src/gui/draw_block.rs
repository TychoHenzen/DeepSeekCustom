//! Transcript block painting. Free functions called from [super::draw].
//! No `DeepSeekGui` methods live here -- those stay in `draw.rs`.

use eframe::egui::{self, Color32, RichText, ScrollArea};
use egui_commonmark::CommonMarkViewer;

use super::format::*;
use super::transcript::{Block, BlockId, BlockKind, Span};
use crate::api::types::ImageAttachment;

const USER_BG: Color32 = Color32::from_rgb(30, 50, 70);
const ASSIST_BG: Color32 = Color32::from_rgb(40, 40, 50);
const USER_LABEL: Color32 = Color32::from_rgb(100, 200, 255);
const ASSIST_LABEL: Color32 = Color32::from_rgb(180, 255, 180);
const THINK_DIM: Color32 = Color32::from_rgb(160, 160, 160);
const IMG_THUMB_CAP: f32 = 200.0;

pub(super) type Toggles = Vec<(Vec<BlockId>, bool)>;

// ---------------------------------------------------------------------------
// Entry point -- the one function `draw.rs` calls
// ---------------------------------------------------------------------------

/// Paint a single transcript block, wrapping it in an egui id scope so
/// every widget id is stable regardless of paint order.
pub(super) fn paint_block(
    ui: &mut egui::Ui,
    block: &Block,
    prefix: &[BlockId],
    cache: &mut egui_commonmark::CommonMarkCache,
    toggles: &mut Toggles,
    pins: &mut Toggles,
) {
    let path = extend_path(prefix, block.id);
    // Every widget id inside a block hangs off this scope. Without it a
    // block's inner ids come from egui's auto-id counter, which is a
    // position in the paint order: two sibling blocks then produce the
    // same ids ("ID clash"), and inserting a block shifts every later
    // block's ids. A `BlockId` is stable and, joined with the enclosing
    // subagent path, unique across the whole transcript tree.
    ui.push_id(&path, |ui| {
        paint_block_inner(ui, block, &path, cache, toggles, pins);
    });
}

// ---------------------------------------------------------------------------
// Per-kind painters
// ---------------------------------------------------------------------------

fn paint_block_inner(
    ui: &mut egui::Ui,
    block: &Block,
    path: &[BlockId],
    cache: &mut egui_commonmark::CommonMarkCache,
    toggles: &mut Toggles,
    pins: &mut Toggles,
) {
    match &block.kind {
        BlockKind::User { text } => paint_user(ui, text),
        BlockKind::Assistant { spans } => paint_assistant(ui, spans, cache),
        BlockKind::ToolCall { .. } => paint_tool(ui, block, path, toggles),
        BlockKind::Notice { text, severity } => {
            let c = severity_color(*severity);
            ui.label(RichText::new(text).color(c));
        }
        BlockKind::Image { image } => paint_img(ui, path, image),
        BlockKind::Subagent { .. } => paint_sub(ui, block, path, cache, toggles, pins),
    }
}

fn paint_user(ui: &mut egui::Ui, text: &str) {
    bubble_frame(USER_BG).show(ui, |ui| {
        ui.label(RichText::new("You").color(USER_LABEL).strong());
        ui.label(text);
    });
}

fn paint_assistant(
    ui: &mut egui::Ui,
    spans: &[Span],
    cache: &mut egui_commonmark::CommonMarkCache,
) {
    bubble_frame(ASSIST_BG).show(ui, |ui| {
        ui.label(RichText::new("Assistant").color(ASSIST_LABEL).strong());
        for (i, span) in spans.iter().enumerate() {
            match span {
                Span::Text(t) => {
                    CommonMarkViewer::new().show(ui, cache, t);
                }
                Span::Reasoning(t) => {
                    // A `CollapsingHeader` takes its id from its label
                    // unless told otherwise, so two reasoning spans in
                    // one block would both be "Reasoning" and clash.
                    egui::CollapsingHeader::new(RichText::new("Reasoning").color(THINK_DIM))
                        .id_salt(i)
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.label(RichText::new(t).color(THINK_DIM));
                        });
                }
            }
        }
    });
}

fn paint_tool(ui: &mut egui::Ui, block: &Block, path: &[BlockId], toggles: &mut Toggles) {
    let BlockKind::ToolCall {
        tool,
        args,
        output,
        is_error,
    } = &block.kind
    else {
        return;
    };
    let header = tool_summary(block.collapsed, tool, args, *is_error);
    let color = tool_color(*is_error);
    let resp = ui.selectable_label(false, RichText::new(&header).color(color));
    if resp.clicked() {
        toggles.push((path.to_vec(), !block.collapsed));
    }
    if block.collapsed {
        return;
    }
    ui.indent(("tool-body", path), |ui| {
        ui.label(RichText::new(args).color(Color32::GRAY));
        let body = output.as_deref().unwrap_or("(running)");
        let body_color = tool_output_color(*is_error);
        ui.label(RichText::new(body).color(body_color));
    });
}

fn paint_img(ui: &mut egui::Ui, path: &[BlockId], image: &ImageAttachment) {
    let Some(bytes) = crate::gui::attachment::decode_image_bytes(image) else {
        ui.label(RichText::new(format!("[image: {}]", image.media_type)).color(IMAGE_LABEL_COLOR));
        return;
    };
    // Keyed on the whole path, not the block id alone. A `BlockId` is
    // only unique within its own transcript, so an image inside a
    // subagent block could otherwise share a loader uri, and a window
    // id, with an image in the main conversation and show the wrong
    // picture.
    let uri = format!("bytes://image-block-{path:?}");
    let open_id = egui::Id::new(("image-block-open", path));
    let mut open = ui.data(|d| d.get_temp::<bool>(open_id).unwrap_or(false));
    let thumb = egui::Image::from_bytes(uri.clone(), bytes.clone())
        .max_size(egui::vec2(IMG_THUMB_CAP, IMG_THUMB_CAP))
        .sense(egui::Sense::click());
    if ui
        .add(thumb)
        .on_hover_text("Click to view full size")
        .clicked()
    {
        open = !open;
    }
    if open {
        egui::Window::new(format!("Image ({})", image.media_type))
            .id(egui::Id::new(("image-block-window", path)))
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.add(egui::Image::from_bytes(uri, bytes));
            });
    }
    ui.data_mut(|d| d.insert_temp(open_id, open));
}

fn paint_sub(
    ui: &mut egui::Ui,
    block: &Block,
    path: &[BlockId],
    cache: &mut egui_commonmark::CommonMarkCache,
    toggles: &mut Toggles,
    pins: &mut Toggles,
) {
    let BlockKind::Subagent {
        backend,
        model,
        depth,
        state,
        elapsed_ms,
        started_at,
        transcript,
        session_turns,
        session_turn_cap,
        send_message_calls,
        send_message_call_cap,
        ..
    } = &block.kind
    else {
        return;
    };
    let ms = subagent_elapsed_ms(*started_at, *elapsed_ms);
    let header = subagent_header_summary(
        backend,
        model,
        *depth,
        *state,
        ms,
        *session_turns,
        *session_turn_cap,
        *send_message_calls,
        *send_message_call_cap,
    );
    let color = subagent_state_color(*state);
    let open = block.pinned || !block.collapsed;
    let ch = egui::CollapsingHeader::new(RichText::new(&header).color(color))
        .id_salt(path)
        .default_open(false)
        .open(Some(open));
    let resp = ch.show(ui, |ui| {
        for (i, b) in transcript.blocks().iter().enumerate() {
            ui.add_space(gap_before(i, &b.kind));
            paint_block(ui, b, path, cache, toggles, pins);
        }
    });
    if resp.header_response.clicked() {
        toggles.push((path.to_vec(), !block.collapsed));
    }
    let pin_label = if block.pinned { "Unpin" } else { "Pin" };
    if ui.small_button(pin_label).clicked() {
        pins.push((path.to_vec(), !block.pinned));
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extend_path(prefix: &[BlockId], id: BlockId) -> Vec<BlockId> {
    let mut p = prefix.to_vec();
    p.push(id);
    p
}

fn bubble_frame(fill: Color32) -> egui::Frame {
    egui::Frame::default()
        .fill(fill)
        .inner_margin(8.0)
        .corner_radius(4.0)
}

/// Scroll area with stick-to-bottom behaviour shared by bubbles and
/// raw-output modes.
pub(super) fn chat_scroll_area(stick: bool) -> ScrollArea {
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(stick)
}
