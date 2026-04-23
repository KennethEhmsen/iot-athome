//! Bus publisher for per-entity state updates.
//!
//! Given a parsed zigbee2mqtt payload and the device's canonical ULID,
//! emit one wire-compatible `iot.device.v1.EntityState` message per
//! recognized entity on `device.zigbee2mqtt.<device_id_lc>.<key>.state`
//! via the plugin host's `bus::publish` capability.
//!
//! Port of the M1 native module (`iot_bus::Bus::publish_proto`) onto
//! the WASM plugin SDK. Logic identical; only the emit path changed.

use iot_plugin_sdk_rust::iot::plugin_host::bus;
use iot_plugin_sdk_rust::iot::plugin_host::log;
use iot_proto_core::iot::device::v1::EntityState;
use iot_proto_core::Ulid;
use prost::Message as _;

use crate::translator::known_entity_keys;

/// Fully-qualified Protobuf type name used in the `iot-type` bus
/// header. Matches the real iot-proto package on the decoder side.
const ENTITY_STATE_TYPE: &str = "iot.device.v1.EntityState";

/// Published on `device.<plugin>.<id>.<entity>.state` per recognised key.
pub fn publish_all(device_id_ulid: &str, friendly: &str, payload: &serde_json::Value) {
    let device_id_lc = device_id_ulid.to_ascii_lowercase();
    let Some(obj) = payload.as_object() else {
        return;
    };

    for key in known_entity_keys(obj.keys().map(String::as_str)) {
        let Some(value) = obj.get(&key) else { continue };
        let subject = format!("device.zigbee2mqtt.{device_id_lc}.{key}.state");

        let state = EntityState {
            device_id: Some(Ulid {
                value: device_id_ulid.to_owned(),
            }),
            entity_id: Some(Ulid {
                value: format!("{friendly}::{key}"),
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
                "zigbee2mqtt-adapter",
                &format!(
                    "bus.publish failed on {subject}: {}: {}",
                    e.code, e.message
                ),
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
