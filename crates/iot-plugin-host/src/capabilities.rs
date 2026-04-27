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

    /// Check an outbound URL against the `net.outbound` allow-list.
    ///
    /// **Match semantics: structural URL parse, not string prefix.**
    /// Both the manifest entry and the plugin-supplied URL are
    /// parsed via [`url::Url`]; an entry authorises a request URL
    /// only when **all** of the following hold:
    ///
    /// * Schemes match exactly (case-insensitive). Only `http` and
    ///   `https` are allowed; `file://`, `gopher://`, `data:`, etc.
    ///   are rejected on both sides.
    /// * Hosts match exactly (case-insensitive). The url crate's
    ///   ASCII-canonical IDN form is used so visually-similar
    ///   Unicode homoglyphs don't sneak past.
    /// * Ports match (with default-port awareness — `https://x:443`
    ///   == `https://x`).
    /// * The request URL's path starts with the entry's path **and**
    ///   the next character past the prefix in the request path is
    ///   `/` or end-of-path (no segment-mid bypass).
    /// * The request URL has **no userinfo** (`https://user@host/x`
    ///   is rejected unconditionally — userinfo can mask the actual
    ///   host from a casual reader).
    ///
    /// Compared with the M5a-era string-prefix matcher this defends
    /// against:
    ///
    /// * `https://api.example.com@evil.com/x` — userinfo bypass.
    /// * `https://API.EXAMPLE.COM/x` — case-only match (the old
    ///   matcher would deny something the operator intended to
    ///   allow; the new matcher canonicalises both sides).
    /// * `https://api.example.com:1337/x` against an entry of
    ///   `https://api.example.com` — port-mismatch must deny.
    /// * `file:///etc/passwd`, `gopher://...` — scheme confusion.
    /// * `https://api.example.com.evil/x` — dotted-suffix host (the
    ///   old boundary-char check caught this; the new structural
    ///   match catches it more cleanly via host equality).
    ///
    /// Operators write entries as full URLs:
    /// `https://api.open-meteo.com` (whole-host scope) or
    /// `https://api.acme.com/v1/forecast` (sub-path scope).
    ///
    /// # Errors
    /// Returns [`Denied`] for any URL that doesn't match an entry,
    /// or for any URL the parser refuses (malformed input). Error
    /// code is the standard `capability.denied`.
    pub fn check_net_outbound(&self, url: &str) -> Result<(), Denied> {
        let req = url::Url::parse(url).map_err(|e| Denied {
            code: "capability.denied",
            message: format!("net.outbound on `{url}`: malformed URL: {e}"),
        })?;

        // Scheme allow-list — http/https only. Plugins shouldn't be
        // able to escape into file://, gopher://, data:, javascript:,
        // or anything else weird via this capability.
        if !matches!(req.scheme(), "http" | "https") {
            return Err(Denied {
                code: "capability.denied",
                message: format!(
                    "net.outbound on `{url}`: scheme `{}` not allowed (only http/https)",
                    req.scheme()
                ),
            });
        }

        // Userinfo bypass class: `https://api.example.com@evil/x`
        // parses with host = "evil" but reads (in haste) like the
        // host is api.example.com. Reject unconditionally — no
        // legitimate API uses URL-encoded credentials these days.
        if !req.username().is_empty() || req.password().is_some() {
            return Err(Denied {
                code: "capability.denied",
                message: format!("net.outbound on `{url}`: userinfo (user/pass) not allowed"),
            });
        }

        if self
            .net
            .outbound
            .iter()
            .any(|entry| url_entry_authorises(entry, &req))
        {
            return Ok(());
        }
        Err(Denied {
            code: "capability.denied",
            message: format!("net.outbound on `{url}` not in manifest allow-list"),
        })
    }
}

