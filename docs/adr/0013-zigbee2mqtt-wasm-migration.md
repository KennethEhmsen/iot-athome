# ADR-0013: Zigbee2MQTT Adapter Migration to WASM Plugin

- **Status:** Accepted
- **Date:** 2026-04-22
- **Anchors:** [ADR-0003](0003-plugin-abi-wasm-component-model.md), [ADR-0011](0011-dev-bus-auth.md), [docs/M2-PLAN.md](../M2-PLAN.md)

## Context

[ADR-0003](0003-plugin-abi-wasm-component-model.md) chose the WASM Component Model as the canonical plugin ABI. M1 shipped the Zigbee2MQTT adapter as a *native* binary running under systemd as an explicit M1-only escape hatch. M2 W3 calls for migrating it to a WASM plugin to retire that escape hatch.

When we sat down to do the migration in M2 W3, two paths fell out:

### Path 1 — Compile rumqttc directly to `wasm32-wasip2`

The current adapter (`plugins/zigbee2mqtt-adapter/`, 587 LOC) consumes MQTT via `rumqttc::AsyncClient`, talks to `iot-registry` over gRPC (`tonic::transport::Channel`), and publishes to NATS via `iot_bus::Bus`.

Blockers:

- `rumqttc` 0.24's async I/O is wired to `tokio::net::TcpStream` — wasip2 has no equivalent today. WASI 0.2 sockets exist but tokio doesn't bind them yet.
- `tonic` is in the same boat for the registry gRPC call.
- TLS material (`ca.crt` + adapter cert/key) is read from disk at startup. wasip2 has filesystem access but plugins shouldn't get raw FS as a capability.

Net: feasible but a multi-week port of upstream crates. Out of scope for M2.

### Path 2 — Push MQTT (and registry) into the host as plugin capabilities

The plugin manifest schema *already* declares an `mqtt:` capability namespace (`crates/iot-plugin-host/src/capabilities.rs`):

```rust
pub struct MqttCapabilities {
    pub publish: Vec<String>,
    pub subscribe: Vec<String>,
    pub bridge: Vec<String>,
}
```

That was deliberate forecasting: the design always intended for the host to own the broker connection and dispatch deserialised payloads to plugins, exactly the way `bus::publish` works today. Plugins stay TLS-free and tokio-free.

This makes the migration a host-side build-out, not a port of upstream crates.

## Decision

**Adopt Path 2.** The migration completes in three discrete pieces:

### Piece 1 — MQTT host capability

WIT additions to `schemas/wit/iot-plugin-host.wit`:

```wit
interface mqtt {
    use types.{plugin-error};
    /// Subscribe to a topic pattern. Inbound messages dispatch to the
    /// plugin's exported `runtime.on-mqtt-message`.
    subscribe: func(topic-pattern: string) -> result<_, plugin-error>;
    /// Publish on the broker. Capability-checked against
    /// `manifest.capabilities.mqtt.publish`.
    publish: func(topic: string, payload: list<u8>, retain: bool)
        -> result<_, plugin-error>;
}

// runtime gains:
on-mqtt-message: func(topic: string, payload: list<u8>) -> result<_, plugin-error>;
```

Host side:

- `crates/iot-plugin-host/src/mqtt.rs` — owns one `rumqttc::AsyncClient` per host process, multiplexes inbound topics across subscribed plugins, capability-checks every `subscribe`/`publish` against the calling plugin's `MqttCapabilities`.
- `HostBindings` gains `mqtt: Option<MqttHandle>`; the binary's `run()` builds the rumqttc client at startup using mTLS material from config (matching M1's adapter config).
- `CapabilityMap::check_mqtt_subscribe(&topic)` + `check_mqtt_publish(&topic)` mirror the existing `check_bus_publish`.

### Piece 2 — Registry upsert as a host capability

The native adapter calls `RegistryServiceClient::upsert_device` to register devices on first sight. We don't want to run gRPC from inside the plugin. Two options:

A. **Add a `registry::upsert-device` host capability** that wraps the tonic client.
B. **Auto-register on the registry side** when an `EntityState` arrives for an unknown `(integration, external_id)` tuple.

We pick (B) for the long-term — it's the cleaner architecture and removes the synchronous gRPC round-trip from the hot path — but it's an `iot-registry` change that fits in M3 (where registry is being rebuilt to add the JCS audit canonicalisation per the retro). For the M2 migration we ship (A) as a transitional host capability:

```wit
interface registry {
    use types.{plugin-error};
    upsert-device: func(
        integration: string,
        external-id: string,
        label: string,
        manufacturer: string,
        model: string,
    ) -> result<string, plugin-error>;  // returns ULID
}
```

The capability map gains:

```rust
pub struct RegistryCapabilities { pub upsert: bool, pub list: bool }
```

When M3 ships (B), the registry-host capability becomes optional and eventually deprecated.

### Piece 3 — Convert `plugins/zigbee2mqtt-adapter` to a WASM crate

- Drop `main.rs` (no binary entry point).
- Drop `rumqttc`, `tonic`, `tokio` runtime, `iot_bus`, `iot_proto` Cargo deps.
- Keep `translator.rs` and `state_publisher.rs` — they're pure functions.
- New `lib.rs`:
  - `init()` → `mqtt::subscribe("zigbee2mqtt/+")`.
  - `on_mqtt_message(topic, payload)` →
    1. `translator::friendly_name_from_topic(topic)`.
    2. `translator::translate(friendly, payload)`.
    3. `registry::upsert_device(...)` for the device shape.
    4. `bus::publish(...)` per recognised entity key.
- `manifest.yaml` declares:
  ```yaml
  capabilities:
    mqtt:    { subscribe: ["zigbee2mqtt/+"] }
    bus:     { publish:   ["device.zigbee2mqtt.>"] }
    registry: { upsert: true, list: true }
  ```
- The 4 translator unit tests carry over unchanged.

## Consequences

- **Host owns broker connections.** A single rumqttc client per host process serves every plugin that declares `mqtt:` capabilities. Reduces broker connection count and centralises mTLS material handling.
- **Plugins are pure WASM**, no native sockets, no tokio runtime. Matches ADR-0003's "pure components" intent.
- **Registry capability is a transitional host call.** It exists to keep the M1 adapter ergonomics (auto-register on first sight) while M3 finalises the bus-driven auto-register design. Marked deprecated when M3 ships.
- **Migration complexity.** Three discrete chunks; the WIT additions ripple through `iot-plugin-host`, `iot-plugin-sdk-rust`, the host binary, and the plugin itself.

## Implementation slip

M2 W3 had this listed as a single checkbox alongside `iotctl plugin install` and signing work. In practice it's a 4–5 day build-out. **The migration ships in M2 W4** alongside the second adapter (Z-Wave or 433-SDR) — they share the MQTT host capability, so doing both at once amortises the host-side work.

The W3 deliverable becomes: this ADR + the M2-PLAN.md update. Rolling the implementation into W4 is preferable to half-shipping it under W3.

## Out of scope for this ADR

- Z-Wave and 433-SDR adapter design (tracked separately in W4).
- `iot-registry`'s bus-driven auto-register (tracked in M3).
- Per-plugin MQTT ACLs at the broker level (tracked in M3 — `M2-PLAN-W3.md` already deferred to "manifest-derived Mosquitto ACLs").
