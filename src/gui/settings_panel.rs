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
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::api::models::list_models;
use crate::config::settings::{BackendConfig, Settings, TriggerMode};
use crate::effort::Effort;
use crate::voice::service::VoiceCommand;

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

                // ── Backend selector ──
                let prev_idx = self.selected_backend_idx;
                let selected_text = self
                    .backend_options
                    .get(self.selected_backend_idx)
                    .map(String::as_str)
                    .unwrap_or("(none configured)");
                egui::ComboBox::from_label("Backend")
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        for (i, opt) in self.backend_options.iter().enumerate() {
                            ui.selectable_value(&mut self.selected_backend_idx, i, opt);
                        }
                    });
                if self.selected_backend_idx != prev_idx {
                    let new_backend = self.backend_options[self.selected_backend_idx].clone();
                    self.switch_backend(new_backend);
                }

                // ── Model selector ──
                let prev_model = self.model.clone();
                egui::ComboBox::from_label("Model")
                    .selected_text(self.model.clone())
                    .show_ui(ui, |ui| {
                        for opt in &self.model_options {
                            ui.selectable_value(&mut self.model, opt.clone(), opt);
                        }
                    });
                if self.model != prev_model {
                    self.switch_model(self.model.clone());
                }

                ui.label(
                    RichText::new(
                        "Switching backends takes effect on the next app start.",
                    )
                    .color(Color32::GRAY)
                    .small(),
                );
                ui.label(
                    RichText::new(
                        "Model changes apply next turn (DeepSeek, Ollama). Claude respawns its child.",
                    )
                    .color(Color32::GRAY)
                    .small(),
                );

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
                    self.effort.store(&self.effort_flag);
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

                // ── Voice section ──
                ui.label(RichText::new("Voice").color(Color32::from_rgb(180, 220, 255)));

                let mut voice_enabled = self.voice_master_enabled;
                if ui.checkbox(&mut voice_enabled, "Voice enabled").changed() {
                    self.voice_master_enabled = voice_enabled;
                    self.send_voice_command(voice_enabled_command(voice_enabled));
                    info!(voice_enabled, "voice enabled toggled via settings panel");
                    apply_voice_enabled(&mut self.settings, voice_enabled);
                    self.persist_settings();
                }

                let mut stt_enabled = self.voice_stt_enabled;
                if ui.checkbox(&mut stt_enabled, "Speech to text").changed() {
                    self.voice_stt_enabled = stt_enabled;
                    self.send_voice_command(stt_enabled_command(stt_enabled));
                    info!(stt_enabled, "speech-to-text toggled via settings panel");
                    apply_stt_enabled(&mut self.settings, stt_enabled);
                    self.persist_settings();
                }

                let mut tts_enabled = self.voice_tts_enabled;
                if ui.checkbox(&mut tts_enabled, "Text to speech").changed() {
                    self.voice_tts_enabled = tts_enabled;
                    self.voice_mode_flag
                        .store(voice_mode_flag_for_tts(tts_enabled), Ordering::SeqCst);
                    self.send_voice_command(tts_enabled_command(tts_enabled));
                    info!(tts_enabled, "text-to-speech toggled via settings panel");
                    apply_tts_enabled(&mut self.settings, tts_enabled);
                    self.persist_settings();
                }

                ui.add_space(4.0);
                ui.label(RichText::new("Trigger mode").color(Color32::GRAY).small());
                let prev_trigger_mode = self.voice_trigger_mode;
                ui.horizontal(|ui| {
                    ui.radio_value(
                        &mut self.voice_trigger_mode,
                        TriggerMode::PushToTalk,
                        "Push to talk",
                    );
                    ui.radio_value(
                        &mut self.voice_trigger_mode,
                        TriggerMode::WakeWord,
                        "Wake word",
                    );
                });
                if self.voice_trigger_mode != prev_trigger_mode {
                    self.send_voice_command(trigger_mode_command(self.voice_trigger_mode));
                    info!(
                        mode = ?self.voice_trigger_mode,
                        "voice trigger mode changed via settings panel"
                    );
                    let mode = self.voice_trigger_mode;
                    apply_trigger_mode(&mut self.settings, mode);
                    self.persist_settings();
                }

                ui.add_space(4.0);
                let wake_response = ui.add(
                    TextEdit::singleline(&mut self.voice_wake_phrase).hint_text("wake phrase"),
                );
                if wake_response.changed() {
                    self.send_voice_command(wake_phrase_command(&self.voice_wake_phrase));
                    info!(
                        phrase = %self.voice_wake_phrase,
                        "wake phrase changed via settings panel"
                    );
                }
                // Save on focus loss, not on every keystroke, so typing
                // a phrase writes the file once.
                if wake_response.lost_focus() {
                    let phrase = self.voice_wake_phrase.clone();
                    apply_wake_phrase(&mut self.settings, &phrase);
                    self.persist_settings();
                }

                ui.add_space(4.0);
                let prev_voice_idx = self.selected_voice_idx;
                egui::ComboBox::from_label("Kokoro voice")
                    .selected_text(&self.voice_id_options[self.selected_voice_idx])
                    .show_ui(ui, |ui| {
                        for (i, opt) in self.voice_id_options.iter().enumerate() {
                            ui.selectable_value(&mut self.selected_voice_idx, i, opt);
                        }
                    });
                if self.selected_voice_idx != prev_voice_idx {
                    let voice_id = self.voice_id_options[self.selected_voice_idx].clone();
                    self.send_voice_command(voice_id_command(&voice_id));
                    info!(voice_id = %voice_id, "kokoro voice changed via settings panel");
                    apply_tts_voice(&mut self.settings, &voice_id);
                    self.persist_settings();
                }

                ui.add_space(4.0);
                let speed_response =
                    ui.add(egui::Slider::new(&mut self.voice_speed, 0.5..=2.0).text("Speed"));
                if speed_response.changed() {
                    self.send_voice_command(speed_command(self.voice_speed));
                    info!(
                        speed = self.voice_speed,
                        "voice speed changed via settings panel"
                    );
                }
                // Save when the drag ends, so one drag writes the file
                // once instead of once per frame.
                if speed_response.drag_stopped() {
                    let speed = self.voice_speed;
                    apply_tts_speed(&mut self.settings, speed);
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
                    self.context_budget_flag
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
        *self.working_dir_flag.lock().unwrap() = candidate;
        info!(
            path = %self.working_dir_buffer,
            "working directory changed via settings panel"
        );
        let dir = self.working_dir_buffer.clone();
        apply_working_dir(&mut self.settings, &dir);
        self.persist_settings();
    }

    /// Handle the backend picker's selection changing.
    ///
    /// Shows the new backend's declared model. Seeds `model_options` with
    /// it, so the dropdown is never empty. Kicks off a background refetch
    /// of the full list. Persists the new default backend.
    ///
    /// The running session keeps its own backend and model until the next
    /// app start, so this never writes `model_flag` for a backend that is
    /// not the running one.
    pub(super) fn switch_backend(&mut self, new_backend: String) {
        if let Some(cfg) = self.settings.resolve_backend(&new_backend) {
            let new_model = cfg.model().to_string();
            self.model = new_model.clone();
            if self.active_backend.as_deref() == Some(new_backend.as_str())
                && let Ok(mut model) = self.model_flag.lock()
            {
                *model = new_model;
            }
            self.model_options = vec![cfg.model().to_string()];
            spawn_model_list_fetch(self.model_list_tx.clone(), new_backend.clone(), cfg.clone());
        }
        info!(backend = %new_backend, "backend changed via settings panel");
        apply_default_backend(&mut self.settings, &new_backend);
        self.persist_settings();
    }

    /// Handle the model dropdown's selection changing.
    ///
    /// Persists the new model onto the currently selected backend's entry
    /// in `settings.json`. Writes it into `model_flag` for the next turn
    /// only while that entry is the backend the session is running on. The
    /// running backend would otherwise be asked for a model name belonging
    /// to a different provider, and every following turn would fail.
    pub(super) fn switch_model(&mut self, new_model: String) {
        let Some(backend_name) = self.backend_options.get(self.selected_backend_idx).cloned()
        else {
            return;
        };
        if self.active_backend.as_deref() == Some(backend_name.as_str())
            && let Ok(mut model) = self.model_flag.lock()
        {
            *model = new_model.clone();
        }
        info!(backend = %backend_name, model = %new_model, "model changed via settings panel");
        apply_backend_model(&mut self.settings, &backend_name, &new_model);
        self.persist_settings();
    }

    /// Apply one background model-discovery result.
    ///
    /// Ignored when `backend_name` no longer matches the selected backend.
    /// That result is already stale by the time it arrives. The currently
    /// active model is added if the fetch omitted it, so the dropdown
    /// never loses the current selection.
    pub(super) fn apply_fetched_model_list(&mut self, backend_name: &str, models: Vec<String>) {
        let current_backend = self
            .backend_options
            .get(self.selected_backend_idx)
            .map(String::as_str);
        if current_backend != Some(backend_name) {
            return;
        }
        self.model_options = models;
        if !self.model_options.contains(&self.model) {
            self.model_options.push(self.model.clone());
        }
    }
}

