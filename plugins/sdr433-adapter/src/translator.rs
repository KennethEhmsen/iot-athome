//! rtl_433 JSON envelope → canonical-device translation.
//!
//! rtl_433 (`-F mqtt://broker:1883,events`) publishes one JSON object
//! per decoded RF packet onto the configured topic (default
//! `rtl_433/<host>/events`, but operators often shorten to
//! `rtl_433/events`). Every envelope carries `model` + `id`
//! identifying the device; what other fields appear depends entirely
//! on the rtl_433 decoder (~200 device families supported).
//!
//! This translator handles the six device shapes the M5 plan called
//! out as the "most common" home-automation set:
//!
//! 1. Temperature/humidity sensors (Acurite, Oregon, …):
//!    `temperature_C`, `humidity`, `battery_ok`.
//! 2. Door/window contacts (Honeywell, Visonic): `state` /
//!    `contact_open`.
//! 3. TPMS tyre-pressure sensors: `pressure_PSI` / `pressure_kPa`,
//!    `temperature_C`.
//! 4. Rain gauges (Acurite, Oregon): `rain_mm` / `rain_in`.
//! 5. Energy / power monitors (OWL, Efergy): `power_W`, `energy_kWh`.
//! 6. Water-meter pulse counters (custom + commercial pulse bridges):
//!    `count` / `water_consumption`.
//!
//! Pure data + pure functions — no protobuf, no tokio. Unit-testable
//! without any host bindings.

use serde_json::Value;

/// One recognised rtl_433 envelope key + the canonical entity
/// metadata we attach to it. Mirrors the z2m adapter's `EntitySpec`
/// shape so both plugins emit consistent EntityState payloads.
///
/// Fields are `pub` so future callers can read them; the current
/// plugin body only uses `entity_spec()`'s Some/None to filter.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct EntitySpec {
    /// Canonical type tag (`sensor.temperature`, `binary_sensor.contact`,
    /// etc.). Aligned with HA's component naming.
    pub type_: &'static str,
    /// SI/customary unit string (`C`, `%`, `kPa`, `mm`, `W`, …).
    pub unit: &'static str,
    /// HA-style device_class hint for the panel UI.
    pub device_class: &'static str,
    /// Capability id: how rules / panel filter on the entity.
    pub capability: &'static str,
}

/// Map an rtl_433 envelope key to its canonical entity. `None` means
/// "noisy field, don't publish" (e.g. `time`, `mic`, `subtype`).
///
/// rtl_433's key naming has no central catalog — fields are decoder-
/// emitted with whatever convention the decoder chose. The mapping
/// below covers the six common-shape keys; adding decoders is just
/// adding match arms.
#[must_use]
pub fn entity_spec(key: &str) -> Option<EntitySpec> {
    Some(match key {
        // 1. Temperature / humidity ------------------------------------
        "temperature_C" => EntitySpec {
            type_: "sensor.temperature",
            unit: "C",
            device_class: "temperature",
            capability: "temperature",
        },
        "temperature_F" => EntitySpec {
            type_: "sensor.temperature",
            unit: "F",
            device_class: "temperature",
            capability: "temperature_f",
        },
        "humidity" => EntitySpec {
            type_: "sensor.humidity",
            unit: "%",
            device_class: "humidity",
            capability: "humidity",
        },

        // 2. Door / window contact -------------------------------------
        // rtl_433 surfaces contact in two flavours depending on the
        // decoder: `state` ("open"/"closed") or `contact_open` (0/1).
        // Both map to the same canonical entity.
        "state" | "contact_open" => EntitySpec {
            type_: "binary_sensor.contact",
            unit: "",
            device_class: "door",
            capability: "contact",
        },

        // 3. TPMS pressure ---------------------------------------------
        "pressure_PSI" => EntitySpec {
            type_: "sensor.pressure",
            unit: "psi",
            device_class: "pressure",
            capability: "pressure_psi",
        },
        "pressure_kPa" => EntitySpec {
            type_: "sensor.pressure",
            unit: "kPa",
            device_class: "pressure",
            capability: "pressure_kpa",
        },

        // 4. Rain gauges -----------------------------------------------
        "rain_mm" => EntitySpec {
            type_: "sensor.rainfall",
            unit: "mm",
            device_class: "precipitation",
            capability: "rainfall_mm",
        },
        "rain_in" => EntitySpec {
            type_: "sensor.rainfall",
            unit: "in",
            device_class: "precipitation",
            capability: "rainfall_in",
        },

        // 5. Energy / power monitors -----------------------------------
        "power_W" => EntitySpec {
            type_: "sensor.power",
            unit: "W",
            device_class: "power",
            capability: "power",
        },
        "energy_kWh" => EntitySpec {
            type_: "sensor.energy",
            unit: "kWh",
            device_class: "energy",
            capability: "energy",
        },

        // 6. Water-meter pulses + generic count ------------------------
        // Various decoders surface their counter as `count`; the
        // dedicated water-meter bridges sometimes use the more
        // explicit `water_consumption` key (litres total).
        "count" => EntitySpec {
            type_: "sensor.pulse_count",
            unit: "pulses",
            device_class: "energy",
            capability: "pulse_count",
        },
        "water_consumption" => EntitySpec {
            type_: "sensor.water_total",
            unit: "L",
            device_class: "water",
            capability: "water_total",
        },

        // Cross-cutting battery indicator (every battery-powered
        // device family includes it). Treated as a binary_sensor —
        // 1=ok, 0=needs replacement.
        "battery_ok" => EntitySpec {
            type_: "binary_sensor.battery",
            unit: "",
            device_class: "battery",
            capability: "battery_ok",
        },

        _ => return None,
    })
}

