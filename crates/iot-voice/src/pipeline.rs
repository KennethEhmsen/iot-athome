//! Pipeline assembly — wires AudioSource → WakeDetector →
//! SpeechRecognizer → IntentParser → IntentSink with the latency
//! budget per ADR-0015.
//!
//! The pipeline owns no models — it accepts each stage as a trait
//! object so the same code drives the test (`Stub*`) and the
//! production (whisper-rs / openWakeWord / piper) configurations.
//!
//! ## Latency contract
//!
//! ADR-0015 §"Latency targets" allots:
//!
//! | Stage              | Budget |
//! |--------------------|--------|
//! | Wake detection     | 200 ms |
//! | Endpoint detection | 400 ms |
//! | STT inference      | 300 ms |
//! | NLU + dispatch     |  50 ms |
//! | **Total**          | **800 ms** |
//!
//! The pipeline tracks each stage's measured latency in
//! [`PipelineMetrics`]. When the daemon emits an audit event it
//! uses these numbers; the operator UI surfaces a "voice is slow"
//! warning when any stage's p99 sustains over 2× budget.

use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;
use tracing::{debug, info, warn};

use crate::audio::{AudioFrame, AudioSource, AudioSourceError};
use crate::intent::{IntentError, IntentParser, IntentSink};
use crate::stt::{SpeechRecognizer, SttError};
use crate::wake::{Wake, WakeDetector, WakeError};

/// Maximum number of audio frames captured between wake and
/// end-of-utterance — a hard cap so a stuck VAD or a continuous-
/// speech edge case can't grow the post-wake window unboundedly.
/// 250 frames × 20 ms = 5 seconds. Most spoken intents finish well
/// under 3 s; the cap is a fence against the pathological case.
const MAX_UTTERANCE_FRAMES: usize = 250;

/// Number of consecutive low-amplitude frames after wake that count
/// as end-of-utterance. 12 frames × 20 ms = 240 ms — comfortably
/// inside ADR-0015's 400 ms endpoint-detection budget but long
/// enough to ride out the natural 100–200 ms inter-word pauses
/// that don't end a phrase.
///
/// The "low-amplitude" threshold is shared with the wake detector
/// for consistency — see [`SILENCE_PEAK_THRESHOLD`] below.
const SILENCE_TAIL_FRAMES: u32 = 12;

