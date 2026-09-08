//! Audio output for synthesized speech.
//!
//! Opens the default cpal output device and plays queued f32 sample
//! buffers. Kokoro produces 24000 Hz mono. [`AudioSink::enqueue`] resamples
//! and channel-duplicates each buffer to the device's actual config before
//! it ever reaches the shared queue, so the audio callback itself only ever
//! pops pre-shaped samples. It never blocks on synthesis and never
//! allocates.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use thiserror::Error;
use tracing::error;

/// Sample rate Kokoro always produces.
const KOKORO_SAMPLE_RATE: u32 = 24_000;

/// How long to sleep between checks while waiting for the playback queue to
/// drain. This is a caller-side poll only. It never runs on the audio
/// callback thread.
const DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Extra time to wait once the queue reports empty. An empty queue only
/// means the audio callback popped the last real sample. The device still
/// has to physically play out whatever it was last handed. Typical device
/// buffers run to a few tens of milliseconds. This padding is generous
/// next to that, so the true tail of playback is never cut short.
const DRAIN_TAIL_PADDING: Duration = Duration::from_millis(150);

/// Ceiling on how long a drain wait can block, so a stalled or closed
/// output device can never hang the caller forever.
const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Errors from opening or configuring the audio output stream.
#[derive(Error, Debug)]
pub enum PlaybackError {
    #[error("no default output audio device found")]
    NoOutputDevice,
    #[error("failed to query default output config: {0}")]
    DefaultConfig(#[from] cpal::DefaultStreamConfigError),
    #[error("output device does not support f32 samples (found {0:?})")]
    UnsupportedSampleFormat(SampleFormat),
    #[error("failed to build output stream: {0}")]
    BuildStream(#[from] cpal::BuildStreamError),
    #[error("failed to start output stream: {0}")]
    PlayStream(#[from] cpal::PlayStreamError),
}

/// Shared playback queue drained by the audio callback.
///
/// `pub`: the moved queue tests, now in the external test crate, build
/// and drain one directly. Its field stays private; the audio callback
/// path (`pop_into`) and every other method keep their original
/// visibility, reached only through the `_for_test` wrappers below.
#[derive(Default)]
pub struct PlaybackQueue {
    samples: VecDeque<f32>,
}

impl PlaybackQueue {
    /// Fill `output` from the queue, padding with silence once it is empty.
    /// Called from the audio callback: no allocation, no blocking work.
    fn pop_into(&mut self, output: &mut [f32]) {
        for slot in output.iter_mut() {
            *slot = self.samples.pop_front().unwrap_or(0.0);
        }
    }

    fn push(&mut self, samples: Vec<f32>) {
        self.samples.extend(samples);
    }

    fn clear(&mut self) {
        self.samples.clear();
    }

    fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Test seam for [`Self::pop_into`]. The real audio callback in
    /// `build_stream` below is the only production caller, and it runs
    /// unconditionally on the audio thread, so `pop_into` keeps its
    /// original visibility and this thin wrapper is the only way a test
    /// outside the crate can reach it.
    #[cfg(feature = "test-support")]
    pub fn pop_into_for_test(&mut self, output: &mut [f32]) {
        self.pop_into(output)
    }

    /// Test seam for [`Self::push`]. See [`Self::pop_into_for_test`]:
    /// `AudioSink::enqueue` is the only production caller and calls it
    /// unconditionally.
    #[cfg(feature = "test-support")]
    pub fn push_for_test(&mut self, samples: Vec<f32>) {
        self.push(samples)
    }

    /// Test seam for [`Self::clear`]. See [`Self::pop_into_for_test`]:
    /// `AudioSink::clear` is the only production caller and calls it
    /// unconditionally.
    #[cfg(feature = "test-support")]
    pub fn clear_for_test(&mut self) {
        self.clear()
    }

    /// Test seam for [`Self::is_empty`]. See [`Self::pop_into_for_test`]:
    /// [`wait_for_empty`] is the production caller and calls it
    /// unconditionally.
    #[cfg(feature = "test-support")]
    pub fn is_empty_for_test(&self) -> bool {
        self.is_empty()
    }
}

/// Plays queued f32 sample buffers through the default output device.
pub struct AudioSink {
    queue: Arc<Mutex<PlaybackQueue>>,
    device_sample_rate: u32,
    device_channels: u16,
    _stream: Stream,
}

impl AudioSink {
    /// Open the default output device and start playback immediately. The
    /// returned sink plays silence until samples are enqueued.
    pub fn start() -> Result<Self, PlaybackError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(PlaybackError::NoOutputDevice)?;
        let supported = device.default_output_config()?;

        if supported.sample_format() != SampleFormat::F32 {
            return Err(PlaybackError::UnsupportedSampleFormat(
                supported.sample_format(),
            ));
        }

        let device_sample_rate = supported.sample_rate().0;
        let device_channels = supported.channels();
        let stream_config: StreamConfig = supported.config();

        let queue = Arc::new(Mutex::new(PlaybackQueue::default()));
        let stream = build_stream(&device, &stream_config, Arc::clone(&queue))?;
        stream.play()?;

        Ok(Self {
            queue,
            device_sample_rate,
            device_channels,
            _stream: stream,
        })
    }

    /// Resample and channel-duplicate `samples` (24000 Hz mono, as Kokoro
    /// produces) to the device's config, then append to the play queue.
    pub fn enqueue(&self, samples: Vec<f32>) {
        let shaped = resample_and_duplicate(
            &samples,
            KOKORO_SAMPLE_RATE,
            self.device_sample_rate,
            self.device_channels,
        );
        if let Ok(mut queue) = self.queue.lock() {
            queue.push(shaped);
        } else {
            error!("audio playback queue lock poisoned, dropping enqueued audio");
        }
    }

    /// Drop everything queued so playback goes silent immediately.
    pub fn clear(&self) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.clear();
        } else {
            error!("audio playback queue lock poisoned, could not clear");
        }
    }

