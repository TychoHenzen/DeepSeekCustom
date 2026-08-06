//! Microphone audio capture for local speech to text. [`AudioCapture`]
//! reads the default cpal input device, downmixes and resamples each
//! callback to 16000 Hz mono using scratch buffers reserved once (so the
//! callback never allocates), then pushes the result into a shared
//! bounded ring buffer.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig};
use thiserror::Error;
use tracing::error;

/// Sample rate whisper needs.
const WHISPER_SAMPLE_RATE: u32 = 16_000;

/// Ring buffer capacity in samples at 16000 Hz. Thirty seconds covers an
/// utterance and the wake-word window, while still bounding memory.
const RING_BUFFER_CAPACITY_SAMPLES: usize = 30 * WHISPER_SAMPLE_RATE as usize;

/// Per-callback scratch capacity, reserved once and reused every callback
/// so the audio thread never allocates in steady state.
const SCRATCH_CAPACITY_SAMPLES: usize = 8192;

/// Errors from opening or configuring the audio input stream.
#[derive(Error, Debug)]
pub enum CaptureError {
    #[error("no default input audio device found")]
    NoInputDevice,
    #[error("failed to query default input config: {0}")]
    DefaultConfig(#[from] cpal::DefaultStreamConfigError),
    #[error("input device reports unsupported sample format: {0:?}")]
    UnsupportedSampleFormat(SampleFormat),
    #[error("failed to build input stream: {0}")]
    BuildStream(#[from] cpal::BuildStreamError),
    #[error("failed to start input stream: {0}")]
    PlayStream(#[from] cpal::PlayStreamError),
}

/// Bounded ring buffer of f32 samples. Once full, pushing drops the
/// oldest samples, since recent audio is what matters most.
///
/// `pub`: the moved ring-buffer tests, now in the external test crate,
/// build and drain one directly. Its fields stay private; every method
/// keeps its original visibility, reached only through the `_for_test`
/// wrappers below.
pub struct RingBuffer {
    samples: VecDeque<f32>,
    capacity: usize,
}

impl RingBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Append `incoming`, dropping the oldest buffered samples once full.
    fn push_slice(&mut self, incoming: &[f32]) {
        for &sample in incoming {
            if self.samples.len() == self.capacity {
                self.samples.pop_front();
            }
            self.samples.push_back(sample);
        }
    }

    /// Take and clear everything buffered.
    fn drain(&mut self) -> Vec<f32> {
        self.samples.drain(..).collect()
    }

    /// Return the most recent `count` samples without removing them.
    fn peek_recent(&self, count: usize) -> Vec<f32> {
        let start = self.samples.len().saturating_sub(count);
        self.samples.iter().skip(start).copied().collect()
    }

    /// Test seam for [`Self::new`]. `AudioCapture::start` is the only
    /// production caller and calls it unconditionally.
    #[cfg(feature = "test-support")]
    pub fn new_for_test(capacity: usize) -> Self {
        Self::new(capacity)
    }

    /// Test seam for [`Self::push_slice`]. See [`Self::new_for_test`]:
    /// the input stream callback in `build_typed_stream` below is the
    /// only production caller and calls it unconditionally.
    #[cfg(feature = "test-support")]
    pub fn push_slice_for_test(&mut self, incoming: &[f32]) {
        self.push_slice(incoming)
    }

    /// Test seam for [`Self::drain`]. See [`Self::new_for_test`]:
    /// `AudioCapture::drain` is the only production caller and calls it
    /// unconditionally.
    #[cfg(feature = "test-support")]
    pub fn drain_for_test(&mut self) -> Vec<f32> {
        self.drain()
    }

    /// Test seam for [`Self::peek_recent`]. See [`Self::new_for_test`]:
    /// `AudioCapture::peek_recent` is the only production caller and
    /// calls it unconditionally.
    #[cfg(feature = "test-support")]
    pub fn peek_recent_for_test(&self, count: usize) -> Vec<f32> {
        self.peek_recent(count)
    }
}

/// Captures microphone audio into a bounded 16000 Hz mono ring buffer.
pub struct AudioCapture {
    buffer: Arc<Mutex<RingBuffer>>,
    stream: Option<Stream>,
}

impl AudioCapture {
    /// Open the default input device and start filling the buffer.
    pub fn start() -> Result<Self, CaptureError> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or(CaptureError::NoInputDevice)?;
        let supported = device.default_input_config()?;

        let format = supported.sample_format();
        let rate = supported.sample_rate().0;
        let channels = supported.channels();
        let config: StreamConfig = supported.config();

        let buffer = Arc::new(Mutex::new(RingBuffer::new(RING_BUFFER_CAPACITY_SAMPLES)));
        let stream = build_stream(
            &device,
            &config,
            format,
            rate,
            channels,
            Arc::clone(&buffer),
        )?;
        stream.play()?;

        Ok(Self {
            buffer,
            stream: Some(stream),
        })
    }

    /// Stop the stream. Buffered samples are kept. A fresh `AudioCapture`
    /// starts a new stream later.
    pub fn stop(&mut self) {
        self.stream = None;
    }

    /// Take and clear everything buffered so far.
    pub fn drain(&self) -> Vec<f32> {
        match self.buffer.lock() {
            Ok(mut buf) => buf.drain(),
            Err(_) => {
                error!("audio capture buffer lock poisoned, returning no samples");
                Vec::new()
            }
        }
    }

    /// Return the most recent samples covering `duration`, without removing
    /// them.
    pub fn peek_recent(&self, duration: Duration) -> Vec<f32> {
        let count = (duration.as_secs_f64() * WHISPER_SAMPLE_RATE as f64).round() as usize;
        match self.buffer.lock() {
            Ok(buf) => buf.peek_recent(count),
            Err(_) => {
                error!("audio capture buffer lock poisoned, returning no samples");
                Vec::new()
            }
        }
    }
}

/// Build the cpal input stream for whichever format the device reports.
fn build_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    rate: u32,
    channels: u16,
    buffer: Arc<Mutex<RingBuffer>>,
) -> Result<Stream, CaptureError> {
    match sample_format {
        SampleFormat::F32 => build_typed_stream::<f32>(device, config, rate, channels, buffer),
        SampleFormat::I16 => build_typed_stream::<i16>(device, config, rate, channels, buffer),
        SampleFormat::U16 => build_typed_stream::<u16>(device, config, rate, channels, buffer),
        other => Err(CaptureError::UnsupportedSampleFormat(other)),
    }
}

