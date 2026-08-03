//! Kokoro text-to-speech engine.
//!
//! Loads the Kokoro ONNX model and its voice embeddings once via
//! [`KokoroSynth::new`], then turns text into 24 kHz mono f32 samples via
//! [`KokoroSynth::synth`]. [`TtsHandle`] wraps this in a dedicated worker
//! thread and plays the returned samples through [`super::playback::AudioSink`].

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use kokoro_en::{KokoroError, KokoroTts, Voice};
use thiserror::Error;
use tracing::{debug, error};

use super::playback::AudioSink;

/// Kokoro's valid speed range.
const SPEED_MIN: f32 = 0.5;
const SPEED_MAX: f32 = 2.0;

/// Sensible default English voice used until the caller picks one.
const DEFAULT_VOICE_ID: &str = "af_heart";

/// Errors produced while loading the Kokoro model or synthesizing speech.
#[derive(Error, Debug)]
pub enum TtsError {
    #[error("Kokoro model file not found: {0}")]
    ModelNotFound(PathBuf),
    #[error("Kokoro voices path not found: {0}")]
    VoicesNotFound(PathBuf),
    #[error("Kokoro synthesis failed: {0}")]
    Synth(#[from] KokoroError),
}

/// Loads the Kokoro ONNX model and voice embeddings once and turns text into
/// 24 kHz mono f32 samples.
pub struct KokoroSynth {
    tts: KokoroTts,
    voice_id: String,
    speed: f32,
}

impl KokoroSynth {
    /// Load the Kokoro model and voices from disk.
    ///
    /// `model_path` must point at a Kokoro ONNX model file. `voices_path` may
    /// point at a directory of `<voice_id>.bin` files or a single combined
    /// voices file. If either path is missing, returns an error naming the
    /// exact path that was tried.
    pub async fn new(
        model_path: impl AsRef<Path>,
        voices_path: impl AsRef<Path>,
    ) -> Result<Self, TtsError> {
        let model_path = model_path.as_ref();
        let voices_path = voices_path.as_ref();

        if !model_path.exists() {
            return Err(TtsError::ModelNotFound(model_path.to_path_buf()));
        }
        if !voices_path.exists() {
            return Err(TtsError::VoicesNotFound(voices_path.to_path_buf()));
        }

        super::cuda_dlls::register_cuda_dll_dirs();
        let tts = KokoroTts::new(model_path, voices_path).await?;
        super::cuda_dlls::log_real_cuda_provider_status();
        Ok(Self {
            tts,
            voice_id: DEFAULT_VOICE_ID.to_string(),
            speed: 1.0,
        })
    }

    /// Select the active voice id, e.g. `af_heart`, `am_michael`, `bf_emma`.
    pub fn set_voice(&mut self, voice_id: impl Into<String>) {
        self.voice_id = voice_id.into();
    }

    /// The active voice id.
    pub fn voice_id(&self) -> &str {
        &self.voice_id
    }

    /// Set the speaking speed, clamped to 0.5-2.0. Default is 1.0.
    pub fn set_speed(&mut self, speed: f32) {
        self.speed = clamp_speed(speed);
    }

