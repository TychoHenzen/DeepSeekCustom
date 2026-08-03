//! Local speech to text via whisper.cpp, through the `whisper-rs` bindings.
//!
//! Loads a GGML Whisper model once via [`WhisperEngine::new`], then turns
//! 16 kHz mono f32 samples into plain English text via
//! [`WhisperEngine::transcribe`]. Mirrors the loading and error-reporting
//! pattern used by `super::tts::KokoroSynth`, so the two engines read alike.

use std::path::{Path, PathBuf};

use thiserror::Error;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperError,
    WhisperState,
};

/// Upper bound on the thread count handed to whisper.cpp, so transcription
/// does not claim every core on a large machine.
const MAX_THREADS: i32 = 8;

/// Thread count used when the platform cannot report available parallelism.
const FALLBACK_THREADS: i32 = 4;

/// Errors produced while loading the Whisper model or transcribing audio.
#[derive(Error, Debug)]
pub enum SttError {
    #[error("Whisper model file not found: {0}")]
    ModelNotFound(PathBuf),
    #[error("Whisper transcription failed: {0}")]
    Transcribe(#[from] WhisperError),
}

/// Loads a GGML Whisper model once and turns 16 kHz mono f32 samples into
/// plain English text.
pub struct WhisperEngine {
    ctx: WhisperContext,
}

impl WhisperEngine {
    /// Load the Whisper model from disk.
    ///
    /// `model_path` must point at a GGML Whisper model file. If it is
    /// missing, returns an error naming the exact path that was tried.
    pub fn new(model_path: impl AsRef<Path>) -> Result<Self, SttError> {
        let model_path = model_path.as_ref();
        if !model_path.exists() {
            return Err(SttError::ModelNotFound(model_path.to_path_buf()));
        }

        let mut params = WhisperContextParameters::default();
        // Ask for the GPU backend explicitly, rather than relying on
        // whisper-rs's own feature-gated default. whisper.cpp falls back to
        // CPU on its own when no GPU backend is compiled in. It also falls
        // back when no GPU device is found. So this is safe either way.
        params.use_gpu(true);
        let ctx = WhisperContext::new_with_params(model_path, params)?;
        Ok(Self { ctx })
    }

    /// Transcribe 16 kHz mono f32 samples into plain English text.
    ///
    /// Creates a fresh whisper.cpp state per call, so this may be called
    /// many times against the same loaded model.
    pub fn transcribe(&self, samples: &[f32]) -> Result<String, SttError> {
        let mut state = self.ctx.create_state()?;
        let params = build_params();
        state.full(params, samples)?;
        Ok(collect_text(&state))
    }
}

/// Build the whisper.cpp run parameters: greedy sampling, English only, no
/// printed progress or timestamp output, and a sensible thread count.
fn build_params<'a, 'b>() -> FullParams<'a, 'b> {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("en"));
    params.set_print_progress(false);
    params.set_print_timestamps(false);
    params.set_print_special(false);
    params.set_print_realtime(false);
    params.set_n_threads(sensible_thread_count());
    params
}

/// Pick a sensible thread count for whisper.cpp: the number of available
/// cores, capped so transcription does not starve everything else running
/// alongside it.
fn sensible_thread_count() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(FALLBACK_THREADS)
        .min(MAX_THREADS)
}

/// Join every segment's text into one plain string. A segment that fails to
/// decode to UTF-8 is skipped rather than failing the whole transcription.
fn collect_text(state: &WhisperState) -> String {
    let mut out = String::new();
    for i in 0..state.full_n_segments() {
        let Some(segment) = state.get_segment(i) else {
            continue;
        };
        let Ok(text) = segment.to_str() else {
            continue;
        };
        out.push_str(text);
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    /// `docs/voice-setup.md`.
    #[test]
    fn transcribe_returns_text_with_a_real_model() {
        let engine = WhisperEngine::new("models/ggml-base.en.bin").expect("real model should load");
        let samples = vec![0.0_f32; 16_000];
        engine
            .transcribe(&samples)
            .expect("transcribe should succeed on silence");
    }
}
