//! Audio source abstraction — yields 16 kHz mono `f32` frames.
//!
//! Two implementations are planned:
//!
//! * `cpal`-backed (host-side daemon) — captures from the platform
//!   audio device. Lands in commit 2 behind a `cpal` feature.
//! * `NatsAudioSource` — for the future ESP32 satellite path
//!   (ADR-0015 §"D. ESP32 satellites"), where firmware streams PCM
//!   on `audio.satellite.<id>.pcm`.
//!
//! For now: `StubAudioSource`, which yields a pre-supplied
//! `Vec<AudioFrame>` and is the only impl needed for end-to-end
//! tests.

use async_trait::async_trait;
use thiserror::Error;

use crate::SAMPLES_PER_FRAME;

/// One 20 ms frame of 16 kHz mono PCM in `f32` (`-1.0..=1.0`).
///
/// `Vec`-backed rather than a fixed-size array because:
///
/// * Real codecs (especially over NATS) sometimes deliver short
///   tail-frames at end-of-stream.
/// * Test fixtures occasionally want to inject silence with a
///   lengths that doesn't match `SAMPLES_PER_FRAME` exactly.
///
/// The pipeline asserts on shape where it matters
/// ([`AudioFrame::is_well_formed`]).
#[derive(Debug, Clone)]
pub struct AudioFrame {
    /// PCM samples, normalised to `-1.0..=1.0`.
    pub samples: Vec<f32>,
    /// Monotonic capture timestamp, milliseconds since pipeline
    /// start. Used by [`crate::PipelineMetrics`] for stage-latency
    /// math; not the wall-clock time.
    pub captured_at_ms: u64,
}

impl AudioFrame {
    /// Build a frame from samples + a capture timestamp.
    #[must_use]
    pub const fn new(samples: Vec<f32>, captured_at_ms: u64) -> Self {
        Self {
            samples,
            captured_at_ms,
        }
    }

    /// Build an all-zero frame at `captured_at_ms`. Useful for
    /// padding silence in tests.
    #[must_use]
    pub fn silence(captured_at_ms: u64) -> Self {
        Self {
            samples: vec![0.0; SAMPLES_PER_FRAME],
            captured_at_ms,
        }
    }

    /// `true` when the frame's sample count equals
    /// [`SAMPLES_PER_FRAME`]. Real impls should ideally always emit
    /// well-formed frames; the wake-detector tolerates short frames
    /// at end-of-stream but the test pipeline asserts.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.samples.len() == SAMPLES_PER_FRAME
    }

    /// Peak absolute amplitude in `[0.0, 1.0]`. Used by
    /// [`StubWakeDetector`] as its (deliberately silly) "wake when
    /// loud" heuristic.
    ///
    /// [`StubWakeDetector`]: crate::wake::StubWakeDetector
    #[must_use]
    pub fn peak(&self) -> f32 {
        self.samples
            .iter()
            .copied()
            .map(f32::abs)
            .fold(0.0f32, f32::max)
    }
}

/// Errors from an [`AudioSource`].
#[derive(Debug, Error)]
pub enum AudioSourceError {
    /// The underlying device is gone (USB unplug, NATS disconnect on
    /// the satellite path, etc.). Pipelines treat this as fatal —
    /// the supervisor restarts the daemon rather than try to recover
    /// in-line.
    #[error("audio source disconnected: {0}")]
    Disconnected(String),
    /// The source can't produce samples at 16 kHz mono `f32`. The
    /// daemon binary is responsible for picking a device that can,
    /// or for inserting a resampler — the trait contract is "16 kHz
    /// mono f32 in, no exceptions".
    #[error("audio source format mismatch: {0}")]
    FormatMismatch(String),
    /// Any other backend-specific error.
    #[error("audio source io: {0}")]
    Io(String),
}

/// A pull-style audio source. The pipeline calls
/// [`AudioSource::next_frame`] in a loop; the source either yields
/// the next frame or returns `Ok(None)` to signal end-of-stream.
///
/// Pull rather than push because:
///
/// * The wake-detector and STT stages need backpressure during STT
///   inference (we'd rather drop pre-wake frames than queue them
///   unbounded behind a slow Whisper pass).
/// * Tests are far cleaner when the source is deterministic — a
///   pre-supplied `Vec` with `next_frame` returning `pop()` is
///   trivially reproducible.
#[async_trait]
pub trait AudioSource: Send {
    /// Yield the next frame. `Ok(None)` means stream-end (test
    /// fixture, file replay). `Err(...)` is fatal.
    async fn next_frame(&mut self) -> Result<Option<AudioFrame>, AudioSourceError>;
}

/// In-memory test source. Yields frames from a pre-supplied
/// `Vec<AudioFrame>`, oldest-first.
#[derive(Debug)]
pub struct StubAudioSource {
    frames: std::collections::VecDeque<AudioFrame>,
}

impl StubAudioSource {
    /// Build from a fixed sequence of frames.
    #[must_use]
    pub fn new(frames: impl IntoIterator<Item = AudioFrame>) -> Self {
        Self {
            frames: frames.into_iter().collect(),
        }
    }

    /// Build a source containing `count` silence frames at
    /// monotonic 20 ms intervals starting from `start_ms`.
    #[must_use]
    pub fn silence_for(count: usize, start_ms: u64) -> Self {
        let frames: Vec<AudioFrame> = (0..count)
            .map(|i| AudioFrame::silence(start_ms + (i as u64) * 20))
            .collect();
        Self::new(frames)
    }
}

#[async_trait]
impl AudioSource for StubAudioSource {
    async fn next_frame(&mut self) -> Result<Option<AudioFrame>, AudioSourceError> {
        Ok(self.frames.pop_front())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn silence_frame_is_well_formed_and_peakzero() {
        let f = AudioFrame::silence(0);
        assert!(f.is_well_formed());
        assert!((f.peak()).abs() < f32::EPSILON);
    }

    #[test]
    fn peak_picks_largest_abs() {
        let f = AudioFrame::new(vec![0.1, -0.7, 0.3, -0.2], 0);
        assert!((f.peak() - 0.7).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn stub_source_yields_frames_then_none() {
        let mut s = StubAudioSource::new(vec![AudioFrame::silence(0), AudioFrame::silence(20)]);
        assert!(s.next_frame().await.unwrap().is_some());
        assert!(s.next_frame().await.unwrap().is_some());
        assert!(s.next_frame().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn silence_for_produces_monotonic_timestamps() {
        let mut s = StubAudioSource::silence_for(3, 100);
        let f0 = s.next_frame().await.unwrap().unwrap();
        let f1 = s.next_frame().await.unwrap().unwrap();
        let f2 = s.next_frame().await.unwrap().unwrap();
        assert_eq!(f0.captured_at_ms, 100);
        assert_eq!(f1.captured_at_ms, 120);
        assert_eq!(f2.captured_at_ms, 140);
    }
}
