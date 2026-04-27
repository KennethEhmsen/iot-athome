//! Host-side bindings for the `iot:plugin-host@1.4.0` WIT world.
//!
//! `wasmtime::component::bindgen!` generates:
//!  * A `Plugin` type wrapping an instantiated component.
//!  * Host traits on the generated `bus::Host` + `log::Host` +
//!    `mqtt::Host` + `net::Host` — implemented below on [`PluginState`].
//!  * `call_runtime_init` / `call_runtime_on_message` /
//!    `call_runtime_on_mqtt_message` accessors.
//!
//! ABI evolution (see also `schemas/wit/iot-plugin-host.wit`):
//!  * 1.1.0 — added the `mqtt` interface + `on-mqtt-message` export
//!    per [ADR-0013](../../../docs/adr/0013-zigbee2mqtt-wasm-migration.md).
//!  * 1.2.0 — added the transitional `registry::upsert-device` capability.
//!  * 1.3.0 (M5a W1) — **removed** `registry::upsert-device`. The
//!    iot-registry bus-watcher (M3 W1.2) auto-registers devices from
//!    `device.>` publishes; adapters drop the explicit upsert call.
//!    Plugins built against 1.2.0 won't load — wasmtime errors at
//!    instantiation when their unresolved import isn't satisfied.
//!  * 1.4.0 — additive `net.http` host capability for outbound HTTP.
//!    Plugins declare URL prefixes in `capabilities.net.outbound`;
//!    host enforces an exact-prefix match before issuing the request.
//!    1.3.0 plugins still load (additive — the `import net;` line is
//!    new but the host satisfies it whether the plugin uses it or not).

#![allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    missing_debug_implementations
)]

use std::sync::Arc;

use iot_audit::AuditLog;
use iot_bus::Bus;
use tracing::{debug, error, info, trace, warn};

use crate::capabilities::CapabilityMap;

wasmtime::component::bindgen!({
    world: "plugin",
    path: "../../schemas/wit",
    // Wasmtime 36+ scopes async to import/export sets via `imports:` /
    // `exports:`. `default: async` marks every function async; returns
    // are wrapped in `wasmtime::Result<T>` (errors become traps).
    imports: { default: async },
    exports: { default: async },
});

/// Per-plugin runtime state passed into the Wasmtime Store.
///
/// `bus`, `audit`, `mqtt`, `self_tx` are all `Option` so unit tests can
/// exercise the capability / wiring logic without requiring live
/// broker connections or the plugin runtime task infrastructure.
pub struct PluginState {
    pub id: String,
    pub capabilities: CapabilityMap,
    pub bus: Option<Bus>,
    pub audit: Option<Arc<AuditLog>>,
    /// Shared MQTT broker handle. `mqtt::subscribe` host calls register
    /// with the broker's router + subscribe to the underlying filter;
    /// `mqtt::publish` goes straight through the broker's client.
    /// `None` when the host was built without MQTT support (most unit
    /// tests).
    pub mqtt: Option<Arc<crate::mqtt::MqttBroker>>,
    /// Shared gRPC channel to the registry service. The 1.2.0
    /// `registry::upsert-device` import that consumed it was removed
    /// in 1.3.0; the field stays for the gRPC-stream metadata path
    /// + future per-plugin admin RPCs. `None` in offline / unit-test
    /// setups.
    pub registry: Option<tonic::transport::Channel>,
    /// Shared HTTP client backing the ABI 1.4.0 `net.http` host
    /// capability. One per host process so connection pooling
    /// survives across plugins. `None` in offline / unit-test setups
    /// — `net::Host::http` then returns `net.not_configured` to the
    /// plugin.
    pub net_client: Option<Arc<reqwest::Client>>,
    /// This plugin's own mailbox, used when registering with
    /// `MqttRouter` so inbound messages route back to us. Filled in by
    /// [`crate::runtime::spawn_plugin_task`] after the mpsc channel
    /// exists; `None` outside a spawned runtime task.
    pub self_tx: Option<tokio::sync::mpsc::Sender<crate::runtime::PluginCommand>>,
    /// WASI preview-2 context. The wasip2 preview adapter emits imports
    /// (environment/stdin/stdout/...) for every compiled plugin; these
    /// satisfy them.
    pub wasi: wasmtime_wasi::WasiCtx,
    pub table: wasmtime::component::ResourceTable,
}

