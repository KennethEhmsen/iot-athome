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
//! Hooks now covered (M3 W2 engine-polish slice):
//!
//! * Idempotency cache. A rolling `(rule_id, subject, payload_sha256)`
//!   set with a 5-second TTL gates duplicate firings — chatty sensors
//!   that publish the same payload multiple times per second don't
//!   re-fire the same action. Entries prune on every check.
//! * Audit entry per firing. When a rule fires, the engine appends a
//!   `"automation.rule_fired"` entry carrying rule id, trigger
//!   subject, action count, and the hash-truncated idempotency key.
//!   The M1/M2 hash-chain plus the W1.4 JCS canonicalisation keeps
//!   this tamper-detectable.
//! * DLQ on action failure. If `fire()` returns `Err`, the engine
//!   publishes a JSON failure record on `sys.automation.dlq` so
//!   operators can watch that subject to catch failures without
//!   scraping logs.
//!
//! Still deferred (no structural blocker — drop-in hooks):
//!
//! * Fancier action types (shell, http call, …).
//! * Per-rule fine-grained bus subscriptions (M3's single
//!   `device.>` subscription fans out in-process; cheap for
//!   dozens of rules).
//! * Proto→JSON decode of `iot.device.v1.EntityState` payloads so
//!   existing z2m state messages are rule-visible. Hook in
//!   `decode_payload`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use futures::StreamExt as _;
use iot_audit::AuditLog;
use iot_bus::Bus;
use iot_proto_core::iot::device::v1::EntityState;
use prost::Message as _;
use tracing::{debug, info, warn};

use crate::expr::eval_bool_with_timeout;
use crate::rule::{RawAction, Rule};

/// How long the idempotency cache remembers a `(rule, subject,
/// payload_hash)` key. 5 s matches the zigbee2mqtt / typical sensor
/// chat rate — two identical payloads landing within this window
/// count as one firing.
const IDEMPOTENCY_TTL: Duration = Duration::from_secs(5);

/// NATS subject the engine publishes failure records to. Operators
/// can `nats sub sys.automation.dlq` to catch every action that
/// refused to emit without scraping logs.
const DLQ_SUBJECT: &str = "sys.automation.dlq";

/// Runtime handle to an instantiated engine.
#[derive(Debug, Clone)]
pub struct Engine {
    rules: Arc<Vec<Rule>>,
    bus: Bus,
    audit: Option<Arc<AuditLog>>,
    idempotency: Arc<Mutex<HashMap<String, Instant>>>,
}

impl Engine {
    /// Build from an already-compiled rule set + a live bus handle.
    #[must_use]
    pub fn new(rules: Vec<Rule>, bus: Bus) -> Self {
        Self::with_audit(rules, bus, None)
    }

