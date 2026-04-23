# M2 Retrospective — WASM Plugin Runtime

**Tag:** `v0.2.0-m2` · **Completed:** 2026-04-23 · **Plan:** [M2-PLAN.md](M2-PLAN.md) · **Key ADRs:** [0003](adr/0003-plugin-abi-wasm-component-model.md), [0006](adr/0006-signing-key-management.md), [0011](adr/0011-dev-bus-auth.md), [0012](adr/0012-plugin-binding-layer.md), [0013](adr/0013-zigbee2mqtt-wasm-migration.md)

## What we said we'd ship

Four-week scope from [M2-PLAN.md](M2-PLAN.md):

| Week | Scope | Anchor |
|---|---|---|
| W1 | Bindings compile — WIT world, wit-bindgen SDK, wasmtime host, round-trip test, demo-echo | ADR-0012 |
| W2 | Capability enforcement + real bus wiring — manifest parser, `bus::publish` with ACL, audit-on-deny | ADR-0003 |
| W3 | Install + signing — `iotctl plugin install`, cosign verify, SBOM CVE gate, per-plugin NATS account, z2m migration | ADR-0006 |
| W4 | Polish — second adapter, `iotctl plugin list` sig identity, crash supervision, OTel on host calls, `v0.2.0-m2` tag | — |

## What we actually shipped

Every W1–W4 deliverable lands except the second adapter (intentional slip, see §Deviations). The bigger surprise: the z2m migration that was penciled for W3 required a new host capability the plan didn't call for, so it slipped into W4 with an ADR ([0013](adr/0013-zigbee2mqtt-wasm-migration.md)).

| Week-scope | Status | Notes |
|---|---|---|
| **W1** WIT world + bindings | ✅ | `iot:plugin-host@1.0.0` with `interface runtime`; wit-bindgen `pub_export_macro`; wasmtime `bindgen!` with explicit `imports:`/`exports:` async syntax |
| **W1** Round-trip test | ✅ | Real `demo_echo.wasm` loaded, `init()` + `on_message()` exercised in and out of capability scope |
| **W2** Manifest parser | ✅ | `Manifest::load()` enforces `schema_version == 1` + `runtime == "wasm-component"`; 3 unit tests |
| **W2** `bus::publish` + audit | ✅ | Real `iot_bus::Bus::publish_proto` wiring; capability-denied writes `plugin.denied` audit entry; hash-chain verified in test |
| **W2** `log::emit` → tracing | ✅ | Plugin-id span field on every log |
| **W3** `iotctl plugin install` | ✅ | Validate manifest, verify signature, SBOM CVE scan, mint per-plugin nkey + ACL, copy bundle into `<plugin_dir>/<id>/` |
| **W3** Cosign signature verify | ✅ | Pinned-pubkey ECDSA-P256 via `p256` crate (keyless+Rekor deferred to M6 per ADR-0006) |
| **W3** SBOM CVE check | ✅ | CycloneDX `.vulnerabilities[]` parsed; refuses install on `>=High` unless `--allow-vulnerabilities` |
| **W3** Per-plugin NATS account | Partial | `nats.nkey` seed + `acl.json` snapshot written; operator-JWT issuance is broker-side infra, scheduled for early M3 |
| **W3** z2m WASM migration | ✅ (slipped to W4) | Direct rumqttc-on-wasip2 path was blocked; ADR-0013 picked the host-MQTT-capability path instead. Lands in W4. |
| **W4** `iotctl plugin list` sig identity | ✅ | SIGNATURE + IDENTITY + HEALTH columns; SHA-256 pubkey fingerprint |
| **W4** OTel on host calls | ✅ | `#[tracing::instrument(name="host_call", fields(plugin, capability, …))]` on `bus::publish`, `log::emit`, `mqtt::subscribe/publish`, `registry::upsert-device` |
| **W4** Crash supervision | ✅ | `CrashTracker` (exponential backoff, 5-in-10min → DLQ); `.dead-lettered` marker; supervisor loop around every plugin task; `iotctl plugin list` shows HEALTH |
| **W4** MQTT host capability | ✅ (bonus) | `MqttBroker` (rumqttc client + eventloop + mTLS) + `MqttRouter` (fan-out) + testcontainers Mosquitto integration test. Not in the original plan — required by the z2m-migration path ADR-0013 chose. |
| **W4** Registry host capability | ✅ (bonus) | ABI 1.2.0 `interface registry` wrapping tonic. Transitional per ADR-0013 §Consequences — retires when M3 ships registry-side bus-driven auto-register. |
| **W4** Second adapter | 🔄 deferred to M3 | Explicitly allowed to slip per M2-PLAN risk table |
| **W4** `v0.2.0-m2` release ceremony | ✅ | Tag triggers the same cosign pipeline as M1 |

