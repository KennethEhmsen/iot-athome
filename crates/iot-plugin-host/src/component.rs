//! Host-side bindings for the `iot:plugin-host@1.0.0` WIT world.
//!
//! `wasmtime::component::bindgen!` generates:
//!  * A `Plugin` type wrapping an instantiated component.
//!  * Host traits on the generated `bus::Host` + `log::Host` — implemented
//!    below on [`PluginState`].
//!  * `call_runtime_init` / `call_runtime_on_message` accessors.

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
/// `bus` and `audit` are `Option` so unit tests can exercise the
/// capability / wiring logic without requiring a live NATS server.
pub struct PluginState {
    pub id: String,
    pub capabilities: CapabilityMap,
    pub bus: Option<Bus>,
    pub audit: Option<Arc<AuditLog>>,
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

impl crate::component::iot::plugin_host::bus::Host for PluginState {
    async fn publish(
        &mut self,
        subject: String,
        iot_type: String,
        payload: Vec<u8>,
    ) -> Result<(), PluginError> {
        if let Err(d) = self.capabilities.check_bus_publish(&subject) {
            warn!(plugin = %self.id, subject, reason = d.code, "capability.denied");
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
            debug!(
                plugin = %self.id, subject, iot_type,
                "bus.publish (no bus configured — dry run)"
            );
            return Ok(());
        };
        debug!(
            plugin = %self.id, subject, iot_type, bytes = payload.len(),
            "bus.publish"
        );
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
