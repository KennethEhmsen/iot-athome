# M3 Retrospective — Automation Engine + Observability Foundations

**Tag:** `v0.3.0-m3` · **Completed:** 2026-04-23 · **Plan:** [M3-PLAN.md](M3-PLAN.md) · **Key ADRs:** [0004](adr/0004-nats-subject-taxonomy.md), [0006](adr/0006-signing-key-management.md), [0008](adr/0008-error-handling.md), [0009](adr/0009-logging-and-tracing.md), [0011](adr/0011-dev-bus-auth.md), [0013](adr/0013-zigbee2mqtt-wasm-migration.md)

## What we said we'd ship

Three-week scope from [M3-PLAN.md](M3-PLAN.md):

| Week | Theme |
|---|---|
| W1 | Rollovers: iot-proto split, registry auto-register, broker JWT, JCS audit |
| W2 | CEL rule engine + JetStream retention + OTel traceparent |
| W3 | Envoy + Timescale + Command Central + 2nd adapter |

Plus rollovers from the M2 retro: retire the registry host capability, complete the per-plugin NATS identity story, fix iot-proto for wasm32-wasip2.

## What we actually shipped

Every W1 and W2 deliverable lands; W3 is partial (Envoy / Timescale / Command Central / 2nd adapter intentionally deferred — see §Deviations). The non-obvious structural change: what M3-PLAN listed as single items (e.g. "OTel traceparent propagation") turned into multi-slice chains, because each was meaningful enough to deserve its own commit rather than a fused superwrite.

| Slice | Status | Notes |
|---|---|---|
| **W1.1** `iot-proto` split | ✅ | New `iot-proto-core` (prost only, wasm32-wasip2-friendly) + `iot-proto` (core + tonic overlay). The z2m plugin's duplicated EntityState/Ulid in `src/pb.rs` retires. |
| **W1.2** Registry bus watcher | ✅ | `iot-registry` subscribes to `device.>`, auto-registers unknown `(integration, external_id)` pairs. `registry::upsert-device` host capability is now belt-and-braces for M3; full retirement in M4. |
| **W1.3** Broker JWT bootstrap | 🏗 | Cryptographic half shipped: `iot_bus::jwt` mints NATS v2 decentralised-auth user JWTs from an account nkey + user pubkey + manifest-derived ACL. The iotctl post-install hook + NATS server reconfig (flip from `no_auth_user` to operator-JWT mode) slipped — captured as an explicit follow-up. ADR-0011 stays nominally active. |
| **W1.4** JCS canonical JSON for audit | ✅ | `iot-audit` switched to `serde_jcs` for canonicalisation; `verify()` now re-computes hashes per entry, not just chain-links them. Audit log is actually tamper-detectable now (previously only linkage was checked). |
| **W2.1** Rule parser + expression evaluator | ✅ | YAML → compiled `Rule`; pure hand-rolled grammar (`>`, `<`, `==`, `&&`, `||`, `!`, paths, parens). Took the M3-PLAN risk-table fallback on `cel-interpreter` — documented upgrade path to full CEL in M4 if rules grow. |
| **W2.2** Engine loop | ✅ | Engine subscribes to `device.>`, matches per-rule, evaluates `when`, dispatches Publish / Log actions. |
| **W2.3** `iotctl rule {add,list,delete,test}` | ✅ | `test` dry-runs without a bus, emits the fired actions' shape — fastest rule-author dev loop. |
| **W2.4** W3C traceparent format | ✅ | `iot_observability::traceparent::TraceContext` (generate, child-of, to/from header). OS RNG for uniqueness, full W3C spec compliance (rejects uppercase hex, all-zero IDs, wrong version, etc.). |
| **W2.5** JetStream last-msg-per-subject | ✅ | `Bus::ensure_device_state_stream()` + `Bus::last_state(subject)`. Panel survives reload because state's on the stream. |
| **W2.5b** Gateway WS replay on connect | ✅ | When the panel subscribes to a concrete subject, the gateway fetches `last_state` + emits as the initial WS message. Wildcard subscriptions still live-only (last-per-subject-across-filter is a follow-up). |
| **W2.6** Task-local TraceContext + bus hookup | ✅ | `with_context(ctx, fut)` scopes the handler; bus `publish_proto` reads the task-local via `current()`; `current_traceparent()` stops being a placeholder. |
| **W2.7** Gateway traceparent middleware | ✅ | Inbound `traceparent` header extracted + scoped around every handler. Panel → gateway → bus now carries a contiguous trace id. |
| **W2.8** `bus::extract_trace_context` | ✅ | Subscriber-side symmetric helper. Subscribers wrap their handler in `with_context` using the extracted context. |
| **Engine polish** (idempotency + audit + DLQ) | ✅ | `Engine` gained a 5-second-TTL idempotency cache, `"automation.rule_fired"` audit entries, and `sys.automation.dlq` publishes on action failure. |
| **Engine proto decode** | ✅ | `decode_payload` now tries prost-decoded `EntityState` after JSON-parse misses. z2m state messages are rule-visible without JSON marshalling. |
| **W3** Envoy + Timescale + Command Central + 2nd adapter | ⏸ | All four deferred to M4. Each is a multi-session build-out; splitting across milestones keeps the M3 cut coherent. |

