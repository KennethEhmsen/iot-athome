//! Text-to-speech (response path).
//!
//! Not wired into the v1 detection loop — the daemon binary calls
//! `Synthesizer::speak` separately when a rule fires an `ack` or
//! when the operator runs `iotctl voice say "<text>"`. The pipeline
//! itself is detection-only.
//!
//! Real impl plan (M5b W3+):
//!
//! * **Piper** — pure-C++ upstream, ONNX Runtime backed; high
//!   quality, ~50 MB voice model, ~80 ms synthesis on a Pi 5. Rust
//!   bindings exist but are early; FFI fallback is the safer path.
//! * **espeak-ng** — older, lower-quality, but ships in every
//!   Linux distro. Acceptable for system messages where naturalness
//!   doesn't matter.
//!
//! For the v1 scaffold this module ships only the trait + a
//! StubSynthesizer that returns a zero-filled `Vec<f32>` matching
//! the requested duration. Tests don't actually compare audio
//! content; they assert the trait was called.

use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SynthError {
    #[error("synthesis backend: {0}")]
    Backend(String),
}

/// Synthesised audio output. Same shape as
/// [`crate::audio::AudioFrame`] — 16 kHz mono `f32`. The daemon's
/// playback path resamples to whatever the platform output device
/// expects.
#[derive(Debug, Clone)]
pub struct SynthesisedAudio {
    pub samples: Vec<f32>,
    /// Total duration in milliseconds. Caller pre-allocates output
    /// buffers from this.
    pub duration_ms: u32,
}

#[async_trait]
pub trait Synthesizer: Send {
    /// Render `text` into 16 kHz mono `f32` audio.
    async fn speak(&mut self, text: &str) -> Result<SynthesisedAudio, SynthError>;
}

/// Test-grade synthesiser. Returns silence proportional to text
/// length (heuristic: 80 ms per word). Lets tests assert on the
/// `duration_ms` field without bundling a model.
#[derive(Debug, Default)]
pub struct StubSynthesizer;

impl StubSynthesizer {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Synthesizer for StubSynthesizer {
    async fn speak(&mut self, text: &str) -> Result<SynthesisedAudio, SynthError> {
        let words = text.split_whitespace().count().max(1);
        // 80 ms/word — a comfortable speaking pace.
        let duration_ms = u32::try_from(words * 80).unwrap_or(u32::MAX);
        let samples = vec![0.0; (crate::SAMPLE_RATE_HZ * duration_ms / 1000) as usize];
        Ok(SynthesisedAudio {
            samples,
            duration_ms,
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stub_synthesizer_scales_with_word_count() {
        let mut s = StubSynthesizer::new();
        let one = s.speak("hello").await.unwrap();
        let three = s.speak("hello there friend").await.unwrap();
        assert_eq!(one.duration_ms, 80);
        assert_eq!(three.duration_ms, 240);
    }

    #[tokio::test]
    async fn stub_synthesizer_handles_empty_text() {
        let mut s = StubSynthesizer::new();
        let out = s.speak("").await.unwrap();
        // We treat empty as one word's worth of silence rather than
        // returning a zero-length buffer — keeps downstream playback
        // code from a divide-by-zero or "did the device close?"
        // edge case.
        assert_eq!(out.duration_ms, 80);
    }
}
