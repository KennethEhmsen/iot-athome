//! Closed-domain intent parsing + dispatch.
//!
//! The pipeline maps free text → `Intent { domain, verb, args, raw,
//! confidence }`. Two parsers ship:
//!
//! * [`RuleIntentParser`] — phrase-based grammar covering the M5b
//!   starter set (lights / scenes / sensors / power / climate). No
//!   ML; deterministic. Confidence is a coarse "matched / didn't"
//!   signal.
//! * [`StubIntentParser`] (in tests) — pre-baked Intent for any
//!   text. Used by end-to-end tests to skip the parser stage when
//!   the test cares about pipeline shape, not NLU correctness.
//!
//! LLM-fallback parsing (free-form requests via local llama.cpp) is
//! M5b W3+ scope.
//!
//! ## Bus integration
//!
//! Intents publish on:
//!
//! ```text
//! command.intent.<domain>.<verb>
//! ```
//!
//! …mirroring the M3 rule-engine subject taxonomy. The library
//! itself doesn't depend on `iot-bus` — `IntentSink` is a trait,
//! and the daemon binary (separate commit) wires a NATS-backed
//! impl. Keeps this crate's tests pure-Rust + fast.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;

/// One parsed intent.
///
/// Doesn't derive `Eq` because `confidence: f32` isn't totally
/// ordered (NaN-bites). `PartialEq` is enough for the tests that
/// need it; downstream consumers ordering intents by confidence
/// can spell `total_cmp` explicitly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Intent {
    /// Top-level domain. One of: `lights`, `scenes`, `sensors`,
    /// `power`, `climate`, `media`, `system`. Open set — operators
    /// can add domains by extending the rule grammar; the rule
    /// engine triggers on `command.intent.<domain>.>`.
    pub domain: String,
    /// Domain-specific verb. `lights.on`, `scenes.activate`,
    /// `sensors.report`, etc.
    pub verb: String,
    /// Verb arguments — typically `{"target": "<name>"}` or richer
    /// `{"target": "kitchen", "brightness": 30}`. JSON for shape
    /// flexibility; rules consume via CEL `payload.target`.
    pub args: serde_json::Value,
    /// The raw transcribed text the parser saw. Audit-trail only;
    /// not consumed by rules.
    pub raw: String,
    /// Parser confidence in `[0.0, 1.0]`. The phrase-based parser
    /// emits 0.95 on exact phrase match, 0.0 on no match (which
    /// surfaces as a `NoMatch` error rather than an Intent).
    pub confidence: f32,
}

#[derive(Debug, Error)]
pub enum IntentError {
    /// No grammar pattern matched the input. The pipeline logs this
    /// at `info!` and (in v1) doesn't dispatch — future LLM-fallback
    /// path (M5b W3+) gets a second crack here before the error
    /// surfaces.
    #[error("no intent matched: {0:?}")]
    NoMatch(String),
    /// Sink-side dispatch failed. Pipelines treat this as
    /// non-fatal — log + continue listening.
    #[error("intent sink: {0}")]
    Sink(String),
}

#[async_trait]
pub trait IntentParser: Send {
    /// Parse free text into an [`Intent`]. Returns
    /// [`IntentError::NoMatch`] when nothing recognises the input —
    /// the pipeline distinguishes "didn't hear a command" from
    /// "heard one and dispatch failed".
    async fn parse(&self, text: &str) -> Result<Intent, IntentError>;
}

/// Where parsed intents go. Implementations: the daemon's
/// NATS-publish (separate commit), `LogIntentSink` (writes to
/// `tracing::info!`), or `StubIntentSink` (in-memory queue for
/// tests).
#[async_trait]
pub trait IntentSink: Send + Sync {
    async fn dispatch(&self, intent: &Intent) -> Result<(), IntentError>;
}

// ---------------------------------------------------------- RuleIntentParser
//
// A simple, ASCII-only, English-only phrase matcher. Patterns are
// hand-coded for the M5b starter set; expanding the grammar is
// adding entries to the match table. When real LLM-fallback NLU
// lands, this stays as the fast path for the common phrases that
// don't justify a 4 GB Llama load.

/// Phrase-based intent parser.
#[derive(Debug, Default)]
pub struct RuleIntentParser;

