# IoT-AtHome — Project Status

**As of:** 2026-04-23 (post-M3)
**Head:** `ffb8d3b` (M3 W2.8: bus::extract_trace_context for subscriber loops)
**Shipped releases:** `v0.1.0-m1` (2026-04-21), `v0.2.0-m2` (2026-04-23), `v0.3.0-m3` (2026-04-23)
**Next release target:** `v0.4.0-m4`
**Commits since M1:** 37 · **Commits since M2:** 17

> This file is a point-in-time snapshot. Regenerate before every milestone
> boundary. Consult `docs/Mn-PLAN.md` + `docs/Mn-RETROSPECTIVE.md` + `docs/adr/`
> for the canonical state — this doc is a navigation map, not a source of truth.

---

## Executive summary

M3 **shipped** — automation engine is live, every inbound HTTP request
carries a W3C traceparent into the bus publishes it spawns, JetStream
retains last-per-subject state so the panel survives reload, and the
audit log is finally tamper-detectable (JCS canonicalisation + verify
recomputes hashes). M1-era architectural debts 1, 4, 6, 7, and 8
retired or in-flight; the registry auto-register-on-bus path makes
the M2 `registry::upsert-device` host capability redundant.

W3 items — Envoy mTLS frontend, TimescaleDB optional backend,
Command Central v1 PWA, and a second adapter — intentionally slip to
M4. Each is a multi-session build-out; splitting across milestones
keeps the M3 cut coherent.

Local verification: **111/111 workspace tests, clippy `-D warnings`
clean, cargo-deny clean**.

---

## Milestone status at a glance

| Milestone | State | Anchor doc |
|---|---|---|
| **M1 — Walking skeleton** | ✅ shipped 2026-04-21 (`v0.1.0-m1`) | `docs/M1-RETROSPECTIVE.md` |
| **M2 — Plugin SDK + Wasmtime Host + Real Plugins** | ✅ shipped 2026-04-23 (`v0.2.0-m2`) | `docs/M2-RETROSPECTIVE.md` |
| **M3 — Automation Engine + Observability Foundations** | ✅ shipped 2026-04-23 (`v0.3.0-m3`) | `docs/M3-RETROSPECTIVE.md` |
| **M4 — Edge-ML reference plugins + M3 W3 carry-overs** | 📐 designed; scaffolded `plugins/power-meter-3ph/` | design doc §7, §8, §9 |
| **M5 — Voice + NILM** | 📐 designed | design doc §4.5, §3.6 |
| **M6 — Hardening + certification** | 📐 designed | design doc §10 |

---

## What M3 delivered

### Automation engine (iot-automation)

- **Rule file format** (YAML): id / triggers / when / actions / idempotency.
- **Expression language**: CEL subset — comparisons, booleans, path access, parens. Hand-rolled; 500 LOC with tests.
- **Engine loop**: subscribes to `device.>`, matches triggers, evaluates `when`, dispatches actions.
- **Idempotency cache**: 5-second TTL on `(rule_id, subject, payload_sha256)`; chatty sensors can't fire a rule twice.
- **Audit per firing**: `automation.rule_fired` entry in the hash-chained log.
- **DLQ**: publishes a failure record to `sys.automation.dlq` when an action fails.
- **Protobuf decode**: `iot.device.v1.EntityState` → rule-friendly JSON so z2m state messages are rule-visible.
- **`iotctl rule {add,list,delete,test}`**: filesystem surface + dry-run against a synthetic payload.

### Observability foundations

- **W3C traceparent** (`iot_observability::traceparent::TraceContext`) — generate, parse, child-of, format.
- **Task-local propagation** — `with_context(ctx, fut).await` scopes the handler; bus reads it via `current()`; outbound publishes inherit.
- **Gateway middleware** — every inbound HTTP/WS request extracts `traceparent` or mints a fresh root, scopes the handler.
- **Bus subscriber helper** — `extract_trace_context(&Message)` for the symmetric server side.
- **Audit hash chain** — JCS canonicalisation (RFC 8785) + `verify()` re-computes hashes: tampering is detected.
- **JetStream last-msg-per-subject** — `DEVICE_STATE` stream + `Bus::last_state(subject)` helper + gateway WS replay on connect.

### Rollovers closed (M1 + M2 debts)