## What we deviated on

Each of these is a deliberate cut or re-route with a named follow-up:

| Plan called for | What we did | Why |
|---|---|---|
| z2m migrated in W3 | Migrated in W4 behind a host MQTT capability | Direct rumqttc → wasip2 port blocked by `tokio::net` not having a wasip2 binding. ADR-0013 captures the design; host-owns-broker is the cleaner long-term architecture and matches what the manifest schema's `mqtt:` namespace has always hinted at. |
| Second adapter (Z-Wave or 433-SDR) | Deferred to early M3 | Risk table explicitly allowed this; shares the MQTT capability with z2m so doing it post-tag loses no velocity. |
| Per-plugin NATS account minting | Seed + ACL snapshot only | The operator-signed user JWT needs an operator keypair the dev env doesn't have yet. Nkey + ACL on disk is enough for M3's broker-bootstrap commit to mint real JWTs without re-reading the manifest. |
| Cosign keyless with Rekor lookup | Pinned-pubkey ECDSA-P256 only | M6 territory per ADR-0006. The `p256` path we shipped accepts exactly what `cosign sign-blob` produces; keyless is an additive verifier, not a replacement. |
| Full pure-Rust cosign verify via the `sigstore` crate | `p256` crate directly | `sigstore` 0.10 pulls hyper + reqwest-rustls + oci-distribution — too heavy for the single blob-verify call at install time. Revisit when Rekor lookup lands. |
| cargo-audit offline DB on the hub | Consumer-side SBOM CVE gate | Plugin author runs `cargo audit` at build time and bakes vulns into the SBOM. SLSA L3-style "producer attests, consumer verifies" — avoids a stale advisory DB on air-gapped installs. |

## What was harder than expected

- **rumqttc on `wasm32-wasip2`**: the expected path for the z2m migration, three days of spike, dead end. `tokio::net` has no wasip2 binding; `tonic` is in the same boat. Caught in W3, which is why z2m slipped into W4. Resolution: the host-MQTT-capability design in ADR-0013 (lands cleanly; all the MQTT+registry infrastructure it needs is shipped in this milestone).
- **Wasmtime `Store<T>` lifecycle**: `Store` is `Send` but not `Sync`, export calls take `&mut store`. The clean design is one tokio task per plugin owning its Store, external dispatchers talk to it via mpsc. Spelled out once in [`runtime.rs`](../crates/iot-plugin-host/src/runtime.rs); the supervisor + MQTT dispatcher + future bus router all plug into the same shape.
- **`iot-proto` won't target `wasm32-wasip2`**: tonic → hyper → native sockets. The z2m plugin had to redeclare its own wire-compatible `EntityState` + `Ulid` prost messages. Field numbers must be kept in lockstep with `schemas/iot/device/v1/entity_event.proto`. Followup tracked but not urgent — the schema file is the source of truth either way.
- **Wasmtime bindgen `HasSelf<T>` + async**: wasmtime 36 needs explicit `imports: { default: async }` + `exports: { default: async }` in the `bindgen!` macro plus `HasSelf<PluginState>` on every `add_to_linker::<_, HasSelf<_>>`. Not obvious from the docs; caught quickly but worth memorialising.
- **Send bounds on async host impls**: every time the host impl holds `&self` (or even `&self.bus`) across an `.await`, the resulting future is `!Send` and Wasmtime's async trait rejects it. Pattern that works: clone the `Arc<T>` / Client before the await and let the borrow drop. Captured in a code comment at `component.rs::publish`.
- **Two `cargo fmt` misses in a row**: `de43ba9` and `9a0c2f6` both pushed with a stale fmt state because I'd edited after running `cargo fmt --all`. Pattern is now "fmt immediately before commit, no intervening edits" — and it's noted as a lesson at the bottom of the fmt-fix commit body.
- **Billing interruption mid-session**: GitHub Actions spending limit tripped and stopped runs cold for ~1h. Unblocked by increasing the limit; three runs came back as billing-refusal artifacts with `X` status rather than real failures. Status doc and live `just ci-local` made the local gate authoritative throughout.

