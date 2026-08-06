//! Unit tests for `deepseek_custom::voice::stt` (`src/voice/stt.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::path::PathBuf;

use deepseek_custom::voice::stt::{MAX_THREADS, SttError, WhisperEngine, sensible_thread_count};

#[test]
fn new_reports_missing_model_path_by_name() {
    let result = WhisperEngine::new("no/such/model.bin");
    let Err(err) = result else {
        panic!("missing model path must error");
    };

    match err {
        SttError::ModelNotFound(path) => {
            assert_eq!(path, PathBuf::from("no/such/model.bin"));
        }
        other => panic!("expected ModelNotFound, got {other:?}"),
    }
}

#[test]
fn sensible_thread_count_stays_within_bounds() {
    let threads = sensible_thread_count();
    assert!(threads >= 1);
    assert!(threads <= MAX_THREADS);
}

/// Needs the real Whisper GGML model on disk. Download it first, see
/// `docs/voice-setup.md`. Run with `cargo test --features voice-models`.
#[test]
#[cfg(feature = "voice-models")]
fn transcribe_returns_text_with_a_real_model() {
    let engine = WhisperEngine::new("models/ggml-base.en.bin").expect("real model should load");
    let samples = vec![0.0_f32; 16_000];
    engine
        .transcribe(&samples)
        .expect("transcribe should succeed on silence");
}
