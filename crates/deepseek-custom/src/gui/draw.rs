use eframe::egui::{self, Color32, RichText, ScrollArea};
use egui_commonmark::CommonMarkViewer;

use super::format::*;
use super::transcript::{Block, BlockId, BlockKind, Span, SubagentState, Transcript};
use super::{ActiveTab, DeepSeekGui};
use crate::api::types::ImageAttachment;

const USER_BG: Color32 = Color32::from_rgb(30, 50, 70);
const ASSIST_BG: Color32 = Color32::from_rgb(40, 40, 50);
const USER_LABEL: Color32 = Color32::from_rgb(100, 200, 255);
const ASSIST_LABEL: Color32 = Color32::from_rgb(180, 255, 180);
const THINK_DIM: Color32 = Color32::from_rgb(160, 160, 160);
const IMG_THUMB_CAP: f32 = 200.0;

type Toggles = Vec<(Vec<BlockId>, bool)>;

impl DeepSeekGui {
    /// Central panel: tab bar plus the selected tab's content.
    pub(super) fn paint_central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                for tab in [
                    ActiveTab::Chat,
                    ActiveTab::Autopilot,
                    ActiveTab::Cascade,
                    ActiveTab::Evolve,
                    ActiveTab::Sessions,
                ] {
                    ui.selectable_value(&mut self.active_tab, tab, format!("{tab:?}"));
                }
            });
            ui.separator();
            match self.active_tab {
                ActiveTab::Chat => self.paint_chat(ui),
                ActiveTab::Autopilot => {
                    let dirty = self.autopilot.render(ui, &mut self.settings);
                    if dirty {
                        self.persist_settings();
                    }
                    ui.separator();
                    self.paint_chat(ui);
                }
                // Both search tabs draw the same live transcript below
                // their own controls, the way the Autopilot tab does: a
                // run's attempts are ordinary subagent dispatches, so each
                // one is already a `Subagent` block down there.
                ActiveTab::Cascade => {
                    let names = self.backends.names().to_vec();
                    let effort = self.effort;
                    let dirty = self.cascade.render(ui, &mut self.settings, &names, effort);
                    if dirty {
                        self.persist_settings();
                    }
                    ui.separator();
                    self.paint_chat(ui);
                }
                ActiveTab::Evolve => {
                    let names = self.backends.names().to_vec();
                    let effort = self.effort;
                    let dirty = self.evolve.render(ui, &mut self.settings, &names, effort);
                    if dirty {
                        self.persist_settings();
                    }
                    ui.separator();
                    self.paint_chat(ui);
                }
                ActiveTab::Sessions => {
                    self.render_sessions_tab(ui);
                }
            }
        });
    }

    fn paint_chat(&mut self, ui: &mut egui::Ui) {
        if self.show_raw_output {
            self.paint_raw(ui);
        } else {
            self.paint_bubbles(ui);
        }
    }

    fn paint_bubbles(&mut self, ui: &mut egui::Ui) {
        let mut toggles: Toggles = Vec::new();
        let mut pins: Toggles = Vec::new();
        let stick = self.follow_output;
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(stick)
            .show(ui, |ui| {
                let blocks = self.transcript.blocks();
                for (i, block) in blocks.iter().enumerate() {
                    ui.add_space(gap_before(i, &block.kind));
                    paint_block(ui, block, &[], &mut self.md_cache, &mut toggles, &mut pins);
                }
            });
        for (path, val) in toggles {
            self.transcript.set_collapsed_by_path(&path, val);
        }
        for (path, val) in pins {
            self.transcript.set_pinned_by_path(&path, val);
        }
    }

    fn paint_raw(&self, ui: &mut egui::Ui) {
        let stick = self.follow_output;
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(stick)
            .show(ui, |ui| {
                for block in self.transcript.blocks() {
                    let text = raw_block_text(block);
                    let color = block_color(&block.kind);
                    ui.label(RichText::new(text).color(color));
                }
            });
    }
}

