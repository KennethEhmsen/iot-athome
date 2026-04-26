//! Plugin SDK (Rust).
//!
//! Plugins compile to a WASM Component Model module targeting the
//! `iot:plugin-host@1.4.0` WIT world ([schemas/wit/iot-plugin-host.wit]).
//!
//! Notable per-version changes plugin authors care about:
//!   * 1.1.0 added the `mqtt` interface + `on-mqtt-message` runtime
//!     export (ADR-0013). Plugins that don't handle MQTT still need
//!     to implement `on_mqtt_message` — return `Ok(())` as a no-op.
//!   * 1.2.0 added the transitional `registry` interface (later
//!     dropped — see below).
//!   * 1.3.0 (M5a W1) **removed** the `registry` interface. The
//!     iot-registry bus-watcher auto-registers any
//!     `(integration, external_id)` pair seen on `device.>` publishes,
//!     so adapters drop their `registry::upsert_device(...)` calls and
//!     simply publish state. Plugins built against 1.2.0 won't load
//!     under a 1.3.0+ host.
//!   * 1.4.0 added the `net` interface (single import: `net::http`).
//!     One-shot HTTP outbound, capability-checked against the
//!     manifest's `capabilities.net.outbound` URL-prefix allow-list —
//!     anything outside the list returns
//!     `PluginError { code: "capability.denied", … }`. The host
//!     enforces a default 10-second timeout, disables automatic
//!     redirects (3xx is surfaced verbatim — the plugin decides
//!     whether to retarget; the next URL must also be allow-listed),
//!     and disables compression negotiation. Non-2xx responses come
//!     back as `Ok(http-response)`, not `Err` — the plugin owns the
//!     status-code policy. Strictly additive: 1.3.0 plugins continue
//!     to load against a 1.4.0 host. Unblocks the M5a-gap class of
//!     integrations (weather, dynamic energy tariffs, calendar,
//!     notification sinks, HTTP-based device APIs).
//!
//! Usage (from a plugin crate that depends on this SDK):
//!
//! ```ignore
//! use iot_plugin_sdk_rust::*;
//!
//! struct Component;
//!
//! impl Guest for Component {
//!     fn init() -> Result<(), PluginError> {
//!         iot::plugin_host::log::emit(
//!             iot::plugin_host::log::Level::Info,
//!             "demo-echo",
//!             "hello from the sandbox",
//!         );
//!         Ok(())
//!     }
//!     fn on_message(subject: String, iot_type: String, payload: Payload)
//!         -> Result<(), PluginError>
//!     {
//!         iot::plugin_host::bus::publish(
//!             &format!("{subject}.echo"), &iot_type, &payload,
//!         )
//!     }
//! }
//!
//! iot_plugin_sdk_rust::export!(Component with_types_in iot_plugin_sdk_rust);
//! ```
//!
//! See [ADR-0003] for the ABI contract and [ADR-0012] for the bindgen choice.

#![forbid(unsafe_code)]
#![allow(clippy::all, clippy::pedantic, clippy::nursery)]

wit_bindgen::generate!({
    world: "plugin",
    path: "../../schemas/wit",
    pub_export_macro: true,
    export_macro_name: "export_plugin",
});

// Re-export host-facing helpers that plugins can pull via the SDK without
// adding iot-core as a direct dependency.
//
// iot-proto (Protobuf types + gRPC clients) is intentionally NOT re-exported
// here — its tonic / socket2 transitive deps don't target wasm32-wasip2.
// Plugins that need Protobuf encoding depend on `prost` directly.
pub use iot_core::DEVICE_SCHEMA_VERSION;
