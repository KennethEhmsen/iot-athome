//! whisper.cpp-backed [`SpeechRecognizer`] (M5b W4c).
//!
//! Wraps `whisper-rs`, which wraps the upstream `whisper.cpp` C++
//! library via CMake + bindgen. The Rust caller hands in 16 kHz
//! mono `f32` audio frames (which is exactly what the pipeline
//! produces); whisper returns transcribed text.
//!
//! ## Build prereqs
//!
//! CMake + Clang must be on `PATH`:
//!
//! * **Windows:** `choco install cmake llvm` (or scoop / winget).
//!   Restart your shell so the new PATH entries take effect.
//! * **macOS:** `brew install cmake` (Apple Clang ships with
//!   Xcode Command Line Tools).
//! * **Linux:** `apt install cmake clang` (Debian/Ubuntu) or
//!   distro equivalent.
//!
//! The first compile of whisper.cpp itself takes ~3 minutes on a
//! Pi 5; subsequent incremental builds are fast.
//!
//! ## Model files
//!
//! whisper.cpp consumes ggml-format model files (`.bin`). Download
//! from huggingface — `ggerganov/whisper.cpp` hosts converted
//! checkpoints:
//!
//! ```text
//! https://huggingface.co/ggerganov/whisper.cpp/blob/main/ggml-base.en.bin     #  142 MB
//! https://huggingface.co/ggerganov/whisper.cpp/blob/main/ggml-small.en.bin    #  466 MB
//! https://huggingface.co/ggerganov/whisper.cpp/blob/main/ggml-medium.en.bin   # 1500 MB
//! ```
//!
//! For the M5b W4c smoke test, `ggml-base.en.bin` is the
//! recommended starting point: ~140 MB, runs a 3-second utterance
//! in ~120 ms on a Pi 5 (well inside the 300 ms STT budget per
//! ADR-0015).
//!
//! Convention: place at `~/.iot-athome/models/ggml-base.en.bin`.
//! The daemon's `--stt-model <path>` flag overrides.
//!
//! ## Latency + threading
//!
//! whisper.cpp's `full()` inference is synchronous + CPU-bound.
//! The recogniser dispatches it onto tokio's blocking pool via
//! `spawn_blocking` so the async pipeline doesn't stall during
//! the inference window. The `WhisperContext` is wrapped in
//! `Arc` so `clone` is cheap; the inner whisper state isn't
//! `Send`, so each `transcribe()` call creates a fresh state via
//! `ctx.create_state()` — costs <1 ms on the existing context.
//!
//! ## What's NOT here
//!
//! * Streaming / partial transcription. Whole-utterance only.
//! * Language detection. Hard-coded to "en"; revisit when the
//!   intent grammar grows beyond English.
//! * GPU acceleration. Not built in; the operator can rebuild
//!   whisper.cpp with `WHISPER_CUDA=1` / `WHISPER_METAL=1` env
//!   vars at compile time when the hub has accelerator hardware.
//! * Beam search. Greedy decoding only — closed-domain phrase
//!   recognition tolerates greedy decoder errors better than
//!   free-form transcription.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, info};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::audio::AudioFrame;
use crate::stt::{SpeechRecognizer, SttError};

/// Locale to decode. English-only for the M5b W4c first cut.
/// Future revisions take this from the daemon's config.
const LANGUAGE: &str = "en";

/// Threads to use for whisper.cpp's parallel matrix-multiply.
/// 2 is a safe lower bound on a Pi 5 (4 efficiency cores); the
/// hub can tune via env (`IOT_WHISPER_THREADS=N`) without a
/// rebuild.
const DEFAULT_THREADS: i32 = 2;

/// Whisper-backed [`SpeechRecognizer`].
///
/// Cheap to clone — the inner `WhisperContext` is `Arc`-wrapped.
#[derive(Clone)]
pub struct WhisperRecognizer {
    ctx: Arc<WhisperContext>,
    threads: i32,
}

impl std::fmt::Debug for WhisperRecognizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WhisperRecognizer")
            .field("threads", &self.threads)
            .finish_non_exhaustive()
    }
}