/// Peak-amplitude threshold below which a frame counts as silence
/// for the endpoint-detection state machine.
const SILENCE_PEAK_THRESHOLD: f32 = 0.05;

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("audio: {0}")]
    Audio(#[from] AudioSourceError),
    #[error("wake: {0}")]
    Wake(#[from] WakeError),
    #[error("stt: {0}")]
    Stt(#[from] SttError),
    /// Intent dispatch failure. Non-fatal at the daemon level — the
    /// outer supervisor logs and resumes — but the pipeline returns
    /// it so callers (incl. tests) can assert on it.
    #[error("intent: {0}")]
    Intent(#[from] IntentError),
}

/// Per-stage latency snapshot. `None` for stages that haven't run
/// yet during the current detection cycle.
#[derive(Debug, Default, Clone)]
pub struct PipelineMetrics {
    /// Wall-clock from pipeline start until [`WakeDetector`] fired.
    pub wake_latency: Option<Duration>,
    /// Wall-clock from wake until end-of-utterance silence run was
    /// hit (or [`MAX_UTTERANCE_FRAMES`] reached).
    pub endpoint_latency: Option<Duration>,
    /// Wall-clock spent inside [`SpeechRecognizer::transcribe`].
    pub stt_latency: Option<Duration>,
    /// Wall-clock spent inside [`IntentParser::parse`] +
    /// [`IntentSink::dispatch`].
    pub nlu_latency: Option<Duration>,
}

impl PipelineMetrics {
    /// Sum of all stages so far. The pipeline reports this as the
    /// "wake to action" latency in audit events.
    #[must_use]
    pub fn total_latency(&self) -> Duration {
        self.wake_latency.unwrap_or_default()
            + self.endpoint_latency.unwrap_or_default()
            + self.stt_latency.unwrap_or_default()
            + self.nlu_latency.unwrap_or_default()
    }

    /// Per ADR-0015 §"Latency targets", the total budget. Pipelines
    /// over this trigger a `warn!` log + audit event.
    pub const TOTAL_BUDGET: Duration = Duration::from_millis(800);
}

/// Wires the four pipeline stages together.
///
/// The daemon binary constructs one and calls [`Pipeline::run`] on
/// its main task; the `run` future completes only when the audio
/// source returns `Ok(None)` — typically never, in production — or
/// surfaces an error.
///
/// The struct owns its stages by value and exposes metrics by
/// reference (`&pipeline.metrics` after `run` completes). Single-
/// task by design: every `run` and `detect_one_cycle` method takes
/// `&mut self`, no internal locks.
pub struct Pipeline<A: AudioSource, W: WakeDetector, S: SpeechRecognizer, P: IntentParser> {
    pub audio: A,
    pub wake: W,
    pub stt: S,
    pub parser: P,
    pub sink: Arc<dyn IntentSink>,
    /// Metrics from the most-recent completed detection cycle.
    pub metrics: PipelineMetrics,
}

impl<A: AudioSource, W: WakeDetector, S: SpeechRecognizer, P: IntentParser> std::fmt::Debug
    for Pipeline<A, W, S, P>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Stages aren't Debug-bound and the trait-object sink won't
        // round-trip a useful diagnostic — use `finish_non_exhaustive`
        // so clippy's missing-fields-in-debug is satisfied.
        f.debug_struct("Pipeline")
            .field("metrics", &self.metrics)
            .finish_non_exhaustive()
    }
}

impl<A, W, S, P> Pipeline<A, W, S, P>
where
    A: AudioSource,
    W: WakeDetector,
    S: SpeechRecognizer,
    P: IntentParser,
{
    pub fn new(audio: A, wake: W, stt: S, parser: P, sink: Arc<dyn IntentSink>) -> Self {
        Self {
            audio,
            wake,
            stt,
            parser,
            sink,
            metrics: PipelineMetrics::default(),
        }
    }

    /// Drive the pipeline until the audio source returns `Ok(None)`.
    ///
    /// Each detection cycle is:
    ///
    /// 1. Pull frames from the source, feeding each to the wake
    ///    detector until it fires.
    /// 2. Once wake fires, capture frames until either
    ///    [`SILENCE_TAIL_FRAMES`] consecutive low-amplitude frames
    ///    or [`MAX_UTTERANCE_FRAMES`] total.
    /// 3. Hand the captured window to the STT.
    /// 4. Hand the transcription to the parser; on success,
    ///    dispatch via the sink.
    /// 5. Reset metrics + loop.
    ///
    /// On any error the loop bails — the supervising daemon
    /// restarts the pipeline. This is deliberately blunt: a
    /// per-cycle "log and continue" path obscures real
    /// hardware-level failures (mic disconnect, model crash) that
    /// the operator needs to see at the supervisor level.
    pub async fn run(&mut self) -> Result<(), PipelineError> {
        let cycle_start = Instant::now();
        loop {
            match self.detect_one_cycle(cycle_start).await? {
                CycleOutcome::EndOfStream => {
                    info!("audio source ended; pipeline shutting down");
                    return Ok(());
                }
                CycleOutcome::Dispatched => {
                    debug!("intent dispatched; resuming wake-detection");
                }
                CycleOutcome::NoIntent => {
                    debug!("transcription matched no intent; resuming wake-detection");
                }
            }
        }
    }

    async fn detect_one_cycle(
        &mut self,
        cycle_start: Instant,
    ) -> Result<CycleOutcome, PipelineError> {
        // ── stage 1: wait for wake ──────────────────────────────
        let wake_started = Instant::now();
        let Some(wake) = self.wait_for_wake().await? else {
            return Ok(CycleOutcome::EndOfStream);
        };
        let wake_latency = wake_started.elapsed();

        // ── stage 2: capture utterance window ───────────────────
        let endpoint_started = Instant::now();
        let frames = self.capture_utterance().await?;
        let endpoint_latency = endpoint_started.elapsed();

        // ── stage 3: STT ────────────────────────────────────────
        let stt_started = Instant::now();
        let text = if frames.is_empty() {
            // Wake fired but stream ended before any post-wake
            // audio — degrade gracefully rather than handing the
            // STT an empty window (which would error).
            warn!(?wake, "wake fired but no post-wake audio; skipping cycle");
            self.update_metrics(wake_latency, endpoint_latency, None, None);
            return Ok(CycleOutcome::NoIntent);
        } else {
            self.stt.transcribe(&frames).await?
        };
        let stt_latency = stt_started.elapsed();

        // ── stage 4: NLU + dispatch ─────────────────────────────
        let nlu_started = Instant::now();
        let outcome = match self.parser.parse(&text).await {
            Ok(intent) => {
                self.sink.dispatch(&intent).await?;
                info!(
                    domain = %intent.domain,
                    verb = %intent.verb,
                    confidence = intent.confidence,
                    raw = %intent.raw,
                    "intent dispatched"
                );
                CycleOutcome::Dispatched
            }
            Err(IntentError::NoMatch(raw)) => {
                info!(text = %raw, "no intent matched; awaiting next wake");
                CycleOutcome::NoIntent
            }
            Err(e) => return Err(PipelineError::from(e)),
        };
        let nlu_latency = nlu_started.elapsed();

        self.update_metrics(
            wake_latency,
            endpoint_latency,
            Some(stt_latency),
            Some(nlu_latency),
        );

        let total = cycle_start.elapsed();
        if total > PipelineMetrics::TOTAL_BUDGET {
            warn!(
                total_ms = total.as_millis(),
                budget_ms = PipelineMetrics::TOTAL_BUDGET.as_millis(),
                "voice cycle exceeded latency budget"
            );
        }

        Ok(outcome)
    }

    async fn wait_for_wake(&mut self) -> Result<Option<Wake>, PipelineError> {
        loop {
            let Some(frame) = self.audio.next_frame().await? else {
                return Ok(None);
            };
            if let Some(wake) = self.wake.observe(&frame).await? {
                return Ok(Some(wake));
            }
        }
    }

    async fn capture_utterance(&mut self) -> Result<Vec<AudioFrame>, PipelineError> {
        let mut frames = Vec::with_capacity(MAX_UTTERANCE_FRAMES);
        let mut silence_run: u32 = 0;
        while frames.len() < MAX_UTTERANCE_FRAMES {
            let Some(frame) = self.audio.next_frame().await? else {
                break;
            };
            if frame.peak() < SILENCE_PEAK_THRESHOLD {
                silence_run += 1;
            } else {
                silence_run = 0;
            }
            frames.push(frame);
            if silence_run >= SILENCE_TAIL_FRAMES {
                // Trim the trailing silence so the STT doesn't see
                // a long padded tail. Keeps the inference window
                // tight for latency.
                let keep = frames.len().saturating_sub(silence_run as usize);
                frames.truncate(keep);
                break;
            }
        }
        Ok(frames)
    }

    fn update_metrics(
        &mut self,
        wake_latency: Duration,
        endpoint_latency: Duration,
        stt_latency: Option<Duration>,
        nlu_latency: Option<Duration>,
    ) {
        self.metrics.wake_latency = Some(wake_latency);
        self.metrics.endpoint_latency = Some(endpoint_latency);
        self.metrics.stt_latency = stt_latency;
        self.metrics.nlu_latency = nlu_latency;
    }
}

enum CycleOutcome {
    /// AudioSource returned `Ok(None)`. Pipeline shuts down cleanly.
    EndOfStream,
    /// Intent was parsed AND dispatched.
    Dispatched,
    /// Wake fired and STT ran, but no intent matched. Loop back
    /// to wake-detection.
    NoIntent,
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::audio::StubAudioSource;
    use crate::intent::{RuleIntentParser, StubIntentSink};
    use crate::stt::StubSpeechRecognizer;
    use crate::wake::StubWakeDetector;

    fn loud_frame(captured_at_ms: u64) -> AudioFrame {
        AudioFrame::new(vec![0.9; crate::SAMPLES_PER_FRAME], captured_at_ms)
    }

    /// Speech-amplitude frame — peak above SILENCE_PEAK_THRESHOLD
    /// but below the wake-trigger threshold. Models the actual
    /// post-wake utterance audio — without these between the wake
    /// frame and the silence tail, `capture_utterance` would
    /// trim the entire window to zero frames.
    fn speech_frame(captured_at_ms: u64) -> AudioFrame {
        AudioFrame::new(vec![0.2; crate::SAMPLES_PER_FRAME], captured_at_ms)
    }

    /// Build a synthetic post-wake utterance: `speech_frames` of
    /// speech then `silence_frames` of silence. The pipeline trims
    /// the silent tail and hands the speech window to STT.
    fn utterance_frames(start_ms: u64, speech_frames: u64, silence_frames: u64) -> Vec<AudioFrame> {
        (0..speech_frames)
            .map(|i| speech_frame(start_ms + 20 * i))
            .chain(
                (0..silence_frames)
                    .map(|i| AudioFrame::silence(start_ms + 20 * speech_frames + 20 * i)),
            )
            .collect()
    }

    #[tokio::test]
    async fn pipeline_dispatches_recognised_intent() {
        // Loud first frame triggers wake → speech then silence
        // triggers endpoint → stub STT returns "turn on the
        // kitchen light" → rule parser produces lights/on/kitchen
        // → stub sink collects.
        let mut frames = vec![loud_frame(0)];
        // 5 speech frames + 15 silence — endpoint fires after 12
        // silent in a row, leaves 5 speech frames in the window.
        frames.extend(utterance_frames(20, 5, 15));
        let audio = StubAudioSource::new(frames);
        let wake = StubWakeDetector::default();
        let stt = StubSpeechRecognizer::new("turn on the kitchen light");
        let parser = RuleIntentParser::new();
        let sink = StubIntentSink::new();
        let sink_clone = sink.clone();

        let mut p = Pipeline::new(audio, wake, stt, parser, Arc::new(sink));
        p.run().await.unwrap();

        let collected = sink_clone.snapshot().await;
        assert_eq!(collected.len(), 1, "expected one dispatched intent");
        assert_eq!(collected[0].domain, "lights");
        assert_eq!(collected[0].verb, "on");
        assert_eq!(collected[0].args, serde_json::json!({"target": "kitchen"}));

        // Metrics populated.
        assert!(p.metrics.wake_latency.is_some());
        assert!(p.metrics.endpoint_latency.is_some());
        assert!(p.metrics.stt_latency.is_some());
        assert!(p.metrics.nlu_latency.is_some());
    }

    #[tokio::test]
    async fn pipeline_no_intent_on_unrecognised_phrase() {
        let mut frames = vec![loud_frame(0)];
        frames.extend(utterance_frames(20, 5, 15));
        let audio = StubAudioSource::new(frames);
        let wake = StubWakeDetector::default();
        let stt = StubSpeechRecognizer::new("recipe for chocolate cake");
        let parser = RuleIntentParser::new();
        let sink = StubIntentSink::new();
        let sink_clone = sink.clone();

        let mut p = Pipeline::new(audio, wake, stt, parser, Arc::new(sink));
        p.run().await.unwrap();

        // Pipeline ran cleanly but nothing dispatched.
        assert_eq!(sink_clone.snapshot().await.len(), 0);
    }

    #[tokio::test]
    async fn pipeline_returns_cleanly_on_silence_only() {
        // Stream that's all silence — wake never fires, pipeline
        // exits when the source runs out.
        let audio = StubAudioSource::silence_for(50, 0);
        let wake = StubWakeDetector::default();
        let stt = StubSpeechRecognizer::new("never called");
        let parser = RuleIntentParser::new();
        let sink = StubIntentSink::new();

        let mut p = Pipeline::new(audio, wake, stt, parser, Arc::new(sink));
        p.run().await.unwrap();
    }

    #[tokio::test]
    async fn pipeline_wake_without_post_audio_skips_cleanly() {
        // Loud frame is the LAST frame — wake fires but capture_
        // utterance immediately hits end-of-stream.
        let audio = StubAudioSource::new(vec![loud_frame(0)]);
        let wake = StubWakeDetector::default();
        let stt = StubSpeechRecognizer::new("never called");
        let parser = RuleIntentParser::new();
        let sink = StubIntentSink::new();
        let sink_clone = sink.clone();

        let mut p = Pipeline::new(audio, wake, stt, parser, Arc::new(sink));
        p.run().await.unwrap();
        assert_eq!(sink_clone.snapshot().await.len(), 0);
    }
}