impl wasmtime_wasi::WasiView for PluginState {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// Fully-qualified type name of the wit-bindgen-generated error.
pub type PluginError = crate::component::iot::plugin_host::types::PluginError;

/// Fire-and-forget audit write. Clones the Arc<AuditLog> so the caller
/// doesn't borrow `self` across the await — otherwise the caller's async
/// fn becomes `!Send` and wasmtime's async bindgen rejects it.
async fn record_denied(
    audit: Option<Arc<AuditLog>>,
    plugin_id: String,
    subject: String,
    reason: String,
) {
    let Some(audit) = audit else { return };
    let payload = serde_json::json!({
        "plugin_id": plugin_id,
        "subject": subject,
        "reason": reason,
    });
    if let Err(e) = audit.append("plugin.denied", payload).await {
        error!(plugin = %plugin_id, error = %e, "audit append failed");
    }
}

// ---------- bus host impl ----------
//
// Every host call is wrapped in a `#[tracing::instrument]` span that
// carries `plugin`, `capability`, and the call-specific args (`subject`,
// `bytes`, …). Once `iot_observability` ships traceparent propagation
// (M3), these spans light up end-to-end across panel → gateway →
// registry → plugin without further wiring.

impl crate::component::iot::plugin_host::bus::Host for PluginState {
    #[tracing::instrument(
        name = "host_call",
        skip(self, payload),
        fields(
            plugin = %self.id,
            capability = "bus.publish",
            subject = %subject,
            iot_type = %iot_type,
            bytes = payload.len(),
        ),
    )]
    async fn publish(
        &mut self,
        subject: String,
        iot_type: String,
        payload: Vec<u8>,
    ) -> Result<(), PluginError> {
        if let Err(d) = self.capabilities.check_bus_publish(&subject) {
            warn!(reason = d.code, "capability.denied");
            record_denied(
                self.audit.clone(),
                self.id.clone(),
                subject.clone(),
                d.code.to_string(),
            )
            .await;
            return Err(PluginError {
                code: d.code.to_string(),
                message: d.message,
            });
        }

        // Capability check passed. Clone the Bus (async-nats Client is
        // Arc'd internally) so we don't borrow `self` across the await —
        // otherwise the future is !Send and wasmtime's async trait rejects it.
        let Some(bus) = self.bus.clone() else {
            debug!("bus.publish (no bus configured — dry run)");
            return Ok(());
        };
        debug!("bus.publish");
        bus.publish_proto(&subject, &iot_type, payload, None)
            .await
            .map_err(|e| PluginError {
                code: "bus.publish_failed".into(),
                message: e.to_string(),
            })
    }
}

// ---------- log host impl ----------

impl crate::component::iot::plugin_host::log::Host for PluginState {
    #[tracing::instrument(
        name = "host_call",
        skip(self),
        fields(
            plugin = %self.id,
            capability = "log.emit",
            level = ?level,
            target = %target,
        ),
    )]
    async fn emit(
        &mut self,
        level: crate::component::iot::plugin_host::log::Level,
        target: String,
        message: String,
    ) {
        use crate::component::iot::plugin_host::log::Level as L;
        let plugin = self.id.as_str();
        match level {
            L::Trace => trace!(plugin, target = %target, "{message}"),
            L::Debug => debug!(plugin, target = %target, "{message}"),
            L::Info => info!(plugin, target = %target, "{message}"),
            L::Warn => warn!(plugin, target = %target, "{message}"),
            L::Error => error!(plugin, target = %target, "{message}"),
        }
    }
}

// ---------- mqtt host impl ----------
//
// Capability enforcement lives here; broker ownership (rumqttc client +
// dispatcher loop) lands in a follow-up commit (`src/mqtt.rs`). Until
// then `publish` is a dry-run, and `subscribe` records the intent via
// an audit entry but doesn't wire an actual dispatch. Plugins calling
// these functions still get the full capability-denied path if their
// manifest doesn't allow-list the topic — the security boundary is in
// place even without the broker wiring.