/// Build the input stream for a concrete device sample type `T`. Scratch
/// buffers are reserved once, before the callback runs, and reused inside
/// it for the downmix and resample steps.
fn build_typed_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    device_rate: u32,
    device_channels: u16,
    buffer: Arc<Mutex<RingBuffer>>,
) -> Result<Stream, CaptureError>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let mut mono_scratch: Vec<f32> = Vec::with_capacity(SCRATCH_CAPACITY_SAMPLES);
    let mut resampled_scratch: Vec<f32> = Vec::with_capacity(SCRATCH_CAPACITY_SAMPLES);

    let stream = device.build_input_stream(
        config,
        move |input: &[T], _info: &cpal::InputCallbackInfo| {
            downmix_into(&mut mono_scratch, input, device_channels);
            resample_mono_into(
                &mut resampled_scratch,
                &mono_scratch,
                device_rate,
                WHISPER_SAMPLE_RATE,
            );
            match buffer.lock() {
                Ok(mut buf) => buf.push_slice(&resampled_scratch),
                Err(_) => error!("audio capture buffer lock poisoned, dropping samples"),
            }
        },
        |err| error!("audio input stream error: {err}"),
        None,
    )?;
    Ok(stream)
}

/// Downmix interleaved `frames` to mono f32 by averaging each frame's
/// channels, clearing and reusing `out`'s existing allocation. `pub`:
/// the moved downmix tests call this directly (wrapped in a small local
/// helper that owns the scratch `Vec`, mirroring what the deleted
/// plain-test-only `downmix_to_mono` did); it is also used
/// unconditionally by `build_typed_stream` below.
pub fn downmix_into<T>(out: &mut Vec<f32>, frames: &[T], channels: u16)
where
    T: Copy,
    f32: FromSample<T>,
{
    out.clear();
    let channels = (channels as usize).max(1);
    for frame in frames.chunks(channels) {
        let sum: f32 = frame.iter().map(|&s| f32::from_sample(s)).sum();
        out.push(sum / frame.len() as f32);
    }
}

/// Linear-interpolation resample of a mono buffer from `src_rate` to
/// `dst_rate`, clearing and reusing `out`'s existing allocation. `pub`:
/// the moved resample tests call this directly (wrapped the same way as
/// [`downmix_into`]); it is also used unconditionally by
/// `build_typed_stream` below.
pub fn resample_mono_into(out: &mut Vec<f32>, samples: &[f32], src_rate: u32, dst_rate: u32) {
    out.clear();
    if samples.is_empty() || src_rate == dst_rate {
        out.extend_from_slice(samples);
        return;
    }

    let ratio = dst_rate as f64 / src_rate as f64;
    let dst_len = ((samples.len() as f64) * ratio).round() as usize;
    for i in 0..dst_len {
        let src_pos = i as f64 / ratio;
        let idx = src_pos.floor() as usize;
        let frac = (src_pos - idx as f64) as f32;
        let a = samples.get(idx).copied().unwrap_or(0.0);
        let b = samples.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
}
