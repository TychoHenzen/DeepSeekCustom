//! The settings sidebar (`Tab` toggles it): backend picker, model picker
//! with its background fetch, effort combo box, working directory field,
//! voice controls, and the Experimental section's context budget slider.
//!
//! The `apply_*` functions below write one control's value into `Settings`.
//! They are plain functions over `&mut Settings`, deliberately, so a
//! round-trip test can call them without building a GUI. The `*_command`
//! functions map a control's new value to the `VoiceCommand` that change
//! should send, kept separate from the egui code for the same reason.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use eframe::egui::{self, Color32, RichText, TextEdit};
use tracing::{info, warn};

use crate::config::settings::Settings;
use crate::effort::Effort;

use super::DeepSeekGui;

impl DeepSeekGui {
    /// Render the settings sidebar, if it is currently toggled open.
    pub(super) fn render_settings_panel(&mut self, ctx: &egui::Context) {
        if !self.settings_visible {
            return;
        }
        egui::SidePanel::right("settings_panel")
            .min_width(220.0)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("Settings");
                ui.separator();

                // Both pickers live on `BackendPicker`, which owns every
                // field they read and write.
                let picked = self.backends.render(ui, &mut self.settings);
                if picked.dirty {
                    self.persist_settings();
                }
                if let Some(command) = picked.switch {
                    self.apply_backend_switch(command);
                }

                ui.add_space(8.0);

                // ── Effort selector ──
                let prev_effort = self.effort;
                egui::ComboBox::from_label("Effort")
                    .selected_text(format!("{:?}", self.effort))
                    .show_ui(ui, |ui| {
                        for level in [
                            Effort::None,
                            Effort::Low,
                            Effort::Medium,
                            Effort::High,
                            Effort::Max,
                        ] {
                            ui.selectable_value(
                                &mut self.effort,
                                level,
                                format!("{level:?}"),
                            );
                        }
                    });
                if self.effort != prev_effort {
                    self.effort.store(&self.handles.effort);
                    info!(effort = ?self.effort, "effort level changed via settings panel");
                    apply_effort(&mut self.settings, self.effort);
                    self.persist_settings();
                }
                ui.label(
                    RichText::new(
                        "Effort changes apply next turn (DeepSeek, Ollama). Claude respawns its child.",
                    )
                    .color(Color32::GRAY)
                    .small(),
                );

                ui.add_space(8.0);
                ui.separator();

                // ── Working directory ──
                ui.label(
                    RichText::new("Working directory")
                        .color(Color32::from_rgb(180, 220, 255)),
                );
                let dir_response = ui.add(
                    TextEdit::singleline(&mut self.working_dir_buffer)
                        .hint_text("path where Bash, Read, Write, and Cd act"),
                );
                if dir_response.lost_focus() {
                    self.commit_working_dir_change();
                }
                if !Path::new(&self.working_dir_buffer).is_dir() {
                    ui.label(
                        RichText::new("  Not a directory - change not applied")
                            .color(Color32::from_rgb(255, 120, 120))
                            .small(),
                    );
                }

                ui.add_space(8.0);

                // ── Output display toggle ──
                let mut show_raw = self.show_raw_output;
                if ui.checkbox(&mut show_raw, "Show raw output").changed() {
                    self.show_raw_output = show_raw;
                    apply_show_raw_output(&mut self.settings, show_raw);
                    self.persist_settings();
                }
                if self.show_raw_output {
                    ui.label(
                        RichText::new("  Plain text with ANSI-like coloring")
                            .color(Color32::GRAY)
                            .small(),
                    );
                } else {
                    ui.label(
                        RichText::new("  Rendered markdown")
                            .color(Color32::GRAY)
                            .small(),
                    );
                }

                ui.add_space(8.0);
                ui.separator();

                // The whole Voice section lives on `VoiceUi`, which owns
                // every field these controls read and write.
                if self.voice
                    .render_section(ui, &mut self.settings, &self.handles.voice_mode) {
                    self.persist_settings();
                }

                ui.add_space(8.0);
                ui.separator();

                // ── Experimental features section ──
                ui.label(RichText::new("Experimental").color(Color32::from_rgb(255, 200, 100)));

                let mut context_budget = self.context_budget;
                let budget_response = ui.add(
                    egui::Slider::new(&mut context_budget, 32_000..=200_000)
                        .step_by(1000.0)
                        .text("Context budget"),
                );
                if budget_response.changed() {
                    self.context_budget = context_budget;
                    self.handles.context_budget
                        .store(context_budget, Ordering::SeqCst);
                    info!(
                        context_budget = context_budget,
                        "context budget changed via settings panel"
                    );
                }
                // Same as the speed slider: one write per drag.
                if budget_response.drag_stopped() {
                    let budget = self.context_budget;
                    apply_context_budget(&mut self.settings, budget);
                    self.persist_settings();
                }
                ui.label(
                    RichText::new(format!(
                        "  Prunes to {} tokens when exceeded",
                        self.context_budget / 3
                    ))
                    .color(Color32::GRAY)
                    .small(),
                );

                ui.add_space(8.0);

                // ── Plain-language gate ──
                let mut plain_language = self.plain_language;
                if ui
                    .checkbox(&mut plain_language, "Plain-language gate")
                    .changed()
                {
                    self.plain_language = plain_language;
                    self.handles
                        .style_plain_language
                        .store(plain_language, Ordering::SeqCst);
                    info!(
                        plain_language,
                        "plain-language gate toggled via settings panel"
                    );
                    apply_plain_language(&mut self.settings, plain_language);
                    self.persist_settings();
                }
                if self.plain_language {
                    let mut grade = self.plain_language_grade;
                    let grade_response = ui.add(
                        egui::Slider::new(&mut grade, 4..=16).text("Target grade"),
                    );
                    if grade_response.changed() {
                        self.plain_language_grade = grade;
                        self.handles
                            .style_target_grade
                            .store(grade, Ordering::SeqCst);
                    }
                    // Same as the budget slider: one write per drag.
                    if grade_response.drag_stopped() {
                        let grade = self.plain_language_grade;
                        apply_target_grade(&mut self.settings, f32::from(grade));
                        self.persist_settings();
                    }
                    ui.label(
                        RichText::new(
                            "  Rewrites a reply reading above the target (DeepSeek, Ollama)",
                        )
                        .color(Color32::GRAY)
                        .small(),
                    );
                }

                ui.add_space(16.0);
                ui.separator();

                // ── Close button ──
                if ui.button("Close panel (Tab)").clicked() {
                    self.settings_visible = false;
                }

                ui.add_space(4.0);
                if ui.button("Quit (Ctrl+Q)").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
    }

