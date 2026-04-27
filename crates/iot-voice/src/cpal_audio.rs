//! `cpal`-backed [`AudioSource`] (M5b W4a).
//!
//! Captures audio from the platform's default input device — the
//! "real microphone" path that `StubAudioSource` was always a
//! placeholder for. Pure-Rust on every supported platform via
//! `cpal`'s host-specific backends:
//!
//! * Windows → wasapi
//! * Linux   → alsa (system `libasound2`)
//! * macOS   → coreaudio
//!
//! No CMake bootstrap. Binary footprint stays small.
//!
//! ## Send/Sync story
//!
//! `cpal::Stream` is `!Send + !Sync` because the platform backends
//! lean on thread-locals. The pipeline's async task wants its
//! `AudioSource` to be `Send` (so tokio's multi-threaded runtime
//! can migrate it). Solution: pin the Stream onto a dedicated OS
//! thread the source spawns at `start()`-time. Frames flow from
//! the cpal callback → `tokio::sync::mpsc::Sender` (owned by that
//! thread) → `mpsc::Receiver` (owned by the source struct). The
//! struct itself never holds a cpal type, so it's `Send` by
//! construction. No unsafe code, which is good because the lib
//! crate `#![forbid(unsafe_code)]`.
//!
//! On `CpalAudioSource::drop`, a oneshot sender signals the OS
//! thread to release the Stream, which stops audio capture.
//!
//! ## Format normalisation
//!
//! The pipeline operates at 16 kHz mono `f32` per
//! [`crate::SAMPLE_RATE_HZ`]. Real input devices are typically
//! 48 kHz stereo `i16` — the source resamples + downmixes on its
//! way through.
//!
//! Resampling here is **deliberately naive**: per-sample
//! decimating average. Correct for spectrally-clean speech;
//! introduces aliasing on broadband noise. Real-grade resampling
//! (windowed-sinc, polyphase) lives in `rubato` and is a clean
//! drop-in once the wake-detector's tolerance is measured. For
//! the scaffold-grade real-mic path this is acceptable.
//!
//! ## Backpressure
//!
//! Frame channel capacity = 16 frames (~320 ms). On overflow —
//! typically when STT inference starves the consumer — the cpal
//! callback drops frames + logs `debug!`. Dropping pre-wake
//! silence is fine; dropping post-wake speech degrades
//! transcription. Capacity is sized so STT can use its 300 ms
//! ADR-0015 budget without backpressure-induced drops.

use std::sync::mpsc as std_mpsc;

use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc as tokio_mpsc;
use tracing::{debug, info, warn};

use crate::audio::{AudioFrame, AudioSource, AudioSourceError};
use crate::SAMPLE_RATE_HZ;

/// How many `AudioFrame`s the cpal callback can buffer before
/// dropping. Each frame is 20 ms, so 16 frames = 320 ms — a
/// comfortable cushion behind STT inference at the 300 ms-per-
/// utterance ADR-0015 budget.
const CHANNEL_CAPACITY: usize = 16;

/// `AudioSource` backed by the platform's default input device.
pub struct CpalAudioSource {
    rx: tokio_mpsc::Receiver<AudioFrame>,
    /// Signal sent on `Drop` to release the Stream-owning thread.
    /// `Option` so `Drop` can `take` it; the inner `Sender` is
    /// `Send` (it's std::sync::mpsc).
    shutdown_tx: Option<std_mpsc::Sender<()>>,
}

impl std::fmt::Debug for CpalAudioSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpalAudioSource").finish_non_exhaustive()
    }
}

impl CpalAudioSource {
    /// Open the default input device on a dedicated OS thread,
    /// start streaming.
    ///
    /// # Errors
    /// `Disconnected` when no input device is available.
    /// `FormatMismatch` when the device's preferred config can't
    /// be coerced. `Io` for any cpal initialisation failure.
    pub fn start() -> Result<Self, AudioSourceError> {
        let (frame_tx, frame_rx) = tokio_mpsc::channel::<AudioFrame>(CHANNEL_CAPACITY);
        let (shutdown_tx, shutdown_rx) = std_mpsc::channel::<()>();
        // Use a setup-result channel so `start()` can surface
        // initialisation errors synchronously instead of silently
        // returning a source that never produces frames.
        let (setup_tx, setup_rx) = std_mpsc::channel::<Result<(), AudioSourceError>>();

        std::thread::Builder::new()
            .name("iot-voice-cpal".into())
            .spawn(move || {
                run_capture_thread(&setup_tx, &shutdown_rx, frame_tx);
            })
            .map_err(|e| AudioSourceError::Io(format!("spawn cpal thread: {e}")))?;

        // Block briefly for the thread's setup result. The setup
        // path is sub-millisecond on a healthy system; if the
        // thread never sends, we surface a clear error rather than
        // returning a zombie source.
        match setup_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                rx: frame_rx,
                shutdown_tx: Some(shutdown_tx),
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(AudioSourceError::Io(
                "cpal thread terminated before reporting setup result".into(),
            )),
        }
    }
}

