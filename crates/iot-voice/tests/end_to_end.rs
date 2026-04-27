//! End-to-end pipeline test using the stub implementations.
//!
//! Drives the full chain — audio → wake → stt → intent → sink —
//! and asserts the dispatched intent matches what the closed-domain
//! grammar should produce for a known transcription. This is the
//! "the scaffold composes" gate per ADR-0015 §M5b W1.
//!
//! The pipeline-internal tests (`src/pipeline.rs::tests`) cover the
//! state-machine edges (silence-only, end-of-stream during wake,
//! unrecognised phrase). This integration test exercises the
//! exported public API surface from outside the crate, which is
//! what a future daemon-binary test would do.

// Workspace lints forbid expect/unwrap/panic in production code.
// Tests are explicitly exempt — same allow-set as the in-lib
// `mod tests` blocks use.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use iot_voice::{
    AudioFrame, Pipeline, RuleIntentParser, StubAudioSource, StubIntentSink, StubSpeechRecognizer,
    StubWakeDetector, SAMPLES_PER_FRAME,
};

fn loud_frame(captured_at_ms: u64) -> AudioFrame {
    AudioFrame::new(vec![0.9; SAMPLES_PER_FRAME], captured_at_ms)
}

/// Speech-amplitude frame — above silence threshold, below wake
/// threshold. Models real post-wake utterance audio so
/// `capture_utterance` doesn't trim the whole window away.
fn speech_frame(captured_at_ms: u64) -> AudioFrame {
    AudioFrame::new(vec![0.2; SAMPLES_PER_FRAME], captured_at_ms)
}

/// Build the typical fixture: loud wake → 5 speech frames → 15
/// silence frames. End-of-utterance is detected after 12 silent
/// frames in a row; the 5 speech frames remain in the window.
fn standard_utterance() -> Vec<AudioFrame> {
    let mut frames = vec![loud_frame(0)];
    for i in 0u64..5 {
        frames.push(speech_frame(20 + 20 * i));
    }
    for i in 0u64..15 {
        frames.push(AudioFrame::silence(120 + 20 * i));
    }
    frames
}

#[tokio::test]
async fn end_to_end_lights_on_intent() {
    let frames = standard_utterance();

    let audio = StubAudioSource::new(frames);
    let wake = StubWakeDetector::default();
    // Stub STT returns the same canned phrase regardless of audio
    // content — adequate for shape testing.
    let stt = StubSpeechRecognizer::new("turn on the kitchen light");
    let parser = RuleIntentParser::new();
    let sink = StubIntentSink::new();
    let sink_handle = sink.clone();

    let mut pipeline = Pipeline::new(audio, wake, stt, parser, Arc::new(sink));
    pipeline.run().await.expect("pipeline ran cleanly");

    let dispatched = sink_handle.snapshot().await;
    assert_eq!(
        dispatched.len(),
        1,
        "expected exactly one intent dispatched"
    );
    let intent = &dispatched[0];
    assert_eq!(intent.domain, "lights");
    assert_eq!(intent.verb, "on");
    assert_eq!(intent.args, serde_json::json!({"target": "kitchen"}));
    assert_eq!(intent.raw, "turn on the kitchen light");
    assert!((0.94..=1.0).contains(&intent.confidence));
}

#[tokio::test]
async fn end_to_end_scene_activation() {
    let frames = standard_utterance();
    let audio = StubAudioSource::new(frames);
    let wake = StubWakeDetector::default();
    let stt = StubSpeechRecognizer::new("activate the movie scene");
    let parser = RuleIntentParser::new();
    let sink = StubIntentSink::new();
    let sink_handle = sink.clone();

    let mut pipeline = Pipeline::new(audio, wake, stt, parser, Arc::new(sink));
    pipeline.run().await.expect("pipeline ran cleanly");

    let dispatched = sink_handle.snapshot().await;
    assert_eq!(dispatched.len(), 1);
    assert_eq!(dispatched[0].domain, "scenes");
    assert_eq!(dispatched[0].verb, "activate");
    assert_eq!(dispatched[0].args, serde_json::json!({"target": "movie"}));
}

#[tokio::test]
async fn end_to_end_metrics_are_populated_after_a_cycle() {
    let frames = standard_utterance();
    let audio = StubAudioSource::new(frames);
    let wake = StubWakeDetector::default();
    let stt = StubSpeechRecognizer::new("turn off the bedroom light");
    let parser = RuleIntentParser::new();
    let sink = StubIntentSink::new();

    let mut pipeline = Pipeline::new(audio, wake, stt, parser, Arc::new(sink));
    pipeline.run().await.unwrap();

    assert!(
        pipeline.metrics.wake_latency.is_some(),
        "wake_latency missing"
    );
    assert!(
        pipeline.metrics.endpoint_latency.is_some(),
        "endpoint_latency missing"
    );
    assert!(
        pipeline.metrics.stt_latency.is_some(),
        "stt_latency missing"
    );
    assert!(
        pipeline.metrics.nlu_latency.is_some(),
        "nlu_latency missing"
    );

    // Stub stages are sub-microsecond, so the total latency for a
    // synthetic test cycle is far below the 800 ms ADR-0015 budget.
    // Real-world latencies will land closer to budget; we don't
    // assert on absolute numbers here — that's a benchmark concern,
    // not a unit-test one.
    let total = pipeline.metrics.total_latency();
    assert!(total < std::time::Duration::from_millis(100));
}
