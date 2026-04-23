//! Rule engine (M3 W2.2).
//!
//! Wires compiled [`Rule`]s to a live bus: subscribes to the union of
//! all their trigger subjects, matches per-message, evaluates each
//! rule's `when` expression against the decoded payload, and emits the
//! configured actions back on the bus.
//!
//! Flow per inbound message:
//!
//! ```text
//!   bus → on_message(subject, payload_bytes)
//!       → for each rule where triggers_on(subject):
//!            decoded = try_decode_payload(payload_bytes)
//!            if eval_bool(rule.when, &decoded) { fire(rule) }
//!       → fire(rule) dispatches each action, currently:
//!            Publish → bus.publish_proto(...)
//!            Log     → tracing::{info,warn,error,…}
//! ```
//!
//! Deliberately-deferred until W2.3:
//!
//! * Idempotency cache (short-lived `(rule_id, subject,
//!   payload_hash)` dedupe).
//! * DLQ on action failure.
//! * Audit entry per firing.
//! * Fancier action types (shell, http call, …).
//!
//! Keeping those out of this slice preserves the "pure dispatch" seam:
//! W2.2 ships subscribe + match + eval + emit, and each follow-up
//! drops into a named hook without restructuring.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use futures::StreamExt as _;
use iot_bus::Bus;
use tracing::{debug, info, warn};

use crate::expr::eval_bool;
use crate::rule::{RawAction, Rule};

/// Runtime handle to an instantiated engine.
#[derive(Debug, Clone)]
pub struct Engine {
    rules: Arc<Vec<Rule>>,
    bus: Bus,
}

impl Engine {
    /// Build from an already-compiled rule set + a live bus handle.
    #[must_use]
    pub fn new(rules: Vec<Rule>, bus: Bus) -> Self {
        Self {
            rules: Arc::new(rules),
            bus,
        }
    }

    /// Subscribe to the union of all rule triggers and run forever.
    ///
    /// # Errors
    /// Bus subscribe failure. Per-message errors are logged and the
    /// loop keeps running — one malformed payload or transient action
    /// failure doesn't take down the engine.
    pub async fn run(self) -> Result<()> {
        // Subscribe once to `device.>` (the only subject family rules
        // currently target). Fine-grained per-rule subscriptions are a
        // W2.3 optimisation; for M3's scale — dozens of rules — fanning
        // out in-process is cheaper than a subscription per trigger.
        let mut sub = self
            .bus
            .raw()
            .subscribe("device.>".to_string())
            .await
            .context("subscribe device.>")?;
        info!(rules = self.rules.len(), "automation engine started");

        while let Some(msg) = sub.next().await {
            self.on_message(msg.subject.as_str(), &msg.payload).await;
        }
        info!("engine subscription ended");
        Ok(())
    }

    /// Process one inbound message. Public so the W2.3 `iotctl rule
    /// test` command can exercise the same dispatch path without
    /// going through the bus.
    pub async fn on_message(&self, subject: &str, payload_bytes: &[u8]) {
        let payload = decode_payload(payload_bytes);

        for rule in self.rules.iter() {
            if !rule.triggers_on(subject) {
                continue;
            }
            match eval_bool(&rule.when, &payload) {
                Ok(true) => {
                    debug!(rule = %rule.id, subject, "rule matched");
                    if let Err(e) = self.fire(rule, subject, &payload).await {
                        warn!(rule = %rule.id, error = %format!("{e:#}"), "action dispatch failed");
                    }
                }
                Ok(false) => {
                    debug!(rule = %rule.id, subject, "condition false, skipping");
                }
                Err(e) => {
                    warn!(rule = %rule.id, error = %e, "expression evaluation failed");
                }
            }
        }
    }

    async fn fire(&self, rule: &Rule, subject: &str, payload: &serde_json::Value) -> Result<()> {
        for action in &rule.actions {
            match action {
                RawAction::Publish {
                    subject: out_subj,
                    iot_type,
                    payload: out_payload,
                } => {
                    let bytes = serde_json::to_vec(out_payload).context("encode action payload")?;
                    self.bus
                        .publish_proto(out_subj, iot_type, bytes, None)
                        .await
                        .with_context(|| format!("publish action {out_subj}"))?;
                    info!(
                        rule = %rule.id,
                        trigger = subject,
                        action_subject = %out_subj,
                        "action: publish"
                    );
                }
                RawAction::Log { level, message } => {
                    let rendered = render_template(message, subject, payload);
                    match level.as_str() {
                        "trace" => tracing::trace!(rule = %rule.id, "{rendered}"),
                        "debug" => tracing::debug!(rule = %rule.id, "{rendered}"),
                        "warn" => warn!(rule = %rule.id, "{rendered}"),
                        "error" => tracing::error!(rule = %rule.id, "{rendered}"),
                        _ => info!(rule = %rule.id, "{rendered}"),
                    }
                }
            }
        }
        Ok(())
    }
}

