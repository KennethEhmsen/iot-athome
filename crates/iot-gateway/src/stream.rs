//! WebSocket stream — pipes selected NATS subjects to the client as JSON.
//!
//! `GET /stream?topics=device.>` subscribes to the matching NATS subject tree.
//! Each NATS message is rewrapped as JSON so the panel doesn't need a
//! protobuf runtime:
//!
//! * For `iot.device.v1.EntityState` we decode the protobuf and expose the
//!   value as native JSON (`value` field) + the device/entity ULIDs.
//! * Anything else falls through as `payload_b64` for the curious.
//!
//! W3a: no auth; W3b layers OIDC bearer validation on top.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::IntoResponse;
// `IntoResponse` is used by the `.into_response()` method calls below.
use base64::prelude::*;
use futures::StreamExt as _;
use iot_proto::iot::device::v1::EntityState;
use prost::Message as _;
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    /// NATS subject filter. Defaults to `device.>`. Must start with one of
    /// the allow-listed public prefixes.
    #[serde(default = "default_topics")]
    pub topics: String,

    /// Fallback bearer for browsers that can't set Authorization on a
    /// WebSocket handshake. Validated by the same `Verifier` as the
    /// REST middleware when OIDC is enabled.
    #[serde(default)]
    pub token: Option<String>,
}

fn default_topics() -> String {
    "device.>".into()
}

const ALLOWED_PREFIXES: &[&str] = &["device.", "automation.", "alert.", "ml."];

pub async fn stream_handler(
    ws: WebSocketUpgrade,
    Query(q): Query<StreamQuery>,
    State(state): State<AppState>,
) -> axum::response::Response {
    // Auth: when OIDC is configured we REQUIRE a token= query param. Bearer
    // header isn't settable on WS handshakes in browsers, so query is the
    // canonical place.
    if let Some(verifier) = state.verifier.as_ref() {
        let Some(token) = &q.token else {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                "missing ?token= query param",
            )
                .into_response();
        };
        if let Err(e) = verifier.verify(token).await {
            warn!(error = %e, "ws token rejected");
            return (axum::http::StatusCode::UNAUTHORIZED, "invalid token").into_response();
        }
    }
    ws.on_upgrade(move |socket| handle_socket(socket, q.topics, state))
}

/// Send a JSON string as a text WS frame. axum 0.8 wraps text in `Utf8Bytes`.
async fn send_text(socket: &mut WebSocket, s: String) -> Result<(), axum::Error> {
    socket.send(Message::Text(s.into())).await
}

