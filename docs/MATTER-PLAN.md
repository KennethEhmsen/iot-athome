# Matter / Thread Integration — Phased Arc

**Anchor ADR:** [ADR-0014](adr/0014-matter-integration-architecture.md) · **Target:** post-`v0.5.0-m5a`, pre-M5b · **Estimated duration:** 3 weeks of focused work over multiple sessions

## Scope re-framing

Matter integration is a **multi-session arc**, not a single-commit slice. The architecture (sidecar + WS→MQTT shim + WASM plugin) decided in ADR-0014 means three components must reach production-quality together. Each gets its own week below.

The arc deliberately stops at **controller mode** (the hub commissions + controls Matter nodes). **Bridge mode** (re-exposing IoT-AtHome devices outward to Apple Home / Google Home / Alexa) requires CSA certification with a vendor ID and ships separately as M6+ work — out of scope here.

## Goal

> A user can plug an nRF52840 USB dongle into the hub, run `just dev --profile matter up`, and commission a Matter device (a smart bulb, a contact sensor) via the panel. The device's state appears under `iotctl device list`, its values stream to the panel WS in real time, and the iot-history backend captures events when `IOT_TIMESCALE_URL` is configured.

## Phased plan

### Phase 1 — Controller sidecar + Thread Border Router

Stand up the python-matter-server controller in a compose service plus the OTBR firmware on the dongle. No IoT-AtHome integration yet — verify the Matter side independently first.

**Tasks:**

