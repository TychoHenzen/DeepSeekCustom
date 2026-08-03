//! Ties capture, VAD, whisper, and Kokoro into one state machine. This is
//! the single seam the GUI talks to: send [`VoiceCommand`]s in, read
//! [`VoiceEvent`]s out. [`VoiceService`] owns a background thread. The
//! [`VoiceReactor`] it wraps holds the actual transition table and is
//! plain and synchronous. It is fully testable with fakes: no microphone,
//! no model, no sound card required.
//!
//! In [`TriggerMode::PushToTalk`], an utterance ending goes back to
//! `Idle` and waits for the next `StartListening`. In
//! [`TriggerMode::WakeWord`], the microphone stays open: each utterance
//! the VAD cuts out is checked against [`super::wake::WakeWordMatcher`],
//! and listening resumes right after, so the mic never has to be told to
//! start again. An explicit `StopListening` or `Shutdown` still stops it
//! in either mode.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tracing::error;

use super::capture::AudioCapture;
use super::stt::WhisperEngine;
use super::tts::TtsHandle;
use super::vad::{VadEvent, VadState};
use super::wake::WakeWordMatcher;
use crate::config::settings::TriggerMode;

/// How often the service thread polls the open capture for new samples.
const CAPTURE_POLL_INTERVAL: Duration = Duration::from_millis(30);

/// States the voice pipeline can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceState {
    Idle,
    Listening,
    Transcribing,
    Speaking,
}

/// Commands accepted by [`VoiceService`].
#[derive(Debug, Clone, PartialEq)]
pub enum VoiceCommand {
    StartListening,
    StopListening,
    Speak(String),
    StopSpeaking,
    SetTriggerMode(TriggerMode),
    /// Master voice switch. `false` stops any listening or speaking in
    /// progress and blocks `StartListening`/`Speak` until re-enabled.
    SetEnabled(bool),
    /// Speech-to-text switch. `false` makes transcription behave as if no
    /// transcriber were configured.
    SetSttEnabled(bool),
    /// Text-to-speech switch. `false` makes `Speak` behave as if no
    /// speaker were configured.
    SetTtsEnabled(bool),
    /// Replace the phrase [`TriggerMode::WakeWord`] listens for.
    SetWakePhrase(String),
    /// Change the active Kokoro voice id, e.g. `af_heart`.
    SetVoice(String),
    /// Change the speaking speed, clamped 0.5-2.0 by the speaker.
    SetSpeed(f32),
    Shutdown,
}

/// Events emitted by [`VoiceService`] on its unbounded channel.
#[derive(Debug, Clone, PartialEq)]
pub enum VoiceEvent {
    StateChanged(VoiceState),
    Transcript(String),
    WakeDetected,
    Error(String),
}

/// The microphone half of the pipeline, behind a trait so tests can drive
/// the state machine without a sound card. Not `Send`: on some platforms
/// the audio stream (`cpal::Stream`) cannot cross threads. A
/// [`CaptureSource`] is always created and used inside the one thread
/// [`VoiceService`] owns. It is never moved into that thread later.
pub trait CaptureSource {
    fn stop(&mut self);
    fn drain(&self) -> Vec<f32>;
}

impl CaptureSource for AudioCapture {
    fn stop(&mut self) {
        AudioCapture::stop(self);
    }

    fn drain(&self) -> Vec<f32> {
        AudioCapture::drain(self)
    }
}

/// Opens a fresh [`CaptureSource`] on `StartListening`. A trait rather than
/// a bare constructor call so tests can supply a fake that never touches a
/// real device.
pub trait CaptureFactory: Send {
    fn start(&self) -> Result<Box<dyn CaptureSource>, String>;
}

/// Opens the real default microphone via [`AudioCapture`].
pub struct RealCaptureFactory;

impl CaptureFactory for RealCaptureFactory {
    fn start(&self) -> Result<Box<dyn CaptureSource>, String> {
        AudioCapture::start()
            .map(|c| Box::new(c) as Box<dyn CaptureSource>)
            .map_err(|e| e.to_string())
    }
}

/// The speech-to-text half of the pipeline, behind a trait so tests can
/// supply canned transcripts or errors without a real model.
pub trait Transcriber: Send {
    fn transcribe(&self, samples: &[f32]) -> Result<String, String>;
}

impl Transcriber for WhisperEngine {
    fn transcribe(&self, samples: &[f32]) -> Result<String, String> {
        WhisperEngine::transcribe(self, samples).map_err(|e| e.to_string())
    }
}

/// The text-to-speech half of the pipeline, behind a trait so tests can
/// observe barge-in (`stop`) without a real audio device.
pub trait Speaker: Send {
    fn speak(&self, text: &str);
    fn stop(&self);
    fn set_voice(&self, voice_id: &str);
    fn set_speed(&self, speed: f32);
}

