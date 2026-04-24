//! Host-side bindings for the `iot:plugin-host@1.3.0` WIT world.
//!
//! `wasmtime::component::bindgen!` generates:
//!  * A `Plugin` type wrapping an instantiated component.
//!  * Host traits on the generated `bus::Host` + `log::Host` + `mqtt::Host` —
//!    implemented below on [`PluginState`].
//!  * `call_runtime_init` / `call_runtime_on_message` / `call_runtime_on_mqtt_message`
//!    accessors.
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
