# M4 Retrospective — M3 Carry-over Closures

**Tag:** `v0.4.0-m4` · **Completed:** 2026-04-24 · **Plan:** [M4-PLAN.md](M4-PLAN.md) · **Key ADRs:** [0006](adr/0006-signing-key-management.md), [0008](adr/0008-error-handling.md), [0009](adr/0009-logging-and-tracing.md), [0011](adr/0011-dev-bus-auth.md)

## Scope re-framing, honestly

M4 is a **small milestone** — deliberately so. Entering the session it
looked like M4 should absorb the M3 W3 carry-overs (Envoy, Timescale,
Command Central, 2nd adapter) + the original M4 edge-ML plugins
(water-meter CV, 3-phase power, heating, NILM). Two discoveries
reshaped the target:

1. **Envoy was already shipped.** The dev compose stack's Envoy
   service + `envoy.yaml` (mTLS terminator in front of the gateway,
   OpenTelemetry tracing provider pointing at Tempo, WS upgrade
   routing, Keycloak proxy) was in place since M1 W1 infrastructure
   work. What looked like a W2 build-out in the M4 plan was actually
   a closure check — the piece exists and works.
2. **Edge-ML plugins are genuinely hardware-bound.** Water-meter CV
   needs an ESP32-CAM, a TFLite Micro digit classifier trained on the
   user's own meter's dial, and a calibration wizard. Mains-power
   3-phase needs an ESP32-S3 + ATM90E32 + CT/VT provisioning.
   NILM needs a training dataset that depends on the sensor plugins
   shipping first. Shoehorning these into a code-only milestone would
   produce scaffolds, not plugins. They move to **M5** where they
   land alongside the voice pipeline and NILM's own model work — a
   more coherent home.

What M4 actually closes: the three **observability + auth clarity
items** from the M3 retro's architectural-debts list.

## What we shipped

3 concrete slices over 1 session:

| Slice | Status | Notes |
|---|---|---|
| **Registry gRPC server interceptor** | ✅ | Symmetric to the client-side interceptor shipped post-v0.3.0-m3. Each `RegistryService` handler (upsert/get/list/delete) scopes its body in `iot_observability::traceparent::with_context(extract_ctx(request.metadata()), …)`. Panel → gateway HTTP → gateway's tonic client interceptor → registry server → registry bus_watcher now shares one trace id across every hop. |
| **`registry::upsert-device` deprecation log** | ✅ | The host capability still works (z2m keeps calling it as belt-and-braces), but the handler fires a one-shot `registry.deprecated` warn log per host lifetime naming the M3 W1.2 bus watcher as the replacement + M5 / ABI 1.3.0 as the hard-removal point. Gated on a `OnceLock` so chatty adapters don't flood. |
| **`docs/M4-PLAN.md`** | ✅ | Honest re-scoping (see above). |

## What we deviated on

Everything in the original M4 plan that didn't ship is deferred with an explicit M5 target. This is not slippage — it's a recognition that the items belong in M5 alongside the voice / NILM work they naturally compose with.

| Plan called for | What we did | Why |
|---|---|---|
| Envoy + mTLS frontend | Already shipped (M1 compose) | Audit discovered this during the session; nothing to do. |
| Broker JWT wiring (iotctl post-install + NATS server reconfig) | Deferred to M5 | The **cryptographic half** (`iot_bus::jwt::issue_user_jwt`) shipped in M3 W1.3 — the minter works and has unit tests. The **wiring half** is a multi-file change (iotctl rule.rs post-install, NATS server config flip from `no_auth_user` to operator-JWT mode, dev cert mint script gains an operator keypair step). Worth its own dedicated session; bolting onto M4 would half-ship it. ADR-0011 stays active until this lands. |
| Real CEL interpreter swap | Deferred to M5 | `cel-interpreter` crate API stability concerns flagged in the M3 W2.1 risk table haven't changed. The hand-rolled subset handles every rule we've written so far; demand for more expressiveness will drive the swap. |
| Wildcard-filtered last-msg replay | Deferred to M5 | Needs a JetStream ephemeral consumer with `DeliverLastPerSubject` policy + gateway WS-handler changes. The concrete-subject replay M3 W2.5b shipped covers the 80% case (panel subscribes to a specific device subject). Wildcards are the 20% improvement. |
| Second adapter (Z-Wave or 433-SDR) | Deferred to M5 | Full WASM plugin port; proves nothing new architecturally after the z2m port. Makes sense as part of the edge-ML M5 work (the rtl_433 bridge carries water-meter pulse data + similar). |
| TimescaleDB optional backend | Deferred to M5 | Substantial new storage impl + sqlx feature-flag work. Not user-visible at M4 scale — current SQLite is fine. |
| Command Central v1 PWA | Deferred to M5 | Full PWA backend + frontend + ephemeral auth + kiosk mode. Multi-session build-out, proper M5 home alongside voice. |
| Edge-ML plugins | Deferred to M5 | Hardware + firmware + trained models. Not session-feasible. |