impl Speaker for TtsHandle {
    fn speak(&self, text: &str) {
        TtsHandle::speak(self, text);
    }

    fn stop(&self) {
        TtsHandle::stop(self);
    }

    fn set_voice(&self, voice_id: &str) {
        TtsHandle::set_voice(self, voice_id);
    }

    fn set_speed(&self, speed: f32) {
        TtsHandle::set_speed(self, speed);
    }
}

/// The state machine itself: synchronous, no I/O of its own beyond calling
/// into the traits above. [`VoiceService`] drives this from a background
/// thread. Tests drive it directly.
struct VoiceReactor {
    state: VoiceState,
    trigger_mode: TriggerMode,
    vad: VadState,
    utterance: Vec<f32>,
    capture_factory: Box<dyn CaptureFactory>,
    capture: Option<Box<dyn CaptureSource>>,
    transcriber: Option<Box<dyn Transcriber>>,
    speaker: Option<Box<dyn Speaker>>,
    wake_matcher: WakeWordMatcher,
    /// Set once a wake match is found with nothing said after it in the
    /// same utterance. The next utterance is then taken as the command
    /// outright, without being checked against the wake phrase again.
    awaiting_wake_command: bool,
    /// Master voice switch. See [`VoiceCommand::SetEnabled`].
    enabled: bool,
    /// Speech-to-text switch. See [`VoiceCommand::SetSttEnabled`].
    stt_enabled: bool,
    /// Text-to-speech switch. See [`VoiceCommand::SetTtsEnabled`].
    tts_enabled: bool,
    /// Set once a `Speak` with no configured speaker has already reported
    /// its `Error`. Every reply now triggers a `Speak` (see the GUI's
    /// turn-end handler), so a machine with no Kokoro model must not spam
    /// one error line per reply. Report it once and go quiet after that.
    speaker_missing_warned: bool,
    events: UnboundedSender<VoiceEvent>,
}

impl VoiceReactor {
    fn new(
        capture_factory: Box<dyn CaptureFactory>,
        transcriber: Option<Box<dyn Transcriber>>,
        speaker: Option<Box<dyn Speaker>>,
        trigger_mode: TriggerMode,
        wake_phrase: &str,
        events: UnboundedSender<VoiceEvent>,
    ) -> Self {
        Self {
            state: VoiceState::Idle,
            trigger_mode,
            vad: VadState::default(),
            utterance: Vec::new(),
            capture_factory,
            capture: None,
            transcriber,
            speaker,
            wake_matcher: WakeWordMatcher::new(wake_phrase),
            awaiting_wake_command: false,
            enabled: true,
            stt_enabled: true,
            tts_enabled: true,
            speaker_missing_warned: false,
            events,
        }
    }

    /// Apply one command. Returns false once the reactor should stop.
    fn handle_command(&mut self, cmd: VoiceCommand) -> bool {
        match cmd {
            VoiceCommand::StartListening => self.start_listening(),
            VoiceCommand::StopListening => self.stop_listening(),
            VoiceCommand::Speak(text) => self.speak(&text),
            VoiceCommand::StopSpeaking => self.stop_speaking(),
            VoiceCommand::SetTriggerMode(mode) => self.trigger_mode = mode,
            VoiceCommand::SetEnabled(enabled) => self.set_enabled(enabled),
            VoiceCommand::SetSttEnabled(enabled) => self.stt_enabled = enabled,
            VoiceCommand::SetTtsEnabled(enabled) => self.tts_enabled = enabled,
            VoiceCommand::SetWakePhrase(phrase) => {
                self.wake_matcher = WakeWordMatcher::new(&phrase)
            }
            VoiceCommand::SetVoice(voice_id) => self.set_voice(&voice_id),
            VoiceCommand::SetSpeed(speed) => self.set_speed(speed),
            VoiceCommand::Shutdown => {
                self.stop_listening();
                self.stop_speaking();
                return false;
            }
        }
        true
    }