impl crate::component::iot::plugin_host::mqtt::Host for PluginState {
    #[tracing::instrument(
        name = "host_call",
        skip(self),
        fields(
            plugin = %self.id,
            capability = "mqtt.subscribe",
            filter = %filter,
        ),
    )]
    async fn subscribe(&mut self, filter: String) -> Result<(), PluginError> {
        if let Err(d) = self.capabilities.check_mqtt_subscribe(&filter) {
            warn!(reason = d.code, "capability.denied");
            record_denied(
                self.audit.clone(),
                self.id.clone(),
                filter.clone(),
                d.code.to_string(),
            )
            .await;
            return Err(PluginError {
                code: d.code.to_string(),
                message: d.message,
            });
        }
        // Register with the router + tell the broker to subscribe.
        // Both pieces need to be wired — tests and the demo-echo
        // roundtrip don't have a broker (or a runtime task) hooked up,
        // so they fall through to the intent-only branch and the
        // capability check remains the enforced gate.
        match (self.mqtt.as_ref(), self.self_tx.as_ref()) {
            (Some(broker), Some(tx)) => {
                broker
                    .router()
                    .register(self.id.clone(), filter.clone(), tx.clone());
                if let Err(e) = broker.subscribe_filter(&filter).await {
                    // Broker-side subscribe failed (channel full etc.).
                    // Capability check already said yes and the router
                    // registration landed — surface as a PluginError
                    // so the plugin can decide to retry.
                    return Err(PluginError {
                        code: "mqtt.broker_subscribe_failed".into(),
                        message: format!("{e:#}"),
                    });
                }
                info!("mqtt.subscribe registered + broker subscribed");
            }
            _ => info!("mqtt.subscribe (broker not wired — intent recorded)"),
        }
        Ok(())
    }

    #[tracing::instrument(
        name = "host_call",
        skip(self, payload),
        fields(
            plugin = %self.id,
            capability = "mqtt.publish",
            topic = %topic,
            retain,
            bytes = payload.len(),
        ),
    )]
    async fn publish(
        &mut self,
        topic: String,
        payload: Vec<u8>,
        retain: bool,
    ) -> Result<(), PluginError> {
        if let Err(d) = self.capabilities.check_mqtt_publish(&topic) {
            warn!(reason = d.code, "capability.denied");
            record_denied(
                self.audit.clone(),
                self.id.clone(),
                topic.clone(),
                d.code.to_string(),
            )
            .await;
            return Err(PluginError {
                code: d.code.to_string(),
                message: d.message,
            });
        }
        let Some(broker) = self.mqtt.clone() else {
            debug!(retain, "mqtt.publish (no broker configured — dry run)");
            return Ok(());
        };
        broker
            .publish(&topic, &payload, retain)
            .await
            .map_err(|e| PluginError {
                code: "mqtt.publish_failed".into(),
                message: format!("{e:#}"),
            })
    }
}

// ---------- registry host impl: removed in ABI 1.3.0 (M5a W1) ----------
//
// The `registry::upsert-device` host capability is gone. The
// iot-registry bus-watcher shipped in M3 W1.2 auto-registers any
// unknown `(integration, external_id)` pair on `device.>` publishes,
// which makes the explicit upsert call redundant. M4 shipped a
// one-shot deprecation warn log; M5a removes the import + handler.
//
// Adapter plugins that previously called registry::upsert simply
// drop the import — publishing on `device.<id>.state` is enough to
// register the device. The host's per-plugin `registry` channel
// field stays in PluginState (kept for the gRPC-stream metadata path
// + future per-plugin admin RPCs); only the WASM import is gone.