fn paint_block(
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
        BlockKind::Assistant { spans } => {
            paint_assistant(ui, spans, cache);
        }
        BlockKind::ToolCall {
            tool,
            args,
            output,
            is_error,
        } => paint_tool(
            ui,
            block.collapsed,
            tool,
            args,
            output.as_deref(),
            *is_error,
            path,
            toggles,
        ),
        BlockKind::Notice { text, severity } => {
            let c = severity_color(*severity);
            ui.label(RichText::new(text).color(c));
        }
        BlockKind::Image { image } => paint_img(ui, path, image),
        BlockKind::Subagent {
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
        } => paint_sub(
            ui,
            block,
            backend,
            model,
            *depth,
            *state,
            *started_at,
            *elapsed_ms,
            transcript,
            *session_turns,
            *session_turn_cap,
            *send_message_calls,
            *send_message_call_cap,
            path,
            cache,
            toggles,
            pins,
        ),
    }
}

fn extend_path(prefix: &[BlockId], id: BlockId) -> Vec<BlockId> {
    let mut p = prefix.to_vec();
    p.push(id);
    p
}

fn paint_user(ui: &mut egui::Ui, text: &str) {
    egui::Frame::default()
        .fill(USER_BG)
        .inner_margin(8.0)
        .corner_radius(4.0)
        .show(ui, |ui| {
            ui.label(RichText::new("You").color(USER_LABEL).strong());
            ui.label(text);
        });
}

fn paint_assistant(
    ui: &mut egui::Ui,
    spans: &[Span],
    cache: &mut egui_commonmark::CommonMarkCache,
) {
    egui::Frame::default()
        .fill(ASSIST_BG)
        .inner_margin(8.0)
        .corner_radius(4.0)
        .show(ui, |ui| {
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

#[allow(clippy::too_many_arguments)]
fn paint_tool(
    ui: &mut egui::Ui,
    collapsed: bool,
    tool: &str,
    args: &str,
    output: Option<&str>,
    is_error: bool,
    path: &[BlockId],
    toggles: &mut Toggles,
) {
    let header = tool_summary(collapsed, tool, args, is_error);
    let color = tool_color(is_error);
    let resp = ui.selectable_label(false, RichText::new(&header).color(color));
    if resp.clicked() {
        toggles.push((path.to_vec(), !collapsed));
    }
    if !collapsed {
        ui.indent(("tool-body", path), |ui| {
            ui.label(RichText::new(args).color(Color32::GRAY));
            match output {
                Some(o) => {
                    let c = tool_output_color(is_error);
                    ui.label(RichText::new(o).color(c));
                }
                None => {
                    ui.label(RichText::new("(running)").color(Color32::GRAY));
                }
            }
        });
    }
}

fn paint_img(ui: &mut egui::Ui, path: &[BlockId], image: &ImageAttachment) {
    let Some(bytes) = crate::gui::attachment::decode_image_bytes(image) else {
        ui.label(RichText::new(format!("[image: {}]", image.media_type)).color(IMAGE_LABEL_COLOR));
        return;
    };
    // Keyed on the whole path, not the block id alone. A `BlockId` is only
    // unique within its own transcript, so an image inside a subagent block
    // could otherwise share a loader uri, and a window id, with an image in
    // the main conversation and show the wrong picture.
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

#[allow(clippy::too_many_arguments)]
fn paint_sub(
    ui: &mut egui::Ui,
    block: &Block,
    backend: &str,
    model: &str,
    depth: u32,
    state: SubagentState,
    started_at: Option<std::time::Instant>,
    stored_ms: u64,
    inner: &Transcript,
    turns: u32,
    turn_cap: u32,
    calls: u32,
    call_cap: u32,
    path: &[BlockId],
    cache: &mut egui_commonmark::CommonMarkCache,
    toggles: &mut Toggles,
    pins: &mut Toggles,
) {
    let ms = subagent_elapsed_ms(started_at, stored_ms);
    let header = subagent_header_summary(
        backend, model, depth, state, ms, turns, turn_cap, calls, call_cap,
    );
    let color = subagent_state_color(state);
    let open = block.pinned || !block.collapsed;
    let ch = egui::CollapsingHeader::new(RichText::new(&header).color(color))
        .id_salt(path)
        .default_open(false)
        .open(Some(open));
    let resp = ch.show(ui, |ui| {
        for (i, b) in inner.blocks().iter().enumerate() {
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
