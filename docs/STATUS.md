# IoT-AtHome — Project Status

**As of:** 2026-04-24 (post-M5a)
**Head:** `24e7f3e` (M5a W4.1: TimescaleDB optional long-term history backend)
**Shipped releases:** `v0.1.0-m1` (2026-04-21), `v0.2.0-m2` (2026-04-23), `v0.3.0-m3` (2026-04-23), `v0.4.0-m4` (2026-04-24), `v0.5.0-m5a` (2026-04-24)
**Next release target:** Command Central v1 PWA (own milestone, M5a.5 / pre-M5b)
**Commits since M4:** 8

> This file is a point-in-time snapshot. Regenerate before every milestone
> boundary. Consult `docs/Mn-PLAN.md` + `docs/Mn-RETROSPECTIVE.md` + `docs/adr/`
> for the canonical state — this doc is a navigation map, not a source of truth.

---

## Executive summary

M5a **shipped** — the code-only half of M5, formally split out at
planning time so each half can ship on its own cadence. **15 of 17
M5a items closed in 8 commits over ~1 day**, plus the 2 deferred
items have explicit next-milestone targets:

* **Closed (15):** broker decentralized auth (retires ADR-0011);
  ABI 1.3.0 removing `registry::upsert-device`; CEL interpreter
  swap (cel-interpreter 0.10 behind the existing facade); wildcard
  JetStream replay; per-call fuel refuel; MQTT sub-refcount with
  last-subscriber `UNSUBSCRIBE`; manifest-driven Mosquitto ACL via
  `iotctl mosquitto regen-acl`; the `sdr433-adapter` 2nd WASM plugin
  (now 3 plugins ship: demo-echo, z2m, sdr433); optional
  TimescaleDB long-term history backend (`iot-history` crate +
  gateway `GET /devices/{id}/history` endpoint + compose `history`
  profile).
* **Deferred:** Command Central v1 PWA (multi-session — full PWA
  app + service worker + ephemeral PIN auth + room-scoped JWTs +
  tablet-friendly tile UI; gets its own dedicated milestone). M5b
  hardware/voice arcs unchanged (edge-ML plugin family, voice
  pipeline).

**8 of 10 M4-retro architectural debts retired in M5a.** Remaining:
#9 SLSA hard-gate (M6 target), #2/#3 already shipped pre-M5a.

Local verification: **138/138 workspace tests + 14 plugin-local
sdr433 tests, clippy `-D warnings` clean, cargo-deny clean
(7 documented advisory ignores).**

---

## Milestone status at a glance

| Milestone | State | Anchor doc |
|---|---|---|
| **M1 — Walking skeleton** | ✅ shipped 2026-04-21 (`v0.1.0-m1`) | `docs/M1-RETROSPECTIVE.md` |
| **M2 — Plugin SDK + Wasmtime Host + Real Plugins** | ✅ shipped 2026-04-23 (`v0.2.0-m2`) | `docs/M2-RETROSPECTIVE.md` |
| **M3 — Automation Engine + Observability Foundations** | ✅ shipped 2026-04-23 (`v0.3.0-m3`) | `docs/M3-RETROSPECTIVE.md` |
| **M4 — M3 carry-over closures** | ✅ shipped 2026-04-24 (`v0.4.0-m4`) | `docs/M4-RETROSPECTIVE.md` |
| **M5a — Platform closure + 2nd adapter + optional history** | ✅ shipped 2026-04-24 (`v0.5.0-m5a`) | `docs/M5a-RETROSPECTIVE.md` |
| **Command Central v1 PWA** | 📐 designed (M5-PLAN W4); next dedicated arc | `docs/M5-PLAN.md` § W4 |
| **M5b — Edge-ML + voice + NILM** | 📐 designed | `docs/M5-PLAN.md` § M5b preview |
| **M6 — Hardening + certification** | 📐 designed | design doc §10 |

---

## What M5a delivered

### Broker decentralized auth (W1.1-7)

