//! Weather poller — demo plugin for the ABI 1.4.0 net.http capability.
//!
//! Scaffold-only smoke test at this version: `init()` issues one
//! `net::http` GET against Open-Meteo's forecast endpoint and logs
//! the status + body length. Verifies the capability path works
//! end-to-end without committing to a full poller / scheduler /
//! translator stack.
//!
//! Reference for future HTTP-poll integrations (Tibber, Octopus
//! Agile, calendar feeds, ntfy.sh, Pushover, weather-from-other-
//! providers). They follow the same shape:
//!
//!   1. Manifest declares `capabilities.net.outbound` URL prefixes.
//!   2. Plugin builds an `HttpRequest` and calls `net::http(req)`.
//!   3. Host's URL-prefix check enforces the manifest allow-list
//!      with a path/query boundary safety property
//!      (`acme.com` does NOT authorise `acme.com.evil.example`).
//!   4. Plugin translates the response body into canonical
//!      `iot.device.v1.EntityState` protobufs and publishes on the
//!      bus. (Translator lands in a follow-up.)

#![forbid(unsafe_code)]

use iot_plugin_sdk_rust::exports::iot::plugin_host::runtime::{Guest, Payload, PluginError};
use iot_plugin_sdk_rust::iot::plugin_host::{log, net};

struct Component;

/// Open-Meteo demo URL — Berlin's lat/lon, current temperature only.
/// The full poller would parameterise location + metrics from the
/// manifest's plugin_config (a future schema field) or a bus
/// subscription on `cmd.weather.>`.
const DEMO_URL: &str =
    "https://api.open-meteo.com/v1/forecast?latitude=52.52&longitude=13.41&current=temperature_2m";

impl Guest for Component {
    fn init() -> Result<(), PluginError> {
        log::emit(log::Level::Info, "weather-poller", "init (scaffold)");

        // One smoke-test fetch. Real poller would tokio-spawn an
        // interval timer; the SDK doesn't expose timers yet (1.5+
        // territory), so init-only is the credible smoke test.
        let req = net::HttpRequest {
            method: "GET".to_string(),
            url: DEMO_URL.to_string(),
            headers: vec![("user-agent".to_string(), "iot-athome-weather-poller/0.0".to_string())],
            body: None,
        };

        match net::http(&req) {
            Ok(resp) => log::emit(
                log::Level::Info,
                "weather-poller",
                &format!(
                    "open-meteo ok: status={} bytes={}",
                    resp.status,
                    resp.body.len()
                ),
            ),
            Err(e) => log::emit(
                log::Level::Warn,
                "weather-poller",
                &format!("open-meteo failed: {}: {}", e.code, e.message),
            ),
        }
        Ok(())
    }

    /// Bus / MQTT callbacks unused — this plugin polls only.
    fn on_message(
        _subject: String,
        _iot_type: String,
        _payload: Payload,
    ) -> Result<(), PluginError> {
        Ok(())
    }

    fn on_mqtt_message(_topic: String, _payload: Payload) -> Result<(), PluginError> {
        Ok(())
    }
}

iot_plugin_sdk_rust::export_plugin!(Component with_types_in iot_plugin_sdk_rust);
