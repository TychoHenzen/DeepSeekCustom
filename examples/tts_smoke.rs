//! Smoke test for `TtsHandle`.
//!
//! Speaks a short line, then a long line that gets cut off after one second
//! to prove barge-in works, then a final line at a faster speed. Requires a
//! real Kokoro model, a voices directory, and working speakers. A human
//! must listen and confirm each stage sounds right, including that the last
//! word of stages 1 and 3 is not cut off.
//!
//! Every stage uses `speak_blocking`, then `TtsHandle::wait_until_drained`
//! to wait for the sink's queue to genuinely empty, instead of a guessed
//! fixed sleep or a sleep computed from the sample count. A fixed sleep let
//! banners race ahead of synthesis (which takes about 3.3 seconds per line
//! on CPU), and a sleep computed from enqueue time (rather than real
//! playback start) clipped the last word of an utterance, since playback
//! starts slightly after `speak_blocking` returns. Waiting on the real
//! drain, then joining the worker thread before the process exits, closes
//! both gaps.
//!
//! Usage:
//!   cargo run --example tts_smoke -- <model_path> <voices_path>
//! or set the KOKORO_MODEL_PATH and KOKORO_VOICES_PATH environment
//! variables and run with no arguments.

use std::env;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use DeepSeekCustom::voice::tts::TtsHandle;

/// Used when no argument and no environment variable is given, so the example
/// can be launched straight from an IDE with no run configuration.
/// model.onnx is the fp32 variant. It is both the best sounding and the
/// fastest here. About 0.25s per line on CUDA and 1.0s on CPU, against 3.7s
/// for the int8 model_quantized.onnx. model_q8f16.onnx is unusable. It
/// crashes ONNX Runtime with an access violation while the session is built.
const DEFAULT_MODEL: &str = "models/model.onnx";
const DEFAULT_VOICES: &str = "voices";

/// Kokoro always produces 24 kHz mono samples. Used only to print the
/// computed playback duration next to the real drain wait for comparison,
/// never to drive a sleep.
const SAMPLE_RATE_HZ: f32 = 24_000.0;

const BARGE_IN_WAIT: Duration = Duration::from_secs(1);
const AFTER_STOP_WAIT: Duration = Duration::from_secs(1);

fn main() {
    tracing_subscriber::fmt::init();

    let (model_path, voices_path) = match resolve_paths() {
        Ok(paths) => paths,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };

    println!("Kokoro model: {}", model_path.display());
    println!("Kokoro voices: {}", voices_path.display());

    let (handle, worker) = TtsHandle::start(model_path, voices_path);

    speak_short_line(&handle);
    speak_and_interrupt_long_line(&handle);
    speak_fast_line(&handle);

    handle.shutdown();
    // Wait for the worker to actually exit. All synth calls above already
    // completed by the time we get here, so this should return almost at
    // once. It exists so the process can never exit while the worker is
    // still mid-call inside the native ONNX Runtime library.
    let _ = worker.join();
    println!("\nDone.");
}

fn speak_short_line(handle: &TtsHandle) {
    println!("\n[1/3] You should hear one short sentence, played once, in full.");
    let samples =
        handle.speak_blocking("Hello. This is a short line to test text to speech playback.");
    println!("  stage 1 produced {samples} samples");
    report_drain_wait(1, handle, samples);
}

fn speak_and_interrupt_long_line(handle: &TtsHandle) {
    println!(
        "\n[2/3] You should hear a long sentence start, then cut off abruptly \
         after about one second (barge-in test)."
    );
    let samples = handle.speak_blocking(
        "This is a much longer line that should take several seconds to speak in \
         full, which gives us enough time to interrupt it before it ever finishes \
         on its own.",
    );
    println!("  stage 2 produced {samples} samples before barge-in");
    // speak_blocking only returns once synthesis is done and the audio is
    // enqueued, so playback has genuinely started here. Now it is safe to
    // wait a moment into real playback before cutting it off.
    thread::sleep(BARGE_IN_WAIT);
    handle.stop();
    thread::sleep(AFTER_STOP_WAIT);
}

fn speak_fast_line(handle: &TtsHandle) {
    println!("\n[3/3] You should hear this line played back noticeably faster than the others.");
    handle.set_speed(1.8);
    let samples = handle.speak_blocking(
        "This final line should play back noticeably faster than the earlier ones.",
    );
    println!("  stage 3 produced {samples} samples");
    report_drain_wait(3, handle, samples);
}

/// Wait for the sink to genuinely drain, then print the wall-clock time
/// that took next to the playback duration computed from `samples`. A real
/// wait shorter than the computed duration means the tail is still cut.
fn report_drain_wait(stage: u8, handle: &TtsHandle, samples: usize) {
    let computed = Duration::from_secs_f32(samples as f32 / SAMPLE_RATE_HZ);
    let start = Instant::now();
    let drained = handle.wait_until_drained();
    let actual = start.elapsed();
    println!(
        "  stage {stage} drain wait: {actual:.3?} actual vs {computed:.3?} computed from \
         sample count (drained={drained})"
    );
}

/// Resolve the model and voices paths from CLI args, falling back to the
/// KOKORO_MODEL_PATH / KOKORO_VOICES_PATH environment variables. Returns an
/// error naming exactly which path is missing or which argument is absent.
fn resolve_paths() -> Result<(PathBuf, PathBuf), String> {
    let mut args = env::args().skip(1);
    let model_arg = args
        .next()
        .or_else(|| env::var("KOKORO_MODEL_PATH").ok())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let voices_arg = args
        .next()
        .or_else(|| env::var("KOKORO_VOICES_PATH").ok())
        .unwrap_or_else(|| DEFAULT_VOICES.to_string());

    let model_path = PathBuf::from(model_arg);
    let voices_path = PathBuf::from(voices_arg);

    if !model_path.exists() {
        return Err(format!(
            "Kokoro model not found at: {}",
            model_path.display()
        ));
    }
    if !voices_path.exists() {
        return Err(format!(
            "Kokoro voices not found at: {}",
            voices_path.display()
        ));
    }

    Ok((model_path, voices_path))
}
