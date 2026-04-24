//! zigbee2mqtt adapter — WASM plugin port of the M1 native adapter.
//!
//! Flow (per ADR-0013, post-M5a-W1):
//!
//!   1. `init()` → subscribe to `zigbee2mqtt/+` via the host MQTT
//!      capability. The host owns the rumqttc connection + mTLS
//!      material; the plugin never touches a socket.
//!   2. On every inbound MQTT message matching the filter,
//!      `on_mqtt_message(topic, payload)` fires:
//!        a. `translator::friendly_name_from_topic` pulls the
//!           `zigbee2mqtt/<friendly>` out of the topic.
//!        b. `state_publisher::publish_all` emits one `EntityState`
//!           protobuf per recognised key on
//!           `device.zigbee2mqtt.<friendly>.<entity>.state`. The
//!           iot-registry bus-watcher (M3 W1.2) sees the publish
//!           and auto-registers the
//!           `("zigbee2mqtt", <friendly>)` pair on first sight, so
//!           no explicit registry call is needed (and as of ABI
//!           1.3.0 / M5a W1, no host capability for it exists).
//!
//! The plugin uses `friendly_name` directly as the device id on
//! the bus subject; the registry mints + remembers a canonical ULID
//! internally, available via `iotctl device list`.
//!
//! Everything downstream (panel, automation) sees identical bytes to
//! what the M1 native adapter produced — the Protobuf wire format is
//! the contract, and `src/pb.rs` matches `schemas/iot/device/v1/*.proto`
//! field-for-field.

#![forbid(unsafe_code)]

mod state_publisher;
mod translator;

use iot_plugin_sdk_rust::exports::iot::plugin_host::runtime::{Guest, Payload, PluginError};
use iot_plugin_sdk_rust::iot::plugin_host::{log, mqtt};

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

        // 3. Publish one EntityState per recognised key. Each bus
        // publish is capability-checked against
        // `capabilities.bus.publish` (manifest declares
        // `device.zigbee2mqtt.>`). The iot-registry bus-watcher
        // (M3 W1.2) auto-registers the `("zigbee2mqtt", friendly)`
        // pair on first sight, so we no longer call
        // `registry::upsert_device` here — the host capability was
        // removed in ABI 1.3.0 (M5a W1).
        //
        // The bus subject embeds `friendly` directly as the device
        // id segment; the registry mints + remembers the canonical
        // ULID internally for `iotctl device list`. Older callers
        // that pass a ULID into `state_publisher::publish_all`'s
        // first slot still work — pass `friendly` as both id + name.
        state_publisher::publish_all(friendly, friendly, &json);
        Ok(())
    }
}

iot_plugin_sdk_rust::export_plugin!(Component with_types_in iot_plugin_sdk_rust);
