//! Bus publisher for per-entity state updates.
//!
//! Given a parsed zigbee2mqtt payload and the device's canonical ULID,
//! publish one `iot.device.v1.EntityState` message per known entity on
//! `device.zigbee2mqtt.<device_id_lc>.<key>.state`.

use iot_bus::Bus;
use iot_proto::iot::common::v1::Ulid as PbUlid;
use iot_proto::iot::device::v1::EntityState;
use prost::Message as _;
use tracing::{instrument, warn};

use crate::translator::known_entity_keys;

/// Published on `device.<plugin>.<id>.<entity>.state` per recognized key.
#[instrument(skip(bus, payload))]
pub async fn publish_all(
    bus: &Bus,
    device_id_uppercase: &str,
    friendly: &str,
    payload: &serde_json::Value,
) {
    let device_id_lc = device_id_uppercase.to_ascii_lowercase();
    let Some(obj) = payload.as_object() else {
        return;
    };

    for key in known_entity_keys(obj.keys().map(String::as_str)) {
        let Some(value) = obj.get(&key) else { continue };
        let subject = match iot_proto::subjects::device_state("zigbee2mqtt", &device_id_lc, &key) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, device_id_lc, key, "bad subject");
                continue;
            }
        };

        let state = EntityState {
            device_id: Some(PbUlid {
                value: device_id_uppercase.to_owned(),
            }),
            entity_id: Some(PbUlid {
                value: format!("{friendly}::{key}"),
            }),
            value: Some(json_to_prost(value.clone())),
            at: Some(prost_types::Timestamp::from(std::time::SystemTime::now())),
            schema_version: iot_core::DEVICE_SCHEMA_VERSION,
        };

        let bytes = state.encode_to_vec();
        if let Err(e) = bus
            .publish_proto(&subject, "iot.device.v1.EntityState", bytes, None)
            .await
        {
            warn!(error = %e, subject, "bus publish failed");
        }
    }
}

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