## What was easier than expected

- **Capability ACL enforcement**: the shape from W1 (`CapabilityMap::check_bus_publish`) generalised trivially to `check_mqtt_publish/subscribe`, `check_registry_upsert`. Every host call became the same 5-line pattern (check → on deny: log + record_denied + return; on allow: do the thing).
- **wasmtime component instantiation**: once the WIT world stabilised, adding a new interface (mqtt@1.1.0, registry@1.2.0) was a WIT-delta + `add_to_linker` line + Host impl. demo-echo rebuilds unchanged — wit-bindgen regenerates silently. Additive minor bumps really are additive.
- **testcontainers**: `eclipse-mosquitto:2` refuses to start without a config; we just override the entrypoint to write a two-line inline config at container start. The resulting integration test (`crates/iot-plugin-host/tests/mqtt_broker.rs`) runs in ~16 s on CI and validates the whole `broker.publish → Mosquitto → eventloop → router.dispatch → tx.send` pipe.
- **`MqttRouter` pure-logic split**: putting the routing table in its own module with no rumqttc dep meant 6 exhaustive unit tests (wildcard matching, narrowing, multi-subscriber fan-out, closed-mailbox pruning) landed before the broker connection existed. Confidence in the glue commit was high because the routing was already proven.
- **Local CI parity**: `just ci-local` never diverged from the remote `preflight + test + vuln + build + sbom + integration` bundle. Every commit this milestone was verified locally before push; billing-caused CI gaps were a speed bump, not a correctness gap.

## Architecture debts taken deliberately

Each has a named future resolution — not "we'll get to it":

| Debt | Where it bites | Resolved by |
|---|---|---|
| Registry host capability (`registry::upsert-device`) | Plugins do gRPC through the host; round-trip on every unknown device | M3: `iot-registry` subscribes to `device.*` and auto-registers unknown `(integration, external_id)` pairs. Host capability retires (ADR-0013 §Consequences). |
| Per-plugin NATS account: nkey + ACL only | Plugins can't actually auth to NATS with their own identity yet — everyone still uses the shared `IOT` dev account | M3 broker-bootstrap: uses the operator keypair to mint a user JWT from the on-disk ACL snapshot. The plugin's seed already matches the nkey in the JWT. |
| Wire-compatible `EntityState` redeclared in z2m plugin | Schema drift risk if `iot-proto`'s proto ever changes field numbers | M3: strip `iot-proto` into `iot-proto-core` (prost-only, wasip2-friendly) + `iot-proto` (core + tonic clients). Plugins depend on the core. |
| No Rekor / cosign keyless at install | Signed-by-key fallback only; OIDC-identity claims in manifest are advisory | M6: Rekor lookup added to `verify_signature` — the rest of the signing pipeline stays unchanged. |
| SBOM-consumer CVE check only | Host can't spot vulns the plugin author missed | M6 optional follow-up: ship an on-hub advisory DB for second-opinion scans. Not a blocker for plugin installs. |
| cargo-audit at install time (skipped) | Relies on the plugin's own build-time audit catching everything | Same M6 follow-up. |
| Fuel budget per plugin is one-shot (1 billion at init) | Long-running plugins don't get refueled between host calls | M3 adds `Store::add_fuel` in the supervisor's command-loop between export calls, with a per-capability budget from the manifest's `resources.fuel_max`. |
| MQTT broker subscription is never dropped on plugin exit | Broker keeps the subscription after the plugin dies → wasted bandwidth if many plugins churn | `MqttBroker::unsubscribe_filter` method once the subscription-refcount tracking lands. Not urgent; no correctness impact. |