impl RuleIntentParser {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl IntentParser for RuleIntentParser {
    async fn parse(&self, text: &str) -> Result<Intent, IntentError> {
        let normalised = normalise(text);
        match_phrase(&normalised, text).ok_or_else(|| IntentError::NoMatch(text.to_owned()))
    }
}

/// Lowercase + strip leading politeness ("please", "could you", etc.)
/// + collapse whitespace.
///
/// Closed-domain grammar match is deliberately strict — operators
/// who need "kitchen lichts on" tolerance get the LLM-fallback path,
/// not creative regex.
fn normalise(text: &str) -> String {
    let lc = text.to_lowercase();
    let lc = lc
        .trim()
        .trim_start_matches("please ")
        .trim_start_matches("could you ")
        .trim_start_matches("can you ")
        .trim_start_matches("would you ");
    lc.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Match against the closed-domain pattern table. The patterns are
/// matched in priority order — more specific patterns first so
/// "turn on the kitchen light" doesn't accidentally hit a generic
/// "turn on" rule.
fn match_phrase(normalised: &str, raw: &str) -> Option<Intent> {
    // ──── lights ────────────────────────────────────────────────
    if let Some(target) = strip_prefix_suffix(normalised, "turn on the ", " light")
        .or_else(|| strip_prefix_suffix(normalised, "turn on the ", " lights"))
        .or_else(|| strip_prefix_suffix(normalised, "turn on ", " light"))
        .or_else(|| strip_prefix_suffix(normalised, "turn on ", " lights"))
    {
        return Some(intent("lights", "on", target, raw));
    }
    if let Some(target) = strip_prefix_suffix(normalised, "turn off the ", " light")
        .or_else(|| strip_prefix_suffix(normalised, "turn off the ", " lights"))
        .or_else(|| strip_prefix_suffix(normalised, "turn off ", " light"))
        .or_else(|| strip_prefix_suffix(normalised, "turn off ", " lights"))
    {
        return Some(intent("lights", "off", target, raw));
    }

    // ──── scenes ────────────────────────────────────────────────
    if let Some(target) = strip_prefix_suffix(normalised, "activate the ", " scene")
        .or_else(|| strip_prefix_suffix(normalised, "activate ", " scene"))
        .or_else(|| strip_prefix_suffix(normalised, "set the scene to ", ""))
    {
        return Some(intent("scenes", "activate", target, raw));
    }

    // ──── sensors (report) ──────────────────────────────────────
    if let Some(target) = strip_prefix_suffix(normalised, "what is the ", "")
        .or_else(|| strip_prefix_suffix(normalised, "what's the ", ""))
        .or_else(|| strip_prefix_suffix(normalised, "tell me the ", ""))
    {
        return Some(intent("sensors", "report", target, raw));
    }

    // ──── system ────────────────────────────────────────────────
    if matches!(normalised, "stop" | "cancel" | "never mind") {
        return Some(Intent {
            domain: "system".into(),
            verb: "cancel".into(),
            args: serde_json::Value::Null,
            raw: raw.to_owned(),
            confidence: 1.0,
        });
    }

    None
}

/// Build an intent with `args = {"target": "<target>"}`.
fn intent(domain: &str, verb: &str, target: &str, raw: &str) -> Intent {
    Intent {
        domain: domain.to_owned(),
        verb: verb.to_owned(),
        args: serde_json::json!({ "target": target }),
        raw: raw.to_owned(),
        confidence: 0.95,
    }
}

/// `strip_prefix_suffix("turn on the kitchen light", "turn on the ", " light")
/// == Some("kitchen")`. Returns the captured middle when both
/// prefix + suffix match, else `None`.
///
/// Empty `suffix` means "match-prefix-and-take-rest".
fn strip_prefix_suffix<'a>(s: &'a str, prefix: &str, suffix: &str) -> Option<&'a str> {
    let inner = s.strip_prefix(prefix)?;
    if suffix.is_empty() {
        Some(inner.trim())
    } else {
        let inner = inner.strip_suffix(suffix)?;
        Some(inner.trim())
    }
}

// ---------------------------------------------------------- LogIntentSink

/// `IntentSink` that writes each intent to `tracing::info!` and
/// nowhere else. Useful for the daemon's first boot before the
/// NATS-publish sink is configured.
#[derive(Debug, Default)]
pub struct LogIntentSink;