## What we deviated on

Each is a deliberate cut with a named follow-up:

| Plan called for | What we did | Why |
|---|---|---|
| Full broker-JWT bootstrap (iotctl post-install + NATS server reconfig) | Cryptographic minter shipped; wiring slipped | The minter is ~200 LOC of crypto that's worth verifying in isolation. The iotctl post-install + server config reshuffle is itself multi-commit work — better as a dedicated session than bolted onto the crypto commit. ADR-0011 stays active until both halves land. |
| Full CEL interpreter (`cel-interpreter` crate or equivalent) | Hand-rolled subset: comparisons, boolean ops, path access | M3-PLAN risk table explicitly named this fallback; the demo rules ("temp > 25") fit the subset. M4 can drop in a real CEL when the rule library outgrows it — `parse` + `eval_bool` are the drop-in-replaceable seams. |
| Envoy + mTLS frontend | Deferred to M4 | Deployment YAML work, not much that benefits from being in a milestone shared with automation-engine code. |
| TimescaleDB optional backend | Deferred to M4 | Large new storage impl + migration path; wants its own focused slice. |
| Command Central v1 PWA | Deferred to M4 | PWA backend + frontend + ephemeral auth is multi-session by itself. |
| Second adapter (Z-Wave / 433-SDR) | Deferred to M4 | WASM plugin port on top of M2's MQTT capability; well-understood after the z2m port — scope-contained but not urgent for M3's acceptance test. |
| Fine-grained per-rule bus subscriptions | Single `device.>` subscription + in-process fan-out | For M3's scale (dozens of rules) in-process dispatch is cheaper than holding many NATS subscriptions. Revisit when rule counts cross ~100. |
| gRPC traceparent interceptors (gateway → registry) | Not wired | gateway → NATS path is end-to-end; gateway → registry gRPC still goes without a traceparent metadata. Small follow-up; the interceptor APIs are in tonic but not yet threaded. |
| Bus-subscriber traceparent wrappers in each service's subscriber loop | Helper shipped, callers not yet updated | `extract_trace_context` + `with_context` are in `iot-bus`; rewriting the three live subscriber loops (registry bus_watcher, automation engine, gateway stream) is one more small commit. |

## What was harder than expected

- **`iot-proto` split via tonic-build `extern_path`**. Getting the generated service code in `iot-proto` to reference messages from `iot-proto-core` (not regenerate its own) was a one-line fix once the `extern_path` + `compile_fds` pattern clicked, but the documentation on this was thin.
- **`serde_jcs` canonicalisation + hash re-compute in `verify()`**. The W1-era audit log was only chain-linking — tampering with a historical payload was undetectable if the stored hash was kept. Retrofitting a proper re-compute broke every existing dev audit log (the old `serde_json::to_string` form and JCS produce different bytes), documented as a one-way break.
- **Clippy pedantic knob `items_after_statements`**. Kept catching `use` inside functions; pushed them to module scope, which is fine but changes the "use-local-as-doc" pattern.
- **`clippy::too_long_first_doc_paragraph`**. Multiple times a module-level doc comment's first paragraph was too long; splitting with a blank line is cheap but was a repeated diff churn.
- **`google.protobuf.Value` → `serde_json::Value` round-trip**. Numeric values unwrap to f64 exclusively, so `serde_json`'s PartialEq treats `255.0` (float) ≠ `255` (int). Tests have to assert via `.as_f64()` not direct eq. Documented in the engine tests.

## What was easier than expected

