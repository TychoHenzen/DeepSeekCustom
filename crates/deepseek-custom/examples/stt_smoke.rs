//! Smoke test for speech to text: the real microphone, the real whisper
//! model, and (in stage 2) the real `VoiceService` wake-word path. Nothing
//! here is faked. A human must watch it run and speak on cue.
//!
//! Stage 1 is push to talk: it opens the microphone directly, records a
//! fixed 5 second window, then transcribes it and reports the result. It
//! does not go through `VoiceService`, since that service ends an utterance
//! on VAD silence rather than a fixed clock, and this stage wants a fixed
//! window instead.
//!
//! Stage 2 drives `VoiceService` in `TriggerMode::WakeWord`, the real seam
//! the GUI will use, and prints every event as it arrives. Speech output is
//! not exercised: a `NullSpeaker` stands in, since `VoiceService` needs a
//! `Speaker` to construct but this example only cares about speech input.
//!
//! Usage:
//!   cargo run --example stt_smoke -- <model_path>
//! or set the WHISPER_MODEL_PATH environment variable and run with no
//! arguments. With neither, it falls back to models/ggml-base.en.bin under
//! the project root, so it also runs straight from an IDE.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use deepseek_custom::config::settings::TriggerMode;
use deepseek_custom::voice::capture::AudioCapture;
use deepseek_custom::voice::resolve_whisper_model_path;
use deepseek_custom::voice::service::{
    RealCaptureFactory, Speaker, Transcriber, VoiceCommand, VoiceEvent, VoiceService,
};
use deepseek_custom::voice::stt::WhisperEngine;
use tokio::sync::mpsc::UnboundedReceiver;

/// How long stage 1 records for before transcribing.
const RECORD_WINDOW: Duration = Duration::from_secs(5);

/// How long stage 2 listens for a wake match before giving up.
const WAKE_TIMEOUT: Duration = Duration::from_secs(30);

const WAKE_PHRASE: &str = "hey deepseek";

fn main() {
    tracing_subscriber::fmt::init();

    let model_path = match resolve_model_path() {
        Ok(path) => path,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };
    println!("Whisper model: {}", model_path.display());

    let engine = match WhisperEngine::new(&model_path) {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            eprintln!("Failed to load whisper model: {e}");
            std::process::exit(1);
        }
    };

    stage1_push_to_talk(&engine);
    stage2_wake_word(&engine);

    println!("\nDone.");
}

/// Record a fixed window from the real microphone, then transcribe it and
/// report the transcript, sample count, and transcription time. A missing
/// input device is reported plainly rather than panicking.
fn stage1_push_to_talk(engine: &Arc<WhisperEngine>) {
    println!("\n[Stage 1/2: push to talk]");
    println!(
        "Speak now. Recording for {} seconds...",
        RECORD_WINDOW.as_secs()
    );

    let mut capture = match AudioCapture::start() {
        Ok(capture) => capture,
        Err(e) => {
            eprintln!("Could not open microphone: {e}");
            return;
        }
    };

    std::thread::sleep(RECORD_WINDOW);
    let samples = capture.drain();
    capture.stop();
    println!("Captured {} samples.", samples.len());

    println!("Transcribing...");
    let start = Instant::now();
    let result = engine.transcribe(&samples);
    let elapsed = start.elapsed();
    match result {
        Ok(text) => println!("Transcript: \"{text}\" (transcribed in {elapsed:.3?})"),
        Err(e) => eprintln!("Transcription failed after {elapsed:.3?}: {e}"),
    }
}

/// Drive the real `VoiceService` in wake-word mode and print every event as
/// it arrives, until a wake match is seen or `WAKE_TIMEOUT` passes.
fn stage2_wake_word(engine: &Arc<WhisperEngine>) {
    println!("\n[Stage 2/2: wake word]");
    println!(
        "Say the wake phrase \"{WAKE_PHRASE}\", for example \
         \"hey deepseek what is the weather\"."
    );
    println!("Listening for up to {} seconds...", WAKE_TIMEOUT.as_secs());

    let transcriber: Box<dyn Transcriber> = Box::new(SharedTranscriber(Arc::clone(engine)));
    let (mut service, mut events) = VoiceService::start(
        Box::new(RealCaptureFactory),
        Some(transcriber),
        Some(Box::new(NullSpeaker)),
        TriggerMode::WakeWord,
        WAKE_PHRASE.to_string(),
    );
    service.send(VoiceCommand::StartListening);

    let deadline = Instant::now() + WAKE_TIMEOUT;
    if !poll_for_wake_match(&mut events, deadline) {
        println!(
            "Timed out after {} seconds without a wake match.",
            WAKE_TIMEOUT.as_secs()
        );
    }
    service.shutdown();
}

/// Poll voice events until a wake match is seen or `deadline` passes.
/// Prints each event as it arrives. On a match, also drains and prints
/// whatever events are already queued right behind it. That is typically
/// the `Transcript` for a command spoken in the same utterance as the
/// wake phrase. This skips waiting out the rest of the timeout for it.
/// Returns whether a match was seen.
fn poll_for_wake_match(events: &mut UnboundedReceiver<VoiceEvent>, deadline: Instant) -> bool {
    while Instant::now() < deadline {
        match events.try_recv() {
            Ok(event) => {
                println!("Event: {event:?}");
                match event {
                    VoiceEvent::WakeDetected => {
                        drain_remaining(events);
                        return true;
                    }
                    VoiceEvent::Error(message) => {
                        eprintln!("Voice service reported an error: {message}");
                        return false;
                    }
                    _ => {}
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    false
}

/// Print and discard every event already queued, without waiting.
fn drain_remaining(events: &mut UnboundedReceiver<VoiceEvent>) {
    while let Ok(event) = events.try_recv() {
        println!("Event: {event:?}");
    }
}

/// Wraps a shared, already-loaded `WhisperEngine` so both stages use the
/// same loaded model instead of loading it twice. `VoiceService` takes its
/// transcriber by value, one instance per stage.
struct SharedTranscriber(Arc<WhisperEngine>);

impl Transcriber for SharedTranscriber {
    fn transcribe(&self, samples: &[f32]) -> Result<String, String> {
        self.0.transcribe(samples).map_err(|e| e.to_string())
    }
}

/// No real text to speech in this example: it only exercises speech input.
/// `VoiceService` still requires a `Speaker` to construct.
struct NullSpeaker;

impl Speaker for NullSpeaker {
    fn speak(&self, _text: &str) {}
    fn stop(&self) {}
    fn set_voice(&self, _voice_id: &str) {}
    fn set_speed(&self, _speed: f32) {}
}

/// Resolve the whisper model path: the first CLI argument, then
/// `WHISPER_MODEL_PATH`, then the usual project-root and local-app-data
/// candidates `resolve_whisper_model_path` tries on its own. Returns an
/// error naming the default path expected, if nothing resolved.
fn resolve_model_path() -> Result<PathBuf, String> {
    let configured = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("WHISPER_MODEL_PATH").ok());
    let project_root =
        std::env::current_dir().map_err(|e| format!("cannot read current directory: {e}"))?;
    resolve_whisper_model_path(configured.as_deref(), &project_root).ok_or_else(|| {
        format!(
            "Whisper model not found at {} (pass a path as the first argument, or set \
             WHISPER_MODEL_PATH; see the warning above for every path tried).",
            project_root
                .join("models")
                .join("ggml-base.en.bin")
                .display()
        )
    })
}
