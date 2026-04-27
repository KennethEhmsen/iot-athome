//! Speech-to-text. Post-wake transcription of an utterance window.
//!
//! Real impl plan (M5b W3+):
//!
//! * **whisper-rs** — wraps `whisper.cpp`; CMake bootstrap; ~150 MB
//!   for a small int8 model; <200 ms inference on a Pi 5 for short
//!   utterances. The realistic v1 default.
//! * **Vosk** — alternative; better multilingual; bigger models;
//!   licence is Apache 2.0 (Vosk) but Kaldi underneath is mixed.
//! * **Whisper-base via ONNX-runtime** — middle path; smaller binary
//!   surface than whisper.cpp, but ORT bootstrap is its own load.
//!
//! For the v1 scaffold this module ships:
//!
//! * The `SpeechRecognizer` trait taking `&[AudioFrame]` (the post-
//!   wake utterance window) and returning a transcription `String`.
//! * `StubSpeechRecognizer` that maps the *first sample* of the
//!   first frame to a pre-baked phrase from a lookup table — good
//!   enough to drive end-to-end tests deterministically without
//!   bundling a model.

use async_trait::async_trait;
use thiserror::Error;

use crate::audio::AudioFrame;

#[derive(Debug, Error)]
pub enum SttError {
    /// Backend (whisper, vosk, ORT, …) failed.
    #[error("stt backend: {0}")]
    Backend(String),
    /// The utterance window was empty — caller didn't provide any
    /// post-wake audio. Shouldn't happen via [`crate::Pipeline`]
    /// (the pipeline assembles a window before calling), but the
    /// trait method is `pub` so we surface the contract.
    #[error("empty utterance window")]
    Empty,
}

/// Transcribe an utterance into text.
#[async_trait]
pub trait SpeechRecognizer: Send {
    /// `frames` is the audio window starting at the first frame
    /// after a [`crate::wake::Wake`] event and running until either
    /// (a) end-of-utterance silence is detected by the pipeline, or
    /// (b) a wall-clock deadline elapses.
    ///
    /// The trait doesn't constrain language detection — real impls
    /// surface the locale they decoded as a separate side channel
    /// (a future revision returns `Transcription { text, lang,
    /// confidence }`). For the v1 scaffold we return raw text; the
    /// `IntentParser` is English-only anyway.
    async fn transcribe(&mut self, frames: &[AudioFrame]) -> Result<String, SttError>;
}

/// Test-grade recogniser that returns canned phrases keyed on a
/// hash of the audio window's first sample byte.
///
/// Reproducible: same input frames → same transcription. Lets tests
/// drive specific intents without a model.
#[derive(Debug, Default)]
pub struct StubSpeechRecognizer {
    /// Map keyed on the first frame's first-sample sign-flag byte
    /// so tests can swap a sample value (e.g. `0.7` vs `-0.7`) to
    /// pick a transcription.
    phrases: std::collections::HashMap<u8, String>,
    /// Fallback if no key matches.
    default_phrase: String,
}

impl StubSpeechRecognizer {
    /// Empty stub — every transcription returns the default phrase.
    #[must_use]
    pub fn new(default_phrase: impl Into<String>) -> Self {
        Self {
            phrases: std::collections::HashMap::new(),
            default_phrase: default_phrase.into(),
        }
    }

    /// Add a `(key, phrase)` mapping. `key` is the byte the test
    /// will encode as the first frame's first-sample sign+magnitude
    /// pattern (see [`StubSpeechRecognizer::lookup_key`]).
    #[must_use]
    pub fn with_mapping(mut self, key: u8, phrase: impl Into<String>) -> Self {
        self.phrases.insert(key, phrase.into());
        self
    }

    /// Derive the lookup key from the first frame. Public so tests
    /// can compute the matching key without copying the formula.
    ///
    /// The `as u8` cast at the end is bounded by construction:
    /// `clamp(-1.0, 1.0)` → `[-1.0, 1.0]`, `+ 1.0` → `[0.0, 2.0]`,
    /// `* 127.0` → `[0.0, 254.0]`. Always non-negative, always
    /// under 255 — clippy's truncation/sign-loss warnings don't
    /// apply, but it can't see that.
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn lookup_key(frames: &[AudioFrame]) -> u8 {
        let first = frames.first().and_then(|f| f.samples.first()).copied();
        first.map_or(0, |s| ((s.clamp(-1.0, 1.0) + 1.0) * 127.0) as u8)
    }
}

#[async_trait]
impl SpeechRecognizer for StubSpeechRecognizer {
    async fn transcribe(&mut self, frames: &[AudioFrame]) -> Result<String, SttError> {
        if frames.is_empty() {
            return Err(SttError::Empty);
        }
        let key = Self::lookup_key(frames);
        Ok(self
            .phrases
            .get(&key)
            .cloned()
            .unwrap_or_else(|| self.default_phrase.clone()))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn frame_with_first_sample(s: f32) -> AudioFrame {
        let mut samples = vec![0.0; crate::SAMPLES_PER_FRAME];
        samples[0] = s;
        AudioFrame::new(samples, 0)
    }

    #[tokio::test]
    async fn stub_returns_default_when_no_mapping() {
        let mut s = StubSpeechRecognizer::new("turn on kitchen light");
        let text = s.transcribe(&[frame_with_first_sample(0.0)]).await.unwrap();
        assert_eq!(text, "turn on kitchen light");
    }

    #[tokio::test]
    async fn stub_returns_mapped_phrase() {
        let frame = frame_with_first_sample(0.5);
        let key = StubSpeechRecognizer::lookup_key(std::slice::from_ref(&frame));
        let mut s = StubSpeechRecognizer::new("default").with_mapping(key, "activate movie scene");
        let text = s.transcribe(std::slice::from_ref(&frame)).await.unwrap();
        assert_eq!(text, "activate movie scene");
    }

    #[tokio::test]
    async fn stub_rejects_empty_window() {
        let mut s = StubSpeechRecognizer::new("default");
        let err = s.transcribe(&[]).await.unwrap_err();
        assert!(matches!(err, SttError::Empty), "got {err:?}");
    }
}
