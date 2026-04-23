# IoT-AtHome — Project Status

**As of:** 2026-04-23
**Head:** `6d594ca` (M2 W4: MqttBroker — rumqttc client + eventloop feeds the router)
**Shipped release:** `v0.1.0-m1` (2026-04-21)
**Next release target:** `v0.2.0-m2`
**Commits since M1:** 16

> This file is a point-in-time snapshot. Regenerate before every milestone
> boundary. Consult `docs/M2-PLAN.md` and `docs/adr/` for the canonical
> design state — this document is a navigational map, not a source of truth.

---

## Executive summary

M1 shipped the walking skeleton (MQTT → adapter → registry → bus → gateway → panel).
M2 is mid-W4 — the WASM plugin runtime is feature-complete at the capability +
supervision + MQTT layer; the last M2 deliverable blocking `v0.2.0-m2` is porting
the M1 z2m adapter onto the new capabilities (ADR-0013). All architectural pieces
that port depends on are now landed and tested.

Local verification is the gate: **59/59 tests pass, clippy `-D warnings` clean,
cargo-deny clean**. GitHub Actions has run intermittently this session due to a
billing limit on the user's account (now unblocked; `6d594ca`'s run is currently
pending).

---

## Milestone status at a glance

| Milestone | State | Anchor doc |
|---|---|---|
| **M1 — Walking skeleton** | ✅ shipped 2026-04-21 | `docs/M1-RETROSPECTIVE.md` |
| **M2 — Plugin SDK + Wasmtime Host + Real Plugins** | 🏗 W4 in flight (~80% done) | `docs/M2-PLAN.md` |
| **M3 — Automation + full observability** | 📐 designed | design doc §4.2, §3 |
| **M4 — Edge-ML reference plugins** | 📐 water-meter / 3-phase / heating scaffolded | design doc §7, §8, §9 |
| **M5 — Voice + NILM** | 📐 designed | design doc §4.5, §3.6 |
| **M6 — Hardening + certification** | 📐 designed | design doc §10 |

---

## M2 week-by-week

### W1 ✅ — Bindings that compile (2026-04-21)

- WIT world `iot:plugin-host@1.0.0` with `interface runtime`
- `iot-plugin-sdk-rust`: `wit_bindgen::generate!` + exported macro
- `iot-plugin-host`: `wasmtime::component::bindgen!` + Host trait impls
- `CapabilityMap` + NATS wildcard matcher, 3 unit tests
- `tests/roundtrip.rs` — real `demo_echo.wasm` loaded + init + deny path
- `plugins/demo-echo/` producing a 3.5 MB debug component

### W2 ✅ — Capability enforcement + real bus wiring

- Manifest parser (`manifest.rs`), 3 unit tests
- `bus::publish` capability check + real `iot_bus::Bus::publish_proto` wiring
- `log::emit` → tracing with plugin-id span field
- Deny test: capability violation → `capability.denied` + `plugin.denied` audit entry (hash-chain verified)
- demo-echo echoes on `<subject>.echo`
- Panel `useAuth` split out of `AuthProvider` (Fast Refresh compliance)

### W3 ✅ — Installation + signing

- `iotctl plugin {install,list,uninstall}` subcommands
- Cosign-compatible ECDSA-P256 signature verification via `p256` crate (Rekor/Fulcio keyless deferred to M6 per ADR-0006)
- SBOM CVE scan — parses CycloneDX `.vulnerabilities[]`, refuses `>=High` unless `--allow-vulnerabilities`
- Per-plugin ed25519 NATS nkey + manifest-derived `acl.json` snapshot
- z2m migration — **design captured in ADR-0013; implementation slipped into W4** (rumqttc-on-wasip2 dead-end → host-side MQTT capability path)

### W4 🏗 — Polish + second adapter (in flight)

**Done this milestone slice:**

- [x] MQTT capability matching (wildcard + narrowing semantics, 5 unit tests)
- [x] Plugin ABI bumped to **1.1.0** — `interface mqtt` + `on-mqtt-message` runtime export
- [x] `mqtt::Host` impl on `PluginState` (capability-checked, instrumented spans)
- [x] `iotctl plugin list` gained SIGNATURE / IDENTITY / HEALTH columns + SHA-256 fingerprint
- [x] OpenTelemetry spans on every host call (`plugin`, `capability` fields)
- [x] `CrashTracker` + DLQ marker (`.dead-lettered`) — exponential backoff, 5-crashes-in-10-min threshold, 6 unit tests
- [x] Per-plugin runtime task (`runtime.rs`) + async `supervise()` loop
- [x] `MqttRouter` (pure in-memory routing table, 6 unit tests)
- [x] `MqttBroker` (rumqttc client + eventloop + mTLS config, 3 unit tests)

