# M5 Plan — Platform Hardening + Edge-ML (split)

**Starts:** post-`v0.4.0-m4` · **Target duration:** 7 weeks total · **Candidate split:** M5a (code-only, 4 weeks) + M5b (hardware + voice, 3 weeks)
**Anchor ADRs:** [0011](adr/0011-dev-bus-auth.md) (retires in M5a W1), [0004](adr/0004-subjects.md), [0006](adr/0006-signing-key-management.md), [0008](adr/0008-error-handling.md), [0013](adr/0013-zigbee2mqtt-wasm-migration.md)

## Scope re-framing

The M4 retrospective consolidated 17 items into M5. That's ~7 weeks of
scope, not the originally-planned 4. This plan makes the split
**formal** so each half can ship on its own cadence:

| Half | Focus | Feasibility | Shipping target |
|---|---|---|---|
| **M5a** | Code-only platform closures + one new WASM adapter | Session-feasible, multi-session | `v0.5.0-m5a` |
| **M5b** | Hardware-bound edge-ML plugins + voice pipeline + NILM training | Needs hardware + firmware + dataset iteration | `v0.6.0-m5b` |

The split mirrors what happened in M4: edge-ML deferred because
shoehorning hardware+models+datasets into a code-only milestone
produces scaffolds, not plugins. M5a takes everything that *can* be
honestly shipped from a laptop; M5b is the hardware-iteration arc
with its own calendar.

## M5a — Goals

The M4 retrospective's architectural-debts table is the scoring
rubric:

| M4 debt # | Item | Retires with |
|---|---|---|
| 1 | Broker JWT bootstrap wiring | M5a W1 |
| 4 | Real CEL interpreter swap | M5a W2 |
| 5 | Wildcard-filtered last-msg replay | M5a W2 |
| 6 | Fuel refueling between plugin host calls | M5a W3 |
| 7 | MQTT sub-refcount / unsubscribe on plugin exit | M5a W3 |
| 8 | Permissive Mosquitto ACL | M5a W3 |
| 10 | `registry::upsert-device` capability removal | M5a W1 (ABI 1.3.0 bump) |

Plus the original-M4 carry-overs that are hardware-free:

- **2nd WASM adapter** (`sdr433-adapter`) — proves MQTT-capability reuse.
- **TimescaleDB optional backend** — long-term time-series retention.
- **Command Central v1 PWA** — per-room kiosk, ephemeral per-person auth, proximity wake.

## M5a — Acceptance criterion

> From `just dev` cold, every plugin's NATS connection uses a
> per-plugin user JWT minted at install time (no more
> `no_auth_user: dev`). ADR-0011 status flips to Superseded. The
> automation engine runs rules through the real `cel-interpreter`
> crate, not the hand-rolled subset. Panel reload pulls last-known
> values for wildcard subscriptions (`device.>`). `iotctl plugin
> list` shows 3 running plugins (demo-echo, z2m, sdr433). An operator
> can flip `IOT_TIMESCALE_URL` to promote historical retention from
> SQLite to Timescale without code changes. Command Central v1 boots
> on a fresh tablet, prompts for an ephemeral per-person auth, and
> shows live device state from its assigned room. `v0.5.0-m5a` signs
> + Rekor-logs cleanly.

## M5a — Week-by-week

### W1 — Auth + capability-removal closures

- [ ] **Broker JWT bootstrap wiring** (retires ADR-0011).
  - Extend `iot_bus::jwt` with `issue_account_jwt(operator, account_public, name, limits, iat)` — symmetric to the existing user-JWT minter. Operator-signed account JWTs are what the NATS server config loads via `resolver_preload`.
  - `tools/devcerts/mint.sh` gains an **operator + account** keypair step. Writes `tools/devcerts/generated/nats/operator.nk`, `operator.jwt`, `iot-account.nk`, `iot-account.jwt` alongside the existing mTLS bundle.
  - `deploy/compose/nats/nats.conf` switches from `accounts { IOT { users: [...] } } no_auth_user: dev` to:
    ```
    operator: /etc/nats/jwt/operator.jwt
    resolver: { type: MEMORY }
    resolver_preload: { <account_pub>: <account_jwt> }
    ```
  - `deploy/compose/dev-stack.yml` NATS service mounts the JWT directory.
  - `iotctl plugin install` post-install step: when `IOT_NATS_OPERATOR_SEED` env is set (or `--operator-seed <path>` passed), read the newly-installed plugin's `nats.nkey` + `acl.json`, call `iot_bus::jwt::issue_user_jwt(account_from_env, nkey_public, plugin_id, acl, now())`, write `<plugin_dir>/<id>/nats.creds` in NATS creds-file format.
  - `iot_bus::Bus::connect` gains a `creds_path: Option<PathBuf>` option; uses `async_nats::ConnectOptions::credentials_file` when set.
  - Supervisor reads `nats.creds` if present, falls back to mTLS-only (dev pre-flip) if not.
  - ADR-0011 status-change commit: "Superseded by [M5a-W1]".

