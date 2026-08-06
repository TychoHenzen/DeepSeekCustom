//! Everything the GUI holds because the harness can listen and speak.
//!
//! This owns the two voice channels, the state the status bar reads, the
//! eight sidebar controls, and the reply text waiting to be spoken. It was
//! split out of `DeepSeekGui`, which held those twelve fields alongside
//! thirty others.
//!
//! The seam is one-way on purpose. This type never reaches back into the
//! GUI: an event that has to start a turn comes back as a return value
//! from `handle_event`, and the GUI decides what to do with it. That keeps
//! the submission path single, which is what makes a spoken turn and a
//! typed turn indistinguishable to the agent.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, RichText, TextEdit};
use tokio::sync::mpsc;
use tracing::{error, info};

use super::transcript::{BlockKind, Severity, Transcript};
use crate::config::settings::{Settings, TriggerMode};
use crate::voice::service::{VoiceCommand, VoiceEvent, VoiceState};

/// Kokoro voice ids offered by the settings panel's voice selector. A
/// fixed list, not a read of `voices/` at startup. This keeps the panel's
/// options stable and testable no matter what is unpacked on disk.
pub const KOKORO_VOICE_IDS: &[&str] = &[
    "af_heart",
    "af_bella",
    "af_nicole",
    "am_michael",
    "am_puck",
    "bf_emma",
    "bm_george",
];

/// What a push-to-talk key binding decided to do this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PttSignal {
    Start,
    Stop,
}

/// The voice channels, the sidebar's voice controls, and the reply text
/// waiting to be spoken.
///
/// Both channels are `None` when voice is switched off or a model file is
/// missing. Every send goes through `send`. That method drops the command
/// in this case, so no caller has to check first.
pub struct VoiceUi {
    rx: Option<mpsc::UnboundedReceiver<VoiceEvent>>,
    tx: Option<mpsc::UnboundedSender<VoiceCommand>>,
    state: VoiceState,

    // ── Sidebar controls ──
    master_enabled: bool,
    stt_enabled: bool,
    tts_enabled: bool,
    trigger_mode: TriggerMode,
    wake_phrase: String,
    voice_id_options: Vec<String>,
    selected_voice_idx: usize,
    speed: f32,

    /// The model's reply text accumulated since the last `TurnEnd`, so
    /// exactly one `Speak` goes out per turn instead of one per chunk.
    /// Holds only `StreamEvent::Text` payloads, never reasoning or tool
    /// output.
    reply_buffer: String,
}

impl VoiceUi {
    /// Seed every control from `settings`, with no channels attached yet.
    ///
    /// This also writes `voice_mode_flag`, because voice reply mode
    /// follows the text-to-speech control rather than a setting of its
    /// own. Doing it here keeps the flag correct from the first turn,
    /// before the user has touched anything.
    pub fn new(settings: &Settings, voice_mode_flag: &Arc<AtomicBool>) -> Self {
        let tts_enabled = settings.voice_tts_enabled();
        voice_mode_flag.store(voice_mode_flag_for_tts(tts_enabled), Ordering::SeqCst);

        let voice_id_options: Vec<String> =
            KOKORO_VOICE_IDS.iter().map(|s| s.to_string()).collect();
        let configured_voice = settings.voice_tts_voice();
        // An unknown id falls back to the first voice rather than
        // failing. A settings file may name a voice this build does not
        // offer. That must not stop the panel from rendering.
        let selected_voice_idx = voice_id_options
            .iter()
            .position(|v| *v == configured_voice)
            .unwrap_or(0);

        Self {
            rx: None,
            tx: None,
            state: VoiceState::Idle,
            master_enabled: settings.voice_enabled(),
            stt_enabled: settings.voice_stt_enabled(),
            tts_enabled,
            trigger_mode: settings.voice_trigger_mode(),
            wake_phrase: settings.voice_wake_phrase(),
            voice_id_options,
            selected_voice_idx,
            speed: settings.voice_tts_speed(),
            reply_buffer: String::new(),
        }
    }

