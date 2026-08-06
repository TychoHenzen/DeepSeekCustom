//! Unit tests for `deepseek_custom::voice::playback` (`src/voice/playback.rs`).
//! Moved out of the production module as part of the two-crate workspace split.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use deepseek_custom::voice::playback::{
    PlaybackQueue, duplicate_channels, resample_mono, wait_for_empty,
};

#[test]
fn resample_mono_same_rate_is_unchanged() {
    let input = vec![0.1, 0.2, 0.3, 0.4];
    let out = resample_mono(&input, 24_000, 24_000);
    assert_eq!(out, input);
}

#[test]
fn resample_mono_upsamples_to_expected_length() {
    let input = vec![0.0, 1.0, 0.0, 1.0];
    let out = resample_mono(&input, 24_000, 48_000);
    assert_eq!(out.len(), 8);
}

#[test]
fn resample_mono_downsamples_to_expected_length() {
    let input = vec![0.0; 24_000];
    let out = resample_mono(&input, 24_000, 12_000);
    assert_eq!(out.len(), 12_000);
}

#[test]
fn resample_mono_empty_input_stays_empty() {
    let out = resample_mono(&[], 24_000, 48_000);
    assert!(out.is_empty());
}

#[test]
fn duplicate_channels_mono_is_unchanged() {
    let input = vec![0.1, 0.2, 0.3];
    let out = duplicate_channels(&input, 1);
    assert_eq!(out, input);
}

#[test]
fn duplicate_channels_stereo_interleaves_pairs() {
    let input = vec![0.1, 0.2];
    let out = duplicate_channels(&input, 2);
    assert_eq!(out, vec![0.1, 0.1, 0.2, 0.2]);
}

#[test]
fn playback_queue_pop_into_pads_with_silence_when_empty() {
    let mut queue = PlaybackQueue::default();
    queue.push_for_test(vec![1.0, 2.0]);
    let mut out = [0.0f32; 4];
    queue.pop_into_for_test(&mut out);
    assert_eq!(out, [1.0, 2.0, 0.0, 0.0]);
}

#[test]
fn playback_queue_pop_into_returns_queued_samples_in_order() {
    let mut queue = PlaybackQueue::default();
    queue.push_for_test(vec![1.0, 2.0, 3.0]);
    let mut out = [0.0f32; 2];
    queue.pop_into_for_test(&mut out);
    assert_eq!(out, [1.0, 2.0]);
    assert!(!queue.is_empty_for_test());
}

#[test]
fn playback_queue_clear_drops_everything_queued() {
    let mut queue = PlaybackQueue::default();
    queue.push_for_test(vec![1.0, 2.0, 3.0]);
    queue.clear_for_test();
    assert!(queue.is_empty_for_test());
    let mut out = [1.0f32; 2];
    queue.pop_into_for_test(&mut out);
    assert_eq!(out, [0.0, 0.0]);
}

#[test]
fn playback_queue_starts_empty() {
    let queue = PlaybackQueue::default();
    assert!(queue.is_empty_for_test());
}

#[test]
fn wait_for_empty_returns_at_once_when_queue_already_empty() {
    let queue = Arc::new(Mutex::new(PlaybackQueue::default()));
    let start = Instant::now();
    let drained = wait_for_empty(&queue, Duration::from_secs(5));
    assert!(drained);
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn wait_for_empty_returns_true_once_another_thread_empties_the_queue() {
    let queue = Arc::new(Mutex::new(PlaybackQueue::default()));
    queue.lock().unwrap().push_for_test(vec![0.0; 10]);

    let emptier = Arc::clone(&queue);
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        emptier.lock().unwrap().clear_for_test();
    });

    let start = Instant::now();
    let drained = wait_for_empty(&queue, Duration::from_secs(5));
    assert!(drained);
    assert!(start.elapsed() >= Duration::from_millis(50));
}

#[test]
fn wait_for_empty_times_out_when_queue_never_empties() {
    let queue = Arc::new(Mutex::new(PlaybackQueue::default()));
    queue.lock().unwrap().push_for_test(vec![0.0; 10]);

    let start = Instant::now();
    let drained = wait_for_empty(&queue, Duration::from_millis(50));
    let elapsed = start.elapsed();
    assert!(!drained);
    assert!(elapsed >= Duration::from_millis(50));
    assert!(elapsed < Duration::from_secs(2));
}
