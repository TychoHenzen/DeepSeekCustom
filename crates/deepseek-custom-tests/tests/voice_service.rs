//! Unit tests for `deepseek_custom::voice::service` (`src/voice/service.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::sync::{Arc, Mutex};

use deepseek_custom::config::settings::TriggerMode;
use deepseek_custom::voice::service::{
    CaptureFactory, CaptureSource, Speaker, Transcriber, VoiceCommand, VoiceEvent, VoiceReactor,
    VoiceState,
};

use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

/// A capture source that never touches real hardware. `chunks` are
/// handed out one at a time on each `drain()` call, in order, so a
/// test can script how audio arrives across successive polls.
struct FakeCapture {
    chunks: Arc<Mutex<Vec<Vec<f32>>>>,
    stopped: Arc<Mutex<bool>>,
}

impl CaptureSource for FakeCapture {
    fn stop(&mut self) {
        *self.stopped.lock().unwrap() = true;
    }

    fn drain(&self) -> Vec<f32> {
        self.chunks.lock().unwrap().pop().unwrap_or_default()
    }
}

struct FakeCaptureFactory {
    chunks: Arc<Mutex<Vec<Vec<f32>>>>,
    stopped: Arc<Mutex<bool>>,
    fail: bool,
}

impl FakeCaptureFactory {
    fn new() -> Self {
        Self {
            chunks: Arc::new(Mutex::new(Vec::new())),
            stopped: Arc::new(Mutex::new(false)),
            fail: false,
        }
    }

    fn failing() -> Self {
        Self {
            fail: true,
            ..Self::new()
        }
    }
}

impl CaptureFactory for FakeCaptureFactory {
    fn start(&self) -> Result<Box<dyn CaptureSource>, String> {
        if self.fail {
            return Err("no input device".to_string());
        }
        Ok(Box::new(FakeCapture {
            chunks: Arc::clone(&self.chunks),
            stopped: Arc::clone(&self.stopped),
        }))
    }
}

struct FakeTranscriber {
    result: Result<String, String>,
}

impl Transcriber for FakeTranscriber {
    fn transcribe(&self, _samples: &[f32]) -> Result<String, String> {
        self.result.clone()
    }
}

#[derive(Clone, Default)]
struct FakeSpeaker {
    stopped: Arc<Mutex<bool>>,
    spoken: Arc<Mutex<Vec<String>>>,
    voice_id: Arc<Mutex<Option<String>>>,
    speed: Arc<Mutex<Option<f32>>>,
}

impl Speaker for FakeSpeaker {
    fn speak(&self, text: &str) {
        self.spoken.lock().unwrap().push(text.to_string());
    }

    fn stop(&self) {
        *self.stopped.lock().unwrap() = true;
    }

    fn set_voice(&self, voice_id: &str) {
        *self.voice_id.lock().unwrap() = Some(voice_id.to_string());
    }

    fn set_speed(&self, speed: f32) {
        *self.speed.lock().unwrap() = Some(speed);
    }
}

fn new_reactor(
    transcriber: Option<Box<dyn Transcriber>>,
    speaker: Option<Box<dyn Speaker>>,
) -> (VoiceReactor, UnboundedReceiver<VoiceEvent>) {
    let (tx, rx) = unbounded_channel();
    let reactor = VoiceReactor::new_for_test(
        Box::new(FakeCaptureFactory::new()),
        transcriber,
        speaker,
        TriggerMode::PushToTalk,
        "hey deepseek",
        tx,
    );
    (reactor, rx)
}

fn new_wake_reactor(
    transcriber: Option<Box<dyn Transcriber>>,
    speaker: Option<Box<dyn Speaker>>,
) -> (VoiceReactor, UnboundedReceiver<VoiceEvent>) {
    let (tx, rx) = unbounded_channel();
    let reactor = VoiceReactor::new_for_test(
        Box::new(FakeCaptureFactory::new()),
        transcriber,
        speaker,
        TriggerMode::WakeWord,
        "hey deepseek",
        tx,
    );
    (reactor, rx)
}

fn drain_events(rx: &mut UnboundedReceiver<VoiceEvent>) -> Vec<VoiceEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

#[test]
fn idle_to_listening_to_transcribing_and_back() {
    let transcriber: Box<dyn Transcriber> = Box::new(FakeTranscriber {
        result: Ok("hello world".to_string()),
    });
    let (mut reactor, mut rx) = new_reactor(Some(transcriber), None);

    reactor.handle_command(VoiceCommand::StartListening);
    reactor.handle_command(VoiceCommand::StopListening);

    let events = drain_events(&mut rx);
    assert_eq!(
        events,
        vec![
            VoiceEvent::StateChanged(VoiceState::Listening),
            VoiceEvent::StateChanged(VoiceState::Transcribing),
            VoiceEvent::Transcript("hello world".to_string()),
            VoiceEvent::StateChanged(VoiceState::Idle),
        ]
    );
}

