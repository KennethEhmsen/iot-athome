//! Wake-word detection.
//!
//! Real impls (M5b W3+):
//!
//! * **openWakeWord** — Python upstream; we'd FFI through PyO3 or
//!   port the lightweight ONNX-runtime model loader.
//! * **Porcupine** — commercial license; high quality but operator
//!   has to bring their own access key.
//! * **A custom CRNN we ship** — small (<1 MB), trainable on the
//!   household's voice, but a real M6+ project.
//!
//! For the v1 scaffold this module ships only the trait + a
//! deliberately-silly `StubWakeDetector` that fires on amplitude.
//! Good enough to drive the end-to-end test; nowhere near "hears
//! the magic phrase".

use async_trait::async_trait;
use thiserror::Error;

use crate::audio::AudioFrame;

/// A wake event. Pipeline includes `confidence` so a future "soft
/// wake" UX (small chime on `0.5..=0.8`, full activation on `> 0.8`)
/// is one boolean threshold flip away.
#[derive(Debug, Clone, PartialEq)]
pub struct Wake {
    /// Detector confidence in `[0.0, 1.0]`.
    pub confidence: f32,
    /// Capture timestamp (ms since pipeline start) of the frame
    /// the wake fired on. Used for latency telemetry.
    pub captured_at_ms: u64,
}

#[derive(Debug, Error)]
pub enum WakeError {
    /// The detector's underlying model failed to load or evaluate.
    /// Real impls map ORT / PyO3 / etc. errors to this.
    #[error("wake detector backend: {0}")]
    Backend(String),
}

/// A streaming wake-word detector.
///
/// The pipeline calls [`WakeDetector::observe`] for every frame; the
/// detector returns `Some(Wake)` exactly when its internal sliding
/// window crosses a confidence threshold. Real detectors maintain
/// internal state across frames; the trait doesn't constrain that
/// shape (use `&mut self`).
#[async_trait]
pub trait WakeDetector: Send {
    /// Feed one frame. Returns `Some(Wake)` when the wake word
    /// fires; `None` when the window so far isn't a wake.
    async fn observe(&mut self, frame: &AudioFrame) -> Result<Option<Wake>, WakeError>;
}

/// Test-grade detector that fires on amplitude.
///
/// **Not** a real wake-word detector. Useful for end-to-end tests
/// because we can hand it a frame whose `peak()` is above threshold
/// and reliably trigger downstream stages without bundling a model.
/// Real impls land in M5b W3+.
#[derive(Debug)]
pub struct StubWakeDetector {
    /// Peak amplitude (absolute) at or above which a frame counts
    /// as a wake. `0.5` is the test default — comfortably above
    /// silence-frame peak (0.0) and any plausibly-noisy fixture.
    pub threshold: f32,
    /// Once a wake fires, the detector goes silent for this many
    /// frames so a single sustained-loud window doesn't fire
    /// repeatedly. Mirrors the cooldown a real detector would
    /// implement against its sliding window.
    pub cooldown_frames: u32,
    cooldown_remaining: u32,
}

impl Default for StubWakeDetector {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            cooldown_frames: 25, // 500 ms at 20 ms/frame
            cooldown_remaining: 0,
        }
    }
}

impl StubWakeDetector {
    #[must_use]
    pub const fn with_threshold(threshold: f32) -> Self {
        Self {
            threshold,
            cooldown_frames: 25,
            cooldown_remaining: 0,
        }
    }
}

#[async_trait]
impl WakeDetector for StubWakeDetector {
    async fn observe(&mut self, frame: &AudioFrame) -> Result<Option<Wake>, WakeError> {
        if self.cooldown_remaining > 0 {
            self.cooldown_remaining -= 1;
            return Ok(None);
        }
        if frame.peak() >= self.threshold {
            self.cooldown_remaining = self.cooldown_frames;
            return Ok(Some(Wake {
                confidence: frame.peak().min(1.0),
                captured_at_ms: frame.captured_at_ms,
            }));
        }
        Ok(None)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn loud_frame(captured_at_ms: u64) -> AudioFrame {
        AudioFrame::new(vec![0.9; crate::SAMPLES_PER_FRAME], captured_at_ms)
    }

    #[tokio::test]
    async fn stub_does_not_fire_on_silence() {
        let mut d = StubWakeDetector::default();
        for ms in (0..1000).step_by(20) {
            let f = AudioFrame::silence(ms);
            assert!(d.observe(&f).await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn stub_fires_on_loud_frame_then_cools_down() {
        // Default cooldown is 25 frames. After a fire, the next 25
        // observe() calls return None (cooldown ticks down to 0);
        // call 26 is free to fire on a loud frame again. The test
        // has to make EXACTLY 25 cooldown observations between
        // fires — fewer = wake stays suppressed; more = also fine
        // (cooldown stays at 0 indefinitely).
        let mut d = StubWakeDetector::default();
        let w = d.observe(&loud_frame(0)).await.unwrap();
        assert!(w.is_some(), "expected wake on first loud frame");
        // 25 cooldown observations.
        for i in 1..=25 {
            let ms = 20 * i;
            assert!(
                d.observe(&loud_frame(ms)).await.unwrap().is_none(),
                "expected no wake during cooldown at {ms}ms"
            );
        }
        // 26th observation past the original fire — cooldown has
        // elapsed, a loud frame fires again.
        let w2 = d.observe(&loud_frame(20 * 26)).await.unwrap();
        assert!(w2.is_some(), "expected wake post-cooldown");
    }
}
