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

- [ ] `iotctl plugin install <path>` CLI command (adds to iot-cli).
- [ ] Cosign signature verification at install time (ADR-0006 keyless chain).
- [ ] SBOM extraction + CVE check via cargo-audit's offline database.
- [ ] Per-plugin NATS account generation: issue a new account with publish/subscribe ACLs from the manifest, store credentials in `/var/lib/iotathome/plugins/<id>/nats.creds`.
- [ ] Migrate `zigbee2mqtt-adapter` from its M1 systemd shape: same Rust code, but compiled to wasm32-wasip2 and loaded via the plugin host. ADR-0011 is superseded at this point.

### W4 — Polish + second adapter

- [ ] Second real adapter: Z-Wave (via zwave-js-server, same sidecar pattern as z2m) or 433-SDR (rtl_433 → adapter).
- [ ] `iotctl plugin list` shows loaded plugins + their declared capabilities + signature identity.
- [ ] Plugin crash → host restart with exponential back-off; dead-letter after 5 crashes.
- [ ] OpenTelemetry wired back in: host-call spans carry plugin id + capability used. Revives the bit deferred from W2.
- [ ] `v0.2.0-m2` tag + retro.

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