/// Filter an iterator of JSON keys down to those in our catalog. Used
/// by the state publisher to decide which keys are worth emitting (the
/// rest are decoder housekeeping like `time`, `mic`, `subtype`,
/// `protocol`).
pub fn known_entity_keys<'a, I: IntoIterator<Item = &'a str>>(keys: I) -> Vec<String> {
    keys.into_iter()
        .filter(|k| entity_spec(k).is_some())
        .map(ToOwned::to_owned)
        .collect()
}

/// Build the canonical device id from an rtl_433 envelope.
///
/// Format: `<model>-<id>` lowercased + NATS-safe; if `channel` is
/// present it's appended as `-<channel>`. Examples:
///
/// * `Acurite-Tower` + id `13245` + channel `A` → `acurite-tower-13245-a`
/// * `Honeywell-ActivLink` + id `98765` → `honeywell-activlink-98765`
///
/// Returns `None` if the envelope lacks `model` (unrecoverable —
/// no other field uniquely identifies the device).
#[must_use]
pub fn device_id_from_envelope(envelope: &Value) -> Option<String> {
    let obj = envelope.as_object()?;

    let model = obj.get("model").and_then(Value::as_str)?;
    let id_part = obj.get("id").map(format_json_id).unwrap_or_default();

    let mut out = if id_part.is_empty() {
        sanitize(model)
    } else {
        format!("{}-{}", sanitize(model), sanitize(&id_part))
    };
    if let Some(channel) = obj.get("channel").map(format_json_id) {
        if !channel.is_empty() {
            out.push('-');
            out.push_str(&sanitize(&channel));
        }
    }
    Some(out)
}

