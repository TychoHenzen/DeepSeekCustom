use eframe::egui;

use super::{ActiveTab, DeepSeekGui};

impl DeepSeekGui {
    pub(super) fn paint_bottom_panels(&mut self, ctx: &egui::Context) {
        // The first bottom panel declared takes the bottommost strip, and
        // each later one stacks above it. The status bar goes first so it
        // stays pinned to the window's bottom edge. The input bar is
        // Chat-only, so declaring it first would drop the status bar to the
        // bottom on every other tab and jump it back up on return.
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| self.paint_status_row(ui));
        if self.active_tab == ActiveTab::Chat {
            egui::TopBottomPanel::bottom("input_bar").show(ctx, |ui| {
                self.attachment.render_strip(ui);
                self.paint_input_row(ui);
            });
        }
    }

    fn paint_input_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let resp = ui.text_edit_singleline(&mut self.input_buffer);
            if !ui.ctx().memory(|mem| mem.focused().is_some()) {
                resp.request_focus();
            }
            self.input_focused = resp.has_focus();
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if enter || ui.button("Send").clicked() {
                self.send_input();
            }
        });
    }

    fn paint_status_row(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let back = self.backends.active_backend();
            let model = self.backends.model();
            ui.label(format!("Backend: {back} ({model})"));
            ui.separator();
            ui.label(format!("Dir: {}", self.working_dir_buffer));
            ui.separator();
            ui.label(format!("Effort: {:?}", self.effort));
            ui.separator();
            ui.label(&self.session_status);
            if !self.token_count.is_empty() {
                ui.separator();
                ui.label(format!("Tokens: {}", self.token_count));
                ui.label(format!(
                    "Cache: {} hit / {} miss",
                    self.total_cache_hit_tokens, self.total_cache_miss_tokens,
                ));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new("Tab: settings | Esc: interrupt | Ctrl+Q: quit")
                        .color(egui::Color32::GRAY),
                );
            });
        });
    }
}
