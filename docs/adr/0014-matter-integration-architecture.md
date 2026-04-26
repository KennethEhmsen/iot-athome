# ADR-0014: Matter / Thread Integration Architecture

- **Status:** Accepted
- **Date:** 2026-04-26
- **Anchors:** [ADR-0003](0003-plugin-abi-wasm-component-model.md), [ADR-0013](0013-zigbee2mqtt-wasm-migration.md), [docs/MATTER-PLAN.md](../MATTER-PLAN.md)

## Context

Matter (formerly Project CHIP) is the Connectivity Standards Alliance's unified IP-based smart-home protocol. It runs over WiFi or Thread (low-power IPv6 mesh). Two distinct integration roles exist:

1. **Controller** — IoT-AtHome's hub commissions and controls Matter nodes on the LAN / Thread mesh. Equivalent to how the hub already controls Zigbee devices via z2m.
2. **Bridge** — IoT-AtHome's hub *re-exposes* its already-registered devices as Matter accessories so Apple Home / Google Home / Alexa controllers can see them.

This ADR addresses **controller mode only**. Bridge mode requires CSA certification for production deployment and a vendor ID; that's M6+ scope and intentionally out of frame here.

The plugin architecture established in ADR-0003 + ADR-0013 makes "WASM plugin speaks the wire protocol directly" tempting. For Matter, that path doesn't work:

- The reference Matter SDK (`connectedhomeip`, CSA-maintained) is C++; no wasm32-wasip2 build target.
- Thread mesh requires a 802.15.4 radio. A Thread Border Router (OTBR) firmware on an nRF52840 USB dongle bridges Thread to IPv6, but the radio access lives at the kernel/USB layer — the wrong abstraction layer for a WASM sandbox even with the future WASI sockets.
- Matter commissioning needs deterministic timing for the device-discovery and PAKE handshake; running it inside the WASM supervisor's fuel-metered call boundaries would be fragile.

So Matter follows the same sidecar pattern as z2m: an external process speaks the wire protocol; a WASM plugin reads its output and republishes on the canonical bus.

## Sidecar candidates

Three reference implementations were evaluated:

### Candidate 1 — `python-matter-server` (HomeAssistant team, MIT)

- WebSocket API on `ws://host:5580/ws` exposing the full controller surface (commissioning, attribute read/write, event subscriptions).
- Mature: HomeAssistant has used it in production for ~3 years with millions of users. Bug surface understood, edge cases documented.
- License compatible (MIT alongside our Apache-2.0 OR MIT).
- Runtime cost: ~150 MB Python + dependencies, ~80 MB RAM idle. Acceptable on a Raspberry Pi 5.
- Thread BR support: pairs with any standard OTBR (Silicon Labs, Nordic, NXP). Documented integration with the SkyConnect / Connect ZBT-1 dongle the HA community uses.

### Candidate 2 — `matter.js` (Project Matter, Apache 2.0)

- Pure TypeScript / Node.js implementation.
- Lighter footprint: ~40 MB Node + deps, ~50 MB RAM idle.
- Newer (~1.5 years in production); fewer real-world battle-tests at scale.
- Apache 2.0 licensed, fine.
- Active monthly releases tracking spec evolution.

### Candidate 3 — `chip-tool` / `connectedhomeip` C++ SDK directly

- Lowest-level, most control.
- Build complexity: clang + ninja + ~30 min from scratch on a Pi 5.
- We'd be writing the WS-or-MQTT shim ourselves, plus tracking spec changes by hand.

## Decision

**Adopt Candidate 1: `python-matter-server` as the Matter controller sidecar.**

Plus a small **Python WS→MQTT bridge** (`tools/matter-bridge/wsmqtt.py`, ~50 lines using `paho-mqtt`) that subscribes to the controller's WebSocket and republishes node events on a canonical MQTT topic shape:

```text
matter/nodes/<node_id>/endpoints/<endpoint_id>/clusters/<cluster_id>/<attribute>
matter/cmd/<node_id>/endpoints/<endpoint_id>/clusters/<cluster_id>/<command>   (request)
matter/cmd/<node_id>/endpoints/<endpoint_id>/clusters/<cluster_id>/<command>/response   (reply)
```

A new WASM plugin `plugins/matter-bridge/` subscribes to `matter/nodes/+/+/+/+/+` via the existing `mqtt.subscribe` host capability (ABI 1.3.0+, no new capability needed), translates each event to canonical `iot.device.v1.EntityState` protobuf, and publishes on `device.matter.<node_id>-<endpoint_id>.<entity>.state`. The iot-registry bus-watcher (M3 W1.2) auto-registers each `(matter, <node_id>-<endpoint_id>)` pair on first publish.

### Why python-matter-server over matter.js

Three reasons:

