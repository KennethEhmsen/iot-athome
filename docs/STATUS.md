# IoT-AtHome — Project Status

**As of:** 2026-04-23 (post-M2)
**Head:** `d084a60` (M2 W4: z2m migrated from native sidecar to WASM plugin)
**Shipped releases:** `v0.1.0-m1` (2026-04-21), `v0.2.0-m2` (2026-04-23)
**Next release target:** `v0.3.0-m3`
**Commits since M1:** 20 · **Commits since M2:** 0 (tagged at HEAD)

> This file is a point-in-time snapshot. Regenerate before every milestone
> boundary. Consult `docs/Mn-PLAN.md` + `docs/Mn-RETROSPECTIVE.md` + `docs/adr/`
> for the canonical state — this doc is a navigation map, not a source of truth.

---

## Executive summary

M2 **shipped** — full WASM plugin runtime with capability enforcement, signed
install pipeline, crash supervision, MQTT + registry host capabilities, and
the z2m adapter migrated onto it from M1's native systemd shape. The M2 DoD
("all three plugins load cleanly and behave identically to an operator") is
met except for the second adapter, which risk-table-slipped into early M3.

Local verification: **58/58 workspace tests, 3/3 out-of-workspace plugin
tests, clippy `-D warnings` clean, cargo-deny clean**. Last CI run
(`d084a60`) pending at the time of writing; all signal-bearing jobs green
on the ancestor `605b1b0` (registry host capability).

---

## Milestone status at a glance

| Milestone | State | Anchor doc |
|---|---|---|
| **M1 — Walking skeleton** | ✅ shipped 2026-04-21 (`v0.1.0-m1`) | `docs/M1-RETROSPECTIVE.md` |
| **M2 — Plugin SDK + Wasmtime Host + Real Plugins** | ✅ shipped 2026-04-23 (`v0.2.0-m2`) | `docs/M2-RETROSPECTIVE.md` |
| **M3 — Automation + full observability** | 📐 designed (next) | design doc §4.2, §3 |
| **M4 — Edge-ML reference plugins** | 📐 `power-meter-3ph/` scaffolded (manifest+README) | design doc §7, §8, §9 |
| **M5 — Voice + NILM** | 📐 designed | design doc §4.5, §3.6 |
| **M6 — Hardening + certification** | 📐 designed | design doc §10 |

---

## What M2 delivered

### Plugin ABI (1.2.0, additive minors from 1.0.0)

| Capability | Wire | Host impl | Enforced by |
|---|---|---|---|
| `bus.publish` | ✅ | ✅ real NATS | `CapabilityMap::check_bus_publish` (NATS wildcard) |
| `log.emit` | ✅ | ✅ → tracing | always allowed |
| `mqtt.subscribe` | ✅ | ✅ router+broker | `CapabilityMap::check_mqtt_subscribe` (narrowing check) |
| `mqtt.publish` | ✅ | ✅ rumqttc publish | `CapabilityMap::check_mqtt_publish` (MQTT wildcard) |
| `registry.upsert-device` | ✅ | ✅ tonic wrapper | `CapabilityMap::check_registry_upsert` (boolean) |
| `net.outbound` | 📐 placeholder | — | M3 |

### Plugin lifecycle

```text
iotctl plugin install → verify cosign sig → SBOM CVE gate → mint nkey + ACL
                                   ↓
                          write to <plugin_dir>/<id>/
                                   ↓
iot-plugin-host run() → discover_plugins → for each: supervise()
                                             ↓
                           load_plugin_dir → spawn_plugin_task → PluginHandle{tx,join}
                                                                  ↓
                           join.await → Result<(), CrashReason> → CrashTracker
                                                                  ↓
                                             Restart{after} or DeadLetter → .dead-lettered marker
```

Every host call is instrumented via `#[tracing::instrument(name="host_call",
fields(plugin, capability, …))]` so M3's traceparent propagation lights up
end-to-end automatically.

### Plugins shipping

| Plugin | Shape | Status |
|---|---|---|
| `demo-echo` | WASM 1.2.0, capabilities: bus.publish | ✅ reference plugin |
| `zigbee2mqtt-adapter` | WASM 1.2.0, capabilities: mqtt.subscribe / bus.publish / registry.upsert | ✅ migrated from M1 native |
| Second adapter (Z-Wave or 433-SDR) | — | ⏸ slipped to early M3 |
| `power-meter-3ph` | scaffold (manifest + README only) | 📐 M4 |

### Security & supply-chain posture

| Control | State | Anchor |
|---|---|---|
| mTLS on every internal hop | ✅ dev certs via `just certs` | ADR-0006 |
| Plugin sig verify at install | ✅ cosign ECDSA-P256 (pinned pubkey) | ADR-0006 |
| SBOM-embedded CVE gate at install | ✅ CycloneDX `.vulnerabilities[]` | W3 |
| Cosign keyless + Rekor lookup | ⏸ M6 | ADR-0006 |
| Per-plugin NATS credentials | ✅ nkey seed + `acl.json` | W3 |
| Operator-issued NATS user JWT | ⏸ broker-side infra | early M3 |
| Capability-denied audit trail | ✅ hash-chained | `iot-audit` |
| Dead-plugin refusal-to-reload | ✅ `.dead-lettered` marker | `supervisor.rs` |
| OTel host-call spans | ✅ plugin + capability fields | W4 |
| TUF metadata distribution | 📐 M6 | ADR-0006 |