- **`iot-proto` split** into `iot-proto-core` (prost, wasm32-wasip2) + `iot-proto` (tonic overlay). Plugins no longer redeclare EntityState.
- **Registry auto-register on bus**: new `iot-registry` bus watcher subscribes to `device.>` + auto-creates unknown devices. Makes M2's `registry::upsert-device` host capability belt-and-braces.
- **JCS canonical JSON for audit** (M1 debt #6 retired).
- **Broker JWT minter** (cryptographic half — `iot_bus::jwt::issue_user_jwt`). Wiring half slipped to early M4.

---

## Architecture snapshot

### Code surface (13 crates + 3 plugins + panel)

| Crate | M3 touches |
|---|---|
| `iot-core` | — |
| `iot-proto-core` | **New**: prost messages + subject/header helpers. wasm32-wasip2-friendly. |
| `iot-proto` | Shrunk to tonic overlay re-exporting iot-proto-core. |
| `iot-bus` | `jwt` (user JWT minter), `jetstream` (DEVICE_STATE helpers), `extract_trace_context`, `current_traceparent` now reads task-local. |
| `iot-registry` | `bus_watcher` module (auto-register on `device.>`). |
| `iot-automation` | `expr` (evaluator), `rule` (parser), `engine` (subscribe + eval + fire + idempotency + audit + DLQ + proto decode). |
| `iot-gateway` | `tracing_mw` (inbound traceparent), WS handler replays last_state on connect. |
| `iot-plugin-host` | — |
| `iot-plugin-sdk-rust` | — |
| `iot-audit` | `serde_jcs`, `verify()` re-computes hashes. |
| `iot-observability` | `traceparent` module (format + task-local `with_context`). |
| `iot-config` | — |
| `iot-cli` | `rule` module (add/list/delete/test subcommands). |

### End-to-end trace path

```
panel  ─GET /api/v1/devices──┐
  (may send traceparent)     │
                             ▼
  gateway traceparent_mw → with_context(ctx) {
                             handler body
                             .publish_proto(…)  ← current_traceparent() reads task-local
                           }
                             │
                        (traceparent header on bus message)
                             ▼
  subscriber (registry / automation / gateway-WS) loop {
      let ctx = extract_trace_context(&msg).unwrap_or_else(new_root);
      with_context(ctx, handle(msg))                                   ← ✅ helper shipped; callers switch in M4
  }
```

### Security & supply-chain posture

| Control | State | Notes |
|---|---|---|
| mTLS on every internal hop | ✅ | Unchanged from M2 |
| Plugin sig verify at install | ✅ | Cosign ECDSA-P256 (pinned pubkey) |
| SBOM CVE gate at install | ✅ | CycloneDX `.vulnerabilities[]` |
| Audit hash chain tamper-detectable | ✅ | **New in M3** — JCS + verify re-computes |
| Per-plugin NATS identity | 🏗 | M3 shipped the JWT minter; server config flip early M4 |
| End-to-end traceparent | 🏗 | gateway ↔ bus wired; subscriber loops + gRPC interceptors M4 |
| Cosign keyless / Rekor | ⏸ | M6 |

---

## ADR index

13 ADRs accepted, no new ones in M3. Structural decisions were pre-approved by existing ADRs (0004 subjects, 0008 error codes, 0009 tracing, 0013 z2m migration).

ADR-0011 (dev bus auth) retires when M4 ships the JWT bootstrap wiring.

---

## Test trajectory

| Boundary | Workspace tests |
|---|---|
| M1 shipped | 12 |
| M2 shipped | 58 (+ 3 out-of-workspace plugin tests) |
| M3 W1 end (audit JCS) | 68 |
| M3 W2.1-2.2 (parser + engine) | 87 |
| M3 W2.3-2.5 (iotctl + jetstream) | 103 |
| M3 W2.6-2.8 + polish (traceparent + proto decode) | **111** |

Zero flakes in M3. All clippy / deny gates clean on every push.

---

## Architectural debts (post-M3 priority order)

Rolled forward. Each has a named resolution.

1. Broker JWT bootstrap wiring (crypto shipped; iotctl post-install + NATS server reconfig slipped to early M4)
2. Subscriber-loop traceparent wrappers not yet dropped into each live loop
3. gRPC traceparent interceptors for gateway → registry
4. Hand-rolled expression subset vs. full CEL (drop-in replaceable)
5. Wildcard-filtered last-msg replay (needs JetStream ephemeral consumer)
6. Fuel refuel between plugin host calls (carried from M2)
7. MQTT broker subscription sub-refcount + unsubscribe on plugin exit
8. Permissive Mosquitto ACL (manifest-derived ACLs at broker level)
9. SLSA provenance still `continue-on-error: true` (private-repo block from M2; M6)
10. `registry::upsert-device` host capability redundant now (deprecation log M4, removal M5)

---

## What ships next (M4)

Carry-overs from M3 W3 + the original M4 design:

**M3 carry-overs:**
1. Envoy + mTLS frontend (gateway behind it; registry ↔ gateway via Envoy upstream TLS)
2. TimescaleDB optional backend (long-term retention)
3. Command Central v1 PWA (per-room kiosk, ephemeral per-person auth, proximity wake)
4. Second adapter (Z-Wave via zwave-js-server, or 433-SDR via rtl_433)
5. Broker JWT wiring
6. gRPC traceparent interceptors
7. Subscriber-loop traceparent wrappers
8. Real CEL interpreter for rules

**Original M4 scope** (reference edge-ML plugins, design doc §7-§9):
- Water-meter CV (ESP32-CAM + TFLM digit classifier)
- Mains-power 3-phase (ESP32-S3 + ATM90E32; scaffold exists)
- Heating flow/return ΔT + COP
- NILM training loop

---

## How to regenerate this file

```bash
git log --oneline v0.3.0-m3..HEAD | wc -l     # commits since latest tag
ls docs/adr/ | wc -l                          # ADR count
just ci-local                                  # test count at the tail
```

Update the dated header, the summary paragraph, the test-trajectory
table, and the "what ships next" list.