#[test]
fn transcript_event_carries_the_transcribed_text() {
    let transcriber: Box<dyn Transcriber> = Box::new(FakeTranscriber {
        result: Ok("turn on the lights".to_string()),
    });
    let (mut reactor, mut rx) = new_reactor(Some(transcriber), None);

    reactor.handle_command(VoiceCommand::StartListening);
    reactor.handle_command(VoiceCommand::StopListening);

    let events = drain_events(&mut rx);
    assert!(events.contains(&VoiceEvent::Transcript("turn on the lights".to_string())));
}

#[test]
fn vad_ends_utterance_on_its_own_without_stop_listening() {
    let transcriber: Box<dyn Transcriber> = Box::new(FakeTranscriber {
        result: Ok("done".to_string()),
    });
    let (mut reactor, mut rx) = new_reactor(Some(transcriber), None);

    reactor.handle_command(VoiceCommand::StartListening);
    drain_events(&mut rx);

    // 30ms frames: 15 loud frames clears the 300ms minimum utterance,
    // then 30 silent frames clears the 800ms hangover.
    const FRAME: usize = 480;
    let speech: Vec<f32> = (0..15 * FRAME)
        .map(|i| if i % 2 == 0 { 0.3 } else { -0.3 })
        .collect();
    let silence = vec![0.0f32; 30 * FRAME];

    reactor.feed_audio(&speech);
    reactor.feed_audio(&silence);

    let events = drain_events(&mut rx);
    assert_eq!(
        events,
        vec![
            VoiceEvent::StateChanged(VoiceState::Transcribing),
            VoiceEvent::Transcript("done".to_string()),
            VoiceEvent::StateChanged(VoiceState::Idle),
        ]
    );
}

#[test]
fn wake_word_match_emits_wake_detected_then_transcript() {
    let transcriber: Box<dyn Transcriber> = Box::new(FakeTranscriber {
        result: Ok("hey deepseek what is the weather".to_string()),
    });
    let (mut reactor, mut rx) = new_wake_reactor(Some(transcriber), None);

    reactor.handle_command(VoiceCommand::StartListening);
    drain_events(&mut rx);

    // Same 30ms-frame speech-then-silence shape as
    // `vad_ends_utterance_on_its_own_without_stop_listening`.
    const FRAME: usize = 480;
    let speech: Vec<f32> = (0..15 * FRAME)
        .map(|i| if i % 2 == 0 { 0.3 } else { -0.3 })
        .collect();
    let silence = vec![0.0f32; 30 * FRAME];

    reactor.feed_audio(&speech);
    reactor.feed_audio(&silence);

    let events = drain_events(&mut rx);
    assert_eq!(
        events,
        vec![
            VoiceEvent::StateChanged(VoiceState::Transcribing),
            VoiceEvent::WakeDetected,
            VoiceEvent::Transcript("what is the weather".to_string()),
            VoiceEvent::StateChanged(VoiceState::Listening),
        ]
    );
}

#[test]
fn wake_word_arriving_while_speaking_is_ignored() {
    let speaker = FakeSpeaker::default();
    let transcriber: Box<dyn Transcriber> = Box::new(FakeTranscriber {
        result: Ok("hey deepseek".to_string()),
    });
    let (mut reactor, mut rx) = new_wake_reactor(Some(transcriber), Some(Box::new(speaker)));

    reactor.handle_command(VoiceCommand::Speak("hello".to_string()));
    drain_events(&mut rx);

    // The assistant's own wake-phrase-shaped speech must not wake it
    // back up. `feed_audio` only processes samples while `Listening`.
    // So nothing happens while `Speaking`, no matter what the audio
    // sounds like.
    const FRAME: usize = 480;
    let speech: Vec<f32> = (0..15 * FRAME)
        .map(|i| if i % 2 == 0 { 0.3 } else { -0.3 })
        .collect();
    reactor.feed_audio(&speech);

    let events = drain_events(&mut rx);
    assert!(events.is_empty());
}

#[test]
fn listening_while_speaking_stops_the_speech() {
    let speaker = FakeSpeaker::default();
    let (mut reactor, mut rx) = new_reactor(None, Some(Box::new(speaker.clone())));

    reactor.handle_command(VoiceCommand::Speak("hello".to_string()));
    assert!(!*speaker.stopped.lock().unwrap());

    reactor.handle_command(VoiceCommand::StartListening);

    assert!(*speaker.stopped.lock().unwrap());
    let events = drain_events(&mut rx);
    assert_eq!(
        events,
        vec![
            VoiceEvent::StateChanged(VoiceState::Speaking),
            VoiceEvent::StateChanged(VoiceState::Idle),
            VoiceEvent::StateChanged(VoiceState::Listening),
        ]
    );
}

