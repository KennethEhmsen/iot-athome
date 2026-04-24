# IoT-AtHome — Project Status

**As of:** 2026-04-24 (post-M4)
**Head:** `8237764` (M4 kickoff: plan, registry gRPC server interceptor, deprecation log)
**Shipped releases:** `v0.1.0-m1` (2026-04-21), `v0.2.0-m2` (2026-04-23), `v0.3.0-m3` (2026-04-23), `v0.4.0-m4` (2026-04-24)
**Next release target:** `v0.5.0-m5`
**Commits since M1:** 40 · **Commits since M3:** 3

> This file is a point-in-time snapshot. Regenerate before every milestone
> boundary. Consult `docs/Mn-PLAN.md` + `docs/Mn-RETROSPECTIVE.md` + `docs/adr/`
> for the canonical state — this doc is a navigation map, not a source of truth.

---

## Executive summary

M4 **shipped** — and honestly re-scoped. Entering the session it looked
like M4 would absorb the M3 W3 carry-overs (Envoy, Timescale, Command
Central, 2nd adapter) + the original M4 edge-ML plugins. Two
discoveries reshaped the target:

1. **Envoy was already shipped** — the dev compose stack's Envoy
   service + `envoy.yaml` (mTLS terminator, OTel tracing, WS upgrade
   routing, Keycloak proxy) was in place since M1 W1. An audit, not a
   build.
2. **Edge-ML plugins are hardware + model-bound** — water-meter CV
   needs an ESP32-CAM + trained TFLM classifier; 3-phase power needs
   an ESP32-S3 + ATM90E32 + CT/VT provisioning; NILM needs the sensor
   plugins shipping first to generate training data. Shoehorning
   these into a code-only milestone would produce scaffolds, not
   plugins. They move to **M5**.

What M4 actually closes: the three **observability + auth clarity
items** from the M3 retro's architectural-debts list.

**Shipped slices (3 over 1 session):**
- **Registry gRPC server interceptor** — symmetric to the client-side
  interceptor shipped post-v0.3.0-m3. Each `RegistryService` handler
  (upsert/get/list/delete) scopes its body in
  `iot_observability::traceparent::with_context(extract_ctx(…), …)`.
  Panel → gateway HTTP → gateway tonic client → registry server →
  registry bus watcher now share one trace id across every hop.
- **`registry::upsert-device` deprecation log** — host capability
  still works (z2m keeps calling it as belt-and-braces), but the
  handler fires a one-shot `registry.deprecated` warn log per host
  lifetime. M5 / ABI 1.3.0 removes it.
- **`docs/M4-PLAN.md`** — honest re-scope document.

Everything else from the original M4 plan that didn't ship is
deferred with an explicit M5 target. This is not slippage — it's a
recognition that the items belong in M5 alongside the voice / NILM
work they naturally compose with.

Local verification: **111/111 workspace tests, clippy `-D warnings`
clean, cargo-deny clean**.

---

## Milestone status at a glance

| Milestone | State | Anchor doc |
|---|---|---|
| **M1 — Walking skeleton** | ✅ shipped 2026-04-21 (`v0.1.0-m1`) | `docs/M1-RETROSPECTIVE.md` |
| **M2 — Plugin SDK + Wasmtime Host + Real Plugins** | ✅ shipped 2026-04-23 (`v0.2.0-m2`) | `docs/M2-RETROSPECTIVE.md` |
| **M3 — Automation Engine + Observability Foundations** | ✅ shipped 2026-04-23 (`v0.3.0-m3`) | `docs/M3-RETROSPECTIVE.md` |
| **M4 — M3 carry-over closures** | ✅ shipped 2026-04-24 (`v0.4.0-m4`) | `docs/M4-RETROSPECTIVE.md` |
| **M5 — Edge-ML + voice + Command Central + Timescale + broker-JWT** | 📐 designed | design doc §4.5, §3.6, §7-§9 |
| **M6 — Hardening + certification** | 📐 designed | design doc §10 |

---

## What M4 delivered

### Cross-service trace propagation, closed

`panel → gateway (HTTP middleware) → gateway tonic client
(post-v0.3 client interceptor) → registry gRPC server (M4 handler
wrap) → registry bus watcher (post-v0.3 subscriber wrap) → automation
engine (post-v0.3 subscriber wrap) → gateway WS subscriber
(post-v0.3)`

Every hop scopes the task-local TraceContext. Nothing drops, nothing
mints a fresh root mid-chain unless the inbound metadata is missing.

### `registry::upsert-device` on its deprecation path

The M2-era host capability (plugin-side `registry.upsert()`) still
works — z2m keeps calling it as belt-and-braces. But `iot-plugin-host`
fires a one-shot `registry.deprecated` warning per host lifetime
naming M3 W1.2 bus-watcher auto-registration as the replacement path
+ M5 / ABI 1.3.0 as the hard-removal point. Gated on a `OnceLock<()>`
so chatty adapters don't flood.

### Honest re-scope

`docs/M4-PLAN.md` explicitly calls out what was discovered shipped
(Envoy) and what moves (edge-ML plugins, broker JWT wiring, 2nd
adapter, Timescale, Command Central). No fake scaffolds.

---

## Architecture snapshot

### Code surface (13 crates + 2 shipping plugins + 1 scaffold + panel)

| Crate | M4 touches |
|---|---|
| `iot-registry` | `service.rs` — all 4 handlers extract `traceparent` from `tonic::metadata` and `with_context(ctx, …)` the body. |
| `iot-plugin-host` | `component.rs` — `REGISTRY_DEPRECATED_LOGGED: OnceLock<()>` gates a one-shot deprecation warn on `upsert-device` calls. |