/// Stringify `id` / `channel` whether rtl_433 emitted them as a
/// number, a string, or a bool.
fn format_json_id(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

/// NATS-safe transform: lowercase + replace anything outside
/// `[a-z0-9-_]` with `_`. NATS subject tokens cannot contain `.`,
/// space, `*`, `>`, etc., so the replacement is conservative rather
/// than spec-exact (the host-level subject builder validates the
/// final string anyway).
#[must_use]
pub fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            let lc = c.to_ascii_lowercase();
            if lc.is_ascii_alphanumeric() || lc == '-' || lc == '_' {
                lc
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---------------------------------------------------------- catalog

    #[test]
    fn entity_spec_covers_six_device_shapes() {
        // Temperature/humidity (shape 1).
        assert_eq!(entity_spec("temperature_C").unwrap().unit, "C");
        assert_eq!(entity_spec("humidity").unwrap().unit, "%");
        // Door/window contact (shape 2).
        assert_eq!(entity_spec("state").unwrap().capability, "contact");
        assert_eq!(entity_spec("contact_open").unwrap().capability, "contact");
        // TPMS (shape 3).
        assert_eq!(entity_spec("pressure_PSI").unwrap().unit, "psi");
        assert_eq!(entity_spec("pressure_kPa").unwrap().unit, "kPa");
        // Rain (shape 4).
        assert_eq!(entity_spec("rain_mm").unwrap().unit, "mm");
        // Power/energy (shape 5).
        assert_eq!(entity_spec("power_W").unwrap().unit, "W");
        assert_eq!(entity_spec("energy_kWh").unwrap().unit, "kWh");
        // Water-meter / pulse counter (shape 6).
        assert_eq!(entity_spec("count").unwrap().capability, "pulse_count");
        assert_eq!(
            entity_spec("water_consumption").unwrap().capability,
            "water_total"
        );
        // Cross-cutting battery indicator.
        assert_eq!(entity_spec("battery_ok").unwrap().device_class, "battery");
    }

    #[test]
    fn entity_spec_skips_decoder_housekeeping() {
        for noise in [
            "time", "mic", "subtype", "protocol", "model", "id", "channel", "msg_type",
        ] {
            assert!(
                entity_spec(noise).is_none(),
                "unexpectedly accepted: {noise}"
            );
        }
    }

    // ---------------------------------------------------------- device id

    #[test]
    fn device_id_acurite_temp() {
        let env = json!({
            "model": "Acurite-Tower",
            "id": 13245,
            "channel": "A",
            "battery_ok": 1,
            "temperature_C": 21.4,
            "humidity": 52,
        });
        assert_eq!(
            device_id_from_envelope(&env).unwrap(),
            "acurite-tower-13245-a"
        );
    }

    #[test]
    fn device_id_honeywell_contact_no_channel() {
        let env = json!({
            "model": "Honeywell-ActivLink",
            "id": 98765,
            "state": "open",
        });
        assert_eq!(
            device_id_from_envelope(&env).unwrap(),
            "honeywell-activlink-98765"
        );
    }

    #[test]
    fn device_id_tpms_string_id() {
        let env = json!({
            "model": "Tyre-Eagle",
            "id": "abc123",
            "pressure_PSI": 32.5,
        });
        assert_eq!(device_id_from_envelope(&env).unwrap(), "tyre-eagle-abc123");
    }

    #[test]
    fn device_id_handles_dots_and_slashes() {
        // A decoder model name that contains a dot would corrupt the
        // NATS subject — sanitize replaces it with `_`.
        let env = json!({
            "model": "Custom.Model/v2",
            "id": 1,
        });
        let id = device_id_from_envelope(&env).unwrap();
        assert!(!id.contains('.'));
        assert!(!id.contains('/'));
        assert_eq!(id, "custom_model_v2-1");
    }

    #[test]
    fn device_id_requires_model() {
        // Without `model` the envelope can't be uniquely identified.
        let env = json!({"id": 1, "temperature_C": 20.0});
        assert!(device_id_from_envelope(&env).is_none());
    }

    // ---------------------------------------------------------- known keys

    #[test]
    fn known_keys_filter_envelope_noise() {
        let env = json!({
            "time": "2026-04-24 14:32:11",
            "model": "Acurite-Tower",
            "id": 13245,
            "channel": "A",
            "battery_ok": 1,
            "temperature_C": 21.4,
            "humidity": 52,
            "mic": "CHECKSUM",
        });
        let keys: Vec<&str> = env
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let mut known = known_entity_keys(keys);
        known.sort();
        // None of `time`, `model`, `id`, `channel`, `mic` should appear.
        // Sorted comparison since serde_json::Map iterates alphabetic.
        assert_eq!(known, vec!["battery_ok", "humidity", "temperature_C"]);
    }

    // ---------------------------------------------------------- 6 shapes

    /// Helper for the per-shape happy-path tests — a single-shape
    /// envelope, the device id we expect, and the entity keys we
    /// expect to publish for it.
    ///
    /// Compares the known-keys set sorted, since `serde_json::Map`
    /// is `BTreeMap`-backed and key iteration is alphabetic; the
    /// state publisher doesn't depend on iteration order anyway.
    fn check_shape(env: Value, want_id: &str, want_keys: &[&str]) {
        let id = device_id_from_envelope(&env).expect("device id");
        assert_eq!(id, want_id, "device id for {env}");
        let keys: Vec<&str> = env
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let mut known = known_entity_keys(keys);
        known.sort();
        let mut want: Vec<String> = want_keys.iter().map(|s| (*s).to_owned()).collect();
        want.sort();
        assert_eq!(known, want, "known keys for {env}");
    }

    #[test]
    fn shape_1_temp_humidity_acurite() {
        check_shape(
            json!({
                "time": "...",
                "model": "Acurite-Tower",
                "id": 13245,
                "channel": "A",
                "battery_ok": 1,
                "temperature_C": 21.4,
                "humidity": 52,
            }),
            "acurite-tower-13245-a",
            &["battery_ok", "temperature_C", "humidity"],
        );
    }

    #[test]
    fn shape_2_door_contact_honeywell() {
        check_shape(
            json!({
                "time": "...",
                "model": "Honeywell-ActivLink",
                "id": 98765,
                "state": "open",
                "battery_ok": 1,
            }),
            "honeywell-activlink-98765",
            &["state", "battery_ok"],
        );
    }

    #[test]
    fn shape_3_tpms_pressure() {
        check_shape(
            json!({
                "time": "...",
                "model": "Tyre-Eagle",
                "type": "TPMS",
                "id": "abc123",
                "pressure_PSI": 32.5,
                "temperature_C": 20.0,
                "battery_ok": 1,
            }),
            "tyre-eagle-abc123",
            &["pressure_PSI", "temperature_C", "battery_ok"],
        );
    }

    #[test]
    fn shape_4_rain_gauge_acurite() {
        check_shape(
            json!({
                "time": "...",
                "model": "Acurite-Rain",
                "id": 4321,
                "battery_ok": 1,
                "rain_in": 1.23,
            }),
            "acurite-rain-4321",
            &["battery_ok", "rain_in"],
        );
    }

    #[test]
    fn shape_5_energy_owl() {
        check_shape(
            json!({
                "time": "...",
                "model": "OWL-CM180",
                "id": 2345,
                "power_W": 1234,
                "energy_kWh": 56.78,
                "battery_ok": 1,
            }),
            "owl-cm180-2345",
            &["power_W", "energy_kWh", "battery_ok"],
        );
    }

    #[test]
    fn shape_6_water_meter_pulse() {
        check_shape(
            json!({
                "time": "...",
                "model": "Watermeter-Pulse",
                "id": 11111,
                "count": 1234,
                "battery_ok": 1,
            }),
            "watermeter-pulse-11111",
            &["count", "battery_ok"],
        );
    }
}