#[test]
fn empty_transcript_is_not_emitted() {
    let transcriber: Box<dyn Transcriber> = Box::new(FakeTranscriber {
        result: Ok("   ".to_string()),
    });
    let (mut reactor, mut rx) = new_reactor(Some(transcriber), None);

    reactor.handle_command(VoiceCommand::StartListening);
    reactor.handle_command(VoiceCommand::StopListening);

    let events = drain_events(&mut rx);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, VoiceEvent::Transcript(_)))
    );
}

#[test]
fn transcription_error_surfaces_as_error_event_not_a_panic() {
    let transcriber: Box<dyn Transcriber> = Box::new(FakeTranscriber {
        result: Err("model crashed".to_string()),
    });
    let (mut reactor, mut rx) = new_reactor(Some(transcriber), None);

    reactor.handle_command(VoiceCommand::StartListening);
    reactor.handle_command(VoiceCommand::StopListening);

    let events = drain_events(&mut rx);
    assert!(events.contains(&VoiceEvent::Error("model crashed".to_string())));
}

#[test]
fn start_listening_failure_emits_error_and_stays_idle() {
    let (tx, mut rx) = unbounded_channel();
    let mut reactor = VoiceReactor::new_for_test(
        Box::new(FakeCaptureFactory::failing()),
        None,
        None,
        TriggerMode::PushToTalk,
        "hey deepseek",
        tx,
    );

    reactor.handle_command(VoiceCommand::StartListening);

    let events = drain_events(&mut rx);
    assert_eq!(
        events,
        vec![VoiceEvent::Error("no input device".to_string())]
    );
}

#[test]
fn set_trigger_mode_updates_stored_mode_without_emitting_events() {
    let (mut reactor, mut rx) = new_reactor(None, None);
    reactor.handle_command(VoiceCommand::SetTriggerMode(TriggerMode::WakeWord));
    assert_eq!(reactor.trigger_mode_for_test(), TriggerMode::WakeWord);
    assert!(drain_events(&mut rx).is_empty());
}

#[test]
fn speak_without_a_configured_speaker_emits_error() {
    let (mut reactor, mut rx) = new_reactor(None, None);
    reactor.handle_command(VoiceCommand::Speak("hi".to_string()));
    let events = drain_events(&mut rx);
    assert_eq!(
        events,
        vec![VoiceEvent::Error(
            "text-to-speech is unavailable".to_string()
        )]
    );
}

#[test]
fn speak_without_a_configured_speaker_only_warns_once() {
    // Every agent reply now sends a Speak. A machine with no Kokoro
    // model must get one error, not one per reply.
    let (mut reactor, mut rx) = new_reactor(None, None);
    reactor.handle_command(VoiceCommand::Speak("first reply".to_string()));
    drain_events(&mut rx);

    reactor.handle_command(VoiceCommand::Speak("second reply".to_string()));
    reactor.handle_command(VoiceCommand::Speak("third reply".to_string()));

    assert!(drain_events(&mut rx).is_empty());
}

#[test]
fn shutdown_stops_listening_and_speaking_then_returns_false() {
    let speaker = FakeSpeaker::default();
    let (mut reactor, _rx) = new_reactor(None, Some(Box::new(speaker.clone())));

    reactor.handle_command(VoiceCommand::StartListening);
    let should_continue = reactor.handle_command(VoiceCommand::Shutdown);

    assert!(!should_continue);
}

#[test]
fn set_voice_forwards_to_speaker() {
    let speaker = FakeSpeaker::default();
    let (mut reactor, _rx) = new_reactor(None, Some(Box::new(speaker.clone())));

    reactor.handle_command(VoiceCommand::SetVoice("am_michael".to_string()));

    assert_eq!(
        *speaker.voice_id.lock().unwrap(),
        Some("am_michael".to_string())
    );
}

#[test]
fn set_speed_forwards_to_speaker() {
    let speaker = FakeSpeaker::default();
    let (mut reactor, _rx) = new_reactor(None, Some(Box::new(speaker.clone())));

    reactor.handle_command(VoiceCommand::SetSpeed(1.5));

    assert_eq!(*speaker.speed.lock().unwrap(), Some(1.5));
}