// ── Voice control-to-command mapping ──
//
// Each function below takes the plain value a settings-panel control just
// changed to and returns the `VoiceCommand` that change should send. Kept
// separate from the control's egui code so each mapping is unit-testable
// without an egui context, the same way `space_ptt_signal` and
// `ctrl_space_toggle_signal` in `gui/mod.rs` are.

/// Build the command for the master voice-enable checkbox.
pub(super) fn voice_enabled_command(enabled: bool) -> VoiceCommand {
    VoiceCommand::SetEnabled(enabled)
}

/// Build the command for the speech-to-text checkbox.
pub(super) fn stt_enabled_command(enabled: bool) -> VoiceCommand {
    VoiceCommand::SetSttEnabled(enabled)
}

/// Build the command for the text-to-speech checkbox.
pub(super) fn tts_enabled_command(enabled: bool) -> VoiceCommand {
    VoiceCommand::SetTtsEnabled(enabled)
}

/// Report the value `voice_mode_flag` should hold for a given text-to-speech
/// checkbox state. The flag always matches the checkbox.
pub(super) fn voice_mode_flag_for_tts(tts_enabled: bool) -> bool {
    tts_enabled
}

/// Build the command for the trigger-mode radio pair.
pub(super) fn trigger_mode_command(mode: TriggerMode) -> VoiceCommand {
    VoiceCommand::SetTriggerMode(mode)
}