    /// Attach a running voice service's two channels.
    pub fn attach(
        &mut self,
        rx: mpsc::UnboundedReceiver<VoiceEvent>,
        tx: mpsc::UnboundedSender<VoiceCommand>,
    ) {
        self.rx = Some(rx);
        self.tx = Some(tx);
    }

    /// True once both channels are attached. Production code never asks:
    /// `send` and `drain_events` already do the right thing without a
    /// channel, which is the point of routing every use through them.
    #[cfg(feature = "test-support")]
    pub fn is_attached(&self) -> bool {
        self.rx.is_some() && self.tx.is_some()
    }

    /// Read the text-to-speech control. Test-only: production code reads
    /// it through `speak_accumulated_reply`, which is the only behavior
    /// that depends on it.
    #[cfg(feature = "test-support")]
    pub fn tts_enabled_for_test(&self) -> bool {
        self.tts_enabled
    }

    /// Set the text-to-speech control without going through the sidebar,
    /// so a test can reach the speaking path without an egui context.
    #[cfg(feature = "test-support")]
    pub fn set_tts_enabled_for_test(&mut self, enabled: bool) {
        self.tts_enabled = enabled;
    }

    /// The reply text accumulated so far, for tests that check what a turn
    /// would speak without draining the buffer.
    #[cfg(feature = "test-support")]
    pub fn reply_buffer_for_test(&self) -> &str {
        &self.reply_buffer
    }

    /// The seven remaining sidebar controls, as `VoiceUi::new` seeded
    /// them. Test-only, and gated one accessor per control rather than
    /// exposed as fields: `render_section` is the only production reader,
    /// and it reads each one straight into an egui widget that writes the
    /// value back in the same statement. There is nothing in between to
    /// make a seam out of, and a live `egui::Ui` is the one thing a test
    /// without a window cannot build.
    #[cfg(feature = "test-support")]
    pub fn master_enabled_for_test(&self) -> bool {
        self.master_enabled
    }

    /// The speech-to-text control. See [`VoiceUi::master_enabled_for_test`].
    #[cfg(feature = "test-support")]
    pub fn stt_enabled_for_test(&self) -> bool {
        self.stt_enabled
    }

    /// The trigger mode radio choice. See
    /// [`VoiceUi::master_enabled_for_test`].
    #[cfg(feature = "test-support")]
    pub fn trigger_mode_for_test(&self) -> TriggerMode {
        self.trigger_mode
    }

    /// The wake phrase field's text. See
    /// [`VoiceUi::master_enabled_for_test`].
    #[cfg(feature = "test-support")]
    pub fn wake_phrase_for_test(&self) -> &str {
        &self.wake_phrase
    }

    /// The Kokoro voice dropdown's options. See
    /// [`VoiceUi::master_enabled_for_test`].
    #[cfg(feature = "test-support")]
    pub fn voice_id_options_for_test(&self) -> &[String] {
        &self.voice_id_options
    }

    /// The Kokoro voice dropdown's selected index. See
    /// [`VoiceUi::master_enabled_for_test`].
    #[cfg(feature = "test-support")]
    pub fn selected_voice_idx_for_test(&self) -> usize {
        self.selected_voice_idx
    }

    /// The speed slider's value. See
    /// [`VoiceUi::master_enabled_for_test`].
    #[cfg(feature = "test-support")]
    pub fn speed_for_test(&self) -> f32 {
        self.speed
    }

    /// The state the status bar reads.
    pub fn state(&self) -> VoiceState {
        self.state
    }

