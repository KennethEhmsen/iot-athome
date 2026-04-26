# weather-poller — net.http capability demo

Demo plugin exercising the ABI 1.4.0 `net.http` host capability
against Open-Meteo's free public weather API. Scaffold-only at
post-v0.5.0-m5a — full poller + translator land in a follow-up
commit.

## Why this exists

The `net.outbound` plugin host capability landed in ABI 1.4.0 as
the architectural unblock for an entire class of integrations:

- Weather APIs (OpenWeather, MET.no, Open-Meteo, AccuWeather)
- Dynamic energy tariffs (Tibber, Octopus Agile, Nord Pool)
- Calendar feeds (CalDAV, Google Calendar)
- Notification sinks (ntfy.sh, Pushover, Telegram, Slack webhooks)
- HTTP-shaped device APIs (Shelly local, Tasmota REST, ESPHome native)

This crate is the canonical reference shape for those plugins. It
shows:

1. How to declare URL prefixes in the manifest's
   `capabilities.net.outbound` array.
2. How to build an `HttpRequest` + call `net::http(req)`.
3. How the host's URL-prefix check enforces the manifest allow-list
   with the dotted-suffix boundary safety property
   (`api.open-meteo.com` does NOT authorise `api.open-meteo.com.evil`).
4. How transport vs. HTTP-level errors surface differently
   (transport → `Err(plugin-error{code: net.transport})`,
   HTTP → `Ok(http-response{status: 4xx/5xx})`).

## Build

```sh
cd plugins/weather-poller
cargo build --release   # produces target/wasm32-wasip2/release/weather_poller.wasm
```

## Install

```sh
iotctl plugin install plugins/weather-poller --allow-unsigned
```

On `init()` the plugin fetches Berlin's current temperature from
Open-Meteo and logs the response status + body length. Verify in
the plugin host's logs:

```
INFO weather-poller: init (scaffold)
INFO weather-poller: open-meteo ok: status=200 bytes=178
```

If the host is offline (no internet), expect:

```
WARN weather-poller: open-meteo failed: net.transport: error sending request: ...
```

## Roadmap

The full poller arc:

1. Schema additions for `plugin_config` (manifest-time location +
   metrics), once the schema review settles.
2. Tokio-spawn-equivalent timer mechanism in the host (deferred
   to the SDK's 1.5 milestone — wasi:clocks/wall-clock + a host
   `runtime::tick` export).
3. Translator turning the Open-Meteo JSON envelope into canonical
   `device.weather.<location>.{temperature_c,humidity,wind_speed,…}.state`
   EntityState publishes.
4. Multi-provider abstraction so MET.no / OpenWeather / AccuWeather
   plug in via the same surface.

This crate ships through step 1 today (manifest scaffold + capability
verification). Steps 2-4 are independent commits whenever the SDK
gains the timer primitive.

## License

Apache-2.0 OR MIT.
