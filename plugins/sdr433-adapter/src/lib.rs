//! rtl_433 SDR adapter — second WASM plugin in the M5a W3 set.
//!
//! Flow: `init()` subscribes to `rtl_433/+` via the host MQTT
//! capability (the host owns the rumqttc connection + mTLS material;
//! the plugin never touches a socket). Every inbound MQTT message
//! matching the filter fires `on_mqtt_message(topic, payload)` which
//! parses the payload as JSON (rtl_433 emits one envelope per decoded
//! RF packet under `-F mqtt://...,events`), derives a canonical
//! `<model>-<id>[-<channel>]` device id via
//! [`translator::device_id_from_envelope`], and emits one
//! `EntityState` protobuf per recognised key on
//! `device.sdr433.<id>.<entity>.state` via
//! [`state_publisher::publish_all`].
//!
//! The iot-registry bus-watcher (M3 W1.2) auto-registers each
//! `(sdr433, <id>)` pair on first publish — no `registry::upsert`
//! capability needed (gone in ABI 1.3.0 / M5a W1).
//!
//! All bus + log calls are capability-checked against the manifest's
//! `capabilities.bus.publish` / `capabilities.mqtt.subscribe`
//! allow-lists. Any call outside the allow-list returns
//! `PluginError { code: "capability.denied", … }` rather than a trap,
//! per ADR-0003 — the plugin handles the denial as a value, not a
//! crash.

#![forbid(unsafe_code)]

mod state_publisher;
mod translator;

use iot_plugin_sdk_rust::exports::iot::plugin_host::runtime::{Guest, Payload, PluginError};
use iot_plugin_sdk_rust::iot::plugin_host::{log, mqtt};

struct Component;

/// The MQTT filter we subscribe to. Must be covered by the manifest's
/// `capabilities.mqtt.subscribe` allow-list or the host rejects it.
const FILTER: &str = "rtl_433/+";

impl Guest for Component {
    fn init() -> Result<(), PluginError> {
        log::emit(log::Level::Info, "sdr433-adapter", "init");
        mqtt::subscribe(FILTER)?;
        log::emit(
            log::Level::Info,
            "sdr433-adapter",
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
            "sdr433-adapter",
            &format!("unexpected bus on_message on subject={subject}"),
        );
        Ok(())
    }

    fn on_mqtt_message(topic: String, payload: Payload) -> Result<(), PluginError> {
        // 1. Parse the envelope. rtl_433 occasionally emits non-JSON
        // packets in odd configurations (e.g. raw hex on a debug
        // topic) — log + drop, don't crash.
        let envelope: serde_json::Value = match serde_json::from_slice(&payload) {
            Ok(v) => v,
            Err(e) => {
                log::emit(
                    log::Level::Warn,
                    "sdr433-adapter",
                    &format!("payload on `{topic}` not valid JSON: {e}"),
                );
                return Ok(());
            }
        };

        // 2. Derive the canonical device id from the envelope.
        // Without `model` rtl_433 doesn't give us anything to key on
        // — log at debug and drop.
        let Some(device_id) = translator::device_id_from_envelope(&envelope) else {
            log::emit(
                log::Level::Debug,
                "sdr433-adapter",
                &format!("envelope on `{topic}` lacks model — skipping"),
            );
            return Ok(());
        };

        // 3. Publish one EntityState per recognised key. Each bus
        // publish is capability-checked against
        // `capabilities.bus.publish` (manifest declares
        // `device.sdr433.>`). The bus-watcher auto-registers the
        // device on first publish (M3 W1.2).
        state_publisher::publish_all(&device_id, &envelope);
        Ok(())
    }
}

iot_plugin_sdk_rust::export_plugin!(Component with_types_in iot_plugin_sdk_rust);