    /// Send a command if the voice subsystem is attached. Silently does
    /// nothing when voice is disabled.
    pub fn send(&self, cmd: VoiceCommand) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(cmd);
        }
    }

    /// Take every voice event queued since the last frame.
    ///
    /// The events come back as a batch rather than being handled inline.
    /// That ends the mutable borrow of the receiver before the caller
    /// starts applying them. Handling one can start a turn, and a turn
    /// needs the rest of the GUI.
    pub fn drain_events(&mut self) -> Vec<VoiceEvent> {
        let mut events = Vec::new();
        if let Some(rx) = self.rx.as_mut() {
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
        }
        events
    }

    /// Apply one voice event. Returns the text of a finished transcript,
    /// which the caller submits as a turn, and `None` for every other
    /// event.
    pub fn handle_event(
        &mut self,
        event: VoiceEvent,
        transcript: &mut Transcript,
    ) -> Option<String> {
        match event {
            VoiceEvent::StateChanged(state) => {
                self.state = state;
                None
            }
            VoiceEvent::Error(message) => {
                error!(%message, "voice error event");
                transcript.push(BlockKind::Notice {
                    text: format!("ERROR: {message}"),
                    severity: Severity::Error,
                });
                None
            }
            // The caller routes this through the input buffer and the
            // same submit path Enter uses. The agent then sees a spoken
            // turn exactly as if it had been typed.
            VoiceEvent::Transcript(text) => Some(text),
            VoiceEvent::WakeDetected => {
                // Reacting to wake detection beyond state tracking is not
                // this step's job.
                None
            }
        }
    }

    /// Accumulate one text delta for the reply that will be spoken when
    /// the turn ends.
    pub fn push_reply_text(&mut self, text: &str) {
        self.reply_buffer.push_str(text);
    }

    /// Drop the accumulated reply without speaking it.
    pub fn clear_reply(&mut self) {
        self.reply_buffer.clear();
    }

    /// Speak the reply text accumulated since the last turn ended, if
    /// text to speech is on. Runs the markdown-to-speech filter first, so
    /// code fences, backticks, and URLs never reach the speaker. Always
    /// clears the buffer, spoken or not, so a later turn never inherits
    /// this one's text.
    pub fn speak_accumulated_reply(&mut self) {
        let reply = std::mem::take(&mut self.reply_buffer);
        if !self.tts_enabled {
            return;
        }
        let spoken = crate::voice::filter_for_speech(&reply);
        if spoken.is_empty() {
            return;
        }
        self.send(VoiceCommand::Speak(spoken));
    }

    /// Apply both push-to-talk bindings for this frame.
    ///
    /// `settings_visible` closes off both, since the sidebar owns the
    /// keyboard while it is open.
    pub fn handle_ptt(&self, keys: PttKeys, settings_visible: bool) {
        // Space held: only while not typing, since space types a
        // character when the input box has focus.
        if let Some(signal) = space_ptt_signal(
            keys.space_pressed,
            keys.space_released,
            keys.ctrl_held,
            keys.input_focused,
            settings_visible,
        ) {
            self.send(ptt_command(signal));
        }

        // Ctrl+Space: works even while typing, since it is not a
        // printable character.
        if let Some(signal) = ctrl_space_toggle_signal(
            keys.ctrl_space_pressed,
            settings_visible,
            self.state == VoiceState::Listening,
        ) {
            self.send(ptt_command(signal));
        }
    }

    /// The sidebar's whole Voice section. Returns true when a control
    /// changed something the settings file holds, so the caller saves it
    /// once per frame instead of once per control.
    pub fn render_section(
        &mut self,
        ui: &mut egui::Ui,
        settings: &mut Settings,
        voice_mode_flag: &Arc<AtomicBool>,
    ) -> bool {
        ui.label(RichText::new("Voice").color(Color32::from_rgb(180, 220, 255)));
        let mut dirty = false;

        let mut master_enabled = self.master_enabled;
        if ui.checkbox(&mut master_enabled, "Voice enabled").changed() {
            self.master_enabled = master_enabled;
            self.send(voice_enabled_command(master_enabled));
            info!(
                voice_enabled = master_enabled,
                "voice enabled toggled via settings panel"
            );
            apply_voice_enabled(settings, master_enabled);
            dirty = true;
        }

        let mut stt_enabled = self.stt_enabled;
        if ui.checkbox(&mut stt_enabled, "Speech to text").changed() {
            self.stt_enabled = stt_enabled;
            self.send(stt_enabled_command(stt_enabled));
            info!(stt_enabled, "speech-to-text toggled via settings panel");
            apply_stt_enabled(settings, stt_enabled);
            dirty = true;
        }

        let mut tts_enabled = self.tts_enabled;
        if ui.checkbox(&mut tts_enabled, "Text to speech").changed() {
            self.tts_enabled = tts_enabled;
            voice_mode_flag.store(voice_mode_flag_for_tts(tts_enabled), Ordering::SeqCst);
            self.send(tts_enabled_command(tts_enabled));
            info!(tts_enabled, "text-to-speech toggled via settings panel");
            apply_tts_enabled(settings, tts_enabled);
            dirty = true;
        }

        ui.add_space(4.0);
        ui.label(RichText::new("Trigger mode").color(Color32::GRAY).small());
        let prev_trigger_mode = self.trigger_mode;
        ui.horizontal(|ui| {
            ui.radio_value(
                &mut self.trigger_mode,
                TriggerMode::PushToTalk,
                "Push to talk",
            );
            ui.radio_value(&mut self.trigger_mode, TriggerMode::WakeWord, "Wake word");
        });
        if self.trigger_mode != prev_trigger_mode {
            self.send(trigger_mode_command(self.trigger_mode));
            info!(
                mode = ?self.trigger_mode,
                "voice trigger mode changed via settings panel"
            );
            apply_trigger_mode(settings, self.trigger_mode);
            dirty = true;
        }

        ui.add_space(4.0);
        let wake_response =
            ui.add(TextEdit::singleline(&mut self.wake_phrase).hint_text("wake phrase"));
        if wake_response.changed() {
            self.send(wake_phrase_command(&self.wake_phrase));
            info!(
                phrase = %self.wake_phrase,
                "wake phrase changed via settings panel"
            );
        }
        // Save on focus loss, not on every keystroke, so typing a phrase
        // writes the file once.
        if wake_response.lost_focus() {
            apply_wake_phrase(settings, &self.wake_phrase.clone());
            dirty = true;
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
            self.send(voice_id_command(&voice_id));
            info!(voice_id = %voice_id, "kokoro voice changed via settings panel");
            apply_tts_voice(settings, &voice_id);
            dirty = true;
        }

        ui.add_space(4.0);
        let speed_response = ui.add(egui::Slider::new(&mut self.speed, 0.5..=2.0).text("Speed"));
        if speed_response.changed() {
            self.send(speed_command(self.speed));
            info!(speed = self.speed, "voice speed changed via settings panel");
        }
        // Save when the drag ends, so one drag writes the file once
        // instead of once per frame.
        if speed_response.drag_stopped() {
            apply_tts_speed(settings, self.speed);
            dirty = true;
        }

        dirty
    }
}

