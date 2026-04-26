//! Manifest-driven capability ACL.
//!
//! Checked on every host call: a plugin's attempt to `bus.publish` on a
//! subject outside its declared `capabilities.bus.publish` allow-list
//! returns `PluginError { code: "capability.denied", ... }`.
//!
//! The rules come straight from the plugin manifest (see
//! [schemas/plugin-manifest.schema.json]) and are parsed into this struct
//! at install time.

use serde::Deserialize;

/// All declared capabilities for a single plugin instance.
///
/// Default is empty — a plugin with no manifest has no ability to touch the
/// host for anything other than [`log`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CapabilityMap {
    #[serde(default)]
    pub bus: BusCapabilities,
    #[serde(default)]
    pub mqtt: MqttCapabilities,
    #[serde(default)]
    pub net: NetCapabilities,
    /// Registry capability — kept on the deserializer for manifest
    /// backward compatibility, but the host no longer offers a
    /// matching WIT import. ABI 1.3.0 (M5a W1) removed
    /// `registry::upsert-device`; the iot-registry bus-watcher
    /// auto-registers devices from `device.>` publishes instead.
    #[serde(default)]
    pub registry: RegistryCapabilities,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct BusCapabilities {
    #[serde(default)]
    pub publish: Vec<String>,
    #[serde(default)]
    pub subscribe: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MqttCapabilities {
    #[serde(default)]
    pub publish: Vec<String>,
    #[serde(default)]
    pub subscribe: Vec<String>,
    #[serde(default)]
    pub bridge: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NetCapabilities {
    #[serde(default)]
    pub outbound: Vec<String>,
}

/// Registry access — historically gated `registry::upsert-device`.
///
/// Gone in ABI 1.3.0; `list` was never implemented. Kept around
/// only so old manifests parse without error; the field has no
/// runtime effect. New manifests should omit it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RegistryCapabilities {
    #[serde(default)]
    pub upsert: bool,
    #[serde(default)]
    pub list: bool,
}

/// Host-side cap check errors. Wire-compatible with
/// `iot.plugin-host.types.plugin-error`.
#[derive(Debug)]
pub struct Denied {
    pub code: &'static str,
    pub message: String,
}

impl CapabilityMap {
    /// Check a subject against the `bus.publish` allow-list. A declaration
    /// matches if it equals the subject, or if it ends in `>` and the subject
    /// starts with the declaration's prefix (NATS wildcard semantics).
    ///
    /// # Errors
    /// Returns [`Denied`] if the subject isn't covered.
    pub fn check_bus_publish(&self, subject: &str) -> Result<(), Denied> {
        if self.bus.publish.iter().any(|p| matches_subject(p, subject)) {
            return Ok(());
        }
        Err(Denied {
            code: "capability.denied",
            message: format!("bus.publish on `{subject}` not in manifest allow-list"),
        })
    }

    /// Check an MQTT topic against the `mqtt.publish` allow-list.
    /// Uses MQTT wildcard semantics (`+` single-level, `#` multi-level-suffix).
    ///
    /// # Errors
    /// Returns [`Denied`] if the topic isn't covered.
    pub fn check_mqtt_publish(&self, topic: &str) -> Result<(), Denied> {
        if self
            .mqtt
            .publish
            .iter()
            .any(|p| matches_mqtt_topic(p, topic))
        {
            return Ok(());
        }
        Err(Denied {
            code: "capability.denied",
            message: format!("mqtt.publish on `{topic}` not in manifest allow-list"),
        })
    }

    /// Check an outbound HTTP URL against the `net.outbound` allow-list.
    ///
    /// Matching rule: the request URL must *start with* one of the manifest's
    /// declared `net.outbound` entries (byte-prefix). Plugins typically declare
    /// e.g. `https://api.open-meteo.com/`, which then permits
    /// `https://api.open-meteo.com/v1/forecast?...` but rejects
    /// `https://api.open-meteo.com.evil.example/`. Plugins that want to talk to
    /// a specific host root should include the trailing `/`.
    ///
    /// Added in ABI 1.4.0 along with the `net::http` host import.
    ///
    /// # Errors
    /// Returns [`Denied`] if `url` isn't covered by an allow-list entry, or if
    /// the manifest declares no `net.outbound` prefixes at all.
    pub fn check_net_outbound(&self, url: &str) -> Result<(), Denied> {
        if self
            .net
            .outbound
            .iter()
            .any(|prefix| url.starts_with(prefix))
        {
            return Ok(());
        }
        Err(Denied {
            code: "capability.denied",
            message: format!("net.outbound to `{url}` not in manifest allow-list"),
        })
    }

    /// Check an MQTT topic filter against the `mqtt.subscribe` allow-list.
    /// The *plugin-requested* filter must itself match one of the manifest-
    /// declared filters under MQTT wildcard semantics — i.e. the plugin can
    /// only narrow, never broaden. (Declaring `sensors/+/temperature` in the
    /// manifest does not entitle the plugin to subscribe to `sensors/#`.)
    ///
    /// # Errors
    /// Returns [`Denied`] if `filter` isn't covered by an allowed filter.
    pub fn check_mqtt_subscribe(&self, filter: &str) -> Result<(), Denied> {
        if self
            .mqtt
            .subscribe
            .iter()
            .any(|p| mqtt_filter_covers(p, filter))
        {
            return Ok(());
        }
        Err(Denied {
            code: "capability.denied",
            message: format!("mqtt.subscribe on `{filter}` not in manifest allow-list"),
        })
    }
}

/// Naive NATS-style match. `foo.>` matches `foo.anything.deep`; `foo.bar`
/// matches exactly `foo.bar`. No `*` single-token wildcards yet — M2 W2
/// will replace this with the real matcher from iot-proto's subject parser.
fn matches_subject(pattern: &str, subject: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('>') {
        subject.starts_with(prefix)
    } else {
        pattern == subject
    }
}

/// MQTT topic matcher. `pattern` uses MQTT 3.1.1 wildcard rules:
/// `+` matches exactly one level; `#` (only as the last segment) matches
/// any remaining suffix; exact segments must match byte-for-byte.
///
/// Used by `check_mqtt_publish` ("does any allow-listed pattern cover
/// the topic the plugin wants to publish on?"). `mqtt_filter_covers`
/// below handles the subscribe-side narrowing check.
///
/// Also reused by the [`crate::mqtt::MqttRouter`] to decide whether a
/// plugin-registered filter matches an inbound broker message.
pub fn matches_mqtt_topic(pattern: &str, topic: &str) -> bool {
    let mut pat = pattern.split('/');
    let mut top = topic.split('/');
    loop {
        match (pat.next(), top.next()) {
            (Some("#"), _) | (None, None) => return true,
            (Some(p), Some(t)) if p == "+" || p == t => {}
            _ => return false,
        }
    }
}

/// Does manifest-declared filter `allowed` cover plugin-requested filter
/// `requested`? i.e. is the topic set matched by `requested` a subset of
/// the topic set matched by `allowed`?
///
/// The plugin is only allowed to *narrow* its subscription. Declaring
/// `sensors/+/temperature` in the manifest does NOT entitle the plugin
/// to request `sensors/#` (which would also match `sensors/a/b/c`).
///
/// Rules, segment-by-segment:
///   * `allowed[i] = "#"` — matches any suffix; `requested` is covered
///     regardless of what's from position `i` onwards.
///   * `allowed[i] = "+"` — must align with a segment in `requested`
///     that is NOT `#` (that would broaden).
///   * `allowed[i] = literal` — `requested[i]` must be the same literal.
///
/// After consuming all of `allowed` without hitting `#`: `requested`
/// must have the same length (otherwise it matches extra segments
/// `allowed` doesn't).
fn mqtt_filter_covers(allowed: &str, requested: &str) -> bool {
    if allowed == requested {
        return true;
    }
    let a_segs: Vec<&str> = allowed.split('/').collect();
    let r_segs: Vec<&str> = requested.split('/').collect();

    for (i, a) in a_segs.iter().enumerate() {
        match *a {
            "#" => return true,
            "+" => match r_segs.get(i) {
                None | Some(&"#") => return false,
                Some(_) => {}
            },
            lit => match r_segs.get(i) {
                Some(&s) if s == lit => {}
                _ => return false,
            },
        }
    }
    r_segs.len() == a_segs.len()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn trailing_wildcard_matches_any_suffix() {
        let m = CapabilityMap {
            bus: BusCapabilities {
                publish: vec!["device.demo-echo.>".into()],
                subscribe: Vec::new(),
            },
            ..Default::default()
        };
        assert!(m
            .check_bus_publish("device.demo-echo.01hxx.temp.state")
            .is_ok());
        assert!(m
            .check_bus_publish("device.other.01hxx.temp.state")
            .is_err());
    }

    #[test]
    fn exact_match() {
        let m = CapabilityMap {
            bus: BusCapabilities {
                publish: vec!["sys.health".into()],
                subscribe: Vec::new(),
            },
            ..Default::default()
        };
        assert!(m.check_bus_publish("sys.health").is_ok());
        assert!(m.check_bus_publish("sys.health.x").is_err());
    }

    #[test]
    fn empty_denies_everything() {
        let m = CapabilityMap::default();
        assert!(m.check_bus_publish("anything").is_err());
        assert!(m.check_mqtt_publish("any/topic").is_err());
        assert!(m.check_mqtt_subscribe("any/topic").is_err());
    }

    // ---------------------------------------------------------- MQTT tests

    fn mqtt_only(publish: Vec<&str>, subscribe: Vec<&str>) -> CapabilityMap {
        CapabilityMap {
            mqtt: MqttCapabilities {
                publish: publish.into_iter().map(String::from).collect(),
                subscribe: subscribe.into_iter().map(String::from).collect(),
                bridge: Vec::new(),
            },
            ..Default::default()
        }
    }

    #[test]
    fn mqtt_publish_exact_match_allowed() {
        let m = mqtt_only(vec!["sensors/kitchen/temp"], vec![]);
        assert!(m.check_mqtt_publish("sensors/kitchen/temp").is_ok());
        assert!(m.check_mqtt_publish("sensors/kitchen/humid").is_err());
    }

    #[test]
    fn mqtt_publish_plus_wildcard_matches_single_level() {
        let m = mqtt_only(vec!["sensors/+/temp"], vec![]);
        assert!(m.check_mqtt_publish("sensors/kitchen/temp").is_ok());
        assert!(m.check_mqtt_publish("sensors/livingroom/temp").is_ok());
        // `+` covers exactly one level, never nests.
        assert!(m.check_mqtt_publish("sensors/room/sub/temp").is_err());
        // Nor does it match zero levels.
        assert!(m.check_mqtt_publish("sensors/temp").is_err());
    }

    #[test]
    fn mqtt_publish_hash_wildcard_matches_any_suffix() {
        let m = mqtt_only(vec!["zigbee2mqtt/#"], vec![]);
        assert!(m.check_mqtt_publish("zigbee2mqtt/kitchen-temp").is_ok());
        assert!(m
            .check_mqtt_publish("zigbee2mqtt/devices/ieee/state")
            .is_ok());
        assert!(m.check_mqtt_publish("other/topic").is_err());
    }

    #[test]
    fn mqtt_subscribe_narrowing_allowed_broadening_denied() {
        // Manifest allows a narrow slice.
        let m = mqtt_only(vec![], vec!["sensors/+/temp"]);
        // Plugin asks for exactly the same slice: OK.
        assert!(m.check_mqtt_subscribe("sensors/+/temp").is_ok());
        // Plugin narrows further to one room: OK (subset).
        assert!(m.check_mqtt_subscribe("sensors/kitchen/temp").is_ok());
        // Plugin tries to broaden to everything under sensors/: DENIED.
        assert!(m.check_mqtt_subscribe("sensors/#").is_err());
        // Plugin tries a completely different root: DENIED.
        assert!(m.check_mqtt_subscribe("actuators/+/state").is_err());
    }

    // --------------------------------------------------- mqtt subscribe tests

    #[test]
    fn mqtt_subscribe_hash_allow_covers_everything_under_prefix() {
        let m = mqtt_only(vec![], vec!["zigbee2mqtt/#"]);
        assert!(m.check_mqtt_subscribe("zigbee2mqtt/+").is_ok());
        assert!(m.check_mqtt_subscribe("zigbee2mqtt/kitchen-temp").is_ok());
        assert!(m.check_mqtt_subscribe("zigbee2mqtt/#").is_ok());
        // Different prefix: denied.
        assert!(m.check_mqtt_subscribe("actuators/#").is_err());
    }

    // -------------------------------------------------- net.outbound tests

    fn net_only(outbound: Vec<&str>) -> CapabilityMap {
        CapabilityMap {
            net: NetCapabilities {
                outbound: outbound.into_iter().map(String::from).collect(),
            },
            ..Default::default()
        }
    }

    #[test]
    fn net_outbound_allows_declared_prefix() {
        let m = net_only(vec!["https://api.open-meteo.com/"]);
        // Exact prefix + a path/query underneath: allowed.
        assert!(m
            .check_net_outbound("https://api.open-meteo.com/v1/forecast?lat=1&lon=2")
            .is_ok());
    }

    #[test]
    fn net_outbound_denies_other_hosts() {
        let m = net_only(vec!["https://api.open-meteo.com/"]);
        assert!(m.check_net_outbound("https://evil.example/").is_err());
    }

    #[test]
    fn net_outbound_denies_subdomain_lookalike() {
        // The trailing `/` in the declared prefix is what stops a host
        // suffix attack like `api.open-meteo.com.attacker.tld`.
        let m = net_only(vec!["https://api.open-meteo.com/"]);
        assert!(m
            .check_net_outbound("https://api.open-meteo.com.attacker.tld/v1/forecast")
            .is_err());
    }

    #[test]
    fn net_outbound_denies_scheme_downgrade() {
        // Prefix is byte-literal — declaring https:// does not also
        // permit http://.
        let m = net_only(vec!["https://api.open-meteo.com/"]);
        assert!(m
            .check_net_outbound("http://api.open-meteo.com/v1/forecast")
            .is_err());
    }

    #[test]
    fn net_outbound_empty_denies_everything() {
        let m = CapabilityMap::default();
        assert!(m.check_net_outbound("https://example.com/").is_err());
    }

    #[test]
    fn net_outbound_multiple_prefixes_any_match() {
        let m = net_only(vec![
            "https://api.open-meteo.com/",
            "https://api.tibber.com/v1-beta/",
        ]);
        assert!(m
            .check_net_outbound("https://api.open-meteo.com/v1/forecast")
            .is_ok());
        assert!(m
            .check_net_outbound("https://api.tibber.com/v1-beta/gql")
            .is_ok());
        assert!(m.check_net_outbound("https://api.tibber.com/v2/").is_err());
    }
}