`iot_bus::jwt` extended with `issue_account_jwt` + `format_creds_file`
+ `verify_account_jwt`. `iotctl nats {bootstrap, mint-user}`
subcommands. `iotctl plugin install --account-seed` post-install
hook (env `IOT_NATS_ACCOUNT_SEED`). `mint.sh` calls `iotctl nats
bootstrap` after the mTLS bundle pass. NATS server config flips
from `accounts {} no_auth_user: dev` to `include
"certs/resolver.conf"`. `iot_bus::Config` gains `creds_path` (env
`IOT_NATS_CREDS`); `Bus::connect` calls
`ConnectOptions::credentials_file` when set. **ADR-0011 status →
Superseded by [M5a W1].**

### ABI 1.3.0 — `registry::upsert-device` removed (W1.8)

Breaking package version bump in `schemas/wit/iot-plugin-host.wit`.
`interface registry` deleted; `import registry` dropped from
`world plugin`. Per-package note explains the major-N + N-1
support rule. `iot-plugin-host` drops the entire `registry::Host`
impl + the `REGISTRY_DEPRECATED_LOGGED` OnceLock from the M4
warn-and-continue path. z2m adapter migrated: manifest abi.version
1.2.0 → 1.3.0, package version 0.2.0 → 0.3.0,
`capabilities.registry` block dropped, `registry::upsert_device`
call removed. The bus-watcher auto-register path takes over.

### Real CEL interpreter (W2.1-2)

`iot-automation/src/expr.rs` now wraps `cel-interpreter` 0.10
behind the same `parse(src) → Expr` / `eval_bool(&Expr, &json) →
bool` facade. New rule-author surface: `in`, `has(...)`, `size(...)`,
list literals, full arithmetic. Defensive `std::panic::catch_unwind`
wrap on `Program::compile` because cel-interpreter's antlr4rust
dep panics on input the lexer fails to tokenise. Workspace
`Cargo.toml` profile overrides cap antlr4rust / cel-* debug info
to `line-tables-only` (Windows MSVC PDB symbol cap LNK1318).

### Wildcard JetStream replay (W2.3-4)

`iot_bus::jetstream::last_state_wildcard(pattern)` opens an
ephemeral consumer with `DeliverPolicy::LastPerSubject` +
`filter_subject = pattern`, drains every distinct subject's last
message exactly once. Gateway WS handler branches on `*` / `>` in
the topic filter — concrete subjects use the M3 `last_state()`
single-fetch path, wildcards use the new helper.

### Plugin-host polish (W3.1-3)

* **Fuel refueling per call** — `runtime::DEFAULT_FUEL_PER_CALL = 10M`
  (overridable via `manifest.resources.fuel_max`). `set_fuel` before
  every guest invocation.
* **MQTT sub-refcount** — `MqttBroker.filter_refcount: HashMap<String,
  usize>`. Only 0→1 issues `SUBSCRIBE`; only 1→0 issues
  `UNSUBSCRIBE`. `MqttRouter::unregister(plugin_id)` returns the
  filters dropped so the supervisor can decrement.
* **Manifest-driven Mosquitto ACL** — new
  `iot_plugin_host::mqtt_acl::generate(user, &[&Manifest])` +
  `iotctl mosquitto regen-acl` subcommand. Walks installed
  plugins, unions their `mqtt.{subscribe,publish}` lists, writes a
  Mosquitto-2.0 ACL file. Fail-closed when zero plugins installed.

### `sdr433-adapter` (W3.4)

New WASM plugin under `plugins/sdr433-adapter/` against ABI
**1.3.0**. Mirrors z2m shape. MQTT subscribe `rtl_433/+`,
JSON-envelope translator → canonical
`device.sdr433.<model>-<id>[-<channel>].<entity>.state` publishes.
Translator catalog covers 6 device shapes: temperature/humidity,
door/window contact, TPMS pressure, rain gauges, energy/power
monitors, water-meter pulse counters. `device_id_from_envelope`
builds NATS-safe canonical ids. **3 plugins now ship in parallel**
(demo-echo, z2m, sdr433-adapter).

### Optional TimescaleDB long-term history (W4.1)

New `iot-history` crate. `HistoryStore` wrapping a sqlx PgPool.
`ensure_schema()` creates the `entity_state_history` table +
`(device_id, ts DESC)` index + a guarded Timescale hypertable
conversion (works on plain Postgres too — keeps the table without
chunk pruning). `from_env()` reads `IOT_TIMESCALE_URL`, returns
`Ok(None)` when unset (M3 SQLite-only path stays default).

