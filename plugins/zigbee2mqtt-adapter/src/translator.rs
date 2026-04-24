//! zigbee2mqtt payload → canonical-device translation.
//!
//! Pure data + pure functions — no protobuf, no tokio, no
//! iot-proto. Unit-testable without any host bindings. The two
//! helpers the plugin uses:
//!
//!   * [`friendly_name_from_topic`] — parse `zigbee2mqtt/<fn>[/sub]`
//!     and recover `<fn>`. The friendly name doubles as the device's
//!     external_id; the iot-registry bus-watcher (M3 W1.2) registers
//!     the `(zigbee2mqtt, <fn>)` pair on first publish, retiring the
//!     M2-era explicit `registry::upsert-device` host call (gone in
//!     ABI 1.3.0 / M5a W1).
//!   * [`known_entity_keys`] — filter a JSON object's keys down to the
//!     ones we publish an `EntityState` for (ignoring noisy internals
//!     like `last_seen`, `update.state`, etc.).
//!   * [`entity_catalog`] — per-key metadata describing units / kind
//!     for entities. The registry sees this through the EntityState
//!     payloads, not via the host call path.

/// One recognized zigbee2mqtt payload key and the canonical entity
/// metadata we attach to it.
///
/// Fields are `pub` so future callers (e.g. the panel-facing device
/// catalog command) can read them; the current plugin body only uses
/// `entity_spec()`'s Some/None to filter keys and doesn't read the
/// struct's contents — hence the `#[allow(dead_code)]`.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct EntitySpec {
    pub type_: &'static str,
    pub unit: &'static str,
    pub device_class: &'static str,
    pub capability: &'static str,
}

/// Extract an entity declaration for one known zigbee2mqtt key.
/// `None` = unknown key (logged at trace in the caller, not an error).
#[must_use]
pub fn entity_spec(key: &str) -> Option<EntitySpec> {
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

/// Filter an iterator of JSON keys down to those in our catalog. Used
/// by the state publisher to decide which values are worth emitting
/// (the rest are noisy zigbee2mqtt internals like `last_seen` /
/// `update`).
pub fn known_entity_keys<'a, I: IntoIterator<Item = &'a str>>(keys: I) -> Vec<String> {
    keys.into_iter()
        .filter(|k| entity_spec(k).is_some())
        .map(ToOwned::to_owned)
        .collect()
}

/// Extract the `friendly_name` segment from a topic like
/// `zigbee2mqtt/kitchen-temp` or `zigbee2mqtt/kitchen-temp/availability`.
#[must_use]
pub fn friendly_name_from_topic(topic: &str) -> Option<&str> {
    let rest = topic.strip_prefix("zigbee2mqtt/")?;
    // For `zigbee2mqtt/<fn>/availability` the friendly name is the first seg.
    Some(rest.split('/').next().unwrap_or(rest))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn entity_spec_covers_common_keys() {
        assert_eq!(
            entity_spec("temperature").expect("temp").type_,
            "sensor.temperature"
        );
        assert_eq!(
            entity_spec("motion").expect("motion").device_class,
            "motion"
        );
        assert_eq!(
            entity_spec("occupancy").expect("occupancy").device_class,
            "motion"
        );
        assert!(entity_spec("last_seen").is_none());
    }

    #[test]
    fn known_entity_keys_filters_noise() {
        let keys = [
            "temperature",
            "humidity",
            "last_seen",
            "update_available",
            "battery",
        ];
        let known: Vec<_> = known_entity_keys(keys.iter().copied());
        // Order preserved from the input iterator.
        assert_eq!(known, vec!["temperature", "humidity", "battery"]);
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
