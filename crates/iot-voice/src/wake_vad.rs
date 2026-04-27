//! Energy-VAD wake detector (M5b W4b).
//!
//! Listens to a sliding window of audio frames, tracks running
//! noise-floor + variance via an exponentially-weighted moving
//! average, and fires `Wake` when the current frame's RMS energy
//! sustains above `noise_floor + k·σ` for a minimum number of
//! frames. The detector self-calibrates to the room's ambient
//! level over the first ~2 seconds — no per-deployment threshold
//! tuning, no operator config.
//!
//! ## What this is, and isn't
//!
//! This is a **VAD** (voice-activity detector), not a phrase-
//! specific wake-word detector. Any sustained speech-like signal
//! near the microphone triggers wake. That's adequate for:
//!
//! * Hub kiosks in single-occupant rooms (the operator is the
//!   only one who'd be near the mic).
//! * Push-to-talk-style usage where the operator deliberately
//!   speaks toward the device.
//! * Closed-domain rule layouts where non-command speech (TV,
//!   conversation) just doesn't match any rule and gets
//!   discarded after STT — no action fires, but the voice cycle
//!   ran and consumed CPU.
//!
//! It's NOT adequate for:
//!
//! * Shared-space hubs where the operator doesn't want
//!   transcription on every nearby utterance.
//! * Privacy-sensitive deployments (every word triggers an STT
//!   pass; logs may carry transcribed-but-rejected text).
//!
//! For those, a phrase-specific wake detector is the next slice
//! (M5b W4b.5):
//!
//! * **rustpotter** — pure-Rust, MIT, trains on a few audio
//!   samples per phrase. Drop-in [`WakeDetector`] impl.
//! * **openWakeWord** — Python upstream, ONNX-runtime FFI
//!   needed; pre-trained "alexa", "hey jarvis", "hey mycroft"
//!   models available.
//! * **Porcupine** — commercial license, high quality, requires
//!   per-deployment access key.
//!
//! Either lands as a sibling [`WakeDetector`] impl behind its
//! own feature flag.

use std::collections::VecDeque;

use async_trait::async_trait;

use crate::audio::AudioFrame;
use crate::wake::{Wake, WakeDetector, WakeError};

/// How many leading frames the detector spends building its
/// initial noise estimate before it'll fire. 100 frames @ 20 ms
/// = 2 s of "warm-up" audio — long enough that ambient
/// background settles into the moving average, short enough that
/// the operator typically doesn't notice.
///
/// During warm-up the detector returns `None` regardless of
/// energy, so a loud cough during startup doesn't immediately
/// fire wake on a wildly-undertuned threshold.
const WARMUP_FRAMES: u32 = 100;

/// EWMA decay factor for the noise-floor estimate. Higher = more
/// reactive to changes; lower = smoother. 0.02 means the
/// half-life is roughly `ln(2) / 0.02` ≈ 35 frames ≈ 700 ms —
/// adapts to a fan turning on but doesn't track speech itself.
const NOISE_DECAY: f32 = 0.02;

/// EWMA decay for the variance estimate. Slightly slower than
/// the noise floor so threshold reacts to changes in *level*
/// faster than to changes in *spread*.
const VARIANCE_DECAY: f32 = 0.01;

/// `k` in `noise_floor + k · σ` — how many standard deviations
/// over the noise floor a frame must reach to count as
/// "speech-like". 2.5 is loose enough that quiet speech still
/// fires, tight enough that fan / HVAC noise stays below.
const K_SIGMA: f32 = 2.5;

/// Hard floor below which the threshold can never drop. Without
/// this, a totally silent room collapses the variance to zero
/// and the smallest crackle fires wake. 0.01 corresponds to
/// roughly -40 dBFS — quieter than any room but louder than
/// digital silence.
const MIN_THRESHOLD: f32 = 0.01;

/// Number of consecutive over-threshold frames required to fire.
/// 4 frames @ 20 ms = 80 ms of sustained speech — long enough
/// to reject single clicks (key clack, glass set on table) but
/// short enough that wake firing feels instant.
const SUSTAINED_FRAMES: u32 = 4;

/// Cooldown after a fire — see [`crate::wake::StubWakeDetector`]
/// for the same idea. 50 frames @ 20 ms = 1 s; long enough that
/// the spoken utterance the wake fired on doesn't re-trigger
/// post-utterance, short enough that consecutive commands work.
const COOLDOWN_FRAMES: u32 = 50;