    /// Apply the sidebar's working-directory text field, if it names a
    /// real, existing directory. Writes the shared `working_dir_flag`, so
    /// the next `Bash`, `Read`, `Write`, or `Cd` call acts there and the
    /// next turn's system prompt reports it, then persists the change.
    /// `project_root` is never touched: it stays the fixed anchor for
    /// `settings.json` itself.
    ///
    /// A path that does not exist or is not a directory is left alone: the
    /// shared handle keeps whatever it last held, and nothing is saved.
    /// The invalid text stays in the buffer so the user can see and fix it.
    pub(super) fn commit_working_dir_change(&mut self) {
        let candidate = PathBuf::from(&self.working_dir_buffer);
        if !candidate.is_dir() {
            warn!(
                path = %self.working_dir_buffer,
                "rejected working directory change: not a directory"
            );
            return;
        }
        *self.handles.working_dir.lock().unwrap() = candidate;
        info!(
            path = %self.working_dir_buffer,
            "working directory changed via settings panel"
        );
        let dir = self.working_dir_buffer.clone();
        apply_working_dir(&mut self.settings, &dir);
        self.persist_settings();
    }
}

// ── Control-to-settings mapping ──
//
// Each function below takes the plain value a settings-panel control just
// changed to and writes it into a `Settings`. Kept separate from the
// control's egui code so each write is unit-testable without an egui
// context, the same way the command builders above are. The voice writer
// creates its config block when it is missing, so a change is never
// silently dropped.

/// Store the effort combo box's selection.
pub fn apply_effort(settings: &mut Settings, effort: Effort) {
    settings.effort = Some(effort);
}

/// Store the raw-output checkbox's value.
pub fn apply_show_raw_output(settings: &mut Settings, show_raw: bool) {
    settings.show_raw_output = Some(show_raw);
}

/// Store the context budget slider's value.
pub fn apply_context_budget(settings: &mut Settings, budget: usize) {
    settings.context_budget = Some(budget);
}

/// Store the plain-language checkbox's value. Creates the `style` block
/// when it is missing, the same way the voice writer does, so the change
/// is never silently dropped.
pub fn apply_plain_language(settings: &mut Settings, enabled: bool) {
    settings.style_mut().plain_language_enabled = enabled;
}

/// Store the target-grade slider's value.
pub fn apply_target_grade(settings: &mut Settings, grade: f32) {
    settings.style_mut().target_grade = Some(grade);
}

/// Store the working-directory sidebar field's value.
pub fn apply_working_dir(settings: &mut Settings, dir: &str) {
    settings.working_dir = Some(dir.to_string());
}
