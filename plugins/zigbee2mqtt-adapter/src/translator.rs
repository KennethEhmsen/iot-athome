//! zigbee2mqtt payload -> canonical Device translation.
//!
//! Shape zigbee2mqtt emits on `zigbee2mqtt/<friendly_name>`:
//!
//! ```json
//! {"temperature": 21.5, "humidity": 45, "battery": 87, "linkquality": 128}
//! ```
//!
//! We pull a short list of well-known keys into the canonical entity catalog
//! and ignore everything else (logged at trace-level in the caller). The list
//! grows organically as new device classes show up in the field.

use iot_proto::iot::common::v1::Ulid as PbUlid;
use iot_proto::iot::device::v1::{Device, Entity, ReadWrite};
use serde::Deserialize;
use std::collections::BTreeSet;

/// Result of translating a single MQTT message.
#[derive(Debug, Clone)]
pub struct Translated {
    pub external_id: String,
    pub device: Device,
}

/// Parse a payload and build a canonical Device skeleton. The caller merges
/// this with the existing registry record (if any) and drives the upsert.
///
/// # Errors
/// Returns `Err` when the payload is not a JSON object.
pub fn translate(friendly_name: &str, payload_bytes: &[u8]) -> anyhow::Result<Translated> {
    let payload: serde_json::Value = serde_json::from_slice(payload_bytes)?;
    let obj = payload
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("payload is not a JSON object"))?;

    let mut entities = Vec::new();
    let mut capabilities = BTreeSet::new();

    for (key, _) in obj {
        if let Some(spec) = entity_spec(key) {
            capabilities.insert(spec.capability.to_owned());
            entities.push(Entity {
                id: Some(PbUlid {
                    value: format!("{friendly_name}::{key}"),
                }),
                r#type: spec.type_.into(),
                unit: spec.unit.into(),
                rw: ReadWrite::Read.into(),
                device_class: spec.device_class.into(),
                meta: std::collections::HashMap::default(),
            });
        }
    }

    let device = Device {
        id: None,
        integration: "zigbee2mqtt".into(),
        external_id: friendly_name.into(),
        manufacturer: String::new(),
        model: String::new(),
        label: friendly_name.into(),
        capabilities: capabilities.into_iter().collect(),
        entities,
        rooms: Vec::new(),
        trust_level: iot_proto::iot::device::v1::TrustLevel::UserAdded.into(),
        schema_version: iot_core::DEVICE_SCHEMA_VERSION,
        plugin_meta: std::collections::HashMap::default(),
        last_seen: None,
    };

    Ok(Translated {
        external_id: friendly_name.to_owned(),
        device,
    })
}

/// Extract an entity declaration for one known zigbee2mqtt key.
struct EntitySpec {
    type_: &'static str,
    unit: &'static str,
    device_class: &'static str,
    capability: &'static str,
}

#[allow(clippy::match_same_arms)]
fn entity_spec(key: &str) -> Option<EntitySpec> {
    Some(match key {
        "temperature" => EntitySpec {
            type_: "sensor.temperature",
            unit: "C",
            device_class: "temperature",
            capability: "temperature",
        },
        "humidity" => EntitySpec {
            type_: "sensor.humidity",
            unit: "%",
            device_class: "humidity",
            capability: "humidity",
        },
        "pressure" => EntitySpec {
            type_: "sensor.pressure",
            unit: "hPa",
            device_class: "pressure",
            capability: "pressure",
        },
        "battery" => EntitySpec {
            type_: "sensor.battery",
            unit: "%",
            device_class: "battery",
            capability: "battery",
        },
        "linkquality" => EntitySpec {
            type_: "sensor.link_quality",
            unit: "",
            device_class: "signal_strength",
            capability: "link_quality",
        },
        "occupancy" | "motion" => EntitySpec {
            type_: "binary_sensor.motion",
            unit: "",
            device_class: "motion",
            capability: "motion",
        },
        "contact" => EntitySpec {
            type_: "binary_sensor.contact",
            unit: "",
            device_class: "door",
            capability: "contact",
        },
        "action" => EntitySpec {
            type_: "event.button",
            unit: "",
            device_class: "button",
            capability: "button",
        },
        _ => return None,
    })
}

/// Extract the `friendly_name` segment from a topic like
/// `zigbee2mqtt/kitchen-temp` or `zigbee2mqtt/kitchen-temp/availability`.
#[must_use]
pub fn friendly_name_from_topic(topic: &str) -> Option<&str> {
    let rest = topic.strip_prefix("zigbee2mqtt/")?;
    // For `zigbee2mqtt/<fn>/availability` the friendly name is the first seg.
    Some(rest.split('/').next().unwrap_or(rest))
}

// Re-export serde's Deserialize so downstream tests can read payloads.
#[allow(dead_code)]
#[derive(Deserialize)]
struct _PayloadProbe(serde_json::Value);

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn translates_temp_humidity() {
        let t = translate(
            "kitchen-temp",
            br#"{"temperature": 21.5, "humidity": 45, "battery": 87, "linkquality": 128}"#,
        )
        .expect("translate");
        assert_eq!(t.external_id, "kitchen-temp");
        assert_eq!(t.device.integration, "zigbee2mqtt");
        let types: Vec<_> = t
            .device
            .entities
            .iter()
            .map(|e| e.r#type.as_str())
            .collect();
        assert!(types.contains(&"sensor.temperature"));
        assert!(types.contains(&"sensor.humidity"));
        assert!(types.contains(&"sensor.battery"));
        assert!(types.contains(&"sensor.link_quality"));
        assert_eq!(t.device.capabilities.len(), 4);
    }

    #[test]
    fn ignores_unknown_keys() {
        let t = translate(
            "node-42",
            br#"{"update": {"state": "idle"}, "temperature": 20.0}"#,
        )
        .expect("translate");
        assert_eq!(t.device.entities.len(), 1, "only temperature is mapped");
    }

    #[test]
    fn rejects_non_object_payload() {
        assert!(translate("x", br#""bare string""#).is_err());
    }

    #[test]
    fn friendly_name_parsing() {
        assert_eq!(
            friendly_name_from_topic("zigbee2mqtt/kitchen-temp"),
            Some("kitchen-temp")
        );
        assert_eq!(
            friendly_name_from_topic("zigbee2mqtt/kitchen-temp/availability"),
            Some("kitchen-temp")
        );
        assert_eq!(friendly_name_from_topic("homeassistant/foo"), None);
    }
}