impl Drop for CpalAudioSource {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            // Signal best-effort. A receive-error here means the
            // thread already exited (e.g. a stream-error tore it
            // down), which is fine — the Stream is gone either way.
            let _ = tx.send(());
        }
    }
}

#[async_trait]
impl AudioSource for CpalAudioSource {
    async fn next_frame(&mut self) -> Result<Option<AudioFrame>, AudioSourceError> {
        Ok(self.rx.recv().await)
    }
}

// -------------------------------------------------- capture thread

/// The OS-thread entry point. Owns the cpal `Stream` (which is
/// !Send) for its lifetime; on shutdown signal, drops it (which
/// stops audio capture) and exits.
fn run_capture_thread(
    setup_tx: &std_mpsc::Sender<Result<(), AudioSourceError>>,
    shutdown_rx: &std_mpsc::Receiver<()>,
    frame_tx: tokio_mpsc::Sender<AudioFrame>,
) {
    let host = cpal::default_host();
    let Some(device) = host.default_input_device() else {
        let _ = setup_tx.send(Err(AudioSourceError::Disconnected(
            "no default input device".into(),
        )));
        return;
    };
    let device_name = device.name().unwrap_or_else(|_| "<unknown>".into());

    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            let _ = setup_tx.send(Err(AudioSourceError::FormatMismatch(format!(
                "default input config: {e}"
            ))));
            return;
        }
    };
    let device_sample_rate = config.sample_rate().0;
    let device_channels = config.channels();
    let sample_format = config.sample_format();
    info!(
        device = %device_name,
        sample_rate = device_sample_rate,
        channels = device_channels,
        sample_format = ?sample_format,
        target_rate = SAMPLE_RATE_HZ,
        "opening cpal input device"
    );

    let stream = match build_stream(&device, &config, frame_tx) {
        Ok(s) => s,
        Err(e) => {
            let _ = setup_tx.send(Err(AudioSourceError::Io(format!("build cpal stream: {e}"))));
            return;
        }
    };
    if let Err(e) = stream.play() {
        let _ = setup_tx.send(Err(AudioSourceError::Io(format!("start cpal stream: {e}"))));
        return;
    }

    // Setup succeeded — let the caller proceed.
    let _ = setup_tx.send(Ok(()));

    // Park until shutdown. Dropping the stream stops capture.
    let _ = shutdown_rx.recv();
    drop(stream);
    debug!(target: "iot_voice::cpal_audio", "cpal capture thread exiting");
}

/// Build a cpal stream that feeds 20 ms `AudioFrame`s into `tx`.
///
/// `tx` is passed by value because exactly one match arm fires
/// (the others are dead code post-monomorphisation), and the
/// chosen arm's closure captures `tx` by move into the cpal
/// callback.
fn build_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    tx: tokio_mpsc::Sender<AudioFrame>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    use cpal::SampleFormat;
    let device_sample_rate = config.sample_rate().0;
    let device_channels = config.channels() as usize;
    let target_rate = SAMPLE_RATE_HZ;

    let mut acc = FrameAccumulator::new(device_sample_rate, target_rate, device_channels);

    let err_fn = |e| warn!(target: "iot_voice::cpal_audio", error = %e, "cpal stream error");
    let stream_config = cpal::StreamConfig {
        channels: config.channels(),
        sample_rate: config.sample_rate(),
        buffer_size: cpal::BufferSize::Default,
    };

    match config.sample_format() {
        SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _| feed(&mut acc, data, &tx),
            err_fn,
            None,
        ),
        SampleFormat::I16 => {
            let mut conv_buf: Vec<f32> = Vec::with_capacity(4096);
            device.build_input_stream(
                &stream_config,
                move |data: &[i16], _| {
                    conv_buf.clear();
                    conv_buf.extend(data.iter().map(|s| f32::from(*s) / f32::from(i16::MAX)));
                    feed(&mut acc, &conv_buf, &tx);
                },
                err_fn,
                None,
            )
        }
        SampleFormat::U16 => {
            let mut conv_buf: Vec<f32> = Vec::with_capacity(4096);
            device.build_input_stream(
                &stream_config,
                move |data: &[u16], _| {
                    conv_buf.clear();
                    conv_buf.extend(data.iter().map(|s| (f32::from(*s) - 32_768.0) / 32_768.0));
                    feed(&mut acc, &conv_buf, &tx);
                },
                err_fn,
                None,
            )
        }
        other => {
            warn!(target: "iot_voice::cpal_audio", ?other, "unsupported cpal sample format");
            Err(cpal::BuildStreamError::StreamConfigNotSupported)
        }
    }
}

