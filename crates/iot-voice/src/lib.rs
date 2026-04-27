//! Local voice pipeline (M5b W2 — ADR-0015).
//!
//! ## Shape
//!
//! ```text
//! AudioSource → WakeDetector → SpeechRecognizer → IntentParser → IntentSink
//!                                                                    │
//!                                                                    ↓
//!                                                              NATS publish
//!                                                              command.intent.<dom>.<verb>
//! ```
//!
//! The pipeline is expressed as a chain of traits so each stage swaps
//! independently — Whisper for the M3-style English-only first cut,
//! Vosk for offline multilingual, Porcupine vs openWakeWord for the
//! wake stage, etc. Each trait has a `Stub*` impl in the same module
//! so the assembled pipeline composes cleanly under `cargo test`
//! without any heavy native deps.
//!
//! ## What this scaffold ships
//!
//! * Traits + stub impls for every pipeline stage.
//! * A `Pipeline` struct that wires them together with the `<800 ms`
//!   wake-to-action budget per ADR-0015.
//! * A closed-domain `IntentParser` that recognises a starter phrase
//!   set covering lights / scenes / sensors. Real LLM-fallback NLU is
//!   M5b W3+ scope.
//! * An end-to-end test (`tests/end_to_end.rs`) that drives the
//!   pipeline with a synthetic `Vec<f32>` audio buffer and asserts an
//!   `Intent` lands in the sink.
//!
//! ## What this scaffold doesn't ship (later commits)
//!
//! * Real audio capture (`cpal` on host, PCM-over-NATS for the future
//!   ESP32 satellites — ADR-0015 §Negative).
//! * Real wake-word detector (openWakeWord — likely FFI; or Porcupine
//!   under a commercial license; or a small CRNN we ship).
//! * Real STT (whisper-rs — needs CMake bootstrap; cross-platform CI
//!   gets an extra job).
//! * Real TTS (Piper — pure-Python upstream; Rust binding is nascent).
//! * Bus adapter (`NatsIntentSink` writing to `command.intent.*`).
//! * Daemon binary (`iot-voice` bin entry).

#![forbid(unsafe_code)]

pub mod audio;
#[cfg(feature = "cpal")]
pub mod cpal_audio;
pub mod intent;
pub mod pipeline;
pub mod piper_synth;
#[cfg(feature = "wake-phrase")]
pub mod rustpotter_wake;
pub mod stt;
pub mod synth;
pub mod wake;
pub mod wake_vad;
#[cfg(feature = "whisper")]
pub mod whisper;

pub use audio::{AudioFrame, AudioSource, AudioSourceError, StubAudioSource};
#[cfg(feature = "cpal")]
pub use cpal_audio::CpalAudioSource;
pub use intent::{
    Intent, IntentError, IntentParser, IntentSink, LogIntentSink, RuleIntentParser, StubIntentSink,
};
pub use pipeline::{Pipeline, PipelineError, PipelineMetrics};
pub use piper_synth::PiperBinarySynthesizer;
#[cfg(feature = "wake-phrase")]
pub use rustpotter_wake::RustpotterWakeDetector;
pub use stt::{SpeechRecognizer, SttError, StubSpeechRecognizer};
pub use synth::{StubSynthesizer, SynthError, SynthesisedAudio, Synthesizer};
pub use wake::{StubWakeDetector, Wake, WakeDetector, WakeError};
pub use wake_vad::EnergyVadWakeDetector;
#[cfg(feature = "whisper")]
pub use whisper::WhisperRecognizer;

/// Sample rate the entire pipeline operates at — 16 kHz mono `f32`.
///
/// Picked to match what Whisper's small/base models expect (16 kHz)
/// and what openWakeWord and most modern wake-word stacks consume.
/// Higher rates give marginal STT gains but blow the per-stage
/// latency budget; lower rates degrade wake-detection accuracy.
pub const SAMPLE_RATE_HZ: u32 = 16_000;

/// Audio frame duration — 20 ms is the standard "VAD frame".
/// At 16 kHz that's 320 samples per frame.
pub const FRAME_DURATION_MS: u32 = 20;

/// Samples per audio frame. Compile-time invariant.
pub const SAMPLES_PER_FRAME: usize = (SAMPLE_RATE_HZ as usize * FRAME_DURATION_MS as usize) / 1000;

const _: () = {
    // Sanity: the sample-per-frame math must produce an integer at
    // the canonical (16 kHz, 20 ms) point. A future rate change that
    // doesn't divide evenly should fail at compile time, not at the
    // first wake-detection callback.
    assert!(SAMPLES_PER_FRAME == 320);
    // Latency budget sanity — frame duration must fit inside the
    // wake-detection slice of the 800 ms budget (200 ms per ADR-0015).
    assert!(FRAME_DURATION_MS <= 200);
};
