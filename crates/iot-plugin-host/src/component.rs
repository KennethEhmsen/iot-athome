//! Host-side bindings for the `iot:plugin-host@1.1.0` WIT world.
//!
//! `wasmtime::component::bindgen!` generates:
//!  * A `Plugin` type wrapping an instantiated component.
//!  * Host traits on the generated `bus::Host` + `log::Host` + `mqtt::Host` —
//!    implemented below on [`PluginState`].
//!  * `call_runtime_init` / `call_runtime_on_message` / `call_runtime_on_mqtt_message`
//!    accessors.
//!
//! 1.1.0 adds the `mqtt` interface + `on-mqtt-message` export per
//! [ADR-0013](../../../docs/adr/0013-zigbee2mqtt-wasm-migration.md). The
//! actual broker connection + inbound-dispatch loop lives in a separate
//! `mqtt` module (not yet in this commit — the host impl here is
//! capability-check-only and returns "no broker wired" for publish).

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
    /// Shared gRPC channel to the registry service. Plugins call it
    /// via the `registry` host capability (ABI 1.2.0+). `None` in
    /// offline / unit-test setups — the host impl returns a clear
    /// `registry.not_configured` PluginError in that case.
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

// ---------- registry host impl (ABI 1.2.0+, transitional per ADR-0013) ----------
//
// Wraps the registry gRPC client so plugins never link tonic/hyper
// (which don't target wasm32-wasip2). Capability-checked against
// `capabilities.registry.upsert`. When the host was started without a
// registry channel (unit tests, offline setups), returns a clear
// `registry.not_configured` PluginError instead of silently dropping
// the call — adapters that *need* registry have no useful fallback.

impl crate::component::iot::plugin_host::registry::Host for PluginState {
    #[tracing::instrument(
        name = "host_call",
        skip(self),
        fields(
            plugin = %self.id,
            capability = "registry.upsert-device",
            integration = %integration,
            external_id = %external_id,
        ),
    )]
    async fn upsert_device(
        &mut self,
        integration: String,
        external_id: String,
        label: String,
        manufacturer: String,
        model: String,
    ) -> Result<String, PluginError> {
        if let Err(d) = self.capabilities.check_registry_upsert() {
            warn!(reason = d.code, "capability.denied");
            record_denied(
                self.audit.clone(),
                self.id.clone(),
                format!("registry.upsert-device({integration}/{external_id})"),
                d.code.to_string(),
            )
            .await;
            return Err(PluginError {
                code: d.code.to_string(),
                message: d.message,
            });
        }
        let Some(channel) = self.registry.clone() else {
            return Err(PluginError {
                code: "registry.not_configured".into(),
                message: "host has no registry channel — check Config::registry_url".into(),
            });
        };

        use iot_proto::iot::device::v1::{Device, TrustLevel};
        use iot_proto::iot::registry::v1::registry_service_client::RegistryServiceClient;
        use iot_proto::iot::registry::v1::UpsertDeviceRequest;

        let mut client = RegistryServiceClient::new(channel);
        let device = Device {
            id: None,
            integration: integration.clone(),
            external_id: external_id.clone(),
            manufacturer,
            model,
            label,
            rooms: Vec::new(),
            capabilities: Vec::new(),
            entities: Vec::new(),
            trust_level: TrustLevel::UserAdded.into(),
            schema_version: iot_core::DEVICE_SCHEMA_VERSION,
            plugin_meta: std::collections::HashMap::default(),
            last_seen: None,
        };
        let resp = client
            .upsert_device(UpsertDeviceRequest {
                device: Some(device),
                idempotency_key: String::new(),
            })
            .await
            .map_err(|e| PluginError {
                code: "registry.upsert_failed".into(),
                message: format!("{e:#}"),
            })?
            .into_inner();
        let ulid = resp
            .device
            .and_then(|d| d.id)
            .map(|u| u.value)
            .ok_or_else(|| PluginError {
                code: "registry.upsert_failed".into(),
                message: "registry returned no device / id".into(),
            })?;
        debug!(ulid = %ulid, created = resp.created, "registry.upsert-device ok");
        Ok(ulid)
    }
}
