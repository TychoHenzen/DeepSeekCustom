//! Unit tests for `deepseek_custom::gui::voice_ui` (`src/gui/voice_ui.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use deepseek_custom::config::settings::{Settings, TriggerMode, VoiceConfig};
use deepseek_custom::gui::transcript::{BlockKind, Severity, Transcript};
use deepseek_custom::gui::voice_ui::{
    KOKORO_VOICE_IDS, PttKeys, PttSignal, VoiceUi, apply_stt_enabled, apply_trigger_mode,
    apply_tts_enabled, apply_tts_speed, apply_tts_voice, apply_voice_enabled, apply_wake_phrase,
    ctrl_space_toggle_signal, space_ptt_signal, speed_command, stt_enabled_command,
    trigger_mode_command, tts_enabled_command, voice_enabled_command, voice_id_command,
    voice_mode_flag_for_tts, voice_state_color, voice_state_label, wake_phrase_command,
};
use deepseek_custom::voice::service::{VoiceCommand, VoiceEvent, VoiceState};

use tokio::sync::mpsc;

fn settings_with_voice(tts_voice: &str) -> Settings {
    let mut settings = Settings::default();
    settings.voice = Some(VoiceConfig {
        tts_voice: Some(tts_voice.to_string()),
        ..VoiceConfig::default()
    });
    settings
}

fn make_voice_ui() -> VoiceUi {
    VoiceUi::new(&Settings::default(), &Arc::new(AtomicBool::new(false)))
}

#[test]
fn a_new_voice_ui_has_no_channels_and_starts_idle() {
    let voice = make_voice_ui();
    assert!(!voice.is_attached());
    assert_eq!(voice.state(), VoiceState::Idle);
}

#[test]
fn attach_wires_both_channels() {
    let mut voice = make_voice_ui();
    let (_tx_events, rx) = mpsc::unbounded_channel::<VoiceEvent>();
    let (tx, _rx_cmd) = mpsc::unbounded_channel::<VoiceCommand>();
    voice.attach(rx, tx);
    assert!(voice.is_attached());
}

