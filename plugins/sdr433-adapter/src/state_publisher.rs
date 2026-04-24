//! Bus publisher for per-entity rtl_433 state updates.
//!
//! Given a parsed rtl_433 envelope and the canonical device id derived
//! from it, emit one wire-compatible `iot.device.v1.EntityState`
//! protobuf per recognised key on
//! `device.sdr433.<device_id>.<key>.state` via the plugin host's
//! `bus::publish` capability.
//!
//! Closely modelled on the z2m adapter's state_publisher — same
//! Protobuf shape, same JSON→prost-Value translation. Differences:
//! the subject prefix is `device.sdr433.` (not `device.zigbee2mqtt.`);
//! the device id is built by [`crate::translator::device_id_from_envelope`]
//! from the JSON envelope rather than parsed from the MQTT topic; and
//! the entity-key catalog is rtl_433-specific (see translator).
//!
//! Bus publishes are capability-checked against `bus.publish` in the
//! manifest (`device.sdr433.>`). The iot-registry bus-watcher (M3
//! W1.2) auto-registers each `(sdr433, <device_id>)` pair on first
//! publish, so the plugin doesn't need a `registry::upsert` host call
//! (gone in ABI 1.3.0 / M5a W1).

use iot_plugin_sdk_rust::iot::plugin_host::{bus, log};
use iot_proto_core::iot::device::v1::EntityState;
use iot_proto_core::Ulid;
use prost::Message as _;

use crate::translator::known_entity_keys;

/// Fully-qualified Protobuf type name used in the `iot-type` bus
/// header. Matches the iot-proto package on the decoder side.
const ENTITY_STATE_TYPE: &str = "iot.device.v1.EntityState";

/// Publish one EntityState per recognised key in the envelope.
///
/// `device_id` is the canonical id from the translator (already
/// lowercased and NATS-safe). `envelope` is the parsed JSON object
/// rtl_433 emitted; only keys recognised by the translator catalog
/// produce a publish — decoder-housekeeping fields (`time`, `mic`,
/// `subtype`, …) are silently dropped.
pub fn publish_all(device_id: &str, envelope: &serde_json::Value) {
    let Some(obj) = envelope.as_object() else {
        return;
    };

    for key in known_entity_keys(obj.keys().map(String::as_str)) {
        let Some(value) = obj.get(&key) else { continue };
        let subject = format!("device.sdr433.{device_id}.{key}.state");

        let state = EntityState {
            device_id: Some(Ulid {
                value: device_id.to_owned(),
            }),
            entity_id: Some(Ulid {
                value: format!("{device_id}::{key}"),
            }),
            value: Some(json_to_prost(value.clone())),
            at: Some(current_timestamp()),
            schema_version: 1,
        };

        let bytes = state.encode_to_vec();
        match bus::publish(&subject, ENTITY_STATE_TYPE, &bytes) {
            Ok(()) => {}
            Err(e) => log::emit(
                log::Level::Warn,
                "sdr433-adapter",
                &format!("bus.publish failed on {subject}: {}: {}", e.code, e.message),
            ),
        }
    }
}

/// Convert a `serde_json::Value` into a `prost_types::Value`.
///
/// Protobuf's `google.protobuf.Value` is a tagged union that matches
/// JSON's shape 1:1, so this is a straight structural translation.
fn json_to_prost(v: serde_json::Value) -> prost_types::Value {
    use prost_types::value::Kind;
    let kind = match v {
        serde_json::Value::Null => Kind::NullValue(0),
        serde_json::Value::Bool(b) => Kind::BoolValue(b),
        serde_json::Value::Number(n) => Kind::NumberValue(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => Kind::StringValue(s),
        serde_json::Value::Array(arr) => Kind::ListValue(prost_types::ListValue {
            values: arr.into_iter().map(json_to_prost).collect(),
        }),
        serde_json::Value::Object(obj) => Kind::StructValue(prost_types::Struct {
            fields: obj
                .into_iter()
                .map(|(k, v)| (k, json_to_prost(v)))
                .collect(),
        }),
    };
    prost_types::Value { kind: Some(kind) }
}

/// Current wall-clock time as a `prost_types::Timestamp`. Uses
/// `SystemTime::now()` which is available on wasm32-wasip2 via the
/// wasi:clocks interface (the preview adapter wires it up for us).
fn current_timestamp() -> prost_types::Timestamp {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    prost_types::Timestamp {
        seconds: i64::try_from(now.as_secs()).unwrap_or(0),
        nanos: i32::try_from(now.subsec_nanos()).unwrap_or(0),
    }
}
