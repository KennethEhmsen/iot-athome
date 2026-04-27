//! Whisper STT smoke test (M5b W4c follow-up).
//!
//! Runs against a real ggml-format whisper.cpp model file when
//! `IOT_WHISPER_MODEL_PATH` is set in the environment. Without the
//! env var, the test self-skips with a clear message — same
//! pattern iot-history's testcontainers-gated tests use.
//!
//! ## Why ignored by default
//!
//! * The model is 140 MB (`ggml-base.en.bin` recommended) —
//!   too big to vendor into the repo or download in CI.
//! * Whisper inference is ~120-200 ms per utterance on a
//!   typical dev machine — fine for a smoke test, not so
//!   great for the default-fast-test loop.
//! * The test requires `--features stt-whisper`, which in
//!   turn requires CMake + Clang on PATH (M5b W4c
//!   build-prereq ladder).
//!
//! ## How to run
//!
//! ```sh
//! # Once: download a model.
//! mkdir -p ~/.iot-athome/models
//! curl -L \
//!   https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin \
//!   -o ~/.iot-athome/models/ggml-base.en.bin
//!
//! # Per run:
//! IOT_WHISPER_MODEL_PATH=~/.iot-athome/models/ggml-base.en.bin \
//!   cargo test --features stt-whisper \
//!     --test whisper_smoke \
//!     -- --ignored
//! ```
//!
//! ## What this verifies
//!
//! End-to-end:
//! 1. The `WhisperRecognizer::load(path)` accepts a real ggml
//!    model file.
//! 2. `transcribe(&[silent_frame; N])` returns Ok — whisper
//!    doesn't choke on synthetic silence input.
//! 3. The returned text is empty-ish or matches a known
//!    "background noise" string — pinning a regression in the
//!    whisper-rs API would surface here.
//!
//! What this does NOT verify:
//! * Real-speech transcription accuracy. That's a
//!   benchmark concern, not a CI gate; manual testing via
//!   `iot-voice listen --use-mic --stt-model …` is the path.
//! * Latency. The latency_budget.rs tests cover stub-path
//!   timing; real-model latency benchmarks are a separate
//!   manual harness (M5b W4 retrospective).

#![cfg(feature = "whisper")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use iot_voice::{AudioFrame, SpeechRecognizer, WhisperRecognizer, SAMPLES_PER_FRAME};

/// Generate `count` 20 ms frames of silent audio. Whisper handles
/// silent input gracefully (returns empty or a short non-speech
/// hallucination); the test asserts on shape, not content.
fn silent_window(count: usize) -> Vec<AudioFrame> {
    (0..count)
        .map(|i| AudioFrame::silence((i as u64) * 20))
        .collect()
}

/// Generate `count` frames of low-amplitude white noise. Useful
/// because some whisper builds bail on totally-zero input
/// ("no audio detected"); 0.001 amplitude looks like a quiet
/// room to the model.
fn quiet_window(count: usize) -> Vec<AudioFrame> {
    (0..count)
        .map(|i| {
            let mut samples = vec![0.0_f32; SAMPLES_PER_FRAME];
            for (j, sample) in samples.iter_mut().enumerate() {
                // Cheap deterministic dither (no rand dep needed).
                let seed = (i * SAMPLES_PER_FRAME + j) as u64;
                #[allow(clippy::cast_precision_loss)]
                let v = ((seed.wrapping_mul(2_654_435_761)) % 1000) as f32 / 1_000_000.0;
                *sample = v - 0.0005;
            }
            AudioFrame::new(samples, (i as u64) * 20)
        })
        .collect()
}

#[tokio::test]
#[ignore = "needs IOT_WHISPER_MODEL_PATH + ~140MB model file"]
async fn whisper_loads_model_and_transcribes_silence() {
    let path = match std::env::var("IOT_WHISPER_MODEL_PATH") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            // Self-skip with a clear log line — better UX than
            // panicking on missing env when the test was opted
            // in via --ignored.
            eprintln!(
                "skipping whisper smoke test: IOT_WHISPER_MODEL_PATH not set; \
                 see crates/iot-voice/tests/whisper_smoke.rs module docs for setup"
            );
            return;
        }
    };
    if !path.is_file() {
        eprintln!(
            "skipping whisper smoke test: IOT_WHISPER_MODEL_PATH={} does not point at a file",
            path.display()
        );
        return;
    }

    let mut recognizer = WhisperRecognizer::load(&path).expect("load whisper model");

    // 1 second of silence (50 frames @ 20 ms). Whisper needs at
    // least ~1 s of input or it short-circuits.
    let frames = silent_window(50);
    let text = recognizer.transcribe(&frames).await.expect("transcribe");

    // Whisper on silence typically returns empty string or a
    // short "[BLANK_AUDIO]"-style placeholder. Either is fine;
    // we just confirm the path runs end-to-end without panic.
    eprintln!("whisper(silent 1s) -> {text:?}");
    assert!(text.len() < 200, "unexpectedly long text from silent input");
}

#[tokio::test]
#[ignore = "needs IOT_WHISPER_MODEL_PATH + ~140MB model file"]
async fn whisper_handles_quiet_noise_input() {
    let path = match std::env::var("IOT_WHISPER_MODEL_PATH") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            eprintln!("skipping (no IOT_WHISPER_MODEL_PATH)");
            return;
        }
    };
    if !path.is_file() {
        eprintln!("skipping (model file missing)");
        return;
    }

    let mut recognizer = WhisperRecognizer::load(&path).expect("load");

    // 1 second of quiet noise. Different code path than pure
    // silence in some whisper versions.
    let frames = quiet_window(50);
    let text = recognizer.transcribe(&frames).await.expect("transcribe");

    eprintln!("whisper(quiet 1s) -> {text:?}");
    // No content assertion — quiet noise shouldn't produce
    // actionable speech, but whisper sometimes hallucinates
    // short phrases on near-silent input.
    assert!(text.len() < 500);
}

#[tokio::test]
#[ignore = "needs IOT_WHISPER_MODEL_PATH"]
async fn whisper_recognizer_clones_cheaply() {
    // The Recognizer is `Clone` because `WhisperContext` is
    // `Arc`-wrapped. Verify that cloning + transcribing on the
    // clone doesn't error — protects against a future API
    // change that'd break the daemon's single-load /
    // multiple-task pattern.
    let path = match std::env::var("IOT_WHISPER_MODEL_PATH") {
        Ok(p) => PathBuf::from(p),
        Err(_) => return,
    };
    if !path.is_file() {
        return;
    }

    let mut a = WhisperRecognizer::load(&path).expect("load");
    let mut b = a.clone();

    let frames = silent_window(50);
    let _ = a.transcribe(&frames).await.expect("a transcribe");
    let _ = b.transcribe(&frames).await.expect("b transcribe");
}