The registry's bus_watcher mirrors every recognised `device.>`
publish via `BusWatcher::with_history(HistoryStore)`. The gateway
exposes `GET /api/v1/devices/{id}/history?from=&to=&limit=` with
stable error codes (`history.disabled` → 503,
`history.bad_from`/`history.bad_to` → 400, `history.query_failed`
→ 502). Compose stack adds a `timescale` service behind a
`history` profile.

---

## Architecture snapshot

### Code surface (14 crates + 3 shipping plugins + 1 scaffold + panel)

| Crate | M5a touches |
|---|---|
| `iot-bus` | `jwt::issue_account_jwt` + `format_creds_file` + `verify_account_jwt`; `Config::creds_path` + `Bus::connect` calls `credentials_file`; `jetstream::last_state_wildcard` ephemeral consumer. |
| `iot-history` | **New crate.** sqlx-Postgres `HistoryStore` with hypertable schema + record + fetch_range + prune_older_than + `from_env()`. |
| `iot-registry` | `bus_watcher::with_history(HistoryStore)` builder; mirrors `device.>` publishes when configured. `lib.rs` run() init reads `IOT_TIMESCALE_URL`. |
| `iot-gateway` | New `GET /api/v1/devices/{id}/history` handler. `AppState.history: Option<HistoryStore>`. WS handler wildcard-replay branch. |
| `iot-automation` | `expr.rs` — full crate-roll into cel-interpreter 0.10 wrap behind unchanged facade. `cel-interpreter` workspace dep. |
| `iot-plugin-host` | `runtime::DEFAULT_FUEL_PER_CALL` + per-call refuel; `MqttBroker.filter_refcount` + `unsubscribe_filter`; `MqttRouter::unregister` returns dropped filters; `registry::Host` impl removed. **New `mqtt_acl` module.** |
| `iot-cli` | New `iotctl nats {bootstrap, mint-user}` + `iotctl mosquitto regen-acl` subcommands. `plugin install --account-seed` flag + post-install creds-mint hook. |
| `iot-plugin-sdk-rust` | Doc comment refresh for ABI 1.3.0 surface. |

### End-to-end paths (current as of v0.5.0-m5a)

```
Plugin install:
  iotctl nats bootstrap → operator/account keys + JWT + resolver.conf
  iotctl plugin install <bundle> --account-seed <path>
    → cosign verify → SBOM scan → manifest parse →
      generate per-plugin nkey + ACL snapshot →
      mint user JWT against account → write nats.creds (0600)

Plugin runtime:
  iot-plugin-host → reads IOT_NATS_CREDS at connect →
    NATS server validates JWT against operator-signed account →
    accepts connection.
  Per call: store.set_fuel(fuel_per_call) → invoke guest →
    capability checks on every host call.
  Plugin exits → router.unregister(plugin_id) → broker.unsubscribe_filter()
    for each filter (last subscriber leaving stops broker delivery).

Bus → registry → optional history:
  device.<plugin>.<id>.<entity>.state arrives →
    bus_watcher.handle() bumps last_seen / auto-registers →
    if history.is_some(): history.record(device_id, subject, payload)
      → entity_state_history hypertable INSERT.

Panel reload → gateway WS handler subscribes →
  if topics contains '*' or '>': last_state_wildcard(topics) →
    drain every subject's last message → forward to client.
  else: last_state(topics) single-fetch → forward.
  Then live subscription stream.
```

### Security & supply-chain posture

| Control | State | Notes |
|---|---|---|
| mTLS on every internal hop | ✅ | Unchanged from M2 |
| Plugin sig verify at install | ✅ | Cosign ECDSA-P256 (pinned pubkey) |
| SBOM CVE gate at install | ✅ | CycloneDX `.vulnerabilities[]` |
| Audit hash chain tamper-detectable | ✅ | JCS + verify re-computes |
| Per-plugin NATS identity | ✅ | **NEW M5a** — `iotctl nats bootstrap` + per-plugin user JWTs |
| End-to-end traceparent | ✅ | server-side gRPC interceptor M4 closure |
| `registry::upsert-device` removed | ✅ | **M5a W1.8** ABI 1.3.0 |
| Real CEL interpreter | ✅ | **NEW M5a** — cel-interpreter 0.10 |
| Wildcard JetStream replay | ✅ | **NEW M5a** |
| Per-call fuel refuel | ✅ | **NEW M5a** — 10M default, manifest-overridable |
| MQTT sub-refcount + exit-unsub | ✅ | **NEW M5a** |
| Manifest-driven Mosquitto ACL | ✅ | **NEW M5a** — `iotctl mosquitto regen-acl` |
| TimescaleDB long-term history | ✅ | **NEW M5a** — opt-in via `IOT_TIMESCALE_URL` |
| Cosign keyless / Rekor | ⏸ | M6 |
| SLSA provenance hard-gate | ⏸ | M6 (still `continue-on-error`) |