1. **Production track record.** HA's userbase has shaken out the long-tail decoder bugs. matter.js is technically newer but hasn't accumulated the same field validation.
2. **Spec lag tolerance.** When a new Matter cluster ships, python-matter-server tends to add support faster (HA's commercial pressure on parity). matter.js follows but with a longer cycle.
3. **Operator base.** Operators familiar with HA can debug python-matter-server failures using the same tools they already know. matter.js is newer territory.

The cost — ~100 MB more RAM than matter.js — is acceptable on the Raspberry Pi 5 hub class we target. Cost matters more on the eventual ESP32-class secondary node, but ESP32 nodes won't run the controller anyway; they'd run the per-room voice mic or the camera bridge, neither of which needs Matter.

### Why the WS→MQTT shim instead of direct WS access

`python-matter-server` exposes WebSocket. The current WASM plugin host only offers `mqtt.subscribe` / `mqtt.publish` host capabilities, not raw WebSocket. We could:

* Add a new `websocket` host capability (large surface area, deferred).
* Add the `net.outbound` HTTP capability and have plugins use HTTP polling (works but loses event-driven ergonomics).
* Add a tiny WS→MQTT shim and reuse the existing MQTT path.

Option 3 is cleanest. The shim is ~50 lines of Python and runs as another compose service. Plugins stay sandboxed behind the existing capability surface (no new ABI churn). When `net.outbound` ships (queued chip), the matter-bridge plugin can optionally subscribe to the controller WebSocket directly — but the MQTT shim path stays as the canonical reference.

## Thread Border Router

A Thread BR is required for Thread-routed Matter devices (most battery-powered sensors and locks). Two supported configurations:

### Configuration A — Nordic nRF52840 USB dongle running OTBR firmware

- Hardware: Nordic nRF52840 dongle (~€18) or Silicon Labs SkyConnect (~€28).
- Firmware: OTBR (`openthread/borderrouter`) flashed via NRF Connect Programmer.
- USB-passthrough into the `python-matter-server` container required (`devices: ["/dev/ttyACM0:/dev/ttyACM0"]` or equivalent in compose).
- This is the recommended dev + low-cost prod path.

### Configuration B — Standalone OTBR appliance

- Any Raspberry Pi-shaped OTBR (HA Yellow, separate Pi 4 with the dongle, etc.).
- Talks to `python-matter-server` over LAN; no USB passthrough needed.
- Cleaner separation, double the hardware cost, and an extra failure-mode surface (network partition between hub and BR). Recommended for users who already own an OTBR appliance.

The MATTER-PLAN.md hardware bill-of-materials documents both with specific part numbers + flashing notes.

## Consequences

**Positive:**

- Plugin sandboxing preserved. The matter-bridge WASM plugin gets the same MQTT-only capability surface as z2m and sdr433-adapter. No new attack surface in the WASM host.
- Spec-evolution outsourced. python-matter-server tracks Matter spec versions; we don't.
- Compose-shaped opt-in. Operators who don't run Matter pay nothing — the controller and shim services live behind a `matter` compose profile (same pattern as the M5a `history` profile for TimescaleDB).
- Thread BR cost is bounded (€18 dongle) and well-documented.

**Negative:**

- **Latency floor of ~50 ms** for the WS→MQTT→bus → registry → panel pipeline. Matter's own discovery and command latency is in the hundreds of milliseconds, so this is dominated by the protocol. Acceptable for state synchronisation; sub-second commands need a future direct-MQTT path that python-matter-server already supports as `command` topics.
- **Three components to operate** (controller + shim + WASM plugin) vs. z2m's two (z2m + WASM plugin). The shim is intentionally minimal; failure-mode debugging stays tractable.
- **Bridge mode** (re-exposing our devices to Apple/Google/Alexa) requires a separate ADR + CSA certification when we get to it. M6+ scope. The architecture chosen here doesn't preclude bridge mode but doesn't reach it either.
- **Python runtime in the dependency stack.** First non-Rust runtime we accept in the dev compose. Localised to the `matter` profile so operators who don't enable Matter never see it.

## Alternatives considered

- **Build Matter inside WASM via a future WASI-CHIP port.** Requires upstream connectedhomeip work; unscoped at the spec level, multi-year. Not feasible.
- **Use chip-tool directly without a daemon.** Forks a process per command; loses event-driven push. Not viable for a controller that needs to track ~50 attribute subscriptions across 10-20 nodes.
- **Use the Apple Home / Google Home APIs as the controller surface.** Locks us into vendor cloud paths; violates the local-first design tenet.
- **Wait for matter.js to mature.** Reasonable in 18 months; not now.

## Supersession trigger

This ADR is retired (status → Superseded) when one of:

- A WASI 0.3+-compatible Rust Matter implementation reaches production parity with python-matter-server, allowing the sidecar collapse.
- Bridge mode is added in a follow-up ADR (the architecture extends rather than replaces; supersession only fires if the controller architecture itself changes).
- A direct WebSocket host capability lands and the WS→MQTT shim is removed.
