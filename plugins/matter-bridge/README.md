# matter-bridge — Matter (CSA) WASM plugin

Bridges Matter controller events into IoT-AtHome's canonical bus.
Third WASM plugin in the project (after `zigbee2mqtt-adapter` and
`sdr433-adapter`).

**Status: scaffold (Phase 2 of the Matter integration arc).** The
plugin compiles to `wasm32-wasip2`, installs via `iotctl plugin
install`, and logs every inbound MQTT message. Translator + state
publisher land in Phase 3.

## Architecture

Per [ADR-0014](../../docs/adr/0014-matter-integration-architecture.md):

```
┌─ Matter device (over Thread / WiFi) ────┐
│                                          │
└──────┬───────────────────────────────────┘
       │ Matter wire
       ↓
┌─ python-matter-server ─────────┐
│  (compose service, --profile   │
│   matter)                      │
│  WebSocket :5580/ws             │
└──────────┬─────────────────────┘
           │ WS events
           ↓
┌─ tools/matter/wsmqtt.py ───────┐
│  (~50-line WS→MQTT shim)        │
└──────────┬─────────────────────┘
           │ MQTT
           ↓
┌─ Mosquitto broker ──────────────┐
│  topic: matter/nodes/<id>/      │
│         endpoints/<eid>/        │
│         clusters/<cid>/<attr>   │
└──────────┬─────────────────────┘
           │ host mqtt capability
           ↓
┌─ matter-bridge (this plugin) ───┐
│  WASM Component, ABI 1.3.0      │
│  translator (Phase 3)            │
└──────────┬─────────────────────┘
           │ bus.publish
           ↓
┌─ NATS bus ─────────────────────┐
│  device.matter.<node-eid>.<x>.state │
└──────────┬─────────────────────┘
           │ device.>
           ↓
┌─ iot-registry bus_watcher ──────┐
│  auto-registers (matter, <id>)  │
└─────────────────────────────────┘
```

## Build

The plugin builds standalone (excluded from the workspace, like z2m
and sdr433-adapter):

```sh
cd plugins/matter-bridge
cargo build --release   # produces target/wasm32-wasip2/release/matter_bridge.wasm
```

The `.cargo/config.toml` pins the build target to `wasm32-wasip2`.

## Install (Phase 1+2 — both must be running)

```sh
# 1. Phase 1: stand up the controller + Thread BR
docker compose -f deploy/compose/dev-stack.yml \
    --profile matter up python-matter-server

# 2. Phase 2: install this plugin
iotctl plugin install plugins/matter-bridge --allow-unsigned
```

The `--profile matter` opt-in keeps the Matter stack out of the default
`just dev` loop — operators who don't run Matter pay nothing.

## Roadmap

See [docs/MATTER-PLAN.md](../../docs/MATTER-PLAN.md) for the full
phased arc (5 phases). This crate ships through Phase 2 today; Phase
3 lands the translator covering OnOff / LevelControl /
TemperatureMeasurement / OccupancySensing / BooleanState clusters.

## License

Apache-2.0 OR MIT, same as the rest of the workspace.