#[test]
fn set_voice_without_a_configured_speaker_is_a_silent_no_op() {
    let (mut reactor, mut rx) = new_reactor(None, None);

    reactor.handle_command(VoiceCommand::SetVoice("af_bella".to_string()));

    assert!(drain_events(&mut rx).is_empty());
}

#[test]
fn set_enabled_false_stops_listening_and_speaking() {
    let speaker = FakeSpeaker::default();
    let (mut reactor, _rx) = new_reactor(None, Some(Box::new(speaker.clone())));

    reactor.handle_command(VoiceCommand::Speak("hello".to_string()));
    assert!(!*speaker.stopped.lock().unwrap());

    reactor.handle_command(VoiceCommand::SetEnabled(false));

    assert!(*speaker.stopped.lock().unwrap());
    assert_eq!(reactor.state_for_test(), VoiceState::Idle);
}

#[test]
fn start_listening_is_ignored_while_voice_disabled() {
    let (mut reactor, mut rx) = new_reactor(None, None);
    reactor.handle_command(VoiceCommand::SetEnabled(false));
    drain_events(&mut rx);

    reactor.handle_command(VoiceCommand::StartListening);

    assert_eq!(reactor.state_for_test(), VoiceState::Idle);
    assert!(drain_events(&mut rx).is_empty());
}

#[test]
fn speak_while_voice_disabled_emits_unavailable_error() {
    let speaker = FakeSpeaker::default();
    let (mut reactor, mut rx) = new_reactor(None, Some(Box::new(speaker)));
    reactor.handle_command(VoiceCommand::SetEnabled(false));
    drain_events(&mut rx);

    reactor.handle_command(VoiceCommand::Speak("hello".to_string()));

    let events = drain_events(&mut rx);
    assert_eq!(
        events,
        vec![VoiceEvent::Error(
            "text-to-speech is unavailable".to_string()
        )]
    );
}

#[test]
fn speak_while_tts_disabled_emits_unavailable_error() {
    let speaker = FakeSpeaker::default();
    let (mut reactor, mut rx) = new_reactor(None, Some(Box::new(speaker)));
    reactor.handle_command(VoiceCommand::SetTtsEnabled(false));
    drain_events(&mut rx);

    reactor.handle_command(VoiceCommand::Speak("hello".to_string()));

    let events = drain_events(&mut rx);
    assert_eq!(
        events,
        vec![VoiceEvent::Error(
            "text-to-speech is unavailable".to_string()
        )]
    );
}

#[test]
fn set_stt_enabled_false_makes_transcribe_return_none() {
    let transcriber: Box<dyn Transcriber> = Box::new(FakeTranscriber {
        result: Ok("hello".to_string()),
    });
    let (mut reactor, mut rx) = new_reactor(Some(transcriber), None);
    reactor.handle_command(VoiceCommand::SetSttEnabled(false));
    drain_events(&mut rx);

    reactor.handle_command(VoiceCommand::StartListening);
    reactor.handle_command(VoiceCommand::StopListening);

    let events = drain_events(&mut rx);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, VoiceEvent::Transcript(_)))
    );
}

#[test]
fn set_stt_and_tts_enabled_update_flags_without_emitting_events() {
    let (mut reactor, mut rx) = new_reactor(None, None);

    reactor.handle_command(VoiceCommand::SetSttEnabled(false));
    reactor.handle_command(VoiceCommand::SetTtsEnabled(false));

    assert!(!reactor.stt_enabled_for_test());
    assert!(!reactor.tts_enabled_for_test());
    assert!(drain_events(&mut rx).is_empty());
}

#[test]
fn set_wake_phrase_updates_the_matcher() {
    let transcriber: Box<dyn Transcriber> = Box::new(FakeTranscriber {
        result: Ok("hello computer what time is it".to_string()),
    });
    let (mut reactor, mut rx) = new_wake_reactor(Some(transcriber), None);
    reactor.handle_command(VoiceCommand::SetWakePhrase("hello computer".to_string()));
    drain_events(&mut rx);

    reactor.handle_command(VoiceCommand::StartListening);
    drain_events(&mut rx);

    // Same 30ms-frame speech-then-silence shape as
    // `vad_ends_utterance_on_its_own_without_stop_listening`.
    const FRAME: usize = 480;
    let speech: Vec<f32> = (0..15 * FRAME)
        .map(|i| if i % 2 == 0 { 0.3 } else { -0.3 })
        .collect();
    let silence = vec![0.0f32; 30 * FRAME];
    reactor.feed_audio(&speech);
    reactor.feed_audio(&silence);

    let events = drain_events(&mut rx);
    assert!(events.contains(&VoiceEvent::WakeDetected));
    assert!(events.contains(&VoiceEvent::Transcript("what time is it".to_string())));
}
