//! Unit tests for `deepseek_custom::voice::capture` (`src/voice/capture.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use deepseek_custom::voice::capture::{RingBuffer, downmix_into, resample_mono_into};

/// Pure wrapper around [`resample_mono_into`], mirroring the
/// `#[cfg(test)]`-only helper the production module used to carry.
fn resample_mono(samples: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    let mut out = Vec::new();
    resample_mono_into(&mut out, samples, src_rate, dst_rate);
    out
}

/// Pure wrapper around [`downmix_into`], mirroring the
/// `#[cfg(test)]`-only helper the production module used to carry.
fn downmix_to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
    let mut out = Vec::new();
    downmix_into(&mut out, interleaved, channels);
    out
}

#[test]
fn resample_mono_same_rate_is_unchanged() {
    let input = vec![0.1, 0.2, 0.3, 0.4];
    let out = resample_mono(&input, 16_000, 16_000);
    assert_eq!(out, input);
}

#[test]
fn resample_mono_downsamples_device_rate_to_16k() {
    let input = vec![0.0; 48_000];
    let out = resample_mono(&input, 48_000, 16_000);
    assert_eq!(out.len(), 16_000);
}

#[test]
fn resample_mono_upsamples_below_16k_device_rate() {
    let input = vec![0.0, 1.0, 0.0, 1.0];
    let out = resample_mono(&input, 8_000, 16_000);
    assert_eq!(out.len(), 8);
}

#[test]
fn resample_mono_empty_input_stays_empty() {
    let out = resample_mono(&[], 44_100, 16_000);
    assert!(out.is_empty());
}

#[test]
fn downmix_to_mono_passthrough_when_already_mono() {
    let input = vec![0.1, 0.2, 0.3];
    let out = downmix_to_mono(&input, 1);
    assert_eq!(out, input);
}

#[test]
fn downmix_to_mono_averages_stereo_pairs() {
    let input = vec![0.0, 1.0, 1.0, 1.0];
    let out = downmix_to_mono(&input, 2);
    assert_eq!(out, vec![0.5, 1.0]);
}

#[test]
fn downmix_to_mono_averages_three_channels() {
    let input = vec![0.0, 0.3, 0.6];
    let out = downmix_to_mono(&input, 3);
    assert!((out[0] - 0.3).abs() < 1e-6);
}

#[test]
fn ring_buffer_starts_empty() {
    let mut buf = RingBuffer::new_for_test(4);
    assert_eq!(buf.drain_for_test().len(), 0);
}

#[test]
fn ring_buffer_push_overwrites_oldest_when_full() {
    let mut buf = RingBuffer::new_for_test(3);
    buf.push_slice_for_test(&[1.0, 2.0, 3.0, 4.0, 5.0]);
    assert_eq!(buf.drain_for_test(), vec![3.0, 4.0, 5.0]);
}

#[test]
fn ring_buffer_drain_empties_the_buffer() {
    let mut buf = RingBuffer::new_for_test(4);
    buf.push_slice_for_test(&[1.0, 2.0]);
    let drained = buf.drain_for_test();
    assert_eq!(drained, vec![1.0, 2.0]);
    assert!(buf.drain_for_test().is_empty());
}

#[test]
fn ring_buffer_peek_recent_leaves_buffer_intact() {
    let mut buf = RingBuffer::new_for_test(4);
    buf.push_slice_for_test(&[1.0, 2.0, 3.0]);
    let peeked = buf.peek_recent_for_test(2);
    assert_eq!(peeked, vec![2.0, 3.0]);
    assert_eq!(buf.drain_for_test(), vec![1.0, 2.0, 3.0]);
}

#[test]
fn ring_buffer_peek_recent_clamps_to_available_samples() {
    let mut buf = RingBuffer::new_for_test(4);
    buf.push_slice_for_test(&[1.0, 2.0]);
    let peeked = buf.peek_recent_for_test(10);
    assert_eq!(peeked, vec![1.0, 2.0]);
}