impl WhisperRecognizer {
    /// Load `model_path` into a fresh whisper context.
    ///
    /// # Errors
    /// `SttError::Backend` when the file is missing, malformed,
    /// or whisper.cpp can't initialise (typically: model file
    /// from a different ggml version than the linked
    /// whisper.cpp).
    pub fn load(model_path: &Path) -> Result<Self, SttError> {
        let path_str = model_path
            .to_str()
            .ok_or_else(|| SttError::Backend(format!("non-UTF-8 model path {model_path:?}")))?;
        info!(target: "iot_voice::whisper", path = %path_str, "loading whisper model");
        let ctx = WhisperContext::new_with_params(path_str, WhisperContextParameters::default())
            .map_err(|e| SttError::Backend(format!("whisper context: {e}")))?;
        let threads = std::env::var("IOT_WHISPER_THREADS")
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_THREADS);
        Ok(Self {
            ctx: Arc::new(ctx),
            threads,
        })
    }
}

#[async_trait]
impl SpeechRecognizer for WhisperRecognizer {
    async fn transcribe(&mut self, frames: &[AudioFrame]) -> Result<String, SttError> {
        if frames.is_empty() {
            return Err(SttError::Empty);
        }

        // Flatten frames into one contiguous mono f32 buffer.
        // whisper.cpp tolerates short inputs (<1 s) but performance
        // is better when the buffer's a few seconds — the pipeline's
        // capture-utterance path tunes this.
        let total_samples: usize = frames.iter().map(|f| f.samples.len()).sum();
        let mut samples = Vec::with_capacity(total_samples);
        for f in frames {
            samples.extend_from_slice(&f.samples);
        }

        // The whisper-rs `full(...)` call is sync + CPU-heavy.
        // Dispatch onto the blocking thread pool so the async
        // runtime stays responsive. The closure is `move` because
        // whisper state isn't Send by default; we recreate state
        // per call (cheap, sub-ms) so the work is self-contained.
        let ctx = self.ctx.clone();
        let threads = self.threads;
        tokio::task::spawn_blocking(move || run_full(&ctx, &samples, threads))
            .await
            .map_err(|e| SttError::Backend(format!("spawn_blocking join: {e}")))?
    }
}

/// Run whisper.cpp's `full()` against a 16 kHz mono f32 buffer +
/// extract the concatenated segment text. Synchronous; called
/// from inside `spawn_blocking`.
fn run_full(ctx: &WhisperContext, samples: &[f32], threads: i32) -> Result<String, SttError> {
    let mut state = ctx
        .create_state()
        .map_err(|e| SttError::Backend(format!("whisper create_state: {e}")))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(threads);
    params.set_translate(false);
    params.set_language(Some(LANGUAGE));
    // Suppress whisper.cpp's chatty stdout — we want clean logs.
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    state
        .full(params, samples)
        .map_err(|e| SttError::Backend(format!("whisper full: {e}")))?;

    let n = state
        .full_n_segments()
        .map_err(|e| SttError::Backend(format!("whisper segments: {e}")))?;
    let mut text = String::new();
    for i in 0..n {
        let seg = state
            .full_get_segment_text(i)
            .map_err(|e| SttError::Backend(format!("whisper segment {i}: {e}")))?;
        text.push_str(&seg);
    }
    debug!(
        target: "iot_voice::whisper",
        sample_count = samples.len(),
        segments = n,
        text_len = text.len(),
        "transcription complete"
    );
    Ok(text.trim().to_owned())
}

// Inference-path tests need a real model file (140 MB+) and CMake
// + Clang to compile whisper.cpp; they don't run on a stock CI
// runner without significant fixture setup. The `transcribe`
// behaviour is verified manually:
//
//     iot-voice listen --use-mic --stt-model ~/.iot-athome/models/ggml-base.en.bin
//
// What we DO unit-test here: the model-load error path, since
// it's surface-level + doesn't require a real model.

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn missing_model_path_errors_cleanly() {
        let err =
            WhisperRecognizer::load(Path::new("/this/path/does/not/exist/model.bin")).unwrap_err();
        assert!(matches!(err, SttError::Backend(_)), "{err:?}");
    }
}