    /// The active speaking speed.
    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// Synthesize `text` into 24 kHz mono f32 samples using the current
    /// voice and speed.
    pub async fn synth(&self, text: &str) -> Result<Vec<f32>, TtsError> {
        let voice = Voice::new(self.voice_id.clone()).with_speed(self.speed);
        let (samples, elapsed) = self.tts.synth(text, voice).await?;
        debug!("kokoro synth: {elapsed:?} for {} samples", samples.len());
        Ok(samples)
    }
}

/// Clamp a requested speed to Kokoro's valid range of 0.5 to 2.0.
fn clamp_speed(speed: f32) -> f32 {
    speed.clamp(SPEED_MIN, SPEED_MAX)
}

/// Collapse whitespace runs to single spaces and trim the ends.
///
/// Applied to every string on the path to [`KokoroSynth::synth`], from
/// both `speak` and `speak_blocking` (see `handle_command`). This stays
/// narrow on purpose. Punctuation marks are not removed. kokoro-en's
/// phonemizer uses punctuation for prosody. Stripping it made speech sound
/// flat, and it did not fix the truncation this step targets. That fix is
/// the `KOKORO_G2P_SEGMENT_ESPEAK` environment variable. It is set once as
/// the first statement in `main`, before any thread starts. See there for
/// why: kokoro-en phonemizes by splitting text on punctuation. It pipes
/// each punctuation-free segment to the system `espeak-ng` binary over
/// stdin, with no trailing newline. That silently drops the last phoneme
/// of whatever it reads. The env var skips the subprocess call entirely.
/// It falls back to the crate's bundled Misaki phonemizer instead, which
/// has no subprocess and no truncation.
fn normalize_for_synth(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Commands accepted by the TTS worker thread.
enum Command {
    /// The optional sender is used by `speak_blocking` to report back how
    /// many samples synthesis produced once it and the enqueue are done.
    /// `speak` leaves it `None` and never waits.
    Speak(String, Option<mpsc::Sender<usize>>),
    Stop,
    SetVoice(String),
    SetSpeed(f32),
    /// Block the caller until the sink's queue has genuinely drained. Used
    /// by `TtsHandle::wait_until_drained`. The bool sent back is true if
    /// playback drained, false if the sink's internal timeout fired first.
    WaitDrained(mpsc::Sender<bool>),
    Shutdown,
}

/// A cheap-to-clone, thread-safe handle to the text-to-speech worker.
///
/// Synthesis and playback happen on a dedicated background thread, so
/// callers never block. If the Kokoro model or an audio output device is
/// unavailable, the handle still works. It logs the reason once, then
/// silently discards every command afterward.
#[derive(Clone)]
pub struct TtsHandle {
    tx: mpsc::Sender<Command>,
}

impl TtsHandle {
    /// Spawn the worker thread and return right away. Loading the model and
    /// opening the output device happen off the calling thread.
    ///
    /// The returned `JoinHandle` lets a caller wait for the worker to exit
    /// after `shutdown()`. A smoke test needs this. It stops the process
    /// from exiting while a synth call is still running inside ONNX
    /// Runtime.
    pub fn start(
        model_path: impl Into<PathBuf>,
        voices_path: impl Into<PathBuf>,
    ) -> (Self, thread::JoinHandle<()>) {
        let (tx, rx) = mpsc::channel();
        let model_path = model_path.into();
        let voices_path = voices_path.into();
        let worker = thread::spawn(move || run_worker(rx, model_path, voices_path));
        (Self { tx }, worker)
    }

    /// Synthesize `text` and enqueue it for playback. Returns right away.
    /// Synthesis happens on the worker thread.
    pub fn speak(&self, text: impl Into<String>) {
        let _ = self.tx.send(Command::Speak(text.into(), None));
    }

    /// Synthesize `text`, enqueue it for playback, and block until
    /// synthesis (not playback) has completed. Returns the number of
    /// samples produced, or 0 if synthesis failed (already logged by the
    /// worker) or the worker is gone.
    ///
    /// Playback keeps running on the device after this returns. This only
    /// waits for the CPU-side synth call to finish. That is what call
    /// ordering and shutdown timing actually depend on. The smoke test uses
    /// this so it can wait for real completion instead of guessing with a
    /// fixed sleep.
    pub fn speak_blocking(&self, text: impl Into<String>) -> usize {
        let (done_tx, done_rx) = mpsc::channel();
        if self
            .tx
            .send(Command::Speak(text.into(), Some(done_tx)))
            .is_err()
        {
            return 0;
        }
        done_rx.recv().unwrap_or(0)
    }

    /// Clear the playback queue so speech cuts off immediately. Used for
    /// barge-in when the user starts talking over the assistant.
    pub fn stop(&self) {
        let _ = self.tx.send(Command::Stop);
    }

    /// Change the active voice for calls to `speak` made after this one.
    pub fn set_voice(&self, voice_id: impl Into<String>) {
        let _ = self.tx.send(Command::SetVoice(voice_id.into()));
    }

    /// Change the speaking speed for calls to `speak` made after this one.
    pub fn set_speed(&self, speed: f32) {
        let _ = self.tx.send(Command::SetSpeed(speed));
    }

    /// Block until playback has genuinely caught up: every sample enqueued
    /// so far has actually played, not just been handed to the queue.
    /// Returns true if playback drained. Returns false if the sink's
    /// generous internal timeout fired first, which means a stalled or
    /// closed output device. Returns true at once if the worker is gone,
    /// since nothing is playing.
    pub fn wait_until_drained(&self) -> bool {
        let (done_tx, done_rx) = mpsc::channel();
        if self.tx.send(Command::WaitDrained(done_tx)).is_err() {
            return true;
        }
        done_rx.recv().unwrap_or(true)
    }

    /// Stop the worker thread. Does not wait for it to exit. Use the
    /// `JoinHandle` returned by `start` when that matters.
    pub fn shutdown(&self) {
        let _ = self.tx.send(Command::Shutdown);
    }
}

/// Load the model, open the output device, then service commands until
/// `Shutdown` or the last handle drops. A setup failure is logged once and
/// the loop falls back to discarding every command it receives.
fn run_worker(rx: mpsc::Receiver<Command>, model_path: PathBuf, voices_path: PathBuf) {
    let Some(runtime) = build_runtime() else {
        discard_forever(rx);
        return;
    };
    let Some(mut synth) = runtime.block_on(load_synth(&model_path, &voices_path)) else {
        discard_forever(rx);
        return;
    };
    let Some(sink) = open_sink() else {
        discard_forever(rx);
        return;
    };

    for cmd in rx {
        if !handle_command(cmd, &runtime, &mut synth, &sink) {
            break;
        }
    }

    // A normal exit, whether from `Shutdown` or the last handle dropping,
    // must not drop `sink` while samples are still queued. Dropping it
    // kills the cpal stream at once and clips whatever was left to play.
    // `Stop` is unaffected: it clears the queue itself and returns before
    // this point is ever reached.
    sink.wait_until_drained();
}

/// A small single-thread runtime so the worker can call Kokoro's async API
/// without blocking any shared executor.
fn build_runtime() -> Option<tokio::runtime::Runtime> {
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => Some(rt),
        Err(e) => {
            error!("tts worker: failed to start its runtime, speech disabled: {e}");
            None
        }
    }
}

async fn load_synth(model_path: &Path, voices_path: &Path) -> Option<KokoroSynth> {
    match KokoroSynth::new(model_path, voices_path).await {
        Ok(synth) => Some(synth),
        Err(e) => {
            error!("tts worker: {e}, speech disabled");
            None
        }
    }
}

fn open_sink() -> Option<AudioSink> {
    match AudioSink::start() {
        Ok(sink) => Some(sink),
        Err(e) => {
            error!("tts worker: {e}, speech disabled");
            None
        }
    }
}

/// Apply one command. Returns false when the worker loop should stop.
fn handle_command(
    cmd: Command,
    runtime: &tokio::runtime::Runtime,
    synth: &mut KokoroSynth,
    sink: &AudioSink,
) -> bool {
    match cmd {
        Command::Speak(text, done) => {
            let text = normalize_for_synth(&text);
            let sample_count = match runtime.block_on(synth.synth(&text)) {
                Ok(samples) => {
                    let sample_count = samples.len();
                    sink.enqueue(samples);
                    sample_count
                }
                Err(e) => {
                    error!("tts worker: synth failed: {e}");
                    0
                }
            };
            if let Some(done) = done {
                let _ = done.send(sample_count);
            }
            true
        }
        Command::Stop => {
            sink.clear();
            true
        }
        Command::SetVoice(voice_id) => {
            synth.set_voice(voice_id);
            true
        }
        Command::SetSpeed(speed) => {
            synth.set_speed(speed);
            true
        }
        Command::WaitDrained(done) => {
            let drained = sink.wait_until_drained();
            let _ = done.send(drained);
            true
        }
        Command::Shutdown => false,
    }
}

/// Consume and discard every command until the channel closes, without
/// doing any work. Used once setup has failed, so the rest of the app can
/// keep calling the handle unchanged.
fn discard_forever(rx: mpsc::Receiver<Command>) {
    for _cmd in rx {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_speed_within_range_is_unchanged() {
        assert_eq!(clamp_speed(1.0), 1.0);
        assert_eq!(clamp_speed(0.75), 0.75);
        assert_eq!(clamp_speed(1.5), 1.5);
    }

    #[test]
    fn clamp_speed_clamps_above_max() {
        assert_eq!(clamp_speed(5.0), SPEED_MAX);
    }

    #[test]
    fn clamp_speed_clamps_below_min() {
        assert_eq!(clamp_speed(0.1), SPEED_MIN);
    }

    #[test]
    fn clamp_speed_boundary_values_pass_through() {
        assert_eq!(clamp_speed(SPEED_MIN), SPEED_MIN);
        assert_eq!(clamp_speed(SPEED_MAX), SPEED_MAX);
    }

    #[test]
    fn normalize_for_synth_keeps_period_ending_sentence() {
        assert_eq!(normalize_for_synth("Hello there."), "Hello there.");
    }

    #[test]
    fn normalize_for_synth_keeps_question_mark_ending_sentence() {
        assert_eq!(normalize_for_synth("Is this working?"), "Is this working?");
    }

    #[test]
    fn normalize_for_synth_keeps_exclamation_ending_sentence() {
        assert_eq!(normalize_for_synth("Watch out!"), "Watch out!");
    }

    #[test]
    fn normalize_for_synth_keeps_text_with_no_terminal_punctuation() {
        assert_eq!(
            normalize_for_synth("no ending punctuation here"),
            "no ending punctuation here"
        );
    }

    #[test]
    fn normalize_for_synth_empty_string_stays_empty() {
        assert_eq!(normalize_for_synth(""), "");
    }

    #[test]
    fn normalize_for_synth_multi_sentence_collapses_internal_whitespace() {
        assert_eq!(
            normalize_for_synth("Hello.   This  is\n\na  test. Done?"),
            "Hello. This is a test. Done?"
        );
    }

    #[tokio::test]
    async fn new_reports_missing_model_path_by_name() {
        let result = KokoroSynth::new("no/such/model.onnx", "no/such/voices").await;
        let Err(err) = result else {
            panic!("missing model path must error");
        };

        match err {
            TtsError::ModelNotFound(path) => {
                assert_eq!(path, PathBuf::from("no/such/model.onnx"));
            }
            other => panic!("expected ModelNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn new_reports_missing_voices_path_by_name() {
        // Use this file's own path as a stand-in for an existing model file
        // so the voices check is the one that trips.
        let model_path = file!();
        let result = KokoroSynth::new(model_path, "no/such/voices").await;
        let Err(err) = result else {
            panic!("missing voices path must error");
        };

        match err {
            TtsError::VoicesNotFound(path) => {
                assert_eq!(path, PathBuf::from("no/such/voices"));
            }
            other => panic!("expected VoicesNotFound, got {other:?}"),
        }
    }

    #[test]
    fn default_voice_id_is_set_before_any_synth() {
        // KokoroSynth cannot be constructed without a real model, so this
        // pins the constant that seeds `voice_id` on construction.
        assert_eq!(DEFAULT_VOICE_ID, "af_heart");
    }

    /// Requires a real Kokoro model and voices directory on disk. Gated
    /// behind `#[ignore]` since it depends on large downloaded files. See
    /// `docs/voice-setup.md`.
    #[tokio::test]
    #[ignore]
    async fn synth_returns_24khz_samples_with_a_real_model() {
        let synth = KokoroSynth::new("models/model.onnx", "voices")
            .await
            .expect("real model should load");
        let samples = synth
            .synth("Hello, world.")
            .await
            .expect("synth should succeed");
        assert!(!samples.is_empty());
    }

    /// A dead worker (receiver already dropped, standing in for a worker
    /// thread that exited after a failed setup) must not make any handle
    /// method panic. Every send just becomes a no-op.
    #[test]
    fn handle_with_dead_worker_discards_commands_without_panicking() {
        let (tx, rx) = mpsc::channel();
        drop(rx);
        let handle = TtsHandle { tx };

        handle.speak("hello");
        handle.stop();
        handle.set_voice("af_heart");
        handle.set_speed(1.2);
        assert!(handle.wait_until_drained());
        handle.shutdown();
    }

    /// `speak_blocking` against a dead worker must return 0 rather than
    /// hang forever waiting for a reply that will never come.
    #[test]
    fn speak_blocking_with_dead_worker_returns_zero_without_hanging() {
        let (tx, rx) = mpsc::channel();
        drop(rx);
        let handle = TtsHandle { tx };

        assert_eq!(handle.speak_blocking("hello"), 0);
    }

    /// `discard_forever` must return once the channel closes rather than
    /// hang, and must not panic on any command variant it sees first.
    #[test]
    fn discard_forever_drains_every_command_variant_then_returns() {
        let (tx, rx) = mpsc::channel();
        tx.send(Command::Speak("hi".into(), None)).unwrap();
        tx.send(Command::Stop).unwrap();
        tx.send(Command::SetVoice("af_heart".into())).unwrap();
        tx.send(Command::SetSpeed(1.5)).unwrap();
        let (drain_tx, _drain_rx) = mpsc::channel();
        tx.send(Command::WaitDrained(drain_tx)).unwrap();
        tx.send(Command::Shutdown).unwrap();
        drop(tx);

        discard_forever(rx);
    }
}