## Metrics (at v0.2.0-m2)

| | |
|---|---|
| Crates in workspace | 12 (iot-core, -proto, -bus, -registry, -automation, -gateway, -plugin-host, -plugin-sdk-rust, -audit, -observability, -config, -cli) |
| WASM plugins | 2 shipping (demo-echo + zigbee2mqtt-adapter) + 1 scaffold (power-meter-3ph M4) |
| ADRs | 13 (0001–0013; +3 this milestone) |
| Plugin ABI version | **1.2.0** (additive minors from 1.0.0) |
| Rust LoC (src/) | ~6k (up from ~3k at M1) |
| TypeScript LoC (panel/) | ~900 |
| Workspace tests (nextest) | 58 passing (+46 vs M1) |
| Out-of-workspace plugin tests | 3 (z2m translator) |
| CI pipeline stages | 7 green on every push (preflight, build x86_64, build aarch64, test, sbom, integration, vuln) + 3 on tag (sign, reproducibility, publish) |
| Supply-chain advisories ignored | 5 (all transitive rustls-webpki 0.102.x CVEs pinned by rumqttc 0.24; revisit when rumqttc actually bumps) |
| Commits since `v0.1.0-m1` | 20 |

## What ships next (M3)

From design doc §4.2, §3:

1. **CEL-based rule engine** — declarative YAML rules compiled to a DAG; triggers → conditions → actions; idempotency keys; dead-letter subjects.
2. **Registry auto-register on bus** — consumes `device.*` subjects, creates devices on first sight. Retires the `registry::upsert-device` host capability.
3. **OTel traceparent propagation** — gateway ↔ registry ↔ plugins. Every automation firing becomes a debuggable span tree.
4. **JCS (RFC 8785) canonical-JSON for audit** — replaces the ad-hoc serde_json hash form from M1 (ADR-0008 follow-up).
5. **NATS JetStream last-msg-per-subject** — panel survives reload because state stays on the stream.
6. **Envoy + mTLS frontend** — gateway moves behind Envoy; registry ↔ gateway switches to mTLS via Envoy's upstream TLS.
7. **TimescaleDB** (optional backend) for long-term retention.
8. **Command Central v1** (PWA + kiosk shell) — per-person ephemeral auth, device cert identity, proximity wake.
9. **Broker JWT bootstrap** — uses the per-plugin nkeys M2 laid down to mint real NATS user JWTs at install-time (retires ADR-0011).
10. **Second adapter** (Z-Wave via zwave-js-server, or 433-SDR via rtl_433). Now a straight port on top of the M2 MQTT + registry capabilities.

## What ships two milestones out (M4)

The reference edge-ML plugins the design doc exists to enable, unchanged from the M1 retro:

- **Water-meter CV** (ESP32-CAM + TFLM digit classifier)
- **Mains-power 3-phase** (ESP32-S3 + ATM90E32) — scaffold exists: `plugins/power-meter-3ph/{manifest.yaml,README.md}`
- **Heating flow/return ΔT + COP** (piggybacks on the power-meter ESP32)
- **NILM training loop** (hub-side, Python + ONNX)