**Remaining before `v0.2.0-m2`:**

- [ ] Wire `Config::mqtt` + `run()` calls `MqttBroker::connect`
- [ ] testcontainers-Mosquitto integration test for the end-to-end pipe
- [ ] Registry host capability (ADR-0013 Piece 2, transitional)
- [ ] z2m plugin rewrite — now a straight port onto the new capabilities
- [ ] Optional: second real adapter (risk table allows slip to M3)
- [ ] `v0.2.0-m2` tag + `docs/M2-RETROSPECTIVE.md`

---

## Architecture snapshot

### Code surface (12 crates + 3 plugins + panel)

| Crate | Purpose | M2 touches |
|---|---|---|
| `iot-core` | Canonical types, schema version | — |
| `iot-proto` | Protobuf types, subject taxonomy | — |
| `iot-bus` | mTLS NATS wrapper | — |
| `iot-registry` | Device registry gRPC service | — |
| `iot-automation` | Rule engine (skeleton; M3) | — |
| `iot-gateway` | REST + WS frontdoor, OIDC bearer | — |
| `iot-plugin-host` | Wasmtime-based plugin runtime | **This milestone's focus** |
| `iot-plugin-sdk-rust` | Plugin author's import | ABI bumped to 1.1.0 |
| `iot-audit` | Hash-chained append log | — |
| `iot-observability` | OTel + tracing config | — |
| `iot-config` | Figment-based layered config | — |
| `iot-cli` | `iotctl` operator CLI | `plugin install/list/uninstall` added |
| `plugins/demo-echo` | Reference WASM plugin | Rebuilt against 1.1.0 |
| `plugins/zigbee2mqtt-adapter` | MQTT→canonical (M1 native) | Awaiting WASM port |
| `plugins/power-meter-3ph` | M4 scaffold (manifest+README only) | — |
| `panel` | React+Vite+OIDC operator UI | — |

### Plugin host capabilities (ABI 1.1.0)

| Capability | Wire | Host impl | Enforced by |
|---|---|---|---|
| `bus.publish` | ✅ | ✅ real NATS | `CapabilityMap::check_bus_publish` (NATS wildcard) |
| `log.emit` | ✅ | ✅ → tracing | always allowed |
| `mqtt.subscribe` | ✅ | ✅ router+broker | `CapabilityMap::check_mqtt_subscribe` (narrowing check) |
| `mqtt.publish` | ✅ | ✅ rumqttc publish | `CapabilityMap::check_mqtt_publish` (MQTT wildcard) |
| `registry.upsert-device` | 📐 planned | — | ADR-0013 Piece 2 |
| `net.outbound` | 📐 placeholder | — | M3 |

### Plugin lifecycle (runtime.rs + supervisor.rs)

```text
load_plugin_dir → Store+Plugin → spawn_plugin_task → PluginHandle{tx, join}
                                                     │
                                                     ├─ tx.send(PluginCommand::OnBusMessage | OnMqttMessage | Shutdown)
                                                     │   (external: broker dispatcher, bus router, supervisor)
                                                     │
                                                     └─ join.await → Result<(), CrashReason>
                                                         │
                                                         └─ supervise() → CrashTracker.record()
                                                             ├─ Decision::Restart{after} → tokio::sleep → loop
                                                             └─ Decision::DeadLetter → write .dead-lettered → Ok(())
```

### Security & supply chain posture

| Control | State | Anchor |
|---|---|---|
| mTLS on every internal hop | ✅ dev certs via `just certs` | ADR-0006 |
| Plugin sig verify at install | ✅ cosign ECDSA-P256 (pinned pubkey) | ADR-0006 |
| SBOM-embedded CVE gate at install | ✅ CycloneDX `.vulnerabilities[]` | W3 |
| Cosign keyless + Rekor lookup | ⏸ M6 | ADR-0006 |
| Per-plugin NATS credentials | ✅ nkey seed + `acl.json` | W3 |
| Operator-issued NATS user JWT | ⏸ broker-side infra | ADR-0011 retires at M2 end |
| Capability-denied audit trail | ✅ hash-chained | `iot-audit` |
| Dead-plugin refusal-to-reload | ✅ `.dead-lettered` marker | supervisor.rs |
| TUF metadata distribution | 📐 M6 | ADR-0006 |
| ETSI EN 303 645 walkthrough | 📐 M6 | design §10 |

---

## ADR index

All 13 ADRs are active; none retired yet (ADR-0011 retires at M2 end).