---

## ADR index

13 ADRs accepted. **ADR-0011 (dev bus auth) status flipped to
Superseded by [M5a W1]** — full retirement note in the ADR file
documents the two-step crypto-then-wiring path. No new ADRs in M5a.

---

## Test trajectory

| Boundary | Workspace tests |
|---|---|
| M1 shipped | 12 |
| M2 shipped | 58 (+ 3 plugin-local) |
| M3 shipped | 111 |
| M4 shipped | 111 (composition of tested pieces) |
| M5a W1 | 118 (+7 net; –2 capability tests on removed `registry::upsert`) |
| M5a W2 | 122 (+4 CEL surface) |
| M5a W3 (1+2) | 135 (+13: 5 mqtt refcount, 5 mqtt_acl, 3 mosquitto regen-acl) |
| M5a W3.4 | 135 + 14 sdr433 plugin-local |
| **M5a W4.1** | **138** (+3: 2 iot-history + 1 bus_watcher history-token) |

Zero workspace flakes during M5a. All clippy / deny gates clean on
every push.

---

## Architectural debts (post-M5a)

Rolled forward with M5a closures marked.

| # | Debt | Status |
|---|---|---|
| 1 | Broker JWT bootstrap wiring | ✅ **shipped M5a W1**; ADR-0011 → Superseded |
| 2 | Subscriber-loop traceparent wrappers | ✅ shipped post-v0.3.0-m3 |
| 3 | gRPC traceparent interceptors | ✅ client + server |
| 4 | Real CEL interpreter | ✅ **shipped M5a W2** |
| 5 | Wildcard last-msg replay | ✅ **shipped M5a W2** |
| 6 | Fuel refuel between plugin host calls | ✅ **shipped M5a W3** |
| 7 | MQTT sub-refcount / unsubscribe | ✅ **shipped M5a W3** |
| 8 | Permissive Mosquitto ACL | ✅ **shipped M5a W3** |
| 9 | SLSA provenance `continue-on-error` | M6 target |
| 10 | `registry::upsert-device` host capability | ✅ **removed M5a W1.8** (ABI 1.3.0) |

Net 8 of 10 retired. Remaining: #9 SLSA M6.

---

## What ships next

**Command Central v1 PWA** — own dedicated milestone (M5a.5 or
pre-M5b). Multi-session by itself: full PWA workspace + service
worker + gateway PIN-issue endpoint + ephemeral room-scoped JWTs
+ tablet-friendly tile UI + WS auth re-wire. Scope unchanged from
`docs/M5-PLAN.md` § W4.

**M5b** — hardware-bound arcs running on separate calendars:

* **Edge-ML plugin family** (design doc §7-§9):
  - Water-meter CV (ESP32-CAM + TFLite Micro digit classifier)
  - Mains-power 3-phase (ESP32-S3 + ATM90E32)
  - Heating flow/return ΔT + COP
  - NILM training loop (hub-side Python + ONNX)
* **Voice pipeline** (original M5):
  - openWakeWord, Whisper/Vosk, closed-domain NLU, Piper TTS,
    llama.cpp Q4 fallback

**M6** — Hardening + certification (per design doc §10):
- Third-party pen test
- ETSI EN 303 645 + OWASP ASVS L2 walkthrough
- SLSA L3 hard-gate (debt #9 closure)
- Cosign keyless / Rekor flow
- Public vulnerability disclosure program

---

## How to regenerate this file

```bash
git log --oneline v0.5.0-m5a..HEAD | wc -l     # commits since latest tag
ls docs/adr/ | wc -l                            # ADR count
just ci-local                                    # test count at the tail
```

Update the dated header, the summary paragraph, the test-trajectory
table, and the "what ships next" list.
