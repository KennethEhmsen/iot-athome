//! Host-side bindings for the `iot:plugin-host@1.0.0` WIT world.
//!
//! `wasmtime::component::bindgen!` generates:
//!  * A `Plugin` type wrapping an instantiated component.
//!  * Host traits (`IotPluginHostBusImports`, `IotPluginHostLogImports`) we
//!    implement below to supply the `bus.publish` + `log.emit` imports.
//!  * `call_runtime_init` / `call_runtime_on_message` on `Plugin::runtime()`.

#![allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    missing_debug_implementations
)]

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
pub struct PluginState {
    pub id: String,
    pub capabilities: CapabilityMap,
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

// ---------- bus host impl ----------

impl crate::component::iot::plugin_host::bus::Host for PluginState {
    async fn publish(
        &mut self,
        subject: String,
        iot_type: String,
        _payload: Vec<u8>,
    ) -> Result<(), PluginError> {
        self.capabilities
            .check_bus_publish(&subject)
            .map_err(|d| PluginError {
                code: d.code.to_string(),
                message: d.message,
            })?;
        // M2 W2 wires the real iot_bus::Bus here. For W1 we only prove
        // the binding compiles + the capability check fires.
        debug!(plugin = %self.id, subject, iot_type, "bus.publish (stub)");
        Ok(())
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