---

## ADR index

All 13 ADRs accepted. ADR-0011 (dev bus auth) is nominally retired by M2
W3's per-plugin nkey minting; full retirement when the broker-side JWT
bootstrap lands in early M3.

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
| 0011 | Dev bus auth (single account) | Accepted · retires with M3 broker-JWT |
| 0012 | Plugin binding layer (wit-bindgen / wasmtime-bindgen) | Accepted |
| 0013 | z2m WASM migration architecture | Accepted · implemented |

---

## Test trajectory

| Boundary | Workspace tests | Plugin (out-of-workspace) tests |
|---|---|---|
| M1 shipped | 12 | — |
| M2 W2 end | 19 | — |
| M2 W3 mid (cosign) | 25 | — |
| M2 W3 end (CVE scan) | 30 | — |
| MQTT capability | 40 | — |
| Runtime+supervise | 50 | — |
| MqttBroker integration | 60 | — |
| z2m migration → WASM plugin | **58** | **3** (z2m translator) |

The workspace count dipped by 4 at z2m migration because the adapter moved
out-of-workspace (same pattern as demo-echo); 3 of the 4 translator tests
carried over to the plugin crate itself, the 4th (`translate()` building a
full `iot-proto::Device`) disappeared when the function did — replaced by
the `registry::upsert-device` host capability that takes plain strings.

Zero flakes across M2. Two `cargo fmt` regressions mid-session; pattern is
now "fmt immediately before commit, no intervening edits".

---

## Architectural debts (priority order)

Rolled forward from M1 + new ones from M2. Each has a named resolution.

1. **Per-plugin NATS: nkey + ACL only, no broker JWT yet** → M3 broker-bootstrap commit
2. **Registry host capability is transitional** → M3 `iot-registry` auto-register on `device.*` bus
3. **`iot-proto` won't target `wasm32-wasip2`** (z2m plugin redeclares EntityState) → M3 split into `iot-proto-core` (prost) + `iot-proto` (core + tonic)
4. **Permissive Mosquitto ACL** → manifest-derived ACLs (M3)
5. **Plaintext gateway↔registry** → Envoy-fronted mTLS (M3)
6. **No OTel cross-service** → traceparent propagation (M3); host-call spans already emit
7. **SQLite-only** → Postgres migration path (M3)
8. **Ad-hoc canonical JSON in audit** → JCS RFC 8785 (M3)
9. **Entity state has no retention** → JetStream last-msg-per-subject (M3)
10. **Fuel budget is one-shot** → per-capability refuel in supervisor command loop (M3)
11. **MQTT broker subscription never dropped on plugin exit** → subscription-refcount (optional, not urgent)
12. **Cosign keyless + Rekor not wired** → M6
13. **No hub-side advisory DB for second-opinion CVE scans** → M6 optional

---

## Risks & open questions

| Risk | Status |
|---|---|
| `wasmtime::component::bindgen!` async maturity | ✅ resolved in W1 |
| Component Model + WASI Preview 2 tooling fragmentation | ✅ pinned `wit-bindgen` + `wasmtime` |
| rumqttc on `wasm32-wasip2` | ✅ **dead-end confirmed**; ADR-0013 host-MQTT path shipped |
| Per-plugin NATS account bootstrap race | ⏸ operator-JWT not wired yet |
| Cosign keyless offline | ✅ fallback path shipped; keyless in M6 |
| GitHub Actions billing | ⚠ encountered mid-milestone; local `just ci-local` is the authoritative gate regardless |
| rustls-webpki 0.102.x CVEs (transitive via rumqttc 0.24) | ⏸ pinned as exceptions; revisit when rumqttc bumps |

---

## What ships `v0.3.0-m3`

Per design doc §4.2 + §3, plus rollovers from M2 deferrals:

1. CEL-based YAML rule engine (triggers → conditions → actions, idempotency keys, DLQ subjects)
2. Registry auto-register on bus (retires `registry::upsert-device` host capability)
3. Broker JWT bootstrap using the per-plugin nkey seeds M2 laid down
4. OTel traceparent propagation (gateway ↔ registry ↔ plugins)
5. JCS (RFC 8785) canonical-JSON for audit log
6. NATS JetStream last-msg-per-subject retention
7. Envoy + mTLS frontend
8. TimescaleDB optional backend
9. Command Central v1 (PWA + kiosk, ephemeral auth, device cert, proximity wake)
10. Second adapter (Z-Wave via zwave-js-server, or 433-SDR via rtl_433) — straight port on top of M2 capabilities
11. `iot-proto` split into `iot-proto-core` (prost, wasip2-friendly) + `iot-proto` (tonic) — retires the z2m plugin's redeclared messages

M3 plan doc lands as `docs/M3-PLAN.md` when M3 starts; this status file gets
regenerated at every milestone boundary.

---

## How to regenerate this file

Run, commit, paste:

```bash
git log --oneline v0.1.0-m1..HEAD | wc -l     # commit count (adjust against current tag)
ls docs/adr/ | wc -l                          # ADR count
just ci-local                                  # test count at the tail
```

Update the dated header, the summary paragraph, the test-trajectory
table, the "today" bullets, and the "what ships next" list.
