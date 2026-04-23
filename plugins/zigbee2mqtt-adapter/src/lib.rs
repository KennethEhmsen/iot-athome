//! zigbee2mqtt adapter — WASM plugin port of the M1 native adapter.
//!
//! Flow (per ADR-0013):
//!
//!   1. `init()` → subscribe to `zigbee2mqtt/+` via the host MQTT
//!      capability. The host owns the rumqttc connection + mTLS
//!      material; the plugin never touches a socket.
//!   2. On every inbound MQTT message matching the filter,
//!      `on_mqtt_message(topic, payload)` fires:
//!        a. `translator::friendly_name_from_topic` pulls the
//!           `zigbee2mqtt/<friendly>` out of the topic.
//!        b. `registry::upsert_device(...)` registers the device in
//!           the registry via the host's gRPC capability (returns the
//!           canonical ULID — mints one first time, same ULID forever
//!           after).
//!        c. `state_publisher::publish_all` emits one
//!           `EntityState` protobuf per recognised key on
//!           `device.zigbee2mqtt.<id>.<entity>.state`.
//!
//! Everything downstream (panel, automation) sees identical bytes to
//! what the M1 native adapter produced — the Protobuf wire format is
//! the contract, and `src/pb.rs` matches `schemas/iot/device/v1/*.proto`
//! field-for-field.

#![forbid(unsafe_code)]

mod pb;
mod state_publisher;
mod translator;

use iot_plugin_sdk_rust::exports::iot::plugin_host::runtime::{Guest, Payload, PluginError};
use iot_plugin_sdk_rust::iot::plugin_host::{log, mqtt, registry};

struct Component;

/// The MQTT filter we subscribe to. Must be covered by the manifest's
/// `capabilities.mqtt.subscribe` allow-list or the host rejects it.
const FILTER: &str = "zigbee2mqtt/+";

impl Guest for Component {
    fn init() -> Result<(), PluginError> {
        log::emit(log::Level::Info, "zigbee2mqtt-adapter", "init");
        mqtt::subscribe(FILTER)?;
        log::emit(
            log::Level::Info,
            "zigbee2mqtt-adapter",
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
            "zigbee2mqtt-adapter",
            &format!("unexpected bus on_message on subject={subject}"),
        );
        Ok(())
    }

    fn on_mqtt_message(topic: String, payload: Payload) -> Result<(), PluginError> {
        // 1. Friendly name out of the topic. Non-matching topics (which
        // shouldn't arrive thanks to the capability filter) are logged
        // and dropped.
        let Some(friendly) = translator::friendly_name_from_topic(&topic) else {
            log::emit(
                log::Level::Debug,
                "zigbee2mqtt-adapter",
                &format!("ignoring non-zigbee topic `{topic}`"),
            );
            return Ok(());
        };

        // 2. Parse payload as JSON. A non-object payload is benign —
        // zigbee2mqtt occasionally sends `null` for availability pings.
        let json: serde_json::Value = match serde_json::from_slice(&payload) {
            Ok(v) => v,
            Err(e) => {
                log::emit(
                    log::Level::Warn,
                    "zigbee2mqtt-adapter",
                    &format!("payload on `{topic}` not valid JSON: {e}"),
                );
                return Ok(());
            }
        };
        if !json.is_object() {
            log::emit(
                log::Level::Debug,
                "zigbee2mqtt-adapter",
                &format!("non-object payload on `{topic}` — skipping"),
            );
            return Ok(());
        }

        // 3. Upsert the device. The registry is idempotent on
        // `(integration, external_id)`, so repeat arrivals for the same
        // friendly_name return the same ULID cheaply.
        let device_ulid = registry::upsert_device(
            "zigbee2mqtt",
            friendly,
            friendly, // label defaults to friendly-name (user can rename via panel)
            "",       // manufacturer unknown from z2m payload alone
            "",       // model ditto — z2m surfaces these in `/bridge/devices` which is M3
        )?;

        // 4. Publish one EntityState per recognised key. Each bus
        // publish is capability-checked against
        // `capabilities.bus.publish` (manifest declares
        // `device.zigbee2mqtt.>`).
        state_publisher::publish_all(&device_ulid, friendly, &json);
        Ok(())
    }
}

iot_plugin_sdk_rust::export_plugin!(Component with_types_in iot_plugin_sdk_rust);