/// Sliding-window length for the over-threshold run counter.
/// We only need to remember whether the last [`SUSTAINED_FRAMES`]
/// frames were over threshold, but keeping a small history makes
/// debugging easier (operator can dump the buffer).
const SLIDING_WINDOW: usize = 16;

/// Energy-VAD-based [`WakeDetector`].
#[derive(Debug)]
pub struct EnergyVadWakeDetector {
    /// Frames seen since `start()`. Pre-warmup the detector
    /// observes but doesn't fire.
    frames_seen: u32,
    /// EWMA of frame RMS — the running noise floor.
    noise_floor: f32,
    /// EWMA of (frame RMS − noise_floor)² — the running variance.
    /// Square root gives σ used for the firing threshold.
    variance: f32,
    /// Recent frame RMS values, for debugging + over-threshold
    /// run-counting. Length capped at `SLIDING_WINDOW`.
    rms_history: VecDeque<f32>,
    /// Number of consecutive over-threshold frames. Resets on a
    /// below-threshold frame.
    over_threshold_run: u32,
    /// Cooldown remaining after a fire. While > 0, no fire.
    cooldown_remaining: u32,
}

impl Default for EnergyVadWakeDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl EnergyVadWakeDetector {
    #[must_use]
    pub fn new() -> Self {
        Self {
            frames_seen: 0,
            noise_floor: 0.0,
            variance: 0.0,
            rms_history: VecDeque::with_capacity(SLIDING_WINDOW),
            over_threshold_run: 0,
            cooldown_remaining: 0,
        }
    }

    /// Currently-active threshold, in normalised RMS units.
    /// Public for diagnostic use (the daemon's `--debug-wake` is
    /// a future flag; for now operators can poke this via a
    /// custom probe).
    #[must_use]
    pub fn current_threshold(&self) -> f32 {
        let sigma = self.variance.sqrt();
        K_SIGMA.mul_add(sigma, self.noise_floor).max(MIN_THRESHOLD)
    }

    /// Last observed frame RMS. Diagnostic.
    #[must_use]
    pub fn last_rms(&self) -> f32 {
        self.rms_history.back().copied().unwrap_or(0.0)
    }

    /// Update the noise + variance EWMAs against a new RMS sample.
    fn update_noise_estimate(&mut self, rms: f32) {
        if self.frames_seen == 1 {
            // First frame: initialise EWMAs to the observed value
            // rather than 0, so we don't spend the first dozen
            // frames climbing out of zero.
            self.noise_floor = rms;
            self.variance = 0.0;
            return;
        }
        let diff = rms - self.noise_floor;
        self.noise_floor += NOISE_DECAY * diff;
        // `mul_add` for the variance update — clippy's
        // suboptimal_flops lint, plus this avoids one rounding
        // step on hardware with FMA support.
        self.variance += VARIANCE_DECAY * diff.mul_add(diff, -self.variance);
    }
}

