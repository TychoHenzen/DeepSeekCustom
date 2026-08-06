//! This module detects voice activity by tracking frame energy. [`VadState`] takes 16000 Hz mono
//! f32 sample chunks as they arrive from [`super::capture::AudioCapture`]
//! and turns them into [`VadEvent::UtteranceStarted`] /
//! [`VadEvent::UtteranceEnded`] decisions. It opens no audio device and
//! touches no hardware: pure sample buffers in, decisions out.
//!
//! Samples are grouped into fixed-size frames (30 ms by default). Each
//! frame's RMS energy is compared against a threshold, either the built-in
//! default or one set by [`VadState::calibrate`] from a short noise-floor
//! sample, so a noisy room does not permanently read as speech. A short
//! burst above threshold does not immediately count as an utterance: it
//! only becomes one once a minimum run of speech frames accumulates,
//! which rejects clicks and pops. Once an utterance has started, it ends
//! only after a matching run of trailing silence frames (the "hangover").

use std::time::Duration;

/// Default frame length used for RMS energy analysis.
const DEFAULT_FRAME_DURATION: Duration = Duration::from_millis(30);

/// Default trailing silence required to end an utterance.
const DEFAULT_SILENCE_HANGOVER: Duration = Duration::from_millis(800);

/// Default minimum speech run required to count as an utterance.
const DEFAULT_MIN_UTTERANCE_DURATION: Duration = Duration::from_millis(300);

/// Sample rate the voice pipeline standardizes on.
const DEFAULT_SAMPLE_RATE: u32 = 16_000;

/// RMS energy threshold used until [`VadState::calibrate`] is called.
const DEFAULT_ENERGY_THRESHOLD: f32 = 0.02;

/// How far above the measured noise floor the calibrated threshold sits.
const DEFAULT_CALIBRATION_MULTIPLIER: f32 = 3.0;

/// Decisions [`VadState`] reports as frames are classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadEvent {
    /// A speech run reached the minimum utterance length.
    UtteranceStarted,
    /// Trailing silence after a started utterance reached the hangover.
    UtteranceEnded,
}

/// Tunable thresholds and durations for [`VadState`].
#[derive(Debug, Clone)]
pub struct VadConfig {
    /// Frame length used for RMS energy analysis.
    pub frame_duration: Duration,
    /// Sample rate of the incoming audio.
    pub sample_rate: u32,
    /// Trailing silence required to end an utterance.
    pub silence_hangover: Duration,
    /// Minimum speech run required to count as an utterance.
    pub min_utterance_duration: Duration,
    /// RMS energy threshold used until calibrated.
    pub default_energy_threshold: f32,
    /// Multiplier applied to the measured noise floor RMS on calibration.
    pub calibration_multiplier: f32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            frame_duration: DEFAULT_FRAME_DURATION,
            sample_rate: DEFAULT_SAMPLE_RATE,
            silence_hangover: DEFAULT_SILENCE_HANGOVER,
            min_utterance_duration: DEFAULT_MIN_UTTERANCE_DURATION,
            default_energy_threshold: DEFAULT_ENERGY_THRESHOLD,
            calibration_multiplier: DEFAULT_CALIBRATION_MULTIPLIER,
        }
    }
}

/// Internal endpointing state. Not part of the public API.
#[derive(Debug, Clone)]
enum Inner {
    /// No speech seen recently.
    Idle,
    /// Speech seen, but not yet long enough to count as an utterance.
    Candidate {
        speech_frames: u32,
        silence_run: u32,
    },
    /// An utterance has started. This tracks trailing silence.
    Active { silence_run: u32 },
}

/// Streaming energy-based endpointer. Feed it successive sample chunks via
/// [`VadState::process_chunk`]. State persists across calls, so chunks need
/// not align to frame boundaries or cover a whole utterance.
#[derive(Debug, Clone)]
pub struct VadState {
    frame_samples: usize,
    hangover_frames: u32,
    min_speech_frames: u32,
    calibration_multiplier: f32,
    threshold: f32,
    scratch: Vec<f32>,
    inner: Inner,
}

impl Default for VadState {
    fn default() -> Self {
        Self::new(VadConfig::default())
    }
}

impl VadState {
    /// Build a new endpointer from `config`. Uses `config.default_energy_threshold`
    /// until [`VadState::calibrate`] is called.
    pub fn new(config: VadConfig) -> Self {
        let frame_samples = frame_len_samples(&config);
        let hangover_frames = duration_to_frames(config.silence_hangover, config.frame_duration);
        let min_speech_frames =
            duration_to_frames(config.min_utterance_duration, config.frame_duration);
        Self {
            frame_samples,
            hangover_frames,
            min_speech_frames,
            calibration_multiplier: config.calibration_multiplier,
            threshold: config.default_energy_threshold,
            scratch: Vec::with_capacity(frame_samples * 2),
            inner: Inner::Idle,
        }
    }

