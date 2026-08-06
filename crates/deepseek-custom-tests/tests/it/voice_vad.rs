//! Unit tests for `deepseek_custom::voice::vad` (`src/voice/vad.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use deepseek_custom::voice::vad::{VadEvent, VadState};

/// 30 ms of 16 kHz audio, the frame size the default config uses.
const FRAME_SAMPLES: usize = 480;

fn silence(frames: usize) -> Vec<f32> {
    vec![0.0; frames * FRAME_SAMPLES]
}

/// Loud alternating signal, well above the default threshold.
fn speech(frames: usize) -> Vec<f32> {
    (0..frames * FRAME_SAMPLES)
        .map(|i| if i % 2 == 0 { 0.3 } else { -0.3 })
        .collect()
}

/// Quieter, still-alternating signal used to model a noisy room.
fn noise(frames: usize) -> Vec<f32> {
    (0..frames * FRAME_SAMPLES)
        .map(|i| if i % 2 == 0 { 0.15 } else { -0.1 })
        .collect()
}

#[test]
fn pure_silence_produces_no_utterance() {
    let mut vad = VadState::default();
    let events = vad.process_chunk(&silence(50));
    assert!(events.is_empty());
}

#[test]
fn speech_burst_reports_utterance_started() {
    let mut vad = VadState::default();
    // 300 ms minimum is 10 frames. Give it comfortably more.
    let events = vad.process_chunk(&speech(15));
    assert_eq!(events, vec![VadEvent::UtteranceStarted]);
}

#[test]
fn speech_then_enough_silence_reports_utterance_ended() {
    let mut vad = VadState::default();
    let mut events = vad.process_chunk(&speech(15));
    // 800 ms hangover is 27 frames (ceil(800/30)). Give it comfortably more.
    events.extend(vad.process_chunk(&silence(30)));
    assert_eq!(
        events,
        vec![VadEvent::UtteranceStarted, VadEvent::UtteranceEnded]
    );
}

#[test]
fn short_burst_below_minimum_length_is_rejected() {
    let mut vad = VadState::default();
    // 5 frames = 150 ms, below the 300 ms minimum.
    let mut events = vad.process_chunk(&speech(5));
    events.extend(vad.process_chunk(&silence(30)));
    assert!(events.is_empty());
}

#[test]
fn trailing_silence_shorter_than_hangover_does_not_end_utterance() {
    let mut vad = VadState::default();
    let mut events = vad.process_chunk(&speech(15));
    // Well under the 27-frame hangover.
    events.extend(vad.process_chunk(&silence(5)));
    assert_eq!(events, vec![VadEvent::UtteranceStarted]);
}

#[test]
fn calibrated_noise_floor_does_not_read_as_permanent_speech() {
    let mut vad = VadState::default();
    vad.calibrate(&noise(20));
    let events = vad.process_chunk(&noise(50));
    assert!(events.is_empty());
}

#[test]
fn uncalibrated_state_treats_same_noise_level_as_speech() {
    // Sanity check that the noise level used above genuinely needs
    // calibration, so the calibrated test is not vacuously true.
    let mut vad = VadState::default();
    let events = vad.process_chunk(&noise(15));
    assert_eq!(events, vec![VadEvent::UtteranceStarted]);
}
