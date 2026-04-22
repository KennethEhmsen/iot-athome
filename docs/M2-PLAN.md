# M2 Plan — Plugin SDK + Wasmtime Host + Real Plugins

**Starts:** post-`v0.1.0-m1` · **Target duration:** 4 weeks · **Anchor ADRs:** [0003](adr/0003-plugin-abi-wasm-component-model.md), [0011](adr/0011-dev-bus-auth.md), [0012](adr/0012-plugin-binding-layer.md)

## Goal

Ship a working WASM-Component-Model plugin runtime with capability
enforcement. Prove it by running three plugins through it: `demo-echo`
(sanity), `zigbee2mqtt-adapter` (migrated from M1's systemd path), and
one new adapter (Z-Wave or 433-SDR — TBD in W2).

## Acceptance criterion

> `iotctl plugin install demo-echo.wasm` → host verifies signature →
> mints a NATS account from the manifest → loads the component →
> plugin's `init()` returns OK → plugin echoes a test message within
> 200 ms. Adapter plugin runs identically (same manifest shape, same
> install path).

## Week-by-week

### W1 — Bindings that compile ✅ (2026-04-21)

- [x] `schemas/wit/iot-plugin-host.wit` — exports wrapped in `interface runtime` so wit-bindgen emits a single `__export_world_plugin_cabi` macro (one per world-level free function was the duplicate-name trigger).
- [x] `iot-plugin-sdk-rust`: `wit_bindgen::generate!` with `pub_export_macro: true` + `export_macro_name: "export_plugin"`.
- [x] `iot-plugin-host`: `wasmtime::component::bindgen!` with the 36+ `imports:/exports:` async syntax, plus Host trait impls on `PluginState`.
- [x] `iot-plugin-host::capabilities` — `CapabilityMap` + NATS `foo.>` wildcard matcher, 3 unit tests.
- [x] Round-trip test (`crates/iot-plugin-host/tests/roundtrip.rs`): loads the real `demo_echo.wasm`, exercises `runtime::init()` + `runtime::on_message()` in and out of capability scope, verifies `capability.denied` is returned as an app-level `PluginError` (not a host trap).
- [x] First compilable demo: `plugins/demo-echo/` with `.cargo/config.toml` pinning `wasm32-wasip2`. Produces a 3.5 MB debug `.wasm` component verified via the `\0asm` magic.
- [x] WASI p2 preview-adapter imports (`wasi:cli/environment`, stdio, etc.) linked via `wasmtime_wasi::p2::add_to_linker_async`.

### W2 — Capability enforcement + one real host call ✅ (2026-04-20)

- [x] Manifest loader: parse `plugin-manifest.schema.json` at install, store a `CapabilityMap` with the plugin instance. `crates/iot-plugin-host/src/manifest.rs` parses the YAML via serde into a strongly-typed `Manifest { capabilities: CapabilityMap, resources, … }`, enforces `schema_version == 1` and `runtime == "wasm-component"`, 3 unit tests.
- [x] `bus::publish` host impl checks subject against `capabilities.bus.publish` before calling `iot_bus::Bus::publish_proto`. The handler clones the `Bus` handle before `.await` (async-nats `Client` is `Arc`-backed) so the future stays `Send` — wasmtime's async bindgen rejects `!Send` futures.
- [x] `log::emit` host impl forwards to `tracing` with plugin id as a span field (`plugin = %self.id`, target = plugin-supplied).
- [x] Deny test: a plugin publishing on an out-of-scope subject gets `capability.denied`; audit entry recorded via free-fn `record_denied` (owned params, not `&self`, so the caller's future stays `Send`). `tests/roundtrip.rs::demo_echo_manifest_load_init_deny_audit` asserts both the `PluginError` code and that `AuditLog::verify()` passes after the write.
- [x] `demo-echo` actually echoes: `on_message` → `bus::publish(&format!("{subject}.echo"), …)`.

### W3 — Installation + signing

- [x] `iotctl plugin install <path>` CLI command (adds to iot-cli) — new `crates/iot-cli/src/plugin.rs` with `install / list / uninstall` subcommands. Re-uses `iot_plugin_host::manifest::Manifest::load` so the CLI and runtime agree byte-for-byte on what a valid manifest is.
- [x] Cosign signature verification at install time (ADR-0006 keyless chain) — pinned-pubkey ECDSA-P256 / SHA-256 / DER-encoded signature, matching `cosign sign-blob` output. `--allow-unsigned` for dev, `--trust-pub` (also `IOT_PLUGIN_TRUST_PUB`) pins the public key. Rekor/Fulcio keyless lookup deferred to M6 per the risk table below.
- [x] SBOM extraction + CVE check. `iotctl plugin install` parses the bundled CycloneDX `sbom.cdx.json` at install time, walks `.vulnerabilities[]`, and refuses installs with any `>= High` severity finding unless `--allow-vulnerabilities` is passed. Findings are always logged. Design note: the scan is a *consumer* of the SBOM — the plugin author is expected to run their own `cargo audit` (or `grype`) at build time and bake results into the SBOM (SLSA L3-style "producer attests, consumer verifies"); the hub doesn't keep its own advisory DB, which would be stale on air-gapped installs and fragile to sync.
- [x] Per-plugin NATS account generation: `iotctl plugin install` now mints a fresh ed25519 nkey for each plugin, writes the seed to `<plugin_dir>/<id>/nats.nkey` (0600 on Unix), and emits `<plugin_dir>/<id>/acl.json` with the manifest's publish/subscribe allow-lists for the broker-side bootstrap. Operator-signed user-JWT issuance is a separate concern (lands with the operator-key infra).
- [~] Migrate `zigbee2mqtt-adapter` from its M1 systemd shape — **architecture decided in [ADR-0013](adr/0013-zigbee2mqtt-wasm-migration.md), implementation slipped into W4**. Direct port (rumqttc-on-wasip2) blocked by tokio's wasip2 socket support; ADR-0013 picks the host-side path: MQTT becomes a host capability (matches the `mqtt:` namespace already in the manifest schema), plugin loses its rumqttc/tonic/tokio deps, host binary owns the single broker connection. The MQTT host wiring W4 needs is also exactly what the second adapter (W4) needs, so doing both together amortises the cost. ADR-0011 retires when this lands.

### W4 — Polish + second adapter

#### W4 sequencing

W4 starts with the **MQTT host capability** (per [ADR-0013](adr/0013-zigbee2mqtt-wasm-migration.md)) because three deliverables share it: the slipped z2m migration, the second adapter, and `iotctl plugin list`'s signature-identity enrichment. After that, the order is:

1. MQTT + registry host capabilities → unblocks z2m + second adapter
2. z2m migration on top of the new capabilities (W3 carry-over)
3. Second adapter (Z-Wave or 433-SDR — pick during the MQTT capability spike when the host-side cost is concrete)
4. `iotctl plugin list` enrichment, OTel wire-up, crash supervision (these three are independent of the MQTT work and can land in any order, including in parallel)
5. `v0.2.0-m2` tag

#### W4 deliverables

- [ ] **MQTT + registry host capabilities** (per ADR-0013).
  - `schemas/wit/iot-plugin-host.wit` — `interface mqtt { subscribe; publish; }`, `interface registry { upsert-device; }`, runtime gains `on-mqtt-message`.
  - `crates/iot-plugin-host/src/mqtt.rs` — owns one `rumqttc::AsyncClient`, dispatches inbound topics to subscribed plugins, capability-checks every call against `MqttCapabilities`.
  - `crates/iot-plugin-host/src/registry.rs` — wraps `RegistryServiceClient`, capability-checks against new `RegistryCapabilities { upsert: bool, list: bool }`.
  - `HostBindings` gains `mqtt: Option<MqttHandle>`, `registry: Option<RegistryServiceClient<Channel>>`.
  - Capability tests + dispatcher integration test (fake MQTT broker via `rumqttc::test_utils` or testcontainers Mosquitto).

- [ ] **z2m migration** (W3 carry-over per ADR-0013).
  - Rewrite `plugins/zigbee2mqtt-adapter/` against the new capabilities. Drop `main.rs`, `rumqttc`, `tonic`, `tokio` runtime, `iot_bus`/`iot_proto` deps.
  - Keep `translator.rs` and `state_publisher.rs` — pure functions, port unchanged.
  - `manifest.yaml` declares `mqtt.subscribe: ["zigbee2mqtt/+"]`, `bus.publish: ["device.zigbee2mqtt.>"]`, `registry: { upsert: true, list: true }`.
  - Translator unit tests carry over.
  - Compiles to `wasm32-wasip2`, loads cleanly under the host, end-to-end equivalent to the M1 native adapter.

- [ ] **Second adapter** (pick after MQTT capability spike). Two candidates:
  - **Z-Wave** via `zwave-js-server` sidecar (Node process, same pattern as z2m). Adapter consumes `zwavejs/+` topics.
  - **433-SDR** via `rtl_433` (publishes JSON on `rtl_433/+` topics). Smaller adapter scope, no sidecar required if the host has an SDR dongle.
  - Either way: a new `plugins/<adapter>/` crate, a new `manifest.yaml`, MQTT + bus capabilities only.
  - Loads identically to z2m (`iotctl plugin install` → discovery → `runtime::on-mqtt-message` round-trip).

- [ ] **`iotctl plugin list` signature identity**. Today the command prints `id  version  runtime  (publish:N subscribe:M)`. W4 adds:
  - **Signature status**: `verified <key-fingerprint>` / `unsigned`. Reads `<plugin_dir>/<id>/plugin.wasm.sig` + the trust-pubkey path; emits the SHA-256 of the verifying key as the fingerprint.
  - **Signature identity** from `manifest.yaml.signatures[].identity` if present (cosign keyless mode would carry an OIDC identity here).
  - File: extend `crates/iot-cli/src/plugin.rs::list`. New helper `signature_status(dir, trust_pub) -> SignatureStatus` returning `Verified { fingerprint, identity }`, `Unsigned`, or `VerificationFailed`. 2 unit tests.

- [ ] **OpenTelemetry on host calls**. The plumbing is in `iot-observability`; W2 deferred wiring it into the plugin host. W4 adds:
  - `#[tracing::instrument(skip(self, payload), fields(plugin = %self.id, capability = "bus.publish", subject = %subject))]` on `bus::Host::publish`.
  - Same on `mqtt::Host::publish` / `mqtt::Host::subscribe`, `registry::Host::upsert_device`, `log::Host::emit`.
  - Span attributes carry plugin id + the capability name + the relevant subject/topic. The OTLP exporter (already wired in `iot-observability`) ships them to the collector.
  - Verify with a unit test that asserts the spans are emitted (using `tracing-subscriber`'s test layer).

- [ ] **Crash supervision** with exponential backoff + DLQ.
  - Plugin host loop: on `wasmtime::Trap` from a plugin instance, log a `plugin.crash` audit entry, record crash count in a per-plugin state map, restart with `min(2^count, 30)`-second backoff. After 5 crashes within a 10-minute window, mark the plugin "dead-lettered" and stop restarting. `iotctl plugin list` shows `dead-lettered` next to the id.
  - File: new `crates/iot-plugin-host/src/supervisor.rs` — owns the `HashMap<plugin_id, SupervisedPlugin>` and the restart loop. The existing `run()` in `lib.rs` becomes a thin wrapper that hands off to the supervisor.
  - Integration test: install a plugin whose `init()` traps, assert restart attempts + eventual DLQ.

- [ ] **`v0.2.0-m2` tag + retro** — once W4's checkboxes are green, tag, run the cosign signing pipeline (already wired from M1), write `docs/M2-RETROSPECTIVE.md`.

## Risks

| Risk | Spike | Resolution |
|---|---|---|
| `wasmtime::component::bindgen!` async support still maturing | 1 day W1 | Fallback: sync host interface, spawn work in a `tokio::spawn` wrapper |
| Component Model + WASI Preview 2 tooling fragmentation | ongoing | Pin `wit-bindgen` and `wasmtime` versions exactly; bump in one PR |
| Per-plugin NATS account bootstrap race (account defined but server hasn't reloaded) | 2h W3 | Use NATS JetStream `$SYS.REQ.ACCOUNT` API with a wait-for-effect check |
| Cosign keyless verification offline | 1 day W3 | Support both keyless (Rekor lookup) AND signed-by-key fallback via pinned pubkey |
| zigbee2mqtt sidecar compiled to wasip2 runs into rumqttc TLS limitations | 2 days W3 | Keep z2m itself as Node process, migrate only the Rust adapter; doesn't change the experience |

## Out of scope for M2

- UI tile plugins (land in M3).
- ML models as plugins (M4).
- Firmware-inclusive plugins (M4).
- Per-plugin Mosquitto ACL generation (M3; for M2 we keep the dev-permissive ACL).
- Plugin marketplace / revocation UX (M6).

## Definition of done

- All three plugins load cleanly and behave identically to an operator
  (`iotctl device list` should not know which integration path a device
  flowed through).
- Capability-deny test in CI.
- Plugin crash → clean restart visible in `iotctl plugin list`.
- `v0.2.0-m2` tag signed via the same cosign pipeline as M1.
