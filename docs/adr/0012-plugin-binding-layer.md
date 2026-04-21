# ADR-0012: Plugin binding layer — wit-bindgen + wasmtime::component::bindgen

- **Status:** Accepted
- **Date:** 2026-04-21
- **Context:** extends ADR-0003 with concrete tooling

## Context

ADR-0003 chose WASM Component Model + WIT as the plugin ABI. That left the
tooling question open: which generator do we use on the plugin side, and
which on the host side? They're different macro families that must agree
on the exact WIT interpretation.

## Decision

**Guest (plugin) side: `wit-bindgen` crate's `generate!` macro** at the
crate root of each plugin. One invocation that points at our published WIT:

```rust
wit_bindgen::generate!({
    world: "plugin",
    path: "../../schemas/wit",
});
```

This generates:
- Import functions for `bus::publish`, `log::emit`, etc.
- A `Guest` trait with the export methods (`init`, `on_message`).

The SDK crate `iot-plugin-sdk-rust` re-exports these so plugin authors say
`use iot_plugin_sdk::prelude::*;` and get the whole surface.

**Host (plugin-host) side: `wasmtime::component::bindgen!` macro** (the
crate `wasmtime` ships it, distinct from `wit-bindgen` proper):

```rust
wasmtime::component::bindgen!({
    world: "plugin",
    path: "../../schemas/wit",
    async: true,
});
```

This generates:
- A `Plugin` type that wraps a loaded component.
- A `PluginImports` trait that the host implements to supply `bus`/`log`.
- `instantiate_async` / `call_init` / `call_on_message` methods.

## Why two macros, not one

`wit-bindgen` and `wasmtime::component::bindgen!` read the same WIT file
but emit different code because the generator targets are mirror images:
guest sees `bus::publish` as a free function, host implements
`PluginImports::bus_publish` as a method. One macro handling both would
need runtime detection of the compilation target; the separation is
cleaner.

## Versioning

Both macros pin to the WIT world version (`iot:plugin-host@1.0.0`). Minor
bumps are additive: adding a new import function lets older plugins still
load (they just don't call it). Breaking changes require a new world
version and a supported-versions list in the host.

## Capability enforcement

The host's `PluginImports` impl checks the plugin's declared manifest
capabilities before each host call:

```rust
impl PluginImports for PluginInstance {
    async fn bus_publish(&mut self, subject: String, iot_type: String, payload: Vec<u8>)
        -> Result<(), PluginError>
    {
        self.capabilities.check_bus_publish(&subject)?;
        self.bus.publish_proto(&subject, &iot_type, payload, None)
            .await
            .map_err(|e| PluginError { code: "bus.publish.failed".into(), message: e.to_string() })
    }
}
```

The `check_bus_publish` function compares against the manifest's
`capabilities.bus.publish` allow-list, denying on mismatch.

## Consequences

- Two macro dependencies (wit-bindgen + wasmtime/component) but they're
  both well-maintained and pin naturally.
- The "bindgen contract" becomes a test: a round-trip test that a
  component built against wit-bindgen@X loads and runs on a host built
  against wasmtime::component::bindgen@Y.
- When Component Model lands async-by-default (wasmtime 40+ planned),
  both sides update in lock-step; world bump not required.

## Alternatives considered

- **Extism**: simpler DX but diverges from standard Component Model.
  Rejected — we want to stay on the WASI CG main line.
- **Raw wasmtime C ABI**: every plugin ships its own FFI, no type safety.
  Rejected in ADR-0003 already.
- **Single bindgen targeting both**: doesn't exist today. If it does
  later, swap in — the WIT file is unchanged.