/// Push `samples` (interleaved, multi-channel) through the
/// resampler/downmixer/frame-builder; emit completed frames on
/// `tx`. On `tx.try_send` failure (channel full) drop + log; see
/// ADR-0015 §"Backpressure".
fn feed(acc: &mut FrameAccumulator, samples: &[f32], tx: &tokio_mpsc::Sender<AudioFrame>) {
    for sample in samples {
        if let Some(frame) = acc.feed(*sample) {
            if let Err(e) = tx.try_send(frame) {
                match e {
                    tokio_mpsc::error::TrySendError::Full(_) => {
                        debug!(
                            target: "iot_voice::cpal_audio",
                            "frame channel full; dropping frame (consumer stalled)"
                        );
                    }
                    tokio_mpsc::error::TrySendError::Closed(_) => {
                        debug!(target: "iot_voice::cpal_audio", "frame channel closed; stopping");
                        return;
                    }
                }
            }
        }
    }
}

// -------------------------------------------------- frame accumulator

/// Multi-channel decimating downmixer + frame slicer.
///
/// State is maintained across cpal callback invocations (the cpal
/// callback runs on a single thread, serialised). Strategy:
///
/// 1. Average across channels per input sample → mono.
/// 2. Decimating resampler: keep one mono sample every
///    `device_rate / target_rate` mono samples.
/// 3. When [`crate::SAMPLES_PER_FRAME`] mono samples have
///    accumulated, emit one [`AudioFrame`].
struct FrameAccumulator {
    chan_buf: Vec<f32>,
    chan_count: usize,
    decimation_ratio: f32,
    decimation_phase: f32,
    frame_buf: Vec<f32>,
    next_frame_ms: u64,
}

impl FrameAccumulator {
    // The u32→f32 + usize→f32 casts are bounded by construction:
    // sample rates fit in 20 bits (max ~192 kHz) and channel
    // counts fit in 8 bits — both well inside f32's 23-bit
    // mantissa, so precision-loss is impossible in practice.
    #[allow(clippy::cast_precision_loss)]
    fn new(device_rate: u32, target_rate: u32, channels: usize) -> Self {
        Self {
            chan_buf: Vec::with_capacity(channels),
            chan_count: channels,
            decimation_ratio: device_rate as f32 / target_rate as f32,
            decimation_phase: 0.0,
            frame_buf: Vec::with_capacity(crate::SAMPLES_PER_FRAME),
            next_frame_ms: 0,
        }
    }

    #[allow(clippy::cast_precision_loss)] // chan_count <= 16 in practice
    fn feed(&mut self, sample: f32) -> Option<AudioFrame> {
        self.chan_buf.push(sample);
        if self.chan_buf.len() < self.chan_count {
            return None;
        }
        let mono = self.chan_buf.iter().sum::<f32>() / self.chan_count as f32;
        self.chan_buf.clear();

        self.decimation_phase += 1.0;
        if self.decimation_phase < self.decimation_ratio {
            return None;
        }
        self.decimation_phase -= self.decimation_ratio;

        self.frame_buf.push(mono);
        if self.frame_buf.len() < crate::SAMPLES_PER_FRAME {
            return None;
        }

        let captured_at_ms = self.next_frame_ms;
        self.next_frame_ms += u64::from(crate::FRAME_DURATION_MS);
        let samples = std::mem::replace(
            &mut self.frame_buf,
            Vec::with_capacity(crate::SAMPLES_PER_FRAME),
        );
        Some(AudioFrame::new(samples, captured_at_ms))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn frame_accumulator_decimates_48k_to_16k_mono() {
        let mut acc = FrameAccumulator::new(48_000, 16_000, 1);
        let mut frames = Vec::new();
        for i in 0..960 {
            let s = (i as f32 / 960.0) - 0.5;
            if let Some(f) = acc.feed(s) {
                frames.push(f);
            }
        }
        assert_eq!(frames.len(), 1, "expected exactly one frame");
        assert_eq!(frames[0].samples.len(), crate::SAMPLES_PER_FRAME);
        assert_eq!(frames[0].captured_at_ms, 0);
    }

    #[test]
    fn frame_accumulator_downmixes_stereo_to_mono() {
        let mut acc = FrameAccumulator::new(16_000, 16_000, 2);
        let mut frames = Vec::new();
        for _ in 0..crate::SAMPLES_PER_FRAME {
            let _ = acc.feed(0.5);
            if let Some(f) = acc.feed(-0.5) {
                frames.push(f);
            }
        }
        assert_eq!(frames.len(), 1);
        for s in &frames[0].samples {
            assert!(s.abs() < f32::EPSILON, "expected ~0.0, got {s}");
        }
    }

    #[test]
    fn frame_accumulator_advances_timestamps() {
        let mut acc = FrameAccumulator::new(16_000, 16_000, 1);
        let mut frames = Vec::new();
        for i in 0..(crate::SAMPLES_PER_FRAME * 2) {
            if let Some(f) = acc.feed((i as f32) * 0.001) {
                frames.push(f);
            }
        }
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].captured_at_ms, 0);
        assert_eq!(frames[1].captured_at_ms, 20);
    }

    // CpalAudioSource::start() needs an actual audio device; not
    // unit-testable on a CI runner without virtual-audio loopback.
    // The frame-accumulator tests cover the resampling math; the
    // cpal-callback wiring is exercised manually via
    // `iot-voice listen --use-mic`.
}