Everything else (observability, automation, bus, gateway, CLI) is
unchanged — M4 is composition of already-tested pieces.

### End-to-end trace path (now complete through gRPC)

```
panel  ─GET /api/v1/devices──┐
  (may send traceparent)     │
                             ▼
  gateway traceparent_mw → with_context(ctx) {
                             handler body
                             ─tonic client (interceptor stamps traceparent)→
                                                                    │
                                                                    ▼
                                             registry gRPC server (extract_ctx + with_context)
                                             │
                                             ▼
                                             handler body
                                             .publish_proto(…) ← current() reads task-local
                                             │
                             .publish_proto(…)  ← current() reads task-local
                           }
                             │
                        (traceparent header on bus messages, both sides)
                             ▼
  subscriber (registry bus_watcher / automation / gateway-WS) loop {
      let ctx = extract_trace_context(&msg).unwrap_or_else(new_root);
      with_context(ctx, handle(msg))   ← shipped post-v0.3.0-m3
  }
```

### Security & supply-chain posture

| Control | State | Notes |
|---|---|---|
| mTLS on every internal hop | ✅ | Unchanged from M2 |
| Plugin sig verify at install | ✅ | Cosign ECDSA-P256 (pinned pubkey) |
| SBOM CVE gate at install | ✅ | CycloneDX `.vulnerabilities[]` |
| Audit hash chain tamper-detectable | ✅ | JCS + verify re-computes |
| Per-plugin NATS identity | 🏗 | JWT minter shipped M3; server config flip + iotctl post-install slipped to M5 |
| End-to-end traceparent | ✅ | **M4 closure** — server-side gRPC interceptor completes the path |
| `registry::upsert-device` on deprecation path | ✅ | **New in M4** — deprecation log; removal M5 / ABI 1.3.0 |
| Cosign keyless / Rekor | ⏸ | M6 |

---

## ADR index

13 ADRs accepted, no new ones in M4. Structural decisions were
pre-approved by existing ADRs.

ADR-0011 (dev bus auth) **stays active** through M5 — the cryptographic
half of the retirement path shipped in M3 (`iot_bus::jwt::issue_user_jwt`
minter has unit tests) but the wiring half (iotctl post-install + NATS
server reconfig + dev cert mint script operator-keypair step) is a
multi-file change that wants its own dedicated session.

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
| M4 shipped | **111** (unchanged — M4 slices are composition of already-tested pieces) |

Zero flakes in M4. All clippy / deny gates clean on every push.

---

## Architectural debts (post-M4 priority order)

Rolled forward. Status updates noted inline.

1. Broker JWT bootstrap wiring — **M5 target**. Crypto half shipped M3.
2. Subscriber-loop traceparent wrappers — ✅ shipped post-v0.3.0-m3.
3. gRPC traceparent interceptors — ✅ client-side post-v0.3.0-m3; **server-side M4**.
4. Hand-rolled expression subset vs. full CEL — **M5 target**. Drop-in replaceable behind the `parse`/`eval_bool` facade.
5. Wildcard-filtered last-msg replay — **M5 target**. Concrete-subject replay covers the 80% case.
6. Fuel refuel between plugin host calls — **M5 target**. Carried from M2.
7. MQTT broker subscription sub-refcount + unsubscribe on plugin exit — backlog.
8. Permissive Mosquitto ACL — **M5**. Manifest-derived ACLs at broker level.
9. SLSA provenance still `continue-on-error: true` — **M6**. Private-repo block from M2.
10. `registry::upsert-device` host capability redundant — ✅ **deprecation log shipped M4**; removal M5 / ABI 1.3.0.

---

## What ships next (M5)

Every item M4 deferred, plus the original M4 edge-ML scope.

**Rollover debts (from M3 retro carried through M4):**
1. Broker JWT bootstrap wiring — retires ADR-0011
2. Real CEL interpreter for rules
3. Wildcard-filtered last-msg replay
4. Fuel refueling + MQTT unsubscribe
5. `registry::upsert-device` capability removal (ABI 1.3.0)

**Original M4 scope, consolidated:**
6. Second adapter (433-SDR), as part of the edge-ML plugin family
7. TimescaleDB optional backend
8. Command Central v1 PWA

**Edge-ML plugin family** (design doc §7-§9):
9. Water-meter CV (ESP32-CAM + TFLM digit classifier)
10. Mains-power 3-phase (ESP32-S3 + ATM90E32)
11. Heating flow/return ΔT + COP
12. NILM training loop (hub-side Python + ONNX)

**Voice pipeline** (original M5):
13. openWakeWord wake detection
14. Whisper/Vosk STT
15. Closed-domain NLU dispatcher
16. Piper TTS responses
17. llama.cpp Q4 fallback on Pi 5

M5 is therefore roughly **7 weeks** of scope, not the originally-planned
4. A candidate split into M5a (edge-ML + Command Central + Timescale +
broker-JWT finalization) and M5b (voice pipeline + NILM training loop)
is worth considering at M5 planning time.

---

## How to regenerate this file

```bash
git log --oneline v0.4.0-m4..HEAD | wc -l     # commits since latest tag
ls docs/adr/ | wc -l                          # ADR count
just ci-local                                  # test count at the tail
```

Update the dated header, the summary paragraph, the test-trajectory
table, and the "what ships next" list.
