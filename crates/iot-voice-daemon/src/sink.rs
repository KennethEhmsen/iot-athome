//! NATS-backed `IntentSink` (M5b W3).
//!
//! Wraps an [`iot_bus::Bus`] handle and publishes each parsed
//! [`Intent`] on the canonical bus subject:
//!
//! ```text
//! command.intent.<domain>.<verb>
//! ```
//!
//! …per ADR-0015's subject-taxonomy. The payload is the same JSON
//! shape `Intent` derives for `serde_json` — `{ domain, verb, args,
//! raw, confidence }`. Rules consume via the existing `payload.<x>`
//! CEL field-access pattern.
//!
//! ## Why JSON, not protobuf
//!
//! Every other on-bus payload in this project is protobuf-encoded
//! (the M3-era convention). Voice intents deviate because:
//!
//! 1. The intent shape is operator-driven and changes over the
//!    M5b → M5c surface — locking it into a `.proto` schema
//!    would lock M5b W3+'s closed-domain grammar into a wire
//!    contract.
//! 2. The rule engine reads JSON for `payload.target` etc anyway —
//!    no decode-shim cost.
//! 3. The `command.intent.>` subject space is operator-input;
//!    machine-level callers should prefer `device.>` /`cmd.>`
//!    where the protobuf typing is load-bearing.
//!
//! Headers still set `iot.type = iot.intent.v1.Intent` so a future
//! protobuf migration is a single header-flip away. When that
//! lands, `iot-proto` grows an `Intent` message and this sink
//! switches to `Bus::publish_proto`.

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use iot_bus::Bus;
use iot_voice::{Intent, IntentError, IntentSink};
use tracing::info;

/// Subject prefix per ADR-0015 §"Bus integration".
const SUBJECT_PREFIX: &str = "command.intent";

/// Iot type-header value for the JSON payload. Future protobuf
/// migration replaces the JSON serializer + bumps to a `.proto`
/// schema; the header name stays.
const IOT_TYPE: &str = "iot.intent.v1.Intent";

/// Iot-AtHome's content-type header for JSON-encoded payloads.
/// Mirrors the M3 audit-event path which also predates a protobuf
/// schema for its message body.
const CONTENT_TYPE_JSON: &str = "application/json";

/// `IntentSink` impl that publishes to NATS via [`iot_bus::Bus`].
#[derive(Clone)]
pub struct NatsIntentSink {
    bus: Bus,
}

impl std::fmt::Debug for NatsIntentSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NatsIntentSink").finish_non_exhaustive()
    }
}

impl NatsIntentSink {
    /// Wrap an existing `Bus`.
    #[must_use]
    pub fn new(bus: Bus) -> Self {
        Self { bus }
    }

    /// Build the NATS subject for an `Intent` per ADR-0015.
    ///
    /// Domain + verb segments are token-validated to NATS subject
    /// rules ASCII-alphanumeric or `-` / `_`; anything else is
    /// substituted with `_` to keep the broker happy. Closed-domain
    /// grammar already produces clean tokens, but the LLM-fallback
    /// path (M5b W4+) might not.
    #[must_use]
    pub fn subject_for(intent: &Intent) -> String {
        format!(
            "{SUBJECT_PREFIX}.{domain}.{verb}",
            domain = sanitise_segment(&intent.domain),
            verb = sanitise_segment(&intent.verb),
        )
    }
}

#[async_trait]
impl IntentSink for NatsIntentSink {
    async fn dispatch(&self, intent: &Intent) -> Result<(), IntentError> {
        let subject = Self::subject_for(intent);
        let payload = serde_json::to_vec(intent)
            .map_err(|e| IntentError::Sink(format!("intent JSON encode: {e}")))?;

        // Use the raw async-nats client because intent payloads are
        // JSON, not protobuf. `Bus::publish_proto` would force a
        // protobuf encode + iot-type that doesn't match the wire
        // contract above. Headers we set explicitly:
        //   iot.type      = "iot.intent.v1.Intent"
        //   content-type  = "application/json"
        let mut headers = async_nats::HeaderMap::new();
        headers.insert("iot.type", IOT_TYPE);
        headers.insert("content-type", CONTENT_TYPE_JSON);
        self.bus
            .raw()
            .publish_with_headers(subject.clone(), headers, payload.into())
            .await
            .map_err(|e| IntentError::Sink(format!("publish: {e}")))?;

        info!(
            target: "iot_voice_daemon::sink",
            subject = %subject,
            domain = %intent.domain,
            verb = %intent.verb,
            confidence = intent.confidence,
            "intent published"
        );
        Ok(())
    }
}

/// Dispatch one intent end-to-end, including a `flush` so the
/// `iot-voice send` short-lived process actually delivers before
/// it exits. Without flush the publish queues into async-nats's
/// in-memory buffer and the process may exit before bytes hit the
/// wire. The `listen` mode doesn't need this — there the daemon
/// stays up + drains naturally.
pub async fn dispatch_and_flush(bus: &Bus, intent: &Intent) -> Result<()> {
    NatsIntentSink::new(bus.clone())
        .dispatch(intent)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    bus.raw().flush().await.context("flush NATS")?;
    Ok(())
}

/// Replace any character that NATS won't accept in a subject token
/// with `_`. The valid set is alphanumeric + `-`, `_`, `.` — but
/// `.` itself is the segment separator, so we substitute it too.
fn sanitise_segment(seg: &str) -> String {
    if seg.is_empty() {
        return "_".to_owned();
    }
    seg.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn sample_intent() -> Intent {
        Intent {
            domain: "lights".into(),
            verb: "on".into(),
            args: serde_json::json!({"target": "kitchen"}),
            raw: "turn on the kitchen light".into(),
            confidence: 0.95,
        }
    }

    #[test]
    fn subject_matches_adr_0015_taxonomy() {
        let s = NatsIntentSink::subject_for(&sample_intent());
        assert_eq!(s, "command.intent.lights.on");
    }

    #[test]
    fn subject_sanitises_unexpected_chars() {
        // Domain/verb that LLM-fallback NLU might emit (M5b W4+).
        let i = Intent {
            domain: "play.music".into(), // dot is forbidden
            verb: "set volume".into(),   // space is forbidden
            args: serde_json::Value::Null,
            raw: "x".into(),
            confidence: 0.5,
        };
        let s = NatsIntentSink::subject_for(&i);
        assert_eq!(s, "command.intent.play_music.set_volume");
    }

    #[test]
    fn subject_substitutes_for_empty_segment() {
        let i = Intent {
            domain: String::new(),
            verb: String::new(),
            args: serde_json::Value::Null,
            raw: "x".into(),
            confidence: 0.0,
        };
        let s = NatsIntentSink::subject_for(&i);
        assert_eq!(s, "command.intent._._");
    }

    #[test]
    fn intent_payload_serialises_as_expected_json() {
        let i = sample_intent();
        let bytes = serde_json::to_vec(&i).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["domain"], "lights");
        assert_eq!(v["verb"], "on");
        assert_eq!(v["args"]["target"], "kitchen");
        assert_eq!(v["raw"], "turn on the kitchen light");
        // f32 → f64 in JSON; tolerate small float drift on round-trip.
        let conf = v["confidence"].as_f64().unwrap();
        assert!((conf - 0.95).abs() < 0.001);
    }
}