- **Tokio `task_local!`**. The `CURRENT.scope(ctx, future).await` primitive is exactly what we needed for trace propagation — no need to reach for tracing-opentelemetry's full context-carrier setup. Our `with_context` fits in 12 lines.
- **Hand-rolled expression grammar**. ~500 LOC (with tests) for a lexer + recursive-descent parser + tree walker. Parser generators would have cost more in ceremony than this cost in code.
- **Idempotency cache as a `Mutex<HashMap>`**. We considered a `DashMap` or time-indexed skiplist; plain `HashMap` with per-call prune is fine at M3 scale. Revisit if the engine's rule count × message rate pushes contention.
- **Gateway middleware via `axum::middleware::from_fn`**. The traceparent middleware is ~40 LOC including docs, plugs in as one `.layer()` call, covers REST + WS + health via single-layer application.
- **JetStream `get_or_create_stream` + `max_messages_per_subject = 1`**. The "last-msg-per-subject" semantic we wanted is literally one config field; no custom retention logic required.

## Architecture debts taken deliberately

Each has a named future resolution:

| Debt | Where it bites | Resolved by |
|---|---|---|
| Broker JWT bootstrap wiring half-shipped | Plugins still auth via `no_auth_user` in dev | Early-M4 slice: iotctl post-install hook using the minter + NATS server config flip. ADR-0011 retires when this lands. |
| `registry::upsert-device` host capability still callable | Technically redundant once the bus watcher is running; plugins keep calling it as belt-and-braces | M4: emit a `registry.deprecated` log line when called; full removal M5. |
| Subscriber loops not yet wrapped in `with_context` | Traceparent context drops at each subscriber; child-spans look like orphans | Follow-up: three small commits (one per live subscriber loop: registry bus_watcher, automation engine, gateway stream). |
| gRPC traceparent interceptor | gateway → registry calls carry no trace id metadata | tonic `Interceptor` layer — one commit per direction. |
| Hand-rolled expression language vs. full CEL | Some legitimate rule patterns (`payload.values.max() > 25`) won't parse | M4 swap to `cel-interpreter` or equivalent behind the existing `parse`/`eval_bool` facade. |
| Wildcard-filtered last-msg replay | Panel subscriptions to `device.>` get no replay on connect | M4: use a JetStream ephemeral consumer with `DeliverLastPerSubject` policy. |
| Fuel budget per plugin is one-shot (carried from M2) | Long-running plugins aren't refueled | M4 supervisor adds `Store::add_fuel` between exports. |
| MQTT broker subscription never unsubscribed on plugin exit | Broker keeps the subscription; wastes bandwidth after many plugin churns | Sub-refcount once the plugin churn rate is actually bothering us. |
| SLSA provenance still `continue-on-error: true` | Private-repo block from M2 retro | M6 — when the repo goes public or moves to a paid org plan. |

## Metrics (at v0.3.0-m3)

| | |
|---|---|
| Crates in workspace | 13 (iot-proto-core new in M3) |
| WASM plugins | 2 shipping (demo-echo + zigbee2mqtt-adapter) + 1 scaffold (power-meter-3ph) |
| ADRs | 13 (no new ADRs in M3 — the architectural calls were all pre-approved by existing ADRs) |
| Plugin ABI version | 1.2.0 (unchanged; M3 didn't touch the plugin surface) |
| Rust LoC (src/) | ~9k (up from ~6k at M2) |
| Workspace tests (nextest) | **111 passing** (+53 vs M2) |
| CI pipeline stages | Unchanged: 7 per-push, 3 on tag |
| Supply-chain advisories ignored | 5 (same rustls-webpki 0.102.x cluster as M2) |
| Commits since `v0.2.0-m2` | 17 |

Test trajectory this milestone: 58 → 62 (proto split) → 68 (JCS audit) → 82 → 87 → 91 → 100 (traceparent) → 103 → 105 → 107 → **111**.

## What ships next (M4)

From design doc + the deferrals above:

1. **Envoy + mTLS frontend** (M3 W3 carry-over).
2. **TimescaleDB optional backend** (M3 W3 carry-over).
3. **Command Central v1 PWA** (M3 W3 carry-over).
4. **Second adapter** — Z-Wave via `zwave-js-server` or 433-SDR via `rtl_433` (M3 W3 carry-over).
5. **Broker JWT bootstrap wiring** (M3 W1.3 slice 2). Retires ADR-0011.
6. **gRPC traceparent interceptors** (M3 W2 follow-up).
7. **Subscriber-loop traceparent wrappers in iot-registry / iot-automation / iot-gateway** (M3 W2 follow-up).
8. **Real CEL interpreter** for rules (M3 W2.1 upgrade path).
9. **Wildcard-filtered last-msg replay** (M3 W2.5 follow-up).
10. **Fuel refueling + MQTT sub unsubscribe** (M2 debts).

M4's design-doc focus — the reference edge-ML plugins (water-meter CV, mains-power 3-phase, heating, NILM training) — lines up with the 2nd adapter slip: both are plugin-shape work that wants the MQTT capability + the broker JWT flow in place first.