- [ ] **`registry::upsert-device` capability removal** — M4 shipped the one-shot deprecation warn; M5a removes the host import entirely.
  - `schemas/wit/iot-plugin-host.wit` bumps ABI version annotation to `1.3.0`; drops `upsert-device` from the `registry` interface.
  - `crates/iot-plugin-host/src/component.rs` drops the handler + the `REGISTRY_DEPRECATED_LOGGED` gate.
  - `crates/iot-plugin-sdk-rust/src/lib.rs` drops the `registry::upsert_device` re-export.
  - z2m adapter verification: re-runs with only bus-publish path; device shows up via `iot-registry` bus-watcher auto-register.
  - `docs/adr/0012-plugin-binding-layer.md` adds a note to the "ABI versioning" section documenting the 1.2.0 → 1.3.0 bump.

### W2 — Rule engine + replay completeness

- [ ] **Real CEL interpreter swap**.
  - Replace the hand-rolled grammar in `iot-automation/src/expr.rs` with the `cel-interpreter` crate (vetted vs. our M3 risk-table concerns — re-evaluate the API churn concern now that 0.x has stabilised).
  - Keep the existing `parse(source: &str) -> Result<Expr>` and `eval_bool(expr, ctx) -> bool` facade unchanged — zero rule-author changes, zero YAML-rule diff.
  - If `cel-interpreter` is still too churny, stay on the hand-roll and document the next revisit point. The facade is drop-in-replaceable either way.
  - Rule-parser test suite stays as-is; passing it under the new interpreter is the acceptance gate.

- [ ] **Wildcard-filtered last-msg replay**.
  - `iot_bus::jetstream` gains `last_state_wildcard(pattern: &str) -> Stream<Message>` backed by a JetStream ephemeral consumer with `DeliverPolicy::LastPerSubject` + `FilterSubject` = pattern.
  - Gateway WS handler: when a client subscribes to a wildcard subject (`device.>`, `device.z2m.*.state`), the replay phase uses the wildcard helper to stream all known subjects' last state before the live firehose kicks in.
  - Test: panel reload at 30s idle with 12 simulated devices across 3 integrations → all 12 last-known states arrive within 1s; no duplicates during the live/replay overlap.

### W3 — Plugin-host polish + broker ACL + 2nd adapter

- [ ] **Fuel refueling between plugin host calls** (M2 carry-over).
  - `iot_plugin_host::supervisor::Instance` gains a `fuel_budget_per_call: u64` + refuels the store between each guest invocation (init / on_message / on_tick).
  - Default budget: 10M fuel (ballpark ~100ms CPU per call for typical adapter work).
  - Exhausting fuel surfaces as a `PluginError::FuelExhausted` + crash counter increment; existing exponential-backoff restart applies.

- [ ] **MQTT sub-refcount + unsubscribe on plugin exit**.
  - `iot_plugin_host::capabilities::mqtt` maintains a per-topic refcount. Plugin A + B both subscribing to `rtl_433/+` increments to 2; A exiting drops to 1; MQTT unsubscribe fires only at 0.
  - Plugin crash or stop triggers the bookkeeping path via the supervisor's existing exit hook.

- [ ] **Mosquitto ACL from manifests**.
  - `deploy/compose/mosquitto/conf.d/acl` generated from installed plugin manifests' `mqtt.{subscribe,publish}` lists.
  - `iotctl plugin install` / `uninstall` invalidate the ACL cache; a small supervisor service regenerates on change + sends Mosquitto `SIGHUP`.
  - Dev-shortcut: `just dev` regenerates ACL on start based on `plugins/*/manifest.yaml`.

- [ ] **`sdr433-adapter`** (2nd WASM adapter; originally M4 W2).
  - New crate under `plugins/sdr433-adapter/` targeting wasm32-wasip2. Mirrors the z2m adapter shape.
  - Manifest: `mqtt.subscribe: [rtl_433/+]`, `bus.publish: [device.sdr433.>]`.
  - rtl_433 JSON envelope → canonical `iot.device.v1.EntityState` on `device.sdr433.<model>-<id>.<channel>.state`.
  - Translator tests on 6 most common rtl_433 device shapes (temperature, humidity, door-window contact, TPMS, rain gauge, water-meter pulse).
  - `iotctl plugin install plugins/sdr433-adapter --allow-unsigned` → adapter loads alongside z2m + demo-echo.

### W4 — Storage + Command Central v1

- [ ] **TimescaleDB optional backend**.
  - `iot-registry` gains a sqlx feature flag `timescale` alongside the existing SQLite path. Behind `#[cfg(feature = "timescale")]` when Timescale URL is set.
  - History store schema: single `entity_state_history(device_id, subject, ts, payload_json)` hypertable with (device_id, ts) chunking.
  - Retention: configurable via env `IOT_HISTORY_RETENTION_DAYS` (default 180).
  - `deploy/compose/dev-stack.yml` adds a `timescale` service (postgres:16 + timescaledb extension), off by default — opt-in via `compose --profile history up`.
  - `iot-gateway` exposes `GET /api/v1/devices/{id}/history?from=&to=` returning recent states from the active history backend.

