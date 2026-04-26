//! Matter (CSA) bridge — third WASM plugin (scaffold).
//!
//! Phase 2 deliverable per [docs/MATTER-PLAN.md] — the plugin
//! compiles to `wasm32-wasip2`, registers via `iotctl plugin
//! install`, and logs every inbound MQTT message to verify the
//! shim → host → plugin delivery path before the translator (Phase
//! 3) lands. The translator + state_publisher modules will mirror
//! the sdr433-adapter shape.
//!
//! Architecture: per [ADR-0014], a python-matter-server controller
//! plus a small Python WS→MQTT shim publish events on
//! `matter/nodes/<node_id>/endpoints/<endpoint_id>/clusters/<cluster_id>/<attribute>`.
//! This plugin subscribes to that topic tree, translates each event
//! to canonical `iot.device.v1.EntityState`, and publishes on
//! `device.matter.<node_id>-<endpoint_id>.<entity>.state`.
//!
//! All bus + log calls are capability-checked against the manifest's
//! `capabilities.bus.publish` / `capabilities.mqtt.subscribe`
//! allow-lists. Out-of-scope calls return `PluginError { code:
//! "capability.denied", … }` per ADR-0003 — handled as a value, not
//! a trap.

#![forbid(unsafe_code)]

use iot_plugin_sdk_rust::exports::iot::plugin_host::runtime::{Guest, Payload, PluginError};
use iot_plugin_sdk_rust::iot::plugin_host::{log, mqtt};

struct Component;

/// MQTT filter we subscribe to. Must be covered by the manifest's
/// `capabilities.mqtt.subscribe` allow-list or the host rejects it.
///
/// Topic shape from ADR-0014:
/// `matter/nodes/<node_id>/endpoints/<endpoint_id>/clusters/<cluster_id>/<attribute>`
/// → 5 wildcard segments after `matter/nodes/`.
const FILTER: &str = "matter/nodes/+/+/+/+/+";

impl Guest for Component {
    fn init() -> Result<(), PluginError> {
        log::emit(log::Level::Info, "matter-bridge", "init (scaffold)");
        mqtt::subscribe(FILTER)?;
        log::emit(
            log::Level::Info,
            "matter-bridge",
            &format!("subscribed to `{FILTER}`"),
        );
        Ok(())
    }

    /// We're MQTT-driven; no bus subscriptions. A bus `on_message`
    /// arrival would be a host-side bug — log + ignore.
    fn on_message(
        subject: String,
        _iot_type: String,
        _payload: Payload,
    ) -> Result<(), PluginError> {
        log::emit(
            log::Level::Warn,
            "matter-bridge",
            &format!("unexpected bus on_message on subject={subject}"),
        );
        Ok(())
    }

    fn on_mqtt_message(topic: String, payload: Payload) -> Result<(), PluginError> {
        // Phase 2 scaffold: log only. Phase 3 will route to
        // `translator::parse_topic + translator::cluster_event` and
        // emit one EntityState per recognised cluster type via
        // `state_publisher::publish_all`.
        log::emit(
            log::Level::Debug,
            "matter-bridge",
            &format!(
                "(phase-2 scaffold) mqtt {topic} ({} bytes) — translator lands phase 3",
                payload.len()
            ),
        );
        Ok(())
    }
}

iot_plugin_sdk_rust::export_plugin!(Component with_types_in iot_plugin_sdk_rust);