| ADR | Topic | Status |
|---|---|---|
| 0001 | Record architecture decisions | Accepted |
| 0002 | Async runtime: Tokio | Accepted |
| 0003 | Plugin ABI: WASM Component Model | Accepted |
| 0004 | NATS subject taxonomy | Accepted |
| 0005 | Canonical device schema versioning | Accepted |
| 0006 | Signing & key management | Accepted |
| 0007 | Database migrations: forward-only | Accepted |
| 0008 | Error handling (thiserror + dotted codes) | Accepted |
| 0009 | Logging & tracing (tracing crate) | Accepted |
| 0010 | Config format & layering (Figment) | Accepted |
| 0011 | Dev bus auth (single account) | Accepted — retires at M2 end |
| 0012 | Plugin binding layer (wit-bindgen / wasmtime-bindgen) | Accepted |
| 0013 | z2m WASM migration architecture | Accepted |

---

## Test trajectory

| Milestone boundary | Workspace tests |
|---|---|
| M1 shipped | 12 |
| M2 W2 end | 19 |
| M2 W3 mid (cosign) | 25 |
| M2 W3 end (CVE scan) | 30 |
| M2 W4 kickoff (list + OTel) | 35 |
| MQTT capability | 40 |
| Supervisor scaffold | 47 |
| Runtime+supervise | 50 |
| MqttRouter | 56 |
| MqttBroker ← **today** | **59** |

Zero flakes across sessions. One pending rustfmt fix mid-session (`a180438`) — pattern recognised and called out: always `cargo fmt --all` immediately before `git commit`.

---

## Architectural debts (retro-driven priority list)

From `docs/M1-RETROSPECTIVE.md`:

1. ~~Single NATS account → per-plugin accounts~~ — seeds + ACL snapshots shipped in M2 W3; operator-JWT issuance still pending
2. Permissive Mosquitto ACL → manifest-derived ACLs (M3)
3. Plaintext gateway↔registry → Envoy-fronted mTLS (M3)
4. No OTel cross-service → traceparent propagation (M3)
5. SQLite-only → Postgres migration path (M3)
6. Ad-hoc canonical JSON in audit → JCS RFC 8785 (M3)
7. Registry UPSERT collision workaround via client-side cache → `GetByExternalId` RPC (planned M2 W1, slipped; ADR-0013 moves it to host capability then M3 bus-driven auto-register)
8. Entity state has no retention → JetStream last-msg-per-subject (M3)

---

## Risks & open questions

| Risk | Status |
|---|---|
| `wasmtime::component::bindgen!` async maturity | ✅ resolved in W1 |
| Component Model + WASI Preview 2 tooling fragmentation | ✅ pinned `wit-bindgen` + `wasmtime` |
| Per-plugin NATS account bootstrap race | ⏸ operator-JWT not wired yet |
| Cosign keyless offline | ✅ fallback path (pinned pubkey) shipped; keyless in M6 |
| rumqttc on wasm32-wasip2 | ✅ root cause diagnosed; **ADR-0013** picked Option 2 (host capability) |
| GitHub Actions billing | ✅ unblocked; `6d594ca` CI pending |

---

## What ships `v0.2.0-m2`

Per M2 plan Definition of Done:

> All three plugins load cleanly and behave identically to an operator
> (`iotctl device list` should not know which integration path a device
> flowed through).

Gate items:

- z2m migration completes on the new MQTT + registry capabilities (blocker)
- Capability-deny test in CI (✅ already)
- Plugin crash → clean restart visible in `iotctl plugin list` (✅ HEALTH column + supervisor loop)
- Tag signed via the same cosign pipeline as M1 (tooling already wired; just need to run it)

The second adapter (Z-Wave or 433-SDR) is **allowed to slip** per the risk table — it can land in early M3.

---

## Recommended next 3 commits

1. **Config wiring + run()** — `iot_plugin_host::Config::mqtt: Option<MqttBrokerConfig>`, `run()` calls `MqttBroker::connect`, passes `Arc<MqttBroker>` into every plugin's `HostBindings`. ~50 LOC.
2. **testcontainers integration test** — Mosquitto container, publish a topic, assert a fixture plugin task received it. Lives next to the existing integration test suite. ~150 LOC.
3. **Registry host capability + z2m port** — the last architecturally novel piece, then the z2m plugin becomes a straight port of `translator.rs` + `state_publisher.rs` with `rumqttc`/`tonic`/`tokio` deps dropped. Roughly 1 session.

After that, **tag `v0.2.0-m2`** and move to M3 (automation engine + JCS audit + Envoy mTLS + Postgres migration + TimescaleDB + Command Central v1).

---

## How to regenerate this file

Run, commit, paste:

```bash
git log --oneline v0.1.0-m1..HEAD | wc -l     # commit count
ls docs/adr/ | wc -l                          # ADR count
just ci-local                                  # test count at the tail
```

Update the dated header, the summary paragraph, the test-trajectory
table, the "today" bullets, and the "recommended next commits" list.
