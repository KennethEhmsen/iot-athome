//! Registry-side bus watcher (M3 W1.2).
//!
//! Subscribes to the `device.>` subject tree and keeps the registry in
//! sync with what's actually alive on the bus:
//!
//!   * Known devices (looked up by ULID, case-insensitive) get
//!     `last_seen` bumped.
//!   * Known devices (looked up by `(integration, external_id)`) also
//!     get `last_seen` bumped — this is the path that lights up when
//!     new adapters publish with their native id in the subject
//!     instead of a pre-minted ULID.
//!   * Unknown `(integration, device_token)` pairs get auto-registered
//!     as a new device. This is the mechanism that lets
//!     `registry::upsert-device` retire (ADR-0013 §Consequences):
//!     plugins can skip the gRPC upsert and just publish state, and
//!     the registry notices on its own.
//!
//! The subject patterns we care about (ADR-0004):
//!
//!   * `device.<plugin>.<device>.<entity>.state`
//!   * `device.<plugin>.<device>.<entity>.event`
//!   * `device.<plugin>.<device>.avail`
//!
//! Anything else under `device.>` is ignored. Commands (`cmd.>`) are
//! explicitly NOT watched — the registry is a passive observer of
//! state, not a command router.

use anyhow::{Context as _, Result};
use iot_bus::Bus;
use iot_proto::iot::device::v1::{Device, TrustLevel};
use tracing::{debug, info, warn};

use crate::repo::DeviceRepo;

/// Outcome of processing one inbound bus subject.
#[derive(Debug, PartialEq, Eq)]
enum MatchOutcome {
    /// Subject didn't match any device.* shape we care about.
    Ignored,
    /// Matched the shape but the integration/device pair isn't
    /// recognized yet — the watcher auto-registered it.
    Registered,
    /// Device already existed; we just bumped `last_seen`.
    Touched,
}

/// Background watcher task. Owns a bus subscription + a repo handle.
#[derive(Debug, Clone)]
pub struct BusWatcher {
    bus: Bus,
    repo: DeviceRepo,
}

impl BusWatcher {
    #[must_use]
    pub fn new(bus: Bus, repo: DeviceRepo) -> Self {
        Self { bus, repo }
    }

    /// Subscribe to `device.>` and loop forever dispatching each
    /// incoming message. Returns only on bus / subscription teardown.
    ///
    /// # Errors
    /// Returns if the initial `subscribe` call fails. Per-message
    /// errors are logged and the loop continues — one malformed
    /// payload or transient db hiccup doesn't drop the whole watcher.
    pub async fn run(self) -> Result<()> {
        let mut sub = self
            .bus
            .raw()
            .subscribe("device.>".to_string())
            .await
            .context("subscribe device.>")?;
        info!("registry bus watcher subscribed to device.>");

        while let Some(msg) = futures::StreamExt::next(&mut sub).await {
            let subject = msg.subject.as_str();
            if let Err(e) = self.handle(subject).await {
                warn!(subject, error = %format!("{e:#}"), "bus watcher handle failed");
            }
        }
        info!("bus watcher subscription ended");
        Ok(())
    }

    async fn handle(&self, subject: &str) -> Result<MatchOutcome> {
        let Some((integration, device_token)) = parse_device_subject(subject) else {
            return Ok(MatchOutcome::Ignored);
        };

        // Path 1: the device token is a ULID of a device we already
        // have. Bump last_seen and move on — the registry already
        // knows this one (probably put there by a
        // `registry::upsert-device` host call).
        if self.repo.touch_last_seen(device_token).await? {
            debug!(subject, integration, device_token, "touched last_seen");
            return Ok(MatchOutcome::Touched);
        }

        // Path 2: the device token is an external_id of a device we
        // have. Same outcome — bump last_seen.
        if let Some(found) = self
            .repo
            .find_by_external_id(integration, device_token)
            .await?
        {
            // touch_last_seen above was indexed on id; re-bump by the
            // canonical id so the update fires. `ok_or_else` path is
            // unreachable in practice since find_by_external_id only
            // returns Some with a populated id.
            let id = found.id.as_ref().map_or("", |u| u.value.as_str());
            if !id.is_empty() {
                self.repo.touch_last_seen(id).await?;
            }
            debug!(subject, integration, device_token, "touched by external_id");
            return Ok(MatchOutcome::Touched);
        }

        // Path 3: brand-new pair. Auto-register. Minimal Device shape
        // (integration + external_id + schema_version); the owning
        // plugin can enrich later via gRPC upsert (M2 capability)
        // until the capability retires.
        let device = Device {
            id: None, // upsert mints a ULID
            integration: integration.into(),
            external_id: device_token.into(),
            manufacturer: String::new(),
            model: String::new(),
            label: device_token.into(),
            rooms: Vec::new(),
            capabilities: Vec::new(),
            entities: Vec::new(),
            trust_level: TrustLevel::UserAdded.into(),
            schema_version: iot_core::DEVICE_SCHEMA_VERSION,
            plugin_meta: std::collections::HashMap::default(),
            last_seen: None,
        };
        let saved = self.repo.upsert(device).await?;
        let ulid = saved.id.as_ref().map_or(String::new(), |u| u.value.clone());
        info!(
            subject,
            integration, device_token, ulid, "auto-registered new device from bus"
        );
        Ok(MatchOutcome::Registered)
    }
}

/// Parse the `device.*` subject tree. Returns `(integration,
/// device_token)` on a shape we care about; `None` otherwise.
///
/// ADR-0004 subject patterns recognised:
///   * `device.<plugin>.<device>.<entity>.state`
///   * `device.<plugin>.<device>.<entity>.event`
///   * `device.<plugin>.<device>.avail`
fn parse_device_subject(subject: &str) -> Option<(&str, &str)> {
    let parts: Vec<&str> = subject.split('.').collect();
    match parts.as_slice() {
        ["device", plugin, device, "avail"] => Some((plugin, device)),
        ["device", plugin, device, _, suffix] if matches!(*suffix, "state" | "event") => {
            Some((plugin, device))
        }
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_state_subjects() {
        assert_eq!(
            parse_device_subject("device.zigbee2mqtt.kitchen-temp.temperature.state"),
            Some(("zigbee2mqtt", "kitchen-temp"))
        );
        assert_eq!(
            parse_device_subject("device.zwave.01hxxyy.onoff.state"),
            Some(("zwave", "01hxxyy"))
        );
    }

    #[test]
    fn parses_event_subjects() {
        assert_eq!(
            parse_device_subject("device.zigbee2mqtt.remote-1.action.event"),
            Some(("zigbee2mqtt", "remote-1"))
        );
    }

    #[test]
    fn parses_avail_subjects() {
        assert_eq!(
            parse_device_subject("device.zigbee2mqtt.kitchen-temp.avail"),
            Some(("zigbee2mqtt", "kitchen-temp"))
        );
    }

    #[test]
    fn ignores_non_device_and_malformed() {
        assert_eq!(parse_device_subject("cmd.zigbee2mqtt.foo.bar"), None);
        assert_eq!(parse_device_subject("device.zigbee2mqtt"), None);
        assert_eq!(parse_device_subject(""), None);
        // device.<plugin>.<device>.<entity>.<unknown_suffix>
        assert_eq!(parse_device_subject("device.zigbee2mqtt.foo.bar.baz"), None);
    }
}