    /// Block until every sample enqueued so far has actually played, not
    /// just been popped off the queue. Uses a generous internal timeout, so
    /// a stalled or closed output device can never hang the caller forever.
    /// Returns true if playback drained. Returns false if the timeout fired
    /// first.
    pub fn wait_until_drained(&self) -> bool {
        self.wait_until_drained_timeout(DEFAULT_DRAIN_TIMEOUT)
    }

    /// Same as `wait_until_drained`, with an explicit timeout in place of
    /// the default.
    pub fn wait_until_drained_timeout(&self, timeout: Duration) -> bool {
        wait_for_empty(&self.queue, timeout)
    }
}

/// Poll `queue` until it reports empty or `timeout` elapses, then wait a
/// bit longer for the device to finish playing the samples it was already
/// handed. Returns true if the queue drained before the timeout fired.
/// Returns false if the timeout fired first. Runs on the calling thread
/// only. It is never called from, and never touches, the audio callback.
/// `pub`: the moved drain tests call this directly; it is also used
/// unconditionally by `AudioSink::wait_until_drained_timeout` above.
pub fn wait_for_empty(queue: &Arc<Mutex<PlaybackQueue>>, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        let empty = queue.lock().map(|q| q.is_empty()).unwrap_or(true);
        if empty {
            thread::sleep(DRAIN_TAIL_PADDING);
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        thread::sleep(DRAIN_POLL_INTERVAL);
    }
}

/// Build the cpal output stream. The callback pops pre-shaped samples out
/// of `queue` and never allocates or blocks on anything but the mutex.
fn build_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    queue: Arc<Mutex<PlaybackQueue>>,
) -> Result<Stream, PlaybackError> {
    let stream = device.build_output_stream(
        config,
        move |output: &mut [f32], _info: &cpal::OutputCallbackInfo| match queue.lock() {
            Ok(mut q) => q.pop_into(output),
            Err(_) => output.fill(0.0),
        },
        |err| error!("audio output stream error: {err}"),
        None,
    )?;
    Ok(stream)
}

/// Linearly resample mono `samples` from `src_rate` to `dst_rate`, then
/// duplicate each frame across `dst_channels` to produce interleaved audio.
/// Pure and allocation-only on its own stack. Safe to call off the audio
/// thread and to unit test without any device.
fn resample_and_duplicate(
    samples: &[f32],
    src_rate: u32,
    dst_rate: u32,
    dst_channels: u16,
) -> Vec<f32> {
    let resampled = resample_mono(samples, src_rate, dst_rate);
    duplicate_channels(&resampled, dst_channels)
}

/// Linear-interpolation resample of a mono buffer. `pub`: the moved
/// resample tests call this directly; it is also used unconditionally by
/// `resample_and_duplicate` above.
pub fn resample_mono(samples: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    if samples.is_empty() || src_rate == dst_rate {
        return samples.to_vec();
    }

    let ratio = dst_rate as f64 / src_rate as f64;
    let dst_len = ((samples.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(dst_len);

    for i in 0..dst_len {
        let src_pos = i as f64 / ratio;
        let idx = src_pos.floor() as usize;
        let frac = (src_pos - idx as f64) as f32;
        let a = samples.get(idx).copied().unwrap_or(0.0);
        let b = samples.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}

/// Repeat each mono sample across `channels` interleaved slots. `pub`:
/// the moved duplication tests call this directly; it is also used
/// unconditionally by `resample_and_duplicate` above.
pub fn duplicate_channels(mono: &[f32], channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return mono.to_vec();
    }

    let channels = channels as usize;
    let mut out = Vec::with_capacity(mono.len() * channels);
    for &sample in mono {
        for _ in 0..channels {
            out.push(sample);
        }
    }
    out
}