- `deploy/compose/matter/` — new directory with `python-matter-server` service definition behind a `matter` compose profile (same opt-in pattern as M5a's `history` profile for Timescale).
- `tools/matter/flash-otbr.md` — operator instructions for flashing the OTBR firmware onto the nRF52840 dongle. Single-page guide with screenshots.
- `tools/matter/quickstart.md` — verifies the controller works in isolation: `chip-tool` test commission of a virtual device, attribute read/write via the WS API.
- `deploy/compose/dev-stack.yml` — adds the `python-matter-server` service under the `matter` profile, USB-passthrough to `/dev/ttyACM0` for the dongle, depends on no other IoT-AtHome service.

**Acceptance:** `docker compose -f deploy/compose/dev-stack.yml --profile matter up python-matter-server` boots clean, the WS endpoint responds on `:5580`, a chip-tool test commission against the running controller succeeds.

**Out of scope this phase:** any IoT-AtHome bus traffic, any plugin code.

### Phase 2 — WS→MQTT shim + matter-bridge plugin scaffold

Build the bridge between the controller's WebSocket and the canonical MQTT topic shape. WASM plugin scaffold compiles but doesn't translate yet.

**Tasks:**

- `tools/matter/wsmqtt.py` — ~50-line Python script subscribing to `python-matter-server`'s WS, republishing events on `matter/nodes/<id>/endpoints/<eid>/clusters/<cid>/<attr>`. Runs as a third compose service under the `matter` profile.
- `plugins/matter-bridge/` — scaffold complete: Cargo.toml, `.cargo/config.toml`, manifest.yaml (ABI 1.3.0+, mqtt.subscribe `matter/nodes/+/+/+/+/+`, bus.publish `device.matter.>`), `src/lib.rs` Guest impl with `init` + `on_mqtt_message` stubs that log + return Ok, README.md.
- Workspace `Cargo.toml` exclude list adds `plugins/matter-bridge` (mirror existing pattern).

**Acceptance:** `cargo build --release --target wasm32-wasip2` from `plugins/matter-bridge/` produces a `plugin.wasm`. `iotctl plugin install plugins/matter-bridge --allow-unsigned` succeeds. The plugin loads in the host but doesn't decode Matter events yet (just logs them to verify MQTT delivery path).

**Out of scope this phase:** translator logic for any specific Matter cluster.

### Phase 3 — Translator: 5 cluster types

Map the most common Matter clusters to canonical `iot.device.v1.EntityState` payloads. Mirrors the sdr433-adapter's "6 device shapes" deliverable.

**Tasks:**

- `plugins/matter-bridge/src/translator.rs` — pure-data + pure-fn module covering the five cluster types from the [Matter spec § 1.5 "Common Clusters"]:
  1. **OnOff (cluster 0x0006)** — boolean on/off, used by smart bulbs, plugs, switches.
  2. **LevelControl (cluster 0x0008)** — 0-254 level, used by dimmers + dimmable bulbs.
  3. **TemperatureMeasurement (cluster 0x0402)** — int16 temperature in 0.01 °C, used by sensors.
  4. **OccupancySensing (cluster 0x0406)** — bitmap occupancy, motion sensors.
  5. **BooleanState (cluster 0x0045)** — boolean state, contact / window sensors / leak detectors.
- `plugins/matter-bridge/src/state_publisher.rs` — emits one EntityState per cluster, mirrors the sdr433-adapter shape.
- Translator unit tests on each of the 5 cluster types: full envelope → expected canonical EntityState round-trip.

**Acceptance:** A commissioned Matter smart bulb's on/off state shows up on `device.matter.<id>.onoff.state` via `iotctl device list`. Bus-watcher auto-registers the device. The panel WS streams updates in real time. Cluster types beyond the five are logged at debug and dropped (not an error).

### Phase 4 — Commissioning UX in the gateway / panel

The first three phases assume devices are already commissioned via `chip-tool` or HA's Matter-server admin UI. Real users need an in-panel commissioning flow.

**Tasks:**

- `iot-gateway`: new endpoint `POST /api/v1/matter/commission` that takes a setup-code (the 11-digit pairing code on a Matter device's QR) and forwards to `python-matter-server`'s `commission_with_code` WS verb.
- `panel/src/pages/MatterCommission.tsx` — a single-button page: scan QR → POST → show progress.
- Re-uses existing OIDC bearer auth; no new auth surface.

**Acceptance:** Operator scans a Matter device's QR code in the panel, the device commissions, `iotctl device list` shows it within 30 s.

### Phase 5 — Bridge mode planning (deferred)

Document what bridge mode (exposing IoT-AtHome devices outward) would need, defer to its own ADR + M6 cert track. Touched only with a planning doc; no implementation.

## Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `python-matter-server` upstream drops a major version mid-arc | Low | Pin to a release tag in compose; bump deliberately |
| nRF52840 OTBR firmware flashing is an operator footgun | High | `tools/matter/flash-otbr.md` with screenshots + a `--dry-run` mode in the install script |
| WS→MQTT shim becomes a pipe-fitting maintenance burden | Medium | Keep it ~50 lines, no business logic, just topic translation. If it grows past 200 lines, rewrite as a proper Rust daemon. |
| Matter spec evolution between phases | Medium | Pin python-matter-server to a known-good tag; smoke-test against new spec versions before bumping |
| Thread mesh + WiFi 2.4 GHz interference | High | Document dongle placement (≥1 m from the WiFi AP); recommend OTBR on a separate Pi for production |
| User commissions 50+ devices and the controller's WS lags | Low | python-matter-server handles ~100 devices in HA deployments. Document the soft cap; test at Phase 4. |

## Definition of done (per phase)

| Phase | DoD |
|---|---|
| 1 | python-matter-server boots clean under `--profile matter`, OTBR dongle flashed, chip-tool test commission succeeds. |
| 2 | matter-bridge plugin loads, MQTT delivery path verified via log-only stub. |
| 3 | All 5 cluster-type translator tests pass. Live smart bulb commissioned via chip-tool publishes `device.matter.*` to the bus; panel WS sees updates. |
| 4 | QR-scan commissioning works end-to-end from panel. |
| 5 | Bridge-mode planning ADR draft (no impl). |

## Out of scope (the whole arc)

- **Bridge mode** (exposing devices outward to Apple/Google/Alexa) — needs CSA cert + vendor ID, M6+ scope, separate ADR.
- **Matter-over-Thread firmware development** — use upstream OTBR.
- **Production CSA certification** — M6+.
- **Any actual Matter wire-protocol implementation** — that's the sidecar's job.
- **More than 5 cluster types in Phase 3** — extension is straightforward (match arms in the catalog) once the architecture proves out on the first 5.

## Reference

- [ADR-0014](adr/0014-matter-integration-architecture.md) — architectural decision record
- [Matter Specification](https://csa-iot.org/all-solutions/matter/) — CSA's published spec
- [python-matter-server](https://github.com/home-assistant-libs/python-matter-server) — chosen sidecar
- [openthread/borderrouter](https://github.com/openthread/borderrouter) — OTBR firmware
- `plugins/zigbee2mqtt-adapter/` — existing sidecar+WASM-plugin reference pattern
- `plugins/sdr433-adapter/` — translator-pattern reference (M5a W3.4)