// ---------- net host impl (ABI 1.4.0) ----------
//
// Outbound HTTP for plugins polling external APIs. URL check
// against the manifest's `capabilities.net.outbound` allow-list
// runs FIRST — anything outside the allow-list returns
// capability.denied without touching reqwest, so a misconfigured
// plugin can't trigger DNS or TCP traffic against unintended hosts.
//
// Transport-level failures (DNS, TCP, TLS, timeout) surface as
// `net.transport`. HTTP-level failures (4xx / 5xx) surface as
// `Ok(http-response)` with the status code populated — plugins
// decide whether a 404 or 500 is "this device says no" or a real
// error. Body is opaque bytes; reqwest's automatic gzip / brotli /
// deflate are all disabled by `build_net_client`, so plugins see
// what came off the wire.
//
// Resource caps + header hardening (Bucket 1 audit fixes):
//
// * `MAX_REQ_BODY_BYTES` / `MAX_RESP_BODY_BYTES` cap memory use;
//   per-call timeout + body cap together bound the worst case the
//   host has to absorb from a malicious or runaway plugin.
// * `FORBIDDEN_REQ_HEADERS` strips host-spoofing /
//   credential-exfiltration headers a plugin might set. The host
//   gets to decide `Host`, `Authorization`, `Cookie`,
//   `Proxy-*`, `X-Forwarded-*` — plugins don't.
// * Streaming chunk-read for the response so we abort early when
//   bytes pile past the cap rather than buffering the whole body
//   first. Catches both Content-Length advertised + chunked
//   no-Content-Length attackers.

/// Per-request body caps. Generous for the documented use case
/// (weather APIs, energy tariffs, calendar feeds, notification
/// sinks) — no legitimate poll surface needs more than this.
const MAX_REQ_BODY_BYTES: usize = 4 * 1024 * 1024; // 4 MiB
const MAX_RESP_BODY_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

/// Headers a plugin must not be allowed to set. Either:
///
/// * Spoof identity to upstream filters (`Host`, `X-Forwarded-*`).
/// * Exfiltrate or override credentials the host might add later
///   (`Authorization`, `Cookie`, `Proxy-Authorization`).
/// * Trigger proxy behaviour the plugin shouldn't choose
///   (`Proxy-*`).
///
/// Comparison is case-insensitive (HTTP header names are too).
const FORBIDDEN_REQ_HEADERS: &[&str] = &[
    "host",
    "authorization",
    "cookie",
    "proxy-authorization",
    "proxy-connection",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-real-ip",
];

/// True if `name` is in [`FORBIDDEN_REQ_HEADERS`]. Hot-path; avoid
/// allocating a lower-cased copy of every header per request.
fn header_forbidden(name: &str) -> bool {
    FORBIDDEN_REQ_HEADERS
        .iter()
        .any(|f| name.eq_ignore_ascii_case(f))
}