/// Decode bus payload bytes into a form the expression evaluator can
/// traverse.
///
/// Current order: JSON first (the most common shape for rule authors +
/// what `iotctl rule test` produces), then fall through to attempting
/// prost-decoded `iot.device.v1.EntityState`. Full protobuf decode is a
/// W2.3 job — for W2.2 we assume rules that care about a given subject
/// know what shape arrives there.
fn decode_payload(bytes: &[u8]) -> serde_json::Value {
    if bytes.is_empty() {
        return serde_json::Value::Null;
    }
    // Try JSON — if the first byte looks like it could be the start of
    // a JSON document, attempt to parse.
    let looks_jsonish = matches!(
        bytes.first(),
        Some(b'{' | b'[' | b'"' | b't' | b'f' | b'n' | b'0'..=b'9')
    );
    if looks_jsonish {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) {
            return v;
        }
    }
    // Fall-through: we don't have a protobuf decoder wired here yet;
    // return Null + trace so the rule's `when` simply evaluates against
    // a Null payload.
    debug!("payload bytes don't look like JSON; surfacing as null (proto decode is W2.3)");
    serde_json::Value::Null
}

/// Substitute `{{trigger}}` + `{{payload.path.to.x}}` placeholders in
/// a log-action message. Minimal renderer — good enough for dev logs,
/// not a full Handlebars.
fn render_template(tpl: &str, trigger: &str, payload: &serde_json::Value) -> String {
    let mut out = tpl.replace("{{trigger}}", trigger);
    // Simple `{{payload.foo.bar}}` substitution — scans for the
    // `{{payload.` prefix and pulls the path until the closing `}}`.
    while let Some(start) = out.find("{{payload.") {
        let Some(end_rel) = out[start..].find("}}") else {
            break;
        };
        let end = start + end_rel;
        let path = &out[start + "{{payload.".len()..end];
        let segs: Vec<&str> = path.split('.').collect();
        let mut cur = payload;
        let mut found = true;
        for s in segs {
            if let Some(v) = cur.get(s) {
                cur = v;
            } else {
                found = false;
                break;
            }
        }
        let replacement = if found {
            cur.to_string()
        } else {
            String::from("null")
        };
        out.replace_range(start..end + 2, &replacement);
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn decode_json_payload() {
        let json = br#"{"value":25}"#;
        let v = decode_payload(json);
        assert_eq!(v["value"], 25);
    }

    #[test]
    fn decode_empty_is_null() {
        assert_eq!(decode_payload(b""), serde_json::Value::Null);
    }

    #[test]
    fn decode_non_json_bytes_is_null() {
        // Random bytes that don't look like the start of a JSON value
        // surface as Null rather than crashing the engine.
        let bytes = &[0xffu8, 0x00, 0x42];
        assert_eq!(decode_payload(bytes), serde_json::Value::Null);
    }

    #[test]
    fn template_substitutes_trigger_and_payload() {
        let payload = serde_json::json!({"value": 21.5, "sensor": {"battery": 87}});
        let s = render_template(
            "triggered by {{trigger}} with value={{payload.value}} battery={{payload.sensor.battery}}",
            "device.z.kt.t.state",
            &payload,
        );
        assert_eq!(
            s,
            "triggered by device.z.kt.t.state with value=21.5 battery=87"
        );
    }

    #[test]
    fn template_missing_path_renders_null() {
        let payload = serde_json::json!({"value": 1});
        let s = render_template("{{payload.missing.deep}}", "t", &payload);
        assert_eq!(s, "null");
    }

    // The full `Engine::on_message` path requires a live bus to
    // exercise the publish step — we cover it with the testcontainers
    // integration test in W2.3 (`iotctl rule test`). The pieces above
    // (decode + template) are what's substantive-but-unit-testable.
}
