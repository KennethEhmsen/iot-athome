//! Phrase-specific wake-word detection via rustpotter (M5b W4b.5).
//!
//! Pure-Rust wake-word detector built on `rustpotter` 3.x. Trains
//! per-phrase models from a handful of audio samples; runtime
//! detection is sub-millisecond per frame, so this fits inside
//! the 200 ms wake-detection budget per ADR-0015 with room to
//! spare.
//!
//! Sister impl to [`crate::wake_vad::EnergyVadWakeDetector`].
//! Pick whichever fits the deployment:
//!
//! | Detector | Pros | Cons |
//! |----------|------|------|
//! | `EnergyVadWakeDetector` | No model file; self-calibrates; pure-Rust default-build | Fires on any sustained speech, not a specific phrase |
//! | `RustpotterWakeDetector` | True wake-word semantics; per-phrase ML | Requires an `.rpw` model file; opt-in `wake-phrase` feature |
//!
//! ## Model files
//!
//! Operator builds an `.rpw` model with rustpotter's CLI from a
//! few audio samples of the chosen phrase ("computer", "hey hub",
//! "ok bridge", whatever). The repository at
//! <https://github.com/GiviMAD/rustpotter> has reference models
//! and trainer instructions.
//!
//! Convention: place at `~/.iot-athome/models/wake-<phrase>.rpw`.
//! The daemon's `--wake-model <path>` flag overrides.
//!
//! ## Sample format
//!
//! rustpotter's `process_samples` consumes `i16` PCM at the rate
//! the model was trained against (usually 16 kHz). The pipeline
//! produces `f32` `[-1.0, 1.0]` at 16 kHz; this adapter
//! re-quantises per call. The `i16` cast is bounded
//! mathematically (clamp + scale-to-15 bits), so precision
//! loss isn't real.

use std::path::Path;

use async_trait::async_trait;
use rustpotter::{Rustpotter, RustpotterConfig, ScoreMode};
use tracing::{debug, info};

use crate::audio::AudioFrame;
use crate::wake::{Wake, WakeDetector, WakeError};

/// Detection threshold — minimum score for a positive detection.
/// 0.5 is rustpotter's documented default; tighter (0.7+) cuts
/// false positives in noisy rooms but raises miss rate.
/// Operators tune via `IOT_WAKE_THRESHOLD` env.
const DEFAULT_THRESHOLD: f32 = 0.5;

/// Cooldown after a detection — same idea as
/// [`crate::wake::StubWakeDetector::cooldown_frames`]: prevent
/// the spoken phrase that fired wake from re-triggering during
/// the post-utterance window. 50 frames @ 20 ms = 1 s.
const COOLDOWN_FRAMES: u32 = 50;

/// `RustpotterWakeDetector` — wraps a loaded rustpotter model.
pub struct RustpotterWakeDetector {
    rp: Rustpotter,
    cooldown_remaining: u32,
}

impl std::fmt::Debug for RustpotterWakeDetector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RustpotterWakeDetector")
            .finish_non_exhaustive()
    }
}

impl RustpotterWakeDetector {
    /// Load `model_path` (an `.rpw` file) into a fresh detector.
    ///
    /// # Errors
    /// `WakeError::Backend` for file-missing, malformed, or
    /// rate-mismatch. The pipeline operates at
    /// [`crate::SAMPLE_RATE_HZ`] (16 kHz) — the model must be
    /// trained at the same rate.
    pub fn load(model_path: &Path) -> Result<Self, WakeError> {
        let path_str = model_path
            .to_str()
            .ok_or_else(|| WakeError::Backend(format!("non-UTF-8 model path {model_path:?}")))?;
        info!(target: "iot_voice::rustpotter_wake", path = %path_str, "loading rustpotter model");

        let mut config = RustpotterConfig::default();
        config.detector.threshold = std::env::var("IOT_WAKE_THRESHOLD")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .filter(|f| (0.0..=1.0).contains(f))
            .unwrap_or(DEFAULT_THRESHOLD);
        config.detector.score_mode = ScoreMode::Average;

        let mut rp = Rustpotter::new(&config)
            .map_err(|e| WakeError::Backend(format!("rustpotter init: {e}")))?;
        rp.add_wakeword_from_file("default", path_str)
            .map_err(|e| WakeError::Backend(format!("load wake-word model: {e}")))?;
        Ok(Self {
            rp,
            cooldown_remaining: 0,
        })
    }
}

#[async_trait]
impl WakeDetector for RustpotterWakeDetector {
    async fn observe(&mut self, frame: &AudioFrame) -> Result<Option<Wake>, WakeError> {
        if self.cooldown_remaining > 0 {
            self.cooldown_remaining -= 1;
            return Ok(None);
        }

        // f32 [-1.0, 1.0] → i16 [-32768, 32767]. Clamp first to
        // avoid the cast wrapping on out-of-range samples.
        // Bounds-checked + small range → cast is precision-safe
        // by construction.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let i16_samples: Vec<i16> = frame
            .samples
            .iter()
            .map(|s| (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16)
            .collect();

        // rustpotter buffers internally + emits a detection when
        // a complete window matches. `Some(detection)` => fire;
        // `None` => keep listening.
        if let Some(detection) = self.rp.process_samples(&i16_samples) {
            debug!(
                target: "iot_voice::rustpotter_wake",
                wake_word = %detection.name,
                score = detection.score,
                "wake-word detected"
            );
            self.cooldown_remaining = COOLDOWN_FRAMES;
            return Ok(Some(Wake {
                confidence: detection.score.clamp(0.0, 1.0),
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

    #[test]
    fn missing_model_path_errors_cleanly() {
        let err = RustpotterWakeDetector::load(Path::new("/this/path/does/not/exist/wake.rpw"))
            .unwrap_err();
        assert!(matches!(err, WakeError::Backend(_)), "{err:?}");
    }
}
