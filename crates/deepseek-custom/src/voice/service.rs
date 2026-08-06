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
///
/// `pub`: the moved reactor tests, now in the external test crate, build
/// and drive one directly rather than going through the threaded
/// [`VoiceService`]. Its fields stay private; only the methods below are
/// its real interface.
pub struct VoiceReactor {
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

    /// Test seam for [`VoiceReactor::new`]. `VoiceService::start` is the
    /// only real caller and it always spawns a real background thread
    /// around the reactor it builds, so there is no existing way for a
    /// test to build a bare reactor without this wrapper.
    #[cfg(feature = "test-support")]
    pub fn new_for_test(
        capture_factory: Box<dyn CaptureFactory>,
        transcriber: Option<Box<dyn Transcriber>>,
        speaker: Option<Box<dyn Speaker>>,
        trigger_mode: TriggerMode,
        wake_phrase: &str,
        events: UnboundedSender<VoiceEvent>,
    ) -> Self {
        Self::new(
            capture_factory,
            transcriber,
            speaker,
            trigger_mode,
            wake_phrase,
            events,
        )
    }

    /// Apply one command. Returns false once the reactor should stop.
    /// `pub`: this, [`Self::feed_audio`], [`Self::transcribe`],
    /// [`Self::speak`], [`Self::set_voice`], and [`Self::set_speed`] are
    /// the reactor's real interface, driven directly by the moved tests.
    pub fn handle_command(&mut self, cmd: VoiceCommand) -> bool {
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
    /// unavailable. See [`Self::handle_command`] for why this is `pub`.
    pub fn set_voice(&mut self, voice_id: &str) {
        if let Some(speaker) = &self.speaker {
            speaker.set_voice(voice_id);
        }
    }

    /// Forward a speed change to the speaker, if configured. See
    /// [`Self::handle_command`] for why this is `pub`.
    pub fn set_speed(&mut self, speed: f32) {
        if let Some(speaker) = &self.speaker {
            speaker.set_speed(speed);
        }
    }

    /// Feed a chunk of newly captured samples through the VAD while
    /// listening. A trailing-silence `UtteranceEnded` ends the utterance
    /// just as an explicit `StopListening` would. Frames fed while not
    /// listening are ignored, since nothing owns the capture then. See
    /// [`Self::handle_command`] for why this is `pub`.
    pub fn feed_audio(&mut self, chunk: &[f32]) {
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
    /// same as none at all. See [`Self::handle_command`] for why this is
    /// `pub`.
    pub fn transcribe(&mut self, samples: &[f32]) -> Option<String> {
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
    /// degrades when no transcriber is configured. See
    /// [`Self::handle_command`] for why this is `pub`.
    pub fn speak(&mut self, text: &str) {
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

    /// Read the tracked state. Every production reader goes through the
    /// `VoiceEvent::StateChanged` events `set_state` already emits; there
    /// is no side-effect-free way to read the field itself without this
    /// accessor.
    #[cfg(feature = "test-support")]
    pub fn state_for_test(&self) -> VoiceState {
        self.state
    }

    /// Read the trigger mode. See [`Self::state_for_test`].
    #[cfg(feature = "test-support")]
    pub fn trigger_mode_for_test(&self) -> TriggerMode {
        self.trigger_mode
    }

    /// Read the speech-to-text switch. See [`Self::state_for_test`].
    #[cfg(feature = "test-support")]
    pub fn stt_enabled_for_test(&self) -> bool {
        self.stt_enabled
    }

    /// Read the text-to-speech switch. See [`Self::state_for_test`].
    #[cfg(feature = "test-support")]
    pub fn tts_enabled_for_test(&self) -> bool {
        self.tts_enabled
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