## What was harder than expected

- **Recognising prior art.** Envoy's existing compose wiring was easy to miss in a session that started by re-scoping M4 from the original "edge-ML" framing. Standing item for retros: `grep` the repo for the item before assuming it needs shipping.
- **Streaming gRPC + task-local context.** `list_devices` spawns a tokio task to pump the response stream; that task outlives the `with_context` scope. Deliberate M4 simplification (bulk-read listings aren't a cross-service-trace concern), but worth naming as something to revisit if listing patterns change.

## What was easier than expected

- **Per-handler `with_context` wrap**. Instead of a full tower::Layer + Service + Future machinery to inject the scope at the tonic server level, each handler method is four lines of wrap. Readable, local, no type gymnastics.
- **`OnceLock<()>` for deprecation log gating**. The "once per process lifetime" pattern in the stdlib is exactly what was needed — no LRU cache or per-plugin state needed.

## Architecture debts — updated

M3 retro's list carried forward with status updates:

| # | Debt | Status at v0.4.0-m4 |
|---|---|---|
| 1 | Broker JWT bootstrap wiring | Unchanged — M5 target. Crypto half shipped M3. |
| 2 | Subscriber-loop traceparent wrappers | ✅ shipped post-v0.3.0-m3 (registry / automation / gateway WS). |
| 3 | gRPC traceparent interceptors | ✅ client-side post-v0.3.0-m3; ✅ server-side M4. |
| 4 | Hand-rolled vs. full CEL | Unchanged — M5 target. |
| 5 | Wildcard last-msg replay | Unchanged — M5 target. |
| 6 | Fuel refuel between plugin host calls | Unchanged — M5 target. |
| 7 | MQTT sub-refcount / unsubscribe | Unchanged — backlog. |
| 8 | Permissive Mosquitto ACL | Unchanged — M5. |
| 9 | SLSA provenance `continue-on-error` | Unchanged — M6. |
| 10 | `registry::upsert-device` host capability redundant | ✅ **deprecation log shipped M4**; removal M5 / ABI 1.3.0. |

## Metrics (at v0.4.0-m4)

| | |
|---|---|
| Crates in workspace | 13 (unchanged from M3) |
| WASM plugins | 2 shipping + 1 scaffold (unchanged) |
| ADRs | 13 (none new in M4) |
| Plugin ABI version | 1.2.0 (unchanged; M4 deprecates `registry::upsert-device` via log, removes in 1.3.0) |
| Rust LoC (src/) | ~9k (M3 end) + ~100 (M4 diff) = ~9.1k |
| Workspace tests (nextest) | 111 (unchanged — M4's three slices are composition of already-tested pieces) |
| CI pipeline stages | Unchanged |
| Commits since `v0.3.0-m3` | 3 (M4 itself) + 2 follow-ups from the same session (subscriber wrappers + gRPC client interceptor) |

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

M5 is therefore roughly **7 weeks** of scope, not the originally-planned 4. A candidate split into M5a (edge-ML + Command Central + Timescale + broker-JWT finalization) and M5b (voice pipeline + NILM training loop) is worth considering at M5 planning time.
