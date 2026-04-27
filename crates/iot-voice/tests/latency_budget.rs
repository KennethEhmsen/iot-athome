//! ADR-0015 §"Latency targets" — synthetic-input latency benchmarks.
//!
//! These run as integration tests (cheap; pure-Rust stubs) but
//! double as a budget watchdog. The numbers measured against
//! stub stages are floor estimates — pipeline overhead with no
//! model inference cost. Real numbers (whisper `full()` etc.)
//! land much closer to the 800 ms budget; the stubs here check
//! that the *pipeline overhead itself* stays sub-budget.
//!
//! ## Budget table (from ADR-0015)
//!
//! | Stage              | Budget |
//! |--------------------|--------|
//! | Wake detection     | 200 ms |
//! | Endpoint detection | 400 ms |
//! | STT inference      | 300 ms |
//! | NLU + dispatch     |  50 ms |
//! | **Total**          | **800 ms** |
//!
//! The asserts here use 2× budget as the soft-fail threshold —
//! enough headroom that GitHub Actions runner noise (which runs
//! tests on shared cores under CPU throttling) doesn't false-flag.
//! When real model impls land (M5b W4c whisper-rs is in-tree
//! behind `--features stt-whisper`), we add a separate
//! `bench`-style harness that measures wall-clock against real
//! `.bin` model files; that's a manual / nightly job, not a CI
//! gate.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use iot_voice::{
    AudioFrame, EnergyVadWakeDetector, Pipeline, PipelineMetrics, RuleIntentParser,
    StubAudioSource, StubIntentSink, StubSpeechRecognizer, SAMPLES_PER_FRAME,
};

fn loud_frame(captured_at_ms: u64, amplitude: f32) -> AudioFrame {
    AudioFrame::new(vec![amplitude; SAMPLES_PER_FRAME], captured_at_ms)
}

fn silence(captured_at_ms: u64) -> AudioFrame {
    AudioFrame::silence(captured_at_ms)
}

/// Build a fixture: 100 frames of warmup-quiet (energy-VAD adapts),
/// then a sustained-loud burst (triggers wake), then speech-amplitude
/// frames (utterance), then silence (endpoint detection).
fn pipeline_input() -> Vec<AudioFrame> {
    let mut frames = Vec::new();
    // 100 frames of quiet for the VAD warmup.
    for i in 0..100u64 {
        frames.push(AudioFrame::new(vec![0.005; SAMPLES_PER_FRAME], 20 * i));
    }
    // Sustained loud — > 4 frames triggers VAD wake.
    for i in 100..120u64 {
        frames.push(loud_frame(20 * i, 0.5));
    }
    // Trailing silence — endpoint detection fires after ~12 silent frames.
    for i in 120..150u64 {
        frames.push(silence(20 * i));
    }
    frames
}

/// Wall-clock the full pipeline cycle against stub stages.
/// Asserts the total stays under 2× ADR-0015's 800 ms budget —
/// catches regressions where pipeline plumbing overhead grows
/// (lock contention, accidental synchronous waits, etc.) without
/// requiring real models.
#[tokio::test]
async fn pipeline_cycle_under_2x_total_budget() {
    let audio = StubAudioSource::new(pipeline_input());
    let wake = EnergyVadWakeDetector::new();
    let stt = StubSpeechRecognizer::new("turn on the kitchen light");
    let parser = RuleIntentParser::new();
    let sink = StubIntentSink::new();
    let sink_handle = sink.clone();

    let mut pipeline = Pipeline::new(audio, wake, stt, parser, Arc::new(sink));

    let started = Instant::now();
    pipeline.run().await.expect("pipeline ran");
    let elapsed = started.elapsed();

    // The stub stages do almost nothing — total wall-clock should
    // be measured in microseconds, not milliseconds. 2× the
    // 800 ms budget is an extremely permissive ceiling that
    // catches catastrophic regressions (deadlock, sync sleep,
    // synchronous I/O) without flagging on shared-runner jitter.
    let ceiling = PipelineMetrics::TOTAL_BUDGET * 2;
    assert!(
        elapsed < ceiling,
        "pipeline cycle took {elapsed:?}, exceeds 2× budget ({ceiling:?})"
    );

    // Confirm the cycle actually fired (otherwise the latency
    // assertion is meaningless — a panic during stage-1 would
    // also produce a fast elapsed time).
    let dispatched = sink_handle.snapshot().await;
    assert_eq!(dispatched.len(), 1, "expected one intent dispatched");
    assert_eq!(dispatched[0].domain, "lights");
}

/// Per-stage ceiling check. Each stage's recorded latency must
/// stay under 2× its allotment. With stub impls they all measure
/// in low microseconds; the value is regression-detection, not
/// realistic model timing.
#[tokio::test]
async fn each_stage_under_2x_budget() {
    let audio = StubAudioSource::new(pipeline_input());
    let wake = EnergyVadWakeDetector::new();
    let stt = StubSpeechRecognizer::new("turn on the kitchen light");
    let parser = RuleIntentParser::new();
    let sink = StubIntentSink::new();

    let mut pipeline = Pipeline::new(audio, wake, stt, parser, Arc::new(sink));
    pipeline.run().await.expect("pipeline ran");

    let m = &pipeline.metrics;
    assert_stage("wake", m.wake_latency, Duration::from_millis(400));
    assert_stage("endpoint", m.endpoint_latency, Duration::from_millis(800));
    assert_stage("stt", m.stt_latency, Duration::from_millis(600));
    assert_stage("nlu", m.nlu_latency, Duration::from_millis(100));
}

fn assert_stage(name: &str, actual: Option<Duration>, ceiling: Duration) {
    let actual = actual.unwrap_or_else(|| panic!("{name} latency was None — stage didn't run"));
    assert!(
        actual < ceiling,
        "{name} stage took {actual:?}, exceeds 2× budget ({ceiling:?})"
    );
}

/// Sanity: pipeline-end-to-end-fast-path. With stub stages, the
/// whole cycle is far under 100 ms. This is the "did anything
/// regress catastrophically" canary — flag-worthy if the cycle
/// jumps from microseconds to >100 ms even though no real model
/// is in the loop.
#[tokio::test]
async fn pipeline_cycle_under_100ms_with_stub_stages() {
    let audio = StubAudioSource::new(pipeline_input());
    let wake = EnergyVadWakeDetector::new();
    let stt = StubSpeechRecognizer::new("turn on the kitchen light");
    let parser = RuleIntentParser::new();
    let sink = StubIntentSink::new();

    let mut pipeline = Pipeline::new(audio, wake, stt, parser, Arc::new(sink));

    let started = Instant::now();
    pipeline.run().await.expect("pipeline ran");
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_millis(100),
        "pipeline-overhead-only cycle took {elapsed:?}; expected < 100 ms — \
         a regression in the pipeline plumbing rather than model cost"
    );
}