async fn handle_socket(mut socket: WebSocket, topics: String, state: AppState) {
    if !ALLOWED_PREFIXES.iter().any(|p| topics.starts_with(p)) {
        let _ = send_text(
            &mut socket,
            r#"{"error":"topics filter must start with device./automation./alert./ml."}"#.into(),
        )
        .await;
        return;
    }

    let Some(bus) = state.bus.clone() else {
        let _ = send_text(&mut socket, r#"{"error":"bus not configured"}"#.into()).await;
        return;
    };

    let mut sub = match bus.raw().subscribe(topics.clone()).await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "nats subscribe failed");
            let _ = send_text(&mut socket, format!(r#"{{"error":"{e}"}}"#)).await;
            return;
        }
    };
    info!(topics, "ws client subscribed");

    // Panel-survives-reload: replay the last retained message(s)
    // from the DEVICE_STATE JetStream before letting the live
    // subscription stream further updates.
    //
    // Two paths depending on the topic filter:
    //   * Concrete subject (no wildcards) — `last_state(&topics)`
    //     fetches the single retained message via raw-get RPC. M3
    //     W2.5b shipped this.
    //   * Wildcard pattern (`device.>`, `device.zigbee2mqtt.*.state`)
    //     — `last_state_wildcard(&topics)` opens an ephemeral
    //     JetStream consumer with `DeliverLastPerSubject` filtered by
    //     the pattern, drains every distinct subject's last message
    //     once, then drops the consumer. M5a W2 shipped this — closes
    //     M4 architectural debt #5.
    if topics.contains('*') || topics.contains('>') {
        match bus.last_state_wildcard(&topics).await {
            Ok(replays) => {
                let count = replays.len();
                for (subject, payload) in replays {
                    let event = shape_event(&subject, "iot.device.v1.EntityState", &payload);
                    if send_text(&mut socket, event.to_string()).await.is_err() {
                        debug!("client dropped during wildcard replay");
                        return;
                    }
                }
                debug!(topics = %topics, count, "wildcard last-per-subject replay complete");
            }
            Err(e) => warn!(
                topics = %topics,
                error = %format!("{e:#}"),
                "wildcard replay fetch failed"
            ),
        }
    } else {
        match bus.last_state(&topics).await {
            Ok(Some(payload)) => {
                // We don't have the original iot-type header in the
                // retained message via the raw-get API. For the M3
                // shape-event path `shape_event` only cares about the
                // payload bytes when iot-type matches
                // `iot.device.v1.EntityState`; otherwise it passes
                // the subject through. Best-effort: try as EntityState.
                let event = shape_event(&topics, "iot.device.v1.EntityState", &payload);
                if send_text(&mut socket, event.to_string()).await.is_err() {
                    debug!("client dropped during replay");
                    return;
                }
                debug!(topics = %topics, "replayed last-known state");
            }
            Ok(None) => debug!(topics = %topics, "no retained state to replay"),
            Err(e) => warn!(topics = %topics, error = %format!("{e:#}"), "replay fetch failed"),
        }
    }

    loop {
        tokio::select! {
            msg = sub.next() => {
                let Some(msg) = msg else { break };
                let iot_type = msg
                    .headers
                    .as_ref()
                    .and_then(|h| h.get("iot-type"))
                    .map_or_else(String::new, ToString::to_string);
                // Scope the forward-to-client under the upstream
                // traceparent so any send-error log line lands under
                // the same trace id as the publisher.
                let ctx = iot_bus::extract_trace_context(&msg).map_or_else(
                    iot_observability::traceparent::TraceContext::new_root,
                    |p| p.child_of(),
                );
                let dropped = iot_observability::traceparent::with_context(ctx, async {
                    let event = shape_event(msg.subject.as_str(), &iot_type, &msg.payload);
                    send_text(&mut socket, event.to_string()).await.is_err()
                })
                .await;
                if dropped {
                    debug!("client dropped");
                    break;
                }
            }
            client_msg = socket.recv() => {
                let stop = matches!(
                    client_msg,
                    None | Some(Err(_) | Ok(Message::Close(_)))
                );
                if stop {
                    break;
                }
            }
        }
    }
    info!("ws client disconnected");
}

/// Rewrap a bus message into the panel-facing JSON shape.
fn shape_event(subject: &str, iot_type: &str, payload: &[u8]) -> serde_json::Value {
    if iot_type == "iot.device.v1.EntityState" {
        if let Ok(state) = EntityState::decode(payload) {
            return serde_json::json!({
                "subject": subject,
                "iot_type": iot_type,
                "device_id": state.device_id.as_ref().map(|u| u.value.clone()),
                "entity_id": state.entity_id.as_ref().map(|u| u.value.clone()),
                "value": prost_to_json(state.value),
                "at": state.at.map(|ts| {
                    chrono::DateTime::<chrono::Utc>::from_timestamp(ts.seconds, u32::try_from(ts.nanos).unwrap_or(0))
                        .map(|d| d.to_rfc3339())
                        .unwrap_or_default()
                }),
            });
        }
    }
    serde_json::json!({
        "subject": subject,
        "iot_type": iot_type,
        "payload_b64": BASE64_STANDARD.encode(payload),
    })
}

/// Convert a `google.protobuf.Value` (wrapped by prost-types) to
/// `serde_json::Value`. Unknown or malformed values become `null`.
fn prost_to_json(v: Option<prost_types::Value>) -> serde_json::Value {
    use prost_types::value::Kind;
    let Some(v) = v else {
        return serde_json::Value::Null;
    };
    match v.kind {
        None | Some(Kind::NullValue(_)) => serde_json::Value::Null,
        Some(Kind::BoolValue(b)) => serde_json::Value::Bool(b),
        Some(Kind::NumberValue(n)) => serde_json::Number::from_f64(n)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Some(Kind::StringValue(s)) => serde_json::Value::String(s),
        Some(Kind::StructValue(s)) => serde_json::Value::Object(
            s.fields
                .into_iter()
                .map(|(k, v)| (k, prost_to_json(Some(v))))
                .collect(),
        ),
        Some(Kind::ListValue(l)) => serde_json::Value::Array(
            l.values
                .into_iter()
                .map(|v| prost_to_json(Some(v)))
                .collect(),
        ),
    }
}