    /// Calibrate the speech/silence threshold from a short sample of the
    /// room's noise floor (silence, background hum, fan noise, and so on).
    /// Does not reset any in-progress utterance state.
    pub fn calibrate(&mut self, noise_floor: &[f32]) {
        let noise_rms = rms(noise_floor);
        self.threshold = noise_rms * self.calibration_multiplier;
    }

    /// Feed the next chunk of 16 kHz mono f32 samples. Returns every
    /// [`VadEvent`] produced by the frames completed in this call, in
    /// order. Leftover samples that do not fill a whole frame are kept
    /// for the next call.
    pub fn process_chunk(&mut self, chunk: &[f32]) -> Vec<VadEvent> {
        self.scratch.extend_from_slice(chunk);

        let mut events = Vec::new();
        let mut offset = 0;
        while self.scratch.len() - offset >= self.frame_samples {
            let frame_end = offset + self.frame_samples;
            if let Some(event) = self.process_frame_at(offset, frame_end) {
                events.push(event);
            }
            offset = frame_end;
        }
        self.scratch.drain(..offset);
        events
    }

    /// Classify and advance the state machine for one frame.
    fn process_frame_at(&mut self, start: usize, end: usize) -> Option<VadEvent> {
        let is_speech = rms(&self.scratch[start..end]) > self.threshold;
        let current = std::mem::replace(&mut self.inner, Inner::Idle);
        let (next_state, event) = match current {
            Inner::Idle if is_speech => self.enter_speech(1),
            Inner::Idle => (Inner::Idle, None),
            Inner::Candidate {
                speech_frames,
                silence_run,
            } => self.advance_candidate(is_speech, speech_frames, silence_run),
            Inner::Active { silence_run } => self.advance_active(is_speech, silence_run),
        };
        self.inner = next_state;
        event
    }

    /// Enter or extend a speech run. Promotes to `Active` (and reports
    /// `UtteranceStarted`) once `speech_frames` reaches the minimum.
    fn enter_speech(&self, speech_frames: u32) -> (Inner, Option<VadEvent>) {
        if speech_frames >= self.min_speech_frames {
            (
                Inner::Active { silence_run: 0 },
                Some(VadEvent::UtteranceStarted),
            )
        } else {
            (
                Inner::Candidate {
                    speech_frames,
                    silence_run: 0,
                },
                None,
            )
        }
    }

    /// Advance a `Candidate` (not-yet-started) run by one frame. A silence
    /// run reaching the hangover before the minimum is hit drops the
    /// candidate silently: it never counted as an utterance.
    fn advance_candidate(
        &self,
        is_speech: bool,
        speech_frames: u32,
        silence_run: u32,
    ) -> (Inner, Option<VadEvent>) {
        if is_speech {
            return self.enter_speech(speech_frames + 1);
        }
        let silence_run = silence_run + 1;
        if silence_run >= self.hangover_frames {
            (Inner::Idle, None)
        } else {
            (
                Inner::Candidate {
                    speech_frames,
                    silence_run,
                },
                None,
            )
        }
    }

    /// Advance an `Active` (already-started) run by one frame. A silence
    /// run reaching the hangover ends the utterance.
    fn advance_active(&self, is_speech: bool, silence_run: u32) -> (Inner, Option<VadEvent>) {
        if is_speech {
            return (Inner::Active { silence_run: 0 }, None);
        }
        let silence_run = silence_run + 1;
        if silence_run >= self.hangover_frames {
            (Inner::Idle, Some(VadEvent::UtteranceEnded))
        } else {
            (Inner::Active { silence_run }, None)
        }
    }
}

/// Frame length in samples for `config`, at least one sample.
fn frame_len_samples(config: &VadConfig) -> usize {
    let raw = config.sample_rate as f64 * config.frame_duration.as_secs_f64();
    (raw.round() as usize).max(1)
}

/// Number of frames needed to cover `duration` at `frame_duration` each,
/// rounded up, at least one frame.
fn duration_to_frames(duration: Duration, frame_duration: Duration) -> u32 {
    let frames = duration.as_secs_f64() / frame_duration.as_secs_f64();
    (frames.ceil() as u32).max(1)
}

/// Root-mean-square energy of `samples`. Empty input has zero energy.
fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|&s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}