    /// Apply the master voice switch. Turning it off stops any listening
    /// or speaking in progress, the same as `Shutdown` does, but leaves
    /// the reactor running so a later `SetEnabled(true)` resumes it.
    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.stop_listening();
            self.stop_speaking();
        }
    }

    /// Forward a voice id change to the speaker, if configured. A no-op
    /// otherwise, matching how `speak` degrades when text-to-speech is
    /// unavailable.
    fn set_voice(&mut self, voice_id: &str) {
        if let Some(speaker) = &self.speaker {
            speaker.set_voice(voice_id);
        }
    }

    /// Forward a speed change to the speaker, if configured.
    fn set_speed(&mut self, speed: f32) {
        if let Some(speaker) = &self.speaker {
            speaker.set_speed(speed);
        }
    }

    /// Feed a chunk of newly captured samples through the VAD while
    /// listening. A trailing-silence `UtteranceEnded` ends the utterance
    /// just as an explicit `StopListening` would. Frames fed while not
    /// listening are ignored, since nothing owns the capture then.
    fn feed_audio(&mut self, chunk: &[f32]) {
        if self.state != VoiceState::Listening || chunk.is_empty() {
            return;
        }
        self.utterance.extend_from_slice(chunk);
        if self
            .vad
            .process_chunk(chunk)
            .contains(&VadEvent::UtteranceEnded)
        {
            // Wake-word mode listens continuously: the VAD ending this
            // utterance on its own means the mic should stay open for
            // the next one.
            let auto_continue = self.trigger_mode == TriggerMode::WakeWord;
            self.finish_utterance(auto_continue);
        }
    }

    /// Barge-in stops any speech in progress, then opens a fresh capture.
    /// A no-op if already listening.
    fn start_listening(&mut self) {
        if !self.enabled {
            return;
        }
        if self.state == VoiceState::Speaking {
            self.stop_speaking();
        }
        if self.state == VoiceState::Listening {
            return;
        }
        match self.capture_factory.start() {
            Ok(capture) => {
                self.capture = Some(capture);
                self.vad = VadState::default();
                self.utterance.clear();
                self.set_state(VoiceState::Listening);
            }
            Err(e) => self.emit_error(e),
        }
    }

    /// Explicit end of an utterance. A no-op unless currently listening.
    /// Always stops for good, even in wake-word mode: an explicit stop
    /// (or shutdown) means the caller wants the mic closed, not reopened.
    fn stop_listening(&mut self) {
        if self.state == VoiceState::Listening {
            self.finish_utterance(false);
        }
    }

    /// Close the capture, hand the buffered utterance to the transcriber,
    /// and route the result. Runs whether the utterance ended because of
    /// an explicit `StopListening` or the VAD's own silence hangover.
    /// Push-to-talk emits the transcript directly. Wake-word mode routes
    /// it through [`Self::handle_wake_transcript`] instead. If
    /// `auto_continue` is set, listening resumes right away rather than
    /// going idle, so wake-word mode's mic never actually closes.
    fn finish_utterance(&mut self, auto_continue: bool) {
        if let Some(mut capture) = self.capture.take() {
            capture.stop();
        }
        let samples = std::mem::take(&mut self.utterance);
        self.set_state(VoiceState::Transcribing);
        let transcript = self.transcribe(&samples);
        if self.trigger_mode == TriggerMode::WakeWord {
            self.handle_wake_transcript(transcript);
        } else if let Some(text) = transcript {
            let _ = self.events.send(VoiceEvent::Transcript(text));
        }
        if auto_continue {
            self.start_listening();
        } else {
            self.set_state(VoiceState::Idle);
        }
    }

    /// Run the transcriber, if one is configured, and return its result.
    /// A missing model leaves transcription silently unavailable rather
    /// than erroring, matching how the rest of the voice subsystem
    /// degrades. An empty or whitespace-only transcript is treated the
    /// same as none at all.
    fn transcribe(&mut self, samples: &[f32]) -> Option<String> {
        if !self.stt_enabled {
            return None;
        }
        let transcriber = self.transcriber.as_ref()?;
        match transcriber.transcribe(samples) {
            Ok(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
            Ok(_) => None,
            Err(e) => {
                self.emit_error(e);
                None
            }
        }
    }

    /// Route a wake-word-mode transcript. If the previous utterance was
    /// the wake phrase alone, this whole utterance is taken as the
    /// command. Otherwise, check the transcript for the wake phrase at
    /// its start. A match emits `WakeDetected`, plus `Transcript` right
    /// away for anything that followed the phrase in the same utterance.
    /// A transcript that does not start with the wake phrase is ordinary
    /// background speech and is dropped.
    fn handle_wake_transcript(&mut self, transcript: Option<String>) {
        let Some(text) = transcript else {
            return;
        };
        if self.awaiting_wake_command {
            self.awaiting_wake_command = false;
            let _ = self.events.send(VoiceEvent::Transcript(text));
            return;
        }
        let Some(wake_match) = self.wake_matcher.match_utterance(&text) else {
            return;
        };
        let _ = self.events.send(VoiceEvent::WakeDetected);
        match wake_match.command {
            Some(command) => {
                let _ = self.events.send(VoiceEvent::Transcript(command));
            }
            None => self.awaiting_wake_command = true,
        }
    }

    /// Speak `text` if a speaker is configured. Emits `Error` if the
    /// master or text-to-speech switch is off, every time: that is a
    /// direct user action and deserves a direct answer.
    ///
    /// A missing speaker is different. It means no Kokoro model was found
    /// at startup, already logged once by [`super::warn_missing`]. Since
    /// every agent reply now sends a `Speak`, repeating that as a
    /// `VoiceEvent::Error` on every single turn would spam a machine with
    /// no model. So this reports it once, via `speaker_missing_warned`,
    /// and silently no-ops after that, the same way [`Self::transcribe`]
    /// degrades when no transcriber is configured.
    fn speak(&mut self, text: &str) {
        if !self.enabled || !self.tts_enabled {
            self.emit_error("text-to-speech is unavailable".to_string());
            return;
        }
        let Some(speaker) = &self.speaker else {
            if !self.speaker_missing_warned {
                self.speaker_missing_warned = true;
                self.emit_error("text-to-speech is unavailable".to_string());
            }
            return;
        };
        speaker.speak(text);
        self.set_state(VoiceState::Speaking);
    }

    /// Stop any speech in progress. A no-op if not currently speaking.
    fn stop_speaking(&mut self) {
        if self.state != VoiceState::Speaking {
            return;
        }
        if let Some(speaker) = &self.speaker {
            speaker.stop();
        }
        self.set_state(VoiceState::Idle);
    }

    fn set_state(&mut self, next: VoiceState) {
        if self.state == next {
            return;
        }
        self.state = next;
        let _ = self.events.send(VoiceEvent::StateChanged(next));
    }

    fn emit_error(&mut self, message: String) {
        let _ = self.events.send(VoiceEvent::Error(message));
    }
}