/// The key state both push-to-talk bindings read, gathered once per frame.
/// Grouped because passing six loose booleans in a fixed order invites a
/// silent swap at the call site.
#[derive(Debug, Clone, Copy, Default)]
pub struct PttKeys {
    pub space_pressed: bool,
    pub space_released: bool,
    pub ctrl_held: bool,
    pub ctrl_space_pressed: bool,
    pub input_focused: bool,
}

/// The command a push-to-talk decision sends.
fn ptt_command(signal: PttSignal) -> VoiceCommand {
    match signal {
        PttSignal::Start => VoiceCommand::StartListening,
        PttSignal::Stop => VoiceCommand::StopListening,
    }
}

/// Decide the push-to-talk action for the space-bar-held binding. Space
/// types a space character when the text input has focus, so this binding
/// only ever fires when the input does NOT have focus. Also suppressed
/// while Ctrl is held (that is the separate Ctrl+Space toggle binding) or
/// the settings panel is open. `Start` fires on press, `Stop` fires on
/// release, matching "held" rather than "toggled".
pub fn space_ptt_signal(
    space_pressed: bool,
    space_released: bool,
    ctrl_held: bool,
    input_focused: bool,
    settings_visible: bool,
) -> Option<PttSignal> {
    if ctrl_held || input_focused || settings_visible {
        return None;
    }
    if space_pressed {
        Some(PttSignal::Start)
    } else if space_released {
        Some(PttSignal::Stop)
    } else {
        None
    }
}