/// Structural match between a manifest allow-list entry and a
/// plugin-supplied request URL. See
/// [`CapabilityMap::check_net_outbound`] for the contract.
///
/// Returns `false` if the entry is itself malformed (we don't
/// rescue manifests with broken URLs — they're a configuration
/// bug operators should fix).
fn url_entry_authorises(entry: &str, req: &url::Url) -> bool {
    let Ok(allow) = url::Url::parse(entry) else {
        return false;
    };

    // Same scheme (case-insensitive — url::Url already lowers).
    if allow.scheme() != req.scheme() {
        return false;
    }
    // Same host (case-insensitive). url::Url returns ASCII-canonical
    // form via IDN-to-ASCII so Unicode homoglyphs are folded.
    if allow.host_str() != req.host_str() {
        return false;
    }
    // Same port (with default-port awareness — Url::port_or_known_default
    // resolves `https` → 443, `http` → 80).
    if allow.port_or_known_default() != req.port_or_known_default() {
        return false;
    }
    // Path-prefix match with segment boundary. Entry path "" is
    // treated as "/", so a host-scope entry like
    // `https://api.example.com` (path = "") authorises every path.
    let allow_path = if allow.path().is_empty() {
        "/"
    } else {
        allow.path()
    };
    let req_path = req.path();
    if !req_path.starts_with(allow_path) {
        return false;
    }
    // Boundary check on path: the next char past the entry's path
    // must be `/` or end-of-path. This catches
    // `entry=/v1/forecast` against `req=/v1/forecastoid` (segment
    // splice attacker). If the entry already ends in `/` then any
    // following char is fine — the operator chose a sub-path
    // scope and what comes after is ordinary URL content.
    if allow_path.ends_with('/') {
        return true;
    }
    matches!(
        req_path.as_bytes().get(allow_path.len()),
        None | Some(&b'/')
    )
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

    // --------------------------------------------------- net.outbound tests

    fn net_only(outbound: Vec<&str>) -> CapabilityMap {
        CapabilityMap {
            net: NetCapabilities {
                outbound: outbound.into_iter().map(String::from).collect(),
            },
            ..Default::default()
        }
    }

    #[test]
    fn net_outbound_authorises_exact_match() {
        let m = net_only(vec!["https://api.open-meteo.com"]);
        assert!(m.check_net_outbound("https://api.open-meteo.com/").is_ok());
    }

    #[test]
    fn net_outbound_authorises_path_under_prefix() {
        let m = net_only(vec!["https://api.open-meteo.com"]);
        assert!(m
            .check_net_outbound("https://api.open-meteo.com/v1/forecast")
            .is_ok());
        assert!(m
            .check_net_outbound("https://api.open-meteo.com/v1/forecast?lat=1&lon=2")
            .is_ok());
        assert!(m
            .check_net_outbound("https://api.open-meteo.com/#frag")
            .is_ok());
    }

    #[test]
    fn net_outbound_blocks_dotted_suffix_attacker() {
        // Pre-Bucket-1 the boundary-char check caught this; the
        // structural matcher catches it more cleanly via host equality.
        let m = net_only(vec!["https://api.open-meteo.com"]);
        assert!(m
            .check_net_outbound("https://api.open-meteo.com.evil.example/x")
            .is_err());
        assert!(m
            .check_net_outbound("https://api.open-meteo.commerce.example/")
            .is_err());
    }

    #[test]
    fn net_outbound_blocks_unknown_host() {
        let m = net_only(vec!["https://api.open-meteo.com"]);
        assert!(m
            .check_net_outbound("https://api.tibber.com/v1/gql")
            .is_err());
    }

    #[test]
    fn net_outbound_with_path_scope_only_authorises_subpath() {
        let m = net_only(vec!["https://api.acme.com/v1/forecast"]);
        assert!(m
            .check_net_outbound("https://api.acme.com/v1/forecast")
            .is_ok());
        assert!(m
            .check_net_outbound("https://api.acme.com/v1/forecast/2026-04")
            .is_ok());
        assert!(m
            .check_net_outbound("https://api.acme.com/v1/forecast?q=x")
            .is_ok());
        // Different path under the same host: denied.
        assert!(m
            .check_net_outbound("https://api.acme.com/v1/billing")
            .is_err());
        // Segment-splice attacker — same host, path that starts
        // with the entry's path but isn't an actual sub-path.
        assert!(m
            .check_net_outbound("https://api.acme.com/v1/forecastoid")
            .is_err());
    }

    #[test]
    fn net_outbound_default_is_deny_all() {
        let m = CapabilityMap::default();
        assert!(m.check_net_outbound("https://api.open-meteo.com/").is_err());
    }

    #[test]
    fn net_outbound_multiple_prefixes_combine() {
        let m = net_only(vec![
            "https://api.open-meteo.com",
            "https://api.tibber.com/v1/gql",
        ]);
        assert!(m
            .check_net_outbound("https://api.open-meteo.com/v1/forecast")
            .is_ok());
        assert!(m
            .check_net_outbound("https://api.tibber.com/v1/gql")
            .is_ok());
        assert!(m
            .check_net_outbound("https://api.tibber.com/v1/billing")
            .is_err());
    }

    // ----------------------------------------- Bucket 1 audit-fix bypass tests

    #[test]
    fn net_outbound_blocks_userinfo_host_masking() {
        // Confused-deputy: parser-fooled human sees
        // `https://api.acme.com@evil.com` as an acme.com URL;
        // url::Url parses host=evil.com. Reject userinfo
        // unconditionally to short-circuit the whole class.
        let m = net_only(vec!["https://api.acme.com"]);
        assert!(m
            .check_net_outbound("https://api.acme.com@evil.com/x")
            .is_err());
        assert!(m
            .check_net_outbound("https://user:pass@api.acme.com/x")
            .is_err());
    }

    #[test]
    fn net_outbound_blocks_non_http_schemes() {
        let m = net_only(vec!["https://api.acme.com"]);
        // file:// — local FS read.
        assert!(m.check_net_outbound("file:///etc/passwd").is_err());
        // gopher:// — historic SSRF vector.
        assert!(m.check_net_outbound("gopher://api.acme.com/x").is_err());
        // data: — could be used to embed payloads.
        assert!(m.check_net_outbound("data:text/plain,Hello").is_err());
        // javascript: — meaningless to reqwest but rejected by
        // construction.
        assert!(m.check_net_outbound("javascript:alert(1)").is_err());
    }

    #[test]
    fn net_outbound_canonicalises_case_to_match_entry() {
        let m = net_only(vec!["https://api.open-meteo.com"]);
        // Mixed case in the request URL is canonicalised by
        // url::Url and matches the lower-case entry.
        assert!(m
            .check_net_outbound("https://API.OPEN-METEO.COM/v1/forecast")
            .is_ok());
        // Same for the scheme.
        assert!(m
            .check_net_outbound("HTTPS://api.open-meteo.com/v1/forecast")
            .is_ok());
    }

    #[test]
    fn net_outbound_port_aware() {
        // Default port (443) match.
        let m = net_only(vec!["https://api.acme.com"]);
        assert!(m.check_net_outbound("https://api.acme.com:443/x").is_ok());
        // Non-default port: must mismatch.
        assert!(m.check_net_outbound("https://api.acme.com:8443/x").is_err());

        // Operator declares an explicit non-default port.
        let m = net_only(vec!["https://api.acme.com:8443"]);
        assert!(m.check_net_outbound("https://api.acme.com:8443/x").is_ok());
        assert!(m.check_net_outbound("https://api.acme.com/x").is_err());
    }

    #[test]
    fn net_outbound_rejects_malformed_input() {
        let m = net_only(vec!["https://api.acme.com"]);
        // Garbage input — parser refuses.
        assert!(m.check_net_outbound("not a url").is_err());
        assert!(m.check_net_outbound("").is_err());
    }

    #[test]
    fn net_outbound_ipv6_literal_match() {
        // url::Url parses `[::1]` correctly. Operators using IPv6
        // literals in net.outbound entries get them matched.
        let m = net_only(vec!["http://[::1]:8080"]);
        assert!(m.check_net_outbound("http://[::1]:8080/").is_ok());
        // Different host with the same port: denied.
        assert!(m.check_net_outbound("http://[::2]:8080/").is_err());
    }

    #[test]
    fn net_outbound_host_only_entry_authorises_all_paths() {
        // Operator declares a whole-host scope (no path component).
        let m = net_only(vec!["https://api.example.com"]);
        assert!(m.check_net_outbound("https://api.example.com/").is_ok());
        assert!(m
            .check_net_outbound("https://api.example.com/anything/deep")
            .is_ok());
    }
}