/// The single seam the GUI talks to. Owns a background thread running a
/// [`VoiceReactor`]. Commands go in via [`VoiceService::send`]. Events
/// come out on the receiver returned by [`VoiceService::start`].
pub struct VoiceService {
    tx: mpsc::Sender<VoiceCommand>,
    handle: Option<thread::JoinHandle<()>>,
}

impl VoiceService {
    /// Spawn the worker thread and return right away. `wake_phrase` is
    /// only used when `trigger_mode` is [`TriggerMode::WakeWord`].
    pub fn start(
        capture_factory: Box<dyn CaptureFactory>,
        transcriber: Option<Box<dyn Transcriber>>,
        speaker: Option<Box<dyn Speaker>>,
        trigger_mode: TriggerMode,
        wake_phrase: String,
    ) -> (Self, UnboundedReceiver<VoiceEvent>) {
        let (tx, rx) = mpsc::channel();
        let (event_tx, event_rx) = unbounded_channel();
        // The reactor is built inside the spawned thread, not out here and
        // then moved in. Its `capture` field can hold a platform audio
        // stream that is not `Send`, so the reactor as a whole must never
        // cross a thread boundary. Only the `Send` ingredients do.
        let handle = thread::spawn(move || {
            let reactor = VoiceReactor::new(
                capture_factory,
                transcriber,
                speaker,
                trigger_mode,
                &wake_phrase,
                event_tx,
            );
            run_service(rx, reactor);
        });
        (
            Self {
                tx,
                handle: Some(handle),
            },
            event_rx,
        )
    }

    /// Send a command to the worker thread. Silently dropped if the worker
    /// has already exited.
    pub fn send(&self, cmd: VoiceCommand) {
        if self.tx.send(cmd).is_err() {
            error!("voice service worker is gone, dropping command");
        }
    }

    /// Ask the worker to shut down and wait for it to exit.
    pub fn shutdown(&mut self) {
        self.send(VoiceCommand::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Service commands until `Shutdown` or the last handle drops, polling the
/// open capture for new samples in between.
fn run_service(rx: mpsc::Receiver<VoiceCommand>, mut reactor: VoiceReactor) {
    loop {
        match rx.recv_timeout(CAPTURE_POLL_INTERVAL) {
            Ok(cmd) => {
                if !reactor.handle_command(cmd) {
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => poll_capture(&mut reactor),
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Drain any newly captured samples and feed them through the reactor.
fn poll_capture(reactor: &mut VoiceReactor) {
    let Some(capture) = &reactor.capture else {
        return;
    };
    let chunk = capture.drain();
    reactor.feed_audio(&chunk);
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

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
        let reactor = VoiceReactor::new(
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
        let reactor = VoiceReactor::new(
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
        let mut reactor = VoiceReactor::new(
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
        assert_eq!(reactor.trigger_mode, TriggerMode::WakeWord);
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
        assert_eq!(reactor.state, VoiceState::Idle);
    }

    #[test]
    fn start_listening_is_ignored_while_voice_disabled() {
        let (mut reactor, mut rx) = new_reactor(None, None);
        reactor.handle_command(VoiceCommand::SetEnabled(false));
        drain_events(&mut rx);

        reactor.handle_command(VoiceCommand::StartListening);

        assert_eq!(reactor.state, VoiceState::Idle);
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

        assert!(!reactor.stt_enabled);
        assert!(!reactor.tts_enabled);
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
}