/// Build the command for the wake-phrase text field.
pub(super) fn wake_phrase_command(phrase: &str) -> VoiceCommand {
    VoiceCommand::SetWakePhrase(phrase.to_string())
}

/// Build the command for the Kokoro voice id selector.
pub(super) fn voice_id_command(voice_id: &str) -> VoiceCommand {
    VoiceCommand::SetVoice(voice_id.to_string())
}

/// Build the command for the speech speed slider.
pub(super) fn speed_command(speed: f32) -> VoiceCommand {
    VoiceCommand::SetSpeed(speed)
}

// ── Background model discovery ──
//
// `list_models` is async. The paint loop must never block on it. The call
// below spawns one fetch and returns right away. A plain `#[test]` has no
// Tokio runtime. Spawning without one panics, so the spawn is skipped
// there instead.

/// Fetch `cfg`'s model list in the background.
///
/// Sends it tagged with `backend_name`. That lets a result be told apart
/// from one resolved for a backend the user has since switched away from.
pub(super) fn spawn_model_list_fetch(
    tx: mpsc::UnboundedSender<(String, Vec<String>)>,
    backend_name: String,
    cfg: BackendConfig,
) {
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    tokio::spawn(async move {
        let models = list_models(&cfg).await;
        let _ = tx.send((backend_name, models));
    });
}

// ── Control-to-settings mapping ──
//
// Each function below takes the plain value a settings-panel control just
// changed to and writes it into a `Settings`. Kept separate from the
// control's egui code so each write is unit-testable without an egui
// context, the same way the command builders above are. The voice writer
// creates its config block when it is missing, so a change is never
// silently dropped.

/// Store the backend picker's selection.
pub(super) fn apply_default_backend(settings: &mut Settings, name: &str) {
    settings.default_backend = Some(name.to_string());
}

/// Store the model dropdown's selection onto the named backend's entry.
///
/// A name that does not resolve to any backend is a no-op. A stale
/// selection must never write to the wrong entry or panic.
pub(super) fn apply_backend_model(settings: &mut Settings, backend_name: &str, model: &str) {
    let Some(backends) = settings.backends.as_mut() else {
        return;
    };
    let Some(entry) = backends.get_mut(backend_name) else {
        return;
    };
    match entry {
        BackendConfig::Api { model: m, .. } => *m = model.to_string(),
        BackendConfig::ClaudeCli { model: m, .. } => *m = model.to_string(),
    }
}

/// Store the effort combo box's selection.
pub(super) fn apply_effort(settings: &mut Settings, effort: Effort) {
    settings.effort = Some(effort);
}

/// Store the raw-output checkbox's value.
pub(super) fn apply_show_raw_output(settings: &mut Settings, show_raw: bool) {
    settings.show_raw_output = Some(show_raw);
}

/// Store the master voice checkbox's value.
pub(super) fn apply_voice_enabled(settings: &mut Settings, enabled: bool) {
    settings.voice_mut().enabled = enabled;
}

/// Store the speech-to-text checkbox's value.
pub(super) fn apply_stt_enabled(settings: &mut Settings, enabled: bool) {
    settings.voice_mut().stt_enabled = enabled;
}

/// Store the text-to-speech checkbox's value.
pub(super) fn apply_tts_enabled(settings: &mut Settings, enabled: bool) {
    settings.voice_mut().tts_enabled = enabled;
}

/// Store the trigger-mode radio pair's selection.
pub(super) fn apply_trigger_mode(settings: &mut Settings, mode: TriggerMode) {
    settings.voice_mut().trigger_mode = mode;
}

/// Store the wake-phrase field's text.
pub(super) fn apply_wake_phrase(settings: &mut Settings, phrase: &str) {
    settings.voice_mut().wake_phrase = Some(phrase.to_string());
}

/// Store the Kokoro voice selector's choice.
pub(super) fn apply_tts_voice(settings: &mut Settings, voice_id: &str) {
    settings.voice_mut().tts_voice = Some(voice_id.to_string());
}

/// Store the speech speed slider's value.
pub(super) fn apply_tts_speed(settings: &mut Settings, speed: f32) {
    settings.voice_mut().tts_speed = Some(speed);
}

/// Store the context budget slider's value.
pub(super) fn apply_context_budget(settings: &mut Settings, budget: usize) {
    settings.context_budget = Some(budget);
}

/// Store the working-directory sidebar field's value.
pub(super) fn apply_working_dir(settings: &mut Settings, dir: &str) {
    settings.working_dir = Some(dir.to_string());
}