- [ ] **Command Central v1 PWA**.
  - New workspace: `apps/command-central/` — React + Vite PWA mirroring the panel's architecture, specialised for tablet kiosks.
  - Ephemeral per-person auth: PIN pad at idle → 15 min session → auto-lock. No user-server round-trip; gateway issues a short-lived room-scoped token.
  - Proximity-wake: tablet's ambient-light / front-camera motion trigger un-dims the UI (`no-sleep` + passive motion JS).
  - Room scoping: device tiles filtered by the `room` metadata field; config via URL `?room=kitchen`.
  - No M5b voice hooks yet — the voice pipeline subscribes to the same bus subjects Command Central renders, no UI contract between them.

## M5a — Risks

| Risk | Spike | Resolution |
|---|---|---|
| `cel-interpreter` API still churning | 1 day W2 | Stay on hand-roll; the facade makes either path drop-in. Document a next-revisit deadline. |
| NATS operator-JWT config breaks `just dev` loop | 4h W1 | Keep the old dev config at `deploy/compose/nats/nats-insecure.conf`, swap via env var while the operator-JWT path beds in. Rollback is 1 line. |
| rtl_433 device-family variance | 1 day W3 | Start with 3 most-common sensor classes; expand translator as real devices arrive. Unit tests per class. |
| TimescaleDB optional-feature-flag complexity | 2 days W4 | Gate aggressively behind `#[cfg(feature = "timescale")]`. CI builds both variants. SQLite stays the default. |
| Command Central tablet-target browser quirks | 2 days W4 | Build to PWA spec + test on Chromium + Firefox desktop; defer iOS/Android certification to M6. |

## M5b — Scope (preview, to be detailed at M5a retro)

Three arcs running in parallel on separate calendars:

**Edge-ML plugin family** (design doc §7-§9):
- Water-meter CV (ESP32-CAM + TFLite Micro digit classifier + calibration wizard)
- Mains-power 3-phase (ESP32-S3 + ATM90E32 + CT/VT provisioning)
- Heating flow/return ΔT + COP (shared ESP32-S3 carrier board with power-meter)
- NILM training loop (hub-side Python + ONNX + per-household fine-tune)

**Voice pipeline** (original M5):
- openWakeWord wake detection
- Whisper/Vosk STT
- Closed-domain NLU dispatcher (lights / scenes / sensors / power)
- Piper TTS responses
- llama.cpp Q4 fallback on Pi 5

**Each is iteration-heavy, not session-feasible.** M5b plan + RFP-style
firmware-partner scoping written at M5a retro.

## Out of scope for M5a — explicitly deferred

| Item | Target | Why not M5a |
|---|---|---|
| **All edge-ML plugins** | M5b | Hardware + firmware + trained models + datasets. |
| **Voice pipeline** | M5b | Wake + STT + NLU + TTS + LLM fallback; latency-sensitive; separate calendar. |
| **NILM training loop** | M5b | Needs the three sensor plugins shipping first to generate training data. |
| **Matter certification** | Post-M6 | CSA membership + test-lab partner + 6-month timeline. |
| **SLSA provenance hard-gate** | M6 | Still `continue-on-error: true` from M2 private-repo block. |
| **Multi-home / tenancy (CRDT federation)** | Post-M6 | Design open; not user-requested at the scale we operate. |

## Definition of done (M5a)

- [ ] `iotctl plugin list` shows 3 running plugins (demo-echo, z2m, sdr433), each with a `nats.creds` file on disk.
- [ ] `just dev` cold-start: no `no_auth_user` line in NATS config; every client hands the server a JWT.
- [ ] ADR-0011 file header shows `Status: Superseded by [M5a-W1]`.
- [ ] `wasm32-wasip2` ABI version annotation in `iot-plugin-host.wit` reads `1.3.0`; `registry::upsert-device` capability is gone; z2m adapter works through the bus-watcher auto-register path only.
- [ ] Workspace tests count grows by the new automation suite under the real CEL interpreter + sdr433 translator tests + history-store repo tests (target: 140-160).
- [ ] `v0.5.0-m5a` tag: sign + reproducibility + cosign Rekor all green; SLSA provenance still advisory per the M2 repo-visibility workaround (M6 fix).
- [ ] Panel reload after 30s idle: wildcard subscription replays every device's last state within 1s.
- [ ] Command Central v1 boots on a fresh tablet, prompts for an ephemeral per-person auth, and shows live device state from its assigned room.
- [ ] `docs/M5a-RETROSPECTIVE.md` + `docs/STATUS.md` refresh + M5b plan draft.
