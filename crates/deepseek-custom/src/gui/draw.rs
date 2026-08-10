use eframe::egui::{self, RichText};

use super::draw_block::{Toggles, chat_scroll_area, paint_block};
use super::format::*;
use super::{ActiveTab, DeepSeekGui};

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
                // their own controls, the way the Autopilot tab does:
                // each attempt is already a `Subagent` block.
                ActiveTab::Cascade => {
                    let names = self.backends.names().to_vec();
                    let dirty = self
                        .cascade
                        .render(ui, &mut self.settings, &names, self.effort);
                    if dirty {
                        self.persist_settings();
                    }
                    ui.separator();
                    self.paint_chat(ui);
                }
                ActiveTab::Evolve => {
                    let names = self.backends.names().to_vec();
                    let dirty = self
                        .evolve
                        .render(ui, &mut self.settings, &names, self.effort);
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
            return;
        }
        self.paint_bubbles(ui);
    }

    fn paint_bubbles(&mut self, ui: &mut egui::Ui) {
        let mut toggles: Toggles = Vec::new();
        let mut pins: Toggles = Vec::new();
        chat_scroll_area(self.follow_output).show(ui, |ui| {
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
        chat_scroll_area(self.follow_output).show(ui, |ui| {
            for block in self.transcript.blocks() {
                let text = raw_block_text(block);
                let color = block_color(&block.kind);
                ui.label(RichText::new(text).color(color));
            }
        });
    }
}
