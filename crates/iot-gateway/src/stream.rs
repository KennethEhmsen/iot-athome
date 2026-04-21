//! WebSocket stream — pipes selected NATS subjects to the client as JSON.
//!
//! `GET /stream?topics=device.>` subscribes to the matching NATS subject tree
//! and emits one JSON line per message:
//!
//! ```json
//! { "subject": "device.zigbee.<id>.temp.state",
//!   "iot_type": "iot.device.v1.EntityState",
//!   "payload_b64": "CgMBAgM..." }
//! ```
//!
//! W3a: no auth; W3b layers OIDC bearer validation on top.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use base64::prelude::*;
use futures::StreamExt as _;
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    /// NATS subject filter. Defaults to `device.>`. Must start with one of
    /// the allow-listed public prefixes.
    #[serde(default = "default_topics")]
    pub topics: String,
}

fn default_topics() -> String {
    "device.>".into()
}

const ALLOWED_PREFIXES: &[&str] = &["device.", "automation.", "alert.", "ml."];

pub async fn stream_handler(
    ws: WebSocketUpgrade,
    Query(q): Query<StreamQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
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

    loop {
        tokio::select! {
            msg = sub.next() => {
                let Some(msg) = msg else { break };
                let iot_type = msg
                    .headers
                    .as_ref()
                    .and_then(|h| h.get("iot-type"))
                    .map_or_else(String::new, ToString::to_string);
                let event = serde_json::json!({
                    "subject": msg.subject.as_str(),
                    "iot_type": iot_type,
                    "payload_b64": BASE64_STANDARD.encode(&msg.payload),
                });
                if send_text(&mut socket, event.to_string()).await.is_err() {
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