/// Decide the push-to-talk action for the Ctrl+Space toggle binding.
/// Unlike the space-bar-held binding, this works even when the text input
/// has focus. Ctrl+Space is not a printable character, so it never
/// collides with typing. Still suppressed while the settings panel is
/// open. Toggles off `currently_listening` rather than press/release.
/// This keeps the state coherent: a listening session this binding
/// started is always one more press of the same key away from stopping.
pub fn ctrl_space_toggle_signal(
    ctrl_space_pressed: bool,
    settings_visible: bool,
    currently_listening: bool,
) -> Option<PttSignal> {
    if !ctrl_space_pressed || settings_visible {
        return None;
    }
    Some(if currently_listening {
        PttSignal::Stop
    } else {
        PttSignal::Start
    })
}

/// Short status-bar label for a voice state.
pub fn voice_state_label(state: VoiceState) -> &'static str {
    match state {
        VoiceState::Idle => "Idle",
        VoiceState::Listening => "Listening",
        VoiceState::Transcribing => "Transcribing",
        VoiceState::Speaking => "Speaking",
    }
}

/// Status-bar color for a voice state. See [`voice_state_label`].
pub fn voice_state_color(state: VoiceState) -> Color32 {
    match state {
        VoiceState::Idle => Color32::from_rgb(128, 128, 128),
        VoiceState::Listening => Color32::from_rgb(0, 200, 0),
        VoiceState::Transcribing => Color32::from_rgb(255, 255, 0),
        VoiceState::Speaking => Color32::from_rgb(100, 149, 237),
    }
}

/// Build the command for the master voice-enable checkbox.
pub fn voice_enabled_command(enabled: bool) -> VoiceCommand {
    VoiceCommand::SetEnabled(enabled)
}

/// Build the command for the speech-to-text checkbox.
pub fn stt_enabled_command(enabled: bool) -> VoiceCommand {
    VoiceCommand::SetSttEnabled(enabled)
}

/// Build the command for the text-to-speech checkbox.
pub fn tts_enabled_command(enabled: bool) -> VoiceCommand {
    VoiceCommand::SetTtsEnabled(enabled)
}

/// Report the value `voice_mode_flag` should hold for a given
/// text-to-speech state. Voice reply mode follows text to speech rather
/// than carrying a setting of its own.
pub fn voice_mode_flag_for_tts(tts_enabled: bool) -> bool {
    tts_enabled
}

/// Build the command for the trigger mode radio buttons.
pub fn trigger_mode_command(mode: TriggerMode) -> VoiceCommand {
    VoiceCommand::SetTriggerMode(mode)
}

/// Build the command for the wake phrase text field.
pub fn wake_phrase_command(phrase: &str) -> VoiceCommand {
    VoiceCommand::SetWakePhrase(phrase.to_string())
}

/// Build the command for the Kokoro voice id selector.
pub fn voice_id_command(voice_id: &str) -> VoiceCommand {
    VoiceCommand::SetVoice(voice_id.to_string())
}

/// Build the command for the speed slider.
pub fn speed_command(speed: f32) -> VoiceCommand {
    VoiceCommand::SetSpeed(speed)
}

/// Store the master voice checkbox's value.
pub fn apply_voice_enabled(settings: &mut Settings, enabled: bool) {
    settings.voice_mut().enabled = enabled;
}

/// Store the speech-to-text checkbox's value.
pub fn apply_stt_enabled(settings: &mut Settings, enabled: bool) {
    settings.voice_mut().stt_enabled = enabled;
}

/// Store the text-to-speech checkbox's value.
pub fn apply_tts_enabled(settings: &mut Settings, enabled: bool) {
    settings.voice_mut().tts_enabled = enabled;
}

/// Store the trigger mode radio buttons' choice.
pub fn apply_trigger_mode(settings: &mut Settings, mode: TriggerMode) {
    settings.voice_mut().trigger_mode = mode;
}

/// Store the wake phrase field's text.
pub fn apply_wake_phrase(settings: &mut Settings, phrase: &str) {
    settings.voice_mut().wake_phrase = Some(phrase.to_string());
}

/// Store the Kokoro voice selector's choice.
pub fn apply_tts_voice(settings: &mut Settings, voice_id: &str) {
    settings.voice_mut().tts_voice = Some(voice_id.to_string());
}

/// Store the speed slider's value.
pub fn apply_tts_speed(settings: &mut Settings, speed: f32) {
    settings.voice_mut().tts_speed = Some(speed);
}
