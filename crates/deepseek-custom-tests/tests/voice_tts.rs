//! Unit tests for `deepseek_custom::voice::tts` (`src/voice/tts.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::path::PathBuf;
use std::sync::mpsc;

use deepseek_custom::voice::tts::{
    Command, DEFAULT_VOICE_ID, KokoroSynth, SPEED_MAX, SPEED_MIN, TtsError, TtsHandle, clamp_speed,
    discard_forever, normalize_for_synth,
};

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
    // Use the crate's own Cargo.toml as a stand-in for an existing model
    // file so the voices check is the one that trips. Resolved from
    // CARGO_MANIFEST_DIR rather than file!(), since file!() resolves
    // workspace-root-relative while the test binary's cwd is the crate
    // directory, so the two no longer line up in a workspace.
    let model_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let result = KokoroSynth::new(&model_path, "no/such/voices").await;
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

/// Needs the real Kokoro model and voices directory on disk. Download
/// them first, see `docs/voice-setup.md`. Run with
/// `cargo test --features voice-models`.
#[tokio::test]
#[cfg(feature = "voice-models")]
async fn synth_returns_24khz_samples_with_a_real_model() {
    // Same as `main()`: use the bundled Misaki phonemizer, not the
    // espeak-ng subprocess, which drops the last phoneme of every line.
    unsafe {
        std::env::set_var("KOKORO_G2P_SEGMENT_ESPEAK", "0");
    }
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
    let handle = TtsHandle::new_for_test(tx);

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
    let handle = TtsHandle::new_for_test(tx);

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
