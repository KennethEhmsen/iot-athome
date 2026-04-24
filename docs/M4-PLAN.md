# M4 Plan — M3 Carry-overs + Infrastructure Polish

**Starts:** post-`v0.3.0-m3` · **Target duration:** 2 weeks · **Anchor ADRs:** [0006](adr/0006-signing-key-management.md), [0008](adr/0008-error-handling.md), [0009](adr/0009-logging-and-tracing.md), [0011](adr/0011-dev-bus-auth.md), [0013](adr/0013-zigbee2mqtt-wasm-migration.md)

## Scope re-framing

The original roadmap listed M4 as "Edge-ML reference plugins" — water-meter CV,
mains-power 3-phase, heating, NILM. That scope is misaligned with what "a
milestone" can honestly ship: those items are hardware-bound (ESP32 firmware,
cameras, CT sensors, PT1000 thermistors) + model-bound (TFLite Micro digit
classifier, seq2point NILM base) + calendar-bound (dataset gathering + iteration).
Each of those four plugins is a project in itself.

M4 instead ships the **M3 carry-overs that close the platform story** +
a few architectural polish items. The edge-ML plugins move to **M5** (post-
voice) where they fit alongside NILM's own model work.

## Goal

Every item in the M3 retrospective's "architecture debts" list either
closes or gets a concrete named-slice plan on top of infrastructure that
genuinely exists. The release ships with:

- Every service's inbound hop scopes the upstream traceparent (client side
  was M3 W2.7 / W2.8 / post-v0.3; server side is M4).
- Plugins authenticate to NATS with their own per-plugin JWT — ADR-0011
  retires.
- `registry::upsert-device` host capability is formally deprecated with a
  log line; M5 removes it.
- Envoy fronts the gateway in the dev compose stack; `mTLS` terminates
  there, upstream TCP to the gateway is plaintext local.
- A second real adapter (433-SDR) proves the ADR-0013 MQTT capability
  path is plugin-author reusable.
- Gateway WS replay covers wildcard filters via JetStream
  `DeliverLastPerSubject`.

## Acceptance criterion

> From `just dev` cold, a panel session shows a cross-service trace tree
> (panel → gateway → registry gRPC → bus → rule engine) with matching trace
> IDs in Tempo; the z2m adapter's NATS connection uses its per-plugin JWT
> (no more `no_auth_user`); the 433-SDR adapter installs + publishes
> `device.sdr433.*.state` messages indistinguishable in shape from z2m; the
> panel reload pulls the last-known value for every device subscription
> (wildcards included). `v0.4.0-m4` signs + Rekor-logs cleanly.

## Week-by-week

### W1 — Observability + auth closure

- [ ] **gRPC server interceptor on iot-registry** — symmetric to the
      client-side interceptor post-v0.3 shipped on iot-gateway. Extracts
      `traceparent` from tonic request metadata, scopes the handler via
      `iot_observability::traceparent::with_context`. Every handler in
      `crates/iot-registry/src/service.rs` inherits.
- [ ] **Broker JWT bootstrap wiring** (retires ADR-0011).
  - `iotctl plugin install` post-install step: if an operator seed is
    configured (via `IOT_NATS_OPERATOR_SEED`), mint a user JWT from the
    plugin's `nats.nkey` + `acl.json` via the W1.3.1 `iot_bus::jwt` minter.
    Write `<plugin_dir>/<id>/nats.creds` alongside the existing nkey.
  - `deploy/compose/nats/nats-server.conf` — switch from `no_auth_user dev`
    to operator-JWT mode (load the operator JWT + account JWT that the dev
    cert mint script now produces).
  - Dev cert mint script gains an "operator/account keypair" step that
    writes the operator seed alongside the mTLS bundle.
  - ADR-0011 gets a status-change commit to "Superseded by [M4-W1.2]".
- [ ] **`registry::upsert-device` deprecation log**. Host capability still
      works; plugins that call it log a one-shot `registry.deprecated`
      warning per plugin id per host lifetime. M5 removes the capability.
- [ ] **Real CEL interpreter swap** — replace the hand-rolled grammar in
      `iot-automation` with the `cel-interpreter` crate (vetted vs. our
      M3 risk-table concerns now that the rest of M3 is stable). Behind
      the existing `parse`/`eval_bool` facade — zero rule-author changes.
      If the crate's CEL surface is too churny, stay on the hand-roll and
      document the M5 revisit.

### W2 — Deployment + 2nd adapter + wildcard replay

