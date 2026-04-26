//! weather-poller — scaffold plugin for the ABI 1.4.0 `net::http`
//! host capability.
//!
//! On `init()` it fires one HTTPS GET against Open-Meteo's
//! current-conditions endpoint, parses the response, and logs a
//! summary line. That single round-trip is the smallest end-to-end
//! exercise of the new capability surface:
//!
//!   plugin           host
//!   ─────            ────
//!   init()      ──▶  capability check on net.outbound prefix
//!                    reqwest.send() on the shared client
//!                    serialize HttpResponse, return to guest
//!   <───── HttpResponse {status, headers, body}
//!   parse JSON, log "got temperature=X°C, code=Y"
//!
//! The full integration (timer-driven re-poll + EntityState publish on
//! `device.weather.<location>.<entity>.state`) lands once the ABI
//! grows a `tasks` import or once a periodic-trigger pattern is
//! established. For this slice the goal is to demonstrate that:
//!
//!   * `capabilities.net.outbound` allow-listing works.
//!   * The host's reqwest plumbing returns a real
//!     status/headers/body to the guest under fuel + timeout.
//!   * The plugin compiles to wasm32-wasip2 against
//!     `iot-plugin-sdk-rust` 1.4.0.
//!
//! Anything outside the manifest's
//! `capabilities.net.outbound: ["https://api.open-meteo.com/"]` returns
//! `PluginError { code: "capability.denied", … }` per ADR-0003 — the
//! plugin handles the denial as a value, not a trap.

#![forbid(unsafe_code)]

use iot_plugin_sdk_rust::exports::iot::plugin_host::runtime::{Guest, Payload, PluginError};
use iot_plugin_sdk_rust::iot::plugin_host::{log, net};

struct Component;

/// Berlin (lat 52.52, lon 13.41). Hard-coded for the scaffold; a real
/// integration takes the location list from a config file or the
/// install-time `commissioning` form.
const FORECAST_URL: &str = concat!(
    "https://api.open-meteo.com/v1/forecast",
    "?latitude=52.52&longitude=13.41&current_weather=true",
);

/// Subset of the Open-Meteo current-conditions response we care about
/// for the demo log line. The full schema has many more fields; we
/// pick the two that prove the capability worked end-to-end.
#[derive(serde::Deserialize)]
struct CurrentWeather {
    temperature: f64,
    weathercode: i64,
}
#[derive(serde::Deserialize)]
struct ForecastResponse {
    current_weather: CurrentWeather,
}

impl Guest for Component {
    fn init() -> Result<(), PluginError> {
        log::emit(log::Level::Info, "weather-poller", "init: fetching forecast");

        let req = net::HttpRequest {
            method: "GET".into(),
            url: FORECAST_URL.into(),
            headers: vec![("user-agent".into(), "iot-athome/weather-poller 0.1.0".into())],
            body: None,
        };
        let resp = match net::http(&req) {
            Ok(r) => r,
            Err(e) => {
                // Capability denial / timeout / transport — log and
                // surface to the supervisor so it shows in the crash
                // tracker. A real integration would back off and
                // retry on the next tick instead of failing init.
                log::emit(
                    log::Level::Error,
                    "weather-poller",
                    &format!("net.http failed: {} — {}", e.code, e.message),
                );
                return Err(e);
            }
        };

        if resp.status / 100 != 2 {
            log::emit(
                log::Level::Warn,
                "weather-poller",
                &format!("non-2xx status {}", resp.status),
            );
            return Ok(());
        }

        match serde_json::from_slice::<ForecastResponse>(&resp.body) {
            Ok(parsed) => log::emit(
                log::Level::Info,
                "weather-poller",
                &format!(
                    "current: temperature={}°C, weathercode={}",
                    parsed.current_weather.temperature, parsed.current_weather.weathercode,
                ),
            ),
            Err(e) => log::emit(
                log::Level::Warn,
                "weather-poller",
                &format!("response not parseable as forecast JSON: {e}"),
            ),
        }
        Ok(())
    }

    /// Bus subscriptions are not declared in this plugin's manifest, so
    /// the host should never deliver a bus message here. Log + ignore
    /// if it ever does (would be a host-side bug).
    fn on_message(
        subject: String,
        _iot_type: String,
        _payload: Payload,
    ) -> Result<(), PluginError> {
        log::emit(
            log::Level::Warn,
            "weather-poller",
            &format!("unexpected bus on_message on subject={subject}"),
        );
        Ok(())
    }

    /// MQTT-driven inputs are not declared either. The 1.1.0+ ABI
    /// requires the export — return Ok as a no-op.
    fn on_mqtt_message(_topic: String, _payload: Payload) -> Result<(), PluginError> {
        Ok(())
    }
}

iot_plugin_sdk_rust::export_plugin!(Component with_types_in iot_plugin_sdk_rust);
