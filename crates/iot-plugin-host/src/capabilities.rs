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
    }
}