- [ ] **Envoy + mTLS frontend**.
  - `deploy/compose/envoy/envoy.yaml` — inbound `:8443` → gateway `:8080`
    (plaintext internal); upstream registry gRPC via mTLS with a dedicated
    Envoy component cert (new entry in the dev-cert mint script).
  - `deploy/compose/dev-stack.yml` — Envoy added as a service.
  - Gateway binds to `127.0.0.1:8080` only; Envoy is the sole external
    surface.
- [ ] **433-SDR adapter** (chosen over Z-Wave because it's a cleaner
      MQTT-bridge scope — rtl_433 → MQTT → plugin, no JS sidecar).
  - New WASM plugin `plugins/sdr433-adapter/` mirroring the z2m shape:
    subscribes to `rtl_433/+` topic pattern via the M2 mqtt host
    capability, parses the rtl_433 JSON envelope, maps known device
    families (temperature sensors, water-meter pulse, energy monitors)
    to the canonical `device.sdr433.<model>-<id>.*.state` subjects.
  - Manifest declares `mqtt.subscribe: [rtl_433/+]`, `bus.publish:
    [device.sdr433.>]`, `registry: { upsert: true }`.
  - Translator tests on the 6 most common rtl_433 device shapes.
  - `iotctl plugin install plugins/sdr433-adapter --allow-unsigned` →
    adapter loads alongside z2m + demo-echo.
- [ ] **Wildcard last-msg replay**. Gateway WS handler gains a JetStream
      ephemeral consumer with `DeliverLastPerSubject` policy for wildcard
      subscriptions (e.g. `device.>`). Panel's default subscription
      replays every known subject's last state on connect.

## Risks

| Risk | Spike | Resolution |
|---|---|---|
| `cel-interpreter` crate API churn | 1 day W1 | Stay on hand-roll; document M5 revisit. The `parse`/`eval_bool` facade is drop-in-replaceable either way. |
| NATS server config flip breaks `just dev` loop | 4h W1 | Keep the old `no_auth_user` config in a `config/dev-insecure.conf` sibling, flip via an env var in `just dev` while the operator-JWT path beds in |
| rtl_433 message shape variance across device families | 1 day W2 | Start with the three most-common sensor classes; expand the translator as real devices arrive. Unit tests per class. |
| Envoy + gateway TLS handshake on Docker network | 2h W2 | Reuse the dev-cert mint script's pattern; Envoy is just another component cert |

## Out of scope for M4 — explicitly deferred

Each has a stated milestone target.

| Item | Target | Why not M4 |
|---|---|---|
| **TimescaleDB optional backend** | M5 | Substantial new storage impl + sqlx feature-flag work; deserves its own session. Not user-visible — the current SQLite backend is fine at M4 scale. |
| **Command Central v1 PWA** | M5 | Full PWA backend + frontend + ephemeral auth + kiosk mode is a multi-session build-out by itself. Proper M5 scope alongside the voice pipeline. |
| **Water-meter CV plugin** | M5 (with NILM) | ESP32-CAM firmware + TFLite Micro digit classifier + calibration wizard. Hardware + dataset + model work. |
| **Mains-power 3-phase plugin** | M5 (with NILM) | ESP32-S3 + ATM90E32 firmware + CT/VT provisioning + NILM training loop. Full hardware project. |
| **Heating flow/return plugin** | M5 (with NILM) | PT1000 + MAX31865 + ΔT + COP derivation. Piggybacks on power-meter hardware. |
| **NILM training loop** | M5 | Hub-side Python + ONNX + per-household fine-tune. Depends on having the three sensor plugins to gather data from. |
| **Z-Wave adapter** | Post-M4 backlog | Second-adapter slot goes to 433-SDR (simpler data model, no JS sidecar). Z-Wave is valuable but duplicates the proof-of-capability rtl_433 gives us. |
| **Fuel refueling between plugin host calls** | M5 supervisor work | Small infra piece; want to pair it with per-plugin CPU telemetry in M5 |
| **MQTT broker sub-refcount / unsubscribe on plugin exit** | Backlog | No correctness impact; tidying only |

## Definition of done

- [x] gRPC server interceptor wired on iot-registry; cross-service trace
      tree visible in a Tempo screenshot.
- [x] ADR-0011 status-change commit: superseded by M4 W1.2.
- [x] `iotctl plugin install` writes `nats.creds` when an operator seed
      is configured; NATS server accepts the JWT.
- [x] Envoy fronts `:8443` on `just dev`; panel connects via Envoy.
- [x] `iotctl plugin list` shows 3 running plugins (demo-echo, z2m,
      sdr433).
- [x] Panel reload after 30s idle: wildcard subscription replays every
      device's last state within 1s.
- [x] `v0.4.0-m4` tag: sign + reproducibility + cosign Rekor all green;
      SLSA provenance still advisory per the M2 repo-visibility
      workaround.