impl crate::component::iot::plugin_host::net::Host for PluginState {
    #[tracing::instrument(
        name = "host_call",
        skip(self, req),
        fields(
            plugin = %self.id,
            capability = "net.http",
            method = %req.method,
            url = %req.url,
            req_bytes = req.body.as_ref().map_or(0, Vec::len),
        ),
    )]
    async fn http(
        &mut self,
        req: crate::component::iot::plugin_host::net::HttpRequest,
    ) -> Result<crate::component::iot::plugin_host::net::HttpResponse, PluginError> {
        // 1. Manifest URL-prefix check.
        if let Err(d) = self.capabilities.check_net_outbound(&req.url) {
            warn!(reason = d.code, "capability.denied");
            record_denied(
                self.audit.clone(),
                self.id.clone(),
                format!("net.http({} {})", req.method, req.url),
                d.code.to_string(),
            )
            .await;
            return Err(PluginError {
                code: d.code.to_string(),
                message: d.message,
            });
        }

        // 2. Pull the shared client. Tests + offline loaders pass
        // None for `net_client`; surface as `net.not_configured` so
        // the plugin sees a clear, actionable error code.
        let Some(client) = self.net_client.as_ref() else {
            return Err(PluginError {
                code: "net.not_configured".into(),
                message: "host has no net.http client — was build_net_client called?".into(),
            });
        };

        // 3. Cap the request body before reqwest sees it.
        if let Some(body) = req.body.as_ref() {
            if body.len() > MAX_REQ_BODY_BYTES {
                return Err(PluginError {
                    code: "net.body_too_large".into(),
                    message: format!(
                        "request body {} bytes exceeds cap {MAX_REQ_BODY_BYTES}",
                        body.len()
                    ),
                });
            }
        }

        // 4. Translate WIT request → reqwest::RequestBuilder.
        // Method is normalised to upper-case; reqwest::Method::from_bytes
        // accepts the canonical set + emits a sensible error for
        // garbage like "GETTT".
        let method = reqwest::Method::from_bytes(req.method.to_ascii_uppercase().as_bytes())
            .map_err(|e| PluginError {
                code: "net.bad_method".into(),
                message: format!("invalid HTTP method `{}`: {e}", req.method),
            })?;
        let mut builder = client.request(method, &req.url);
        // Header filter — see FORBIDDEN_REQ_HEADERS doc for the
        // attack classes this defends against.
        for (k, v) in &req.headers {
            if header_forbidden(k) {
                warn!(header = %k, "net.http: dropping plugin-supplied forbidden header");
                continue;
            }
            builder = builder.header(k.as_str(), v.as_str());
        }
        if let Some(body) = req.body {
            builder = builder.body(body);
        }

        // 5. Execute. Transport-level failures (DNS, TCP, TLS,
        // timeout) → net.transport. HTTP-level (4xx/5xx) lands as
        // Ok with the status code populated.
        let resp = builder.send().await.map_err(|e| {
            warn!(error = %e, "net.http transport failed");
            PluginError {
                code: "net.transport".into(),
                message: format!("{e}"),
            }
        })?;

        let status = resp.status().as_u16();
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .filter_map(|(k, v)| {
                v.to_str()
                    .ok()
                    .map(|s| (k.as_str().to_owned(), s.to_owned()))
            })
            .collect();

        // 6. Early-reject on advertised Content-Length over the cap
        // — saves us streaming a body we'll just throw away.
        if let Some(len) = resp.content_length() {
            if len > MAX_RESP_BODY_BYTES as u64 {
                return Err(PluginError {
                    code: "net.body_too_large".into(),
                    message: format!(
                        "response Content-Length {len} bytes exceeds cap {MAX_RESP_BODY_BYTES}"
                    ),
                });
            }
        }

        // 7. Stream the body chunk-by-chunk with a running cap.
        // Catches both Content-Length-advertised and chunked-
        // transfer-encoding cases (no Content-Length) where the
        // body lies about its size.
        use futures::StreamExt as _;
        let mut stream = resp.bytes_stream();
        let mut body: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| PluginError {
                code: "net.transport".into(),
                message: format!("read body chunk: {e}"),
            })?;
            if body.len().saturating_add(chunk.len()) > MAX_RESP_BODY_BYTES {
                return Err(PluginError {
                    code: "net.body_too_large".into(),
                    message: format!("streamed response body exceeded cap {MAX_RESP_BODY_BYTES}"),
                });
            }
            body.extend_from_slice(&chunk);
        }
        let resp_bytes = body.len();
        debug!(status, resp_bytes, "net.http ok");

        Ok(crate::component::iot::plugin_host::net::HttpResponse {
            status,
            headers,
            body,
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod net_header_filter_tests {
    use super::header_forbidden;

    #[test]
    fn forbids_host_spoofing_and_credential_headers() {
        // Host-spoofing class.
        assert!(header_forbidden("Host"));
        assert!(header_forbidden("host"));
        assert!(header_forbidden("HOST"));
        assert!(header_forbidden("X-Forwarded-For"));
        assert!(header_forbidden("x-forwarded-host"));
        assert!(header_forbidden("X-Real-IP"));

        // Credential exfiltration / override class.
        assert!(header_forbidden("Authorization"));
        assert!(header_forbidden("authorization"));
        assert!(header_forbidden("Cookie"));
        assert!(header_forbidden("Proxy-Authorization"));
        assert!(header_forbidden("proxy-connection"));
    }

    #[test]
    fn permits_typical_api_headers() {
        // The headers a polling-API plugin normally needs.
        assert!(!header_forbidden("Content-Type"));
        assert!(!header_forbidden("Accept"));
        assert!(!header_forbidden("Accept-Language"));
        assert!(!header_forbidden("User-Agent"));
        assert!(!header_forbidden("X-Request-Id"));
        assert!(!header_forbidden("X-Custom-Vendor-Header"));
    }
}