    /// Build with an optional audit log. In-process tests pass
    /// `None`; real deployments wire the shared `iot-audit` handle
    /// so every firing shows up in the hash chain.
    #[must_use]
    pub fn with_audit(rules: Vec<Rule>, bus: Bus, audit: Option<Arc<AuditLog>>) -> Self {
        Self {
            rules: Arc::new(rules),
            bus,
            audit,
            idempotency: Arc::new(Mutex::new(HashMap::new())),
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
            // Scope the handler under the inbound traceparent so the
            // audit entry + any DLQ publish carries the upstream trace
            // id. Missing / malformed → fresh root.
            let ctx = iot_bus::extract_trace_context(&msg).map_or_else(
                iot_observability::traceparent::TraceContext::new_root,
                |p| p.child_of(),
            );
            let subject = msg.subject.to_string();
            iot_observability::traceparent::with_context(ctx, async {
                self.on_message(&subject, &msg.payload).await;
            })
            .await;
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
            match eval_bool_with_timeout(&rule.when, &payload).await {
                Ok(true) => {
                    // Short-circuit on idempotency: same rule, same
                    // subject, same payload bytes within the TTL count
                    // as one firing.
                    let key = idempotency_key(&rule.id, subject, payload_bytes);
                    if !self.claim_idempotency(&key) {
                        debug!(rule = %rule.id, subject, "duplicate within idempotency window, skipping");
                        continue;
                    }
                    debug!(rule = %rule.id, subject, "rule matched");
                    match self.fire(rule, subject, &payload).await {
                        Ok(()) => self.record_firing(rule, subject, &key).await,
                        Err(e) => {
                            let reason = format!("{e:#}");
                            warn!(rule = %rule.id, error = %reason, "action dispatch failed");
                            self.dead_letter(rule, subject, &reason).await;
                        }
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

    /// Try to stake a claim on `key`. Returns `true` if the caller
    /// should proceed to fire; `false` if another firing within the
    /// TTL already beat us to it. Also prunes expired entries on the
    /// way through.
    fn claim_idempotency(&self, key: &str) -> bool {
        #[allow(clippy::unwrap_used)] // Mutex is only poisoned on panic, which we'd surface.
        let mut guard = self.idempotency.lock().unwrap();
        let now = Instant::now();
        // Prune first — keeps the map bounded in steady-state.
        guard.retain(|_, t| now.duration_since(*t) < IDEMPOTENCY_TTL);
        if guard.contains_key(key) {
            return false;
        }
        guard.insert(key.to_owned(), now);
        true
    }

    async fn record_firing(&self, rule: &Rule, trigger: &str, idempotency_key: &str) {
        let Some(audit) = self.audit.clone() else {
            return;
        };
        let payload = serde_json::json!({
            "rule_id": rule.id,
            "trigger_subject": trigger,
            "action_count": rule.actions.len(),
            // The idempotency key is SHA-256 hex; keep it grep-friendly
            // by storing the truncated form alongside the rule id.
            "idempotency": idempotency_key.get(..16).unwrap_or(idempotency_key),
        });
        if let Err(e) = audit.append("automation.rule_fired", payload).await {
            warn!(rule = %rule.id, error = %e, "audit append failed");
        }
    }

    async fn dead_letter(&self, rule: &Rule, trigger: &str, reason: &str) {
        let record = serde_json::json!({
            "rule_id": rule.id,
            "trigger_subject": trigger,
            "reason": reason,
            "at_unix_ms": now_unix_ms(),
        });
        // Best-effort — if the bus is so broken we can't publish the
        // DLQ, we've already logged at warn. Don't double-log.
        match serde_json::to_vec(&record) {
            Ok(bytes) => {
                let _ = self
                    .bus
                    .publish_proto(DLQ_SUBJECT, "application/json", bytes, None)
                    .await;
            }
            Err(e) => warn!(rule = %rule.id, error = %e, "serialise DLQ record failed"),
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

/// Stable idempotency key for a `(rule, subject, payload)` triple.
/// SHA-256 of the concatenation; hex-encoded. The rule id + subject
/// prefix the hash input so two rules with the same trigger + payload
/// don't collide.
fn idempotency_key(rule_id: &str, subject: &str, payload: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    let mut h = Sha256::new();
    h.update(rule_id.as_bytes());
    h.update(b"|");
    h.update(subject.as_bytes());
    h.update(b"|");
    h.update(payload);
    let digest = h.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        let _ = std::fmt::write(&mut out, format_args!("{b:02x}"));
    }
    out
}

fn now_unix_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(dur.as_millis()).unwrap_or(u64::MAX)
}

/// Decode bus payload bytes into a form the expression evaluator can
/// traverse.
///
/// Order tried:
///   1. JSON — the shape `iotctl rule test` produces + what most
///      greenfield rule authors write.
///   2. Prost-decoded `iot.device.v1.EntityState` — the shape
///      z2m-and-friends actually publish on `device.*.*.*.state`.
///      The inner `google.protobuf.Value` unwraps to its JSON
///      equivalent, surfaced under `payload.value`, with the other
///      EntityState fields (`device_id`, `entity_id`, `at`,
///      `schema_version`) alongside.
///
/// If neither parser accepts the bytes, the payload surfaces as
/// Null. Rules that care about shape are expected to know what
/// subjects they target.
fn decode_payload(bytes: &[u8]) -> serde_json::Value {
    if bytes.is_empty() {
        return serde_json::Value::Null;
    }

    // Try JSON first — cheap shape check on the leading byte avoids
    // attempting a full parse on obvious protobuf bytes.
    let looks_jsonish = matches!(
        bytes.first(),
        Some(b'{' | b'[' | b'"' | b't' | b'f' | b'n' | b'0'..=b'9')
    );
    if looks_jsonish {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) {
            return v;
        }
    }

    // Fallback: try `iot.device.v1.EntityState`. Wire format is
    // whatever prost produces; if the bytes aren't EntityState we
    // surface Null and move on.
    if let Ok(entity) = EntityState::decode(bytes) {
        return entity_state_to_json(&entity);
    }

    debug!("payload decoded as neither JSON nor EntityState; surfacing as null");
    serde_json::Value::Null
}

/// Flatten an `EntityState` into a rule-friendly JSON object.
/// `entity.value` (itself a `google.protobuf.Value` union) unwraps to
/// the natural JSON kind so rule authors can write
/// `payload.value > 25` instead of `payload.value.numberValue > 25`.
fn entity_state_to_json(
    entity: &iot_proto_core::iot::device::v1::EntityState,
) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    if let Some(id) = &entity.device_id {
        obj.insert(
            "device_id".into(),
            serde_json::Value::String(id.value.clone()),
        );
    }
    if let Some(id) = &entity.entity_id {
        obj.insert(
            "entity_id".into(),
            serde_json::Value::String(id.value.clone()),
        );
    }
    if let Some(v) = &entity.value {
        obj.insert("value".into(), prost_value_to_json(v));
    }
    if let Some(ts) = &entity.at {
        // Rule authors rarely string-compare timestamps; stringify
        // as `<seconds>.<nanos>` so the field is present and stable.
        obj.insert(
            "at".into(),
            serde_json::Value::String(format!("{}.{:09}", ts.seconds, ts.nanos)),
        );
    }
    obj.insert(
        "schema_version".into(),
        serde_json::Value::Number(entity.schema_version.into()),
    );
    serde_json::Value::Object(obj)
}

/// `google.protobuf.Value` → `serde_json::Value`. Inverse of the
/// encoding the z2m adapter does for its bus publishes, so the
/// round-trip is lossless.
fn prost_value_to_json(v: &prost_types::Value) -> serde_json::Value {
    use prost_types::value::Kind;
    let Some(kind) = &v.kind else {
        return serde_json::Value::Null;
    };
    match kind {
        Kind::NullValue(_) => serde_json::Value::Null,
        Kind::BoolValue(b) => serde_json::Value::Bool(*b),
        Kind::NumberValue(n) => serde_json::Number::from_f64(*n)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Kind::StringValue(s) => serde_json::Value::String(s.clone()),
        Kind::ListValue(lst) => {
            serde_json::Value::Array(lst.values.iter().map(prost_value_to_json).collect())
        }
        Kind::StructValue(st) => serde_json::Value::Object(
            st.fields
                .iter()
                .map(|(k, v)| (k.clone(), prost_value_to_json(v)))
                .collect(),
        ),
    }
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

    // --------------------------------------------------- idempotency helpers

    #[test]
    fn idempotency_key_stable_across_identical_inputs() {
        let a = idempotency_key("rule-x", "device.a.b.c.state", b"{}");
        let b = idempotency_key("rule-x", "device.a.b.c.state", b"{}");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64, "SHA-256 hex = 64 chars");
    }

    // --------------------------------------------------- proto decode

    #[test]
    fn decode_entity_state_exposes_value_and_ids() {
        use iot_proto_core::iot::common::v1::Ulid;
        let msg = EntityState {
            device_id: Some(Ulid {
                value: "01HXXDEVICE".into(),
            }),
            entity_id: Some(Ulid {
                value: "01HXXENTITY".into(),
            }),
            value: Some(prost_types::Value {
                kind: Some(prost_types::value::Kind::NumberValue(21.5)),
            }),
            at: Some(prost_types::Timestamp {
                seconds: 1_700_000_000,
                nanos: 123_000_000,
            }),
            schema_version: 1,
        };
        let bytes = msg.encode_to_vec();
        let decoded = decode_payload(&bytes);
        assert_eq!(decoded["device_id"], "01HXXDEVICE");
        assert_eq!(decoded["entity_id"], "01HXXENTITY");
        assert_eq!(decoded["value"], 21.5);
        assert_eq!(decoded["schema_version"], 1);
        // `at` is present as a string with nanos zero-padded.
        assert!(decoded["at"].is_string());
    }

    #[test]
    fn decode_entity_state_with_struct_value() {
        // A payload whose value itself is a struct — maps to a nested
        // JSON object under payload.value.
        let inner = prost_types::Struct {
            fields: [
                (
                    "r".to_string(),
                    prost_types::Value {
                        kind: Some(prost_types::value::Kind::NumberValue(255.0)),
                    },
                ),
                (
                    "online".to_string(),
                    prost_types::Value {
                        kind: Some(prost_types::value::Kind::BoolValue(true)),
                    },
                ),
            ]
            .into_iter()
            .collect(),
        };
        let msg = EntityState {
            device_id: None,
            entity_id: None,
            value: Some(prost_types::Value {
                kind: Some(prost_types::value::Kind::StructValue(inner)),
            }),
            at: None,
            schema_version: 1,
        };
        let bytes = msg.encode_to_vec();
        let decoded = decode_payload(&bytes);
        // `google.protobuf.Value`'s NumberValue always unwraps to f64,
        // so struct fields that look integer-y stay float on the
        // JSON side.
        assert_eq!(decoded["value"]["r"].as_f64(), Some(255.0));
        assert_eq!(decoded["value"]["online"], true);
    }

    #[test]
    fn idempotency_key_differs_per_field() {
        let base = idempotency_key("rule-x", "device.a.b.c.state", b"{}");
        // Rule id differs.
        let r = idempotency_key("rule-y", "device.a.b.c.state", b"{}");
        assert_ne!(base, r);
        // Subject differs.
        let s = idempotency_key("rule-x", "device.a.b.d.state", b"{}");
        assert_ne!(base, s);
        // Payload differs.
        let p = idempotency_key("rule-x", "device.a.b.c.state", br#"{"x":1}"#);
        assert_ne!(base, p);
    }
}