#[async_trait]
impl IntentSink for LogIntentSink {
    async fn dispatch(&self, intent: &Intent) -> Result<(), IntentError> {
        tracing::info!(
            target: "iot_voice::intent",
            domain = %intent.domain,
            verb = %intent.verb,
            confidence = intent.confidence,
            raw = %intent.raw,
            args = %intent.args,
            "intent (log-sink only; bus publish not wired)"
        );
        Ok(())
    }
}

// ---------------------------------------------------------- StubIntentSink

/// `IntentSink` that collects intents into a `Vec` for inspection
/// by tests. Cheap to clone; the inner buffer is `Arc<Mutex<...>>`.
#[derive(Debug, Default, Clone)]
pub struct StubIntentSink {
    pub collected: std::sync::Arc<Mutex<Vec<Intent>>>,
}

impl StubIntentSink {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot what's been collected so far. Async because the
    /// inner mutex is `tokio::sync::Mutex` so we don't block the
    /// runtime.
    pub async fn snapshot(&self) -> Vec<Intent> {
        self.collected.lock().await.clone()
    }
}

#[async_trait]
impl IntentSink for StubIntentSink {
    async fn dispatch(&self, intent: &Intent) -> Result<(), IntentError> {
        self.collected.lock().await.push(intent.clone());
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rule_parser_recognises_lights_on() {
        let p = RuleIntentParser::new();
        let i = p.parse("turn on the kitchen light").await.unwrap();
        assert_eq!(i.domain, "lights");
        assert_eq!(i.verb, "on");
        assert_eq!(i.args, serde_json::json!({"target": "kitchen"}));
    }

    #[tokio::test]
    async fn rule_parser_recognises_lights_off() {
        let p = RuleIntentParser::new();
        let i = p.parse("turn off the bedroom lights").await.unwrap();
        assert_eq!(i.domain, "lights");
        assert_eq!(i.verb, "off");
        assert_eq!(i.args, serde_json::json!({"target": "bedroom"}));
    }

    #[tokio::test]
    async fn rule_parser_recognises_scene_activation() {
        let p = RuleIntentParser::new();
        let i = p.parse("activate the movie scene").await.unwrap();
        assert_eq!(i.domain, "scenes");
        assert_eq!(i.verb, "activate");
        assert_eq!(i.args, serde_json::json!({"target": "movie"}));
    }

    #[tokio::test]
    async fn rule_parser_strips_polite_prefix() {
        let p = RuleIntentParser::new();
        let i = p.parse("please turn on the hallway light").await.unwrap();
        assert_eq!(i.domain, "lights");
        assert_eq!(i.args, serde_json::json!({"target": "hallway"}));
    }

    #[tokio::test]
    async fn rule_parser_recognises_cancel() {
        let p = RuleIntentParser::new();
        let i = p.parse("stop").await.unwrap();
        assert_eq!(i.domain, "system");
        assert_eq!(i.verb, "cancel");
    }

    #[tokio::test]
    async fn rule_parser_recognises_sensor_query() {
        let p = RuleIntentParser::new();
        let i = p.parse("what is the kitchen temperature").await.unwrap();
        assert_eq!(i.domain, "sensors");
        assert_eq!(i.verb, "report");
        assert_eq!(i.args, serde_json::json!({"target": "kitchen temperature"}));
    }

    #[tokio::test]
    async fn rule_parser_returns_no_match_on_garbage() {
        let p = RuleIntentParser::new();
        let err = p.parse("blah blah blah").await.unwrap_err();
        assert!(matches!(err, IntentError::NoMatch(_)), "{err:?}");
    }

    #[tokio::test]
    async fn stub_sink_collects_intents() {
        let s = StubIntentSink::new();
        let i = Intent {
            domain: "lights".into(),
            verb: "on".into(),
            args: serde_json::Value::Null,
            raw: "x".into(),
            confidence: 1.0,
        };
        s.dispatch(&i).await.unwrap();
        s.dispatch(&i).await.unwrap();
        assert_eq!(s.snapshot().await.len(), 2);
    }

    #[test]
    fn intent_serde_round_trips() {
        let i = Intent {
            domain: "lights".into(),
            verb: "on".into(),
            args: serde_json::json!({"target": "kitchen"}),
            raw: "turn on the kitchen light".into(),
            confidence: 0.95,
        };
        let s = serde_json::to_string(&i).unwrap();
        let back: Intent = serde_json::from_str(&s).unwrap();
        assert_eq!(back, i);
    }
}