#[async_trait]
impl WakeDetector for EnergyVadWakeDetector {
    async fn observe(&mut self, frame: &AudioFrame) -> Result<Option<Wake>, WakeError> {
        self.frames_seen += 1;

        // RMS of the frame's samples.
        let sum_sq: f32 = frame.samples.iter().map(|s| s * s).sum();
        let count = frame.samples.len().max(1);
        #[allow(clippy::cast_precision_loss)] // count <= 320 in practice
        let rms = (sum_sq / count as f32).sqrt();

        // Update history window.
        if self.rms_history.len() >= SLIDING_WINDOW {
            self.rms_history.pop_front();
        }
        self.rms_history.push_back(rms);

        // Pre-warmup: observe-only, no fire.
        if self.frames_seen <= WARMUP_FRAMES {
            self.update_noise_estimate(rms);
            return Ok(None);
        }

        // Cooldown: skip threshold check, but still keep the
        // EWMA tracking so we don't lose ground during a wake's
        // utterance window.
        if self.cooldown_remaining > 0 {
            self.cooldown_remaining -= 1;
            self.update_noise_estimate(rms);
            return Ok(None);
        }

        let threshold = self.current_threshold();
        if rms >= threshold {
            self.over_threshold_run += 1;
            // Don't pull this frame's loud RMS into the noise
            // floor — it's signal, not noise. Variance still
            // gets the squared-deviation update so threshold
            // reacts to the loud burst on the next post-cooldown
            // frame.
        } else {
            self.over_threshold_run = 0;
            self.update_noise_estimate(rms);
        }

        if self.over_threshold_run >= SUSTAINED_FRAMES {
            self.over_threshold_run = 0;
            self.cooldown_remaining = COOLDOWN_FRAMES;
            // Confidence: how many σs above the noise floor.
            // Capped at 1.0 because the trait contract is `[0, 1]`.
            let sigma = self.variance.sqrt().max(f32::EPSILON);
            let confidence = ((rms - self.noise_floor) / sigma / (K_SIGMA * 2.0)).clamp(0.0, 1.0);
            return Ok(Some(Wake {
                confidence,
                captured_at_ms: frame.captured_at_ms,
            }));
        }
        Ok(None)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn frame_at(captured_at_ms: u64, amplitude: f32) -> AudioFrame {
        AudioFrame::new(vec![amplitude; crate::SAMPLES_PER_FRAME], captured_at_ms)
    }

    #[tokio::test]
    async fn does_not_fire_during_warmup() {
        let mut d = EnergyVadWakeDetector::new();
        // Even a loud burst during warmup is silent.
        for ms in (0..u64::from(WARMUP_FRAMES) * 20).step_by(20) {
            let f = frame_at(ms, 0.9);
            assert!(
                d.observe(&f).await.unwrap().is_none(),
                "warm-up fired at {ms}ms"
            );
        }
    }

    #[tokio::test]
    async fn fires_on_sustained_loud_after_quiet_warmup() {
        let mut d = EnergyVadWakeDetector::new();
        // Warm up against quiet background.
        for ms in (0..u64::from(WARMUP_FRAMES) * 20).step_by(20) {
            let _ = d.observe(&frame_at(ms, 0.005)).await.unwrap();
        }
        // Now feed sustained speech-amplitude frames; should fire
        // within SUSTAINED_FRAMES + a small margin.
        let mut fired_at = None;
        for i in 0..(SUSTAINED_FRAMES + 4) {
            let ms = u64::from(WARMUP_FRAMES) * 20 + (u64::from(i) * 20);
            let res = d.observe(&frame_at(ms, 0.4)).await.unwrap();
            if let Some(w) = res {
                fired_at = Some(w.captured_at_ms);
                break;
            }
        }
        assert!(
            fired_at.is_some(),
            "expected a wake within sustained-frame window"
        );
    }

    #[tokio::test]
    async fn does_not_fire_on_single_loud_click() {
        let mut d = EnergyVadWakeDetector::new();
        for ms in (0..u64::from(WARMUP_FRAMES) * 20).step_by(20) {
            let _ = d.observe(&frame_at(ms, 0.005)).await.unwrap();
        }
        // One loud frame followed by silence — clicks shouldn't
        // fire. SUSTAINED_FRAMES = 4 so a single loud frame's
        // run-counter increments to 1 then resets.
        let base = u64::from(WARMUP_FRAMES) * 20;
        let click = d.observe(&frame_at(base, 0.9)).await.unwrap();
        assert!(click.is_none(), "single click fired wake");
        for i in 1..10 {
            let ms = base + i * 20;
            assert!(d.observe(&frame_at(ms, 0.005)).await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn enforces_cooldown_after_fire() {
        let mut d = EnergyVadWakeDetector::new();
        for ms in (0..u64::from(WARMUP_FRAMES) * 20).step_by(20) {
            let _ = d.observe(&frame_at(ms, 0.005)).await.unwrap();
        }
        // Fire once.
        let mut base = u64::from(WARMUP_FRAMES) * 20;
        let mut fired = false;
        for i in 0..10 {
            let ms = base + (i * 20);
            if d.observe(&frame_at(ms, 0.5)).await.unwrap().is_some() {
                fired = true;
                base = ms + 20;
                break;
            }
        }
        assert!(fired, "did not fire on speech");
        // Now during cooldown, even sustained loud frames don't
        // re-fire. COOLDOWN_FRAMES = 50.
        for i in 0..u64::from(COOLDOWN_FRAMES) {
            let ms = base + i * 20;
            assert!(
                d.observe(&frame_at(ms, 0.6)).await.unwrap().is_none(),
                "cooldown-period fire at offset {i}"
            );
        }
    }

    #[test]
    fn current_threshold_floors_at_min() {
        let d = EnergyVadWakeDetector::new();
        // Before warm-up: noise_floor = 0.0, variance = 0.0, so
        // the formula gives 0. Threshold should still floor at
        // MIN_THRESHOLD so a fully-silent room can't trigger
        // wakes on quantisation noise.
        assert!((d.current_threshold() - MIN_THRESHOLD).abs() < f32::EPSILON);
    }
}