#[test]
fn new_falls_back_to_the_first_voice_on_an_unknown_voice_id() {
    let voice = VoiceUi::new(
        &settings_with_voice("not_a_real_voice"),
        &Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(voice.selected_voice_idx_for_test(), 0);
}

#[test]
fn new_seeds_every_control_from_settings() {
    let mut settings = Settings::default();
    settings.voice = Some(VoiceConfig {
        enabled: true,
        stt_enabled: true,
        tts_enabled: true,
        trigger_mode: TriggerMode::WakeWord,
        wake_phrase: Some("hey computer".into()),
        tts_voice: Some("am_michael".into()),
        tts_speed: Some(1.4),
        ..VoiceConfig::default()
    });
    let voice = VoiceUi::new(&settings, &Arc::new(AtomicBool::new(false)));
    assert!(voice.master_enabled_for_test());
    assert!(voice.stt_enabled_for_test());
    assert!(voice.tts_enabled_for_test());
    assert_eq!(voice.trigger_mode_for_test(), TriggerMode::WakeWord);
    assert_eq!(voice.wake_phrase_for_test(), "hey computer");
    assert_eq!(
        voice.voice_id_options_for_test()[voice.selected_voice_idx_for_test()],
        "am_michael"
    );
    assert_eq!(voice.speed_for_test(), 1.4);
}

#[test]
fn new_selects_a_known_voice_id() {
    let voice = VoiceUi::new(
        &settings_with_voice("bf_emma"),
        &Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(
        voice.voice_id_options_for_test()[voice.selected_voice_idx_for_test()],
        "bf_emma"
    );
}

#[test]
fn new_sets_the_voice_mode_flag_from_the_seeded_tts_value() {
    let mut settings = Settings::default();
    // `voice_tts_enabled` is gated on the master switch too, so text to
    // speech alone does not turn voice reply mode on.
    settings.voice = Some(VoiceConfig {
        enabled: true,
        tts_enabled: true,
        ..VoiceConfig::default()
    });
    let flag = Arc::new(AtomicBool::new(false));
    let _voice = VoiceUi::new(&settings, &flag);
    assert!(flag.load(Ordering::SeqCst));
}

#[test]
fn a_state_changed_event_updates_the_tracked_state() {
    let mut voice = make_voice_ui();
    let mut transcript = Transcript::default();
    let submit = voice.handle_event(
        VoiceEvent::StateChanged(VoiceState::Listening),
        &mut transcript,
    );
    assert!(submit.is_none());
    assert_eq!(voice.state(), VoiceState::Listening);
}

#[test]
fn an_error_event_posts_a_notice_and_submits_nothing() {
    let mut voice = make_voice_ui();
    let mut transcript = Transcript::default();
    let submit = voice.handle_event(VoiceEvent::Error("mic unplugged".into()), &mut transcript);
    assert!(submit.is_none());
    let posted = transcript
        .blocks()
        .iter()
        .filter(|b| {
            matches!(
                &b.kind,
                BlockKind::Notice {
                    severity: Severity::Error,
                    text,
                } if text.contains("mic unplugged")
            )
        })
        .count();
    assert_eq!(posted, 1);
}

#[test]
fn a_transcript_event_hands_back_the_text_to_submit() {
    let mut voice = make_voice_ui();
    let mut transcript = Transcript::default();
    let submit = voice.handle_event(
        VoiceEvent::Transcript("hello there".into()),
        &mut transcript,
    );
    assert_eq!(submit.as_deref(), Some("hello there"));
    assert!(
        transcript.blocks().is_empty(),
        "the caller submits the turn, so nothing is posted here"
    );
}

#[test]
fn a_wake_event_changes_nothing() {
    let mut voice = make_voice_ui();
    let mut transcript = Transcript::default();
    let submit = voice.handle_event(VoiceEvent::WakeDetected, &mut transcript);
    assert!(submit.is_none());
    assert!(transcript.blocks().is_empty());
}

#[test]
fn drain_events_yields_nothing_without_a_channel() {
    assert!(make_voice_ui().drain_events().is_empty());
}

#[test]
fn drain_events_takes_every_queued_event() {
    let mut voice = make_voice_ui();
    let (tx_events, rx) = mpsc::unbounded_channel::<VoiceEvent>();
    let (tx, _rx_cmd) = mpsc::unbounded_channel::<VoiceCommand>();
    voice.attach(rx, tx);
    tx_events.send(VoiceEvent::WakeDetected).expect("send");
    tx_events
        .send(VoiceEvent::StateChanged(VoiceState::Listening))
        .expect("send");
    assert_eq!(voice.drain_events().len(), 2);
    assert!(voice.drain_events().is_empty(), "a drain empties the queue");
}

#[test]
fn speaking_a_reply_sends_one_speak_command_and_clears_the_buffer() {
    let mut voice = make_voice_ui();
    let (_tx_events, rx) = mpsc::unbounded_channel::<VoiceEvent>();
    let (tx, mut rx_cmd) = mpsc::unbounded_channel::<VoiceCommand>();
    voice.attach(rx, tx);
    voice.set_tts_enabled_for_test(true);
    voice.push_reply_text("hello ");
    voice.push_reply_text("world");
    voice.speak_accumulated_reply();
    assert!(matches!(
        rx_cmd.try_recv(),
        Ok(VoiceCommand::Speak(text)) if text.contains("hello world")
    ));
    voice.speak_accumulated_reply();
    assert!(
        rx_cmd.try_recv().is_err(),
        "the buffer must not survive into a later turn"
    );
}

#[test]
fn a_reply_is_not_spoken_while_text_to_speech_is_off() {
    let mut voice = make_voice_ui();
    let (_tx_events, rx) = mpsc::unbounded_channel::<VoiceEvent>();
    let (tx, mut rx_cmd) = mpsc::unbounded_channel::<VoiceCommand>();
    voice.attach(rx, tx);
    voice.set_tts_enabled_for_test(false);
    voice.push_reply_text("hello world");
    voice.speak_accumulated_reply();
    assert!(rx_cmd.try_recv().is_err());
}

#[test]
fn clear_reply_drops_the_accumulated_text() {
    let mut voice = make_voice_ui();
    let (_tx_events, rx) = mpsc::unbounded_channel::<VoiceEvent>();
    let (tx, mut rx_cmd) = mpsc::unbounded_channel::<VoiceCommand>();
    voice.attach(rx, tx);
    voice.set_tts_enabled_for_test(true);
    voice.push_reply_text("dropped text");
    voice.clear_reply();
    voice.speak_accumulated_reply();
    assert!(rx_cmd.try_recv().is_err());
}

#[test]
fn sending_without_a_channel_is_harmless() {
    make_voice_ui().send(VoiceCommand::StopSpeaking);
}

#[test]
fn space_ptt_starts_listening_on_press_when_input_not_focused() {
    assert_eq!(
        space_ptt_signal(true, false, false, false, false),
        Some(PttSignal::Start)
    );
}

#[test]
fn space_ptt_stops_listening_on_release() {
    assert_eq!(
        space_ptt_signal(false, true, false, false, false),
        Some(PttSignal::Stop)
    );
}

#[test]
fn space_ptt_suppressed_while_the_input_has_focus() {
    assert_eq!(space_ptt_signal(true, false, false, true, false), None);
}

#[test]
fn space_ptt_suppressed_while_the_settings_panel_is_open() {
    assert_eq!(space_ptt_signal(true, false, false, false, true), None);
}

#[test]
fn space_ptt_suppressed_while_ctrl_is_held() {
    assert_eq!(space_ptt_signal(true, false, true, false, false), None);
}

#[test]
fn ctrl_space_toggles_listening_on() {
    assert_eq!(
        ctrl_space_toggle_signal(true, false, false),
        Some(PttSignal::Start)
    );
}

#[test]
fn ctrl_space_toggles_listening_off() {
    assert_eq!(
        ctrl_space_toggle_signal(true, false, true),
        Some(PttSignal::Stop)
    );
}

#[test]
fn ctrl_space_suppressed_while_the_settings_panel_is_open() {
    assert_eq!(ctrl_space_toggle_signal(true, true, false), None);
}

#[test]
fn ctrl_space_does_nothing_without_a_press() {
    assert_eq!(ctrl_space_toggle_signal(false, false, false), None);
}

#[test]
fn handle_ptt_sends_start_on_a_space_press() {
    let mut voice = make_voice_ui();
    let (_tx_events, rx) = mpsc::unbounded_channel::<VoiceEvent>();
    let (tx, mut rx_cmd) = mpsc::unbounded_channel::<VoiceCommand>();
    voice.attach(rx, tx);
    voice.handle_ptt(
        PttKeys {
            space_pressed: true,
            ..PttKeys::default()
        },
        false,
    );
    assert!(matches!(
        rx_cmd.try_recv(),
        Ok(VoiceCommand::StartListening)
    ));
}

#[test]
fn handle_ptt_sends_nothing_while_the_settings_panel_is_open() {
    let mut voice = make_voice_ui();
    let (_tx_events, rx) = mpsc::unbounded_channel::<VoiceEvent>();
    let (tx, mut rx_cmd) = mpsc::unbounded_channel::<VoiceCommand>();
    voice.attach(rx, tx);
    voice.handle_ptt(
        PttKeys {
            space_pressed: true,
            ctrl_space_pressed: true,
            ..PttKeys::default()
        },
        true,
    );
    assert!(rx_cmd.try_recv().is_err());
}

#[test]
fn ctrl_space_takes_no_input_focused_parameter_and_always_fires() {
    // This binding must work even while the text input has focus, so
    // its signature has no focus parameter to gate on in the first
    // place. This test only confirms it fires given ctrl+space pressed.
    assert_eq!(
        ctrl_space_toggle_signal(true, false, false),
        Some(PttSignal::Start)
    );
}

#[test]
fn a_new_voice_ui_has_sensible_control_defaults() {
    let voice = make_voice_ui();
    assert!(!voice.master_enabled_for_test());
    assert!(!voice.stt_enabled_for_test());
    assert!(!voice.tts_enabled_for_test());
    assert_eq!(voice.trigger_mode_for_test(), TriggerMode::PushToTalk);
    assert_eq!(voice.wake_phrase_for_test(), "hey deepseek");
    assert_eq!(voice.selected_voice_idx_for_test(), 0);
    assert_eq!(voice.speed_for_test(), 1.0);
    assert_eq!(
        voice.voice_id_options_for_test().len(),
        KOKORO_VOICE_IDS.len()
    );
    assert_eq!(voice.voice_id_options_for_test()[0], "af_heart");
}

#[test]
fn voice_state_label_text_for_each_state() {
    assert_eq!(voice_state_label(VoiceState::Idle), "Idle");
    assert_eq!(voice_state_label(VoiceState::Listening), "Listening");
    assert_eq!(voice_state_label(VoiceState::Transcribing), "Transcribing");
    assert_eq!(voice_state_label(VoiceState::Speaking), "Speaking");
}

#[test]
fn voice_state_color_for_each_state() {
    let colors = [
        voice_state_color(VoiceState::Idle),
        voice_state_color(VoiceState::Listening),
        voice_state_color(VoiceState::Transcribing),
        voice_state_color(VoiceState::Speaking),
    ];
    for (i, a) in colors.iter().enumerate() {
        for b in colors.iter().skip(i + 1) {
            assert_ne!(a, b, "each voice state must be told apart by color");
        }
    }
}

#[test]
fn voice_enabled_command_wraps_the_checkbox_value() {
    assert!(matches!(
        voice_enabled_command(true),
        VoiceCommand::SetEnabled(true)
    ));
}

#[test]
fn stt_enabled_command_wraps_the_checkbox_value() {
    assert!(matches!(
        stt_enabled_command(false),
        VoiceCommand::SetSttEnabled(false)
    ));
}

#[test]
fn tts_enabled_command_wraps_the_checkbox_value() {
    assert!(matches!(
        tts_enabled_command(true),
        VoiceCommand::SetTtsEnabled(true)
    ));
}

#[test]
fn voice_mode_flag_for_tts_matches_the_checkbox_state() {
    assert!(voice_mode_flag_for_tts(true));
    assert!(!voice_mode_flag_for_tts(false));
}

#[test]
fn trigger_mode_command_wraps_the_radio_choice() {
    assert!(matches!(
        trigger_mode_command(TriggerMode::WakeWord),
        VoiceCommand::SetTriggerMode(TriggerMode::WakeWord)
    ));
}

#[test]
fn wake_phrase_command_wraps_the_field_text() {
    assert!(matches!(
        wake_phrase_command("hey there"),
        VoiceCommand::SetWakePhrase(phrase) if phrase == "hey there"
    ));
}

#[test]
fn voice_id_command_wraps_the_selected_voice() {
    assert!(matches!(
        voice_id_command("af_bella"),
        VoiceCommand::SetVoice(id) if id == "af_bella"
    ));
}

#[test]
fn speed_command_wraps_the_slider_value() {
    assert!(matches!(speed_command(1.5), VoiceCommand::SetSpeed(s) if s == 1.5));
}

#[test]
fn every_voice_writer_round_trips_through_settings() {
    let mut settings = Settings::default();
    apply_voice_enabled(&mut settings, true);
    apply_stt_enabled(&mut settings, true);
    apply_tts_enabled(&mut settings, true);
    apply_trigger_mode(&mut settings, TriggerMode::WakeWord);
    apply_wake_phrase(&mut settings, "hey harness");
    apply_tts_voice(&mut settings, "am_puck");
    apply_tts_speed(&mut settings, 1.25);
    assert!(settings.voice_enabled());
    assert!(settings.voice_stt_enabled());
    assert!(settings.voice_tts_enabled());
    assert_eq!(settings.voice_trigger_mode(), TriggerMode::WakeWord);
    assert_eq!(settings.voice_wake_phrase(), "hey harness");
    assert_eq!(settings.voice_tts_voice(), "am_puck");
    assert_eq!(settings.voice_tts_speed(), 1.25);
}
