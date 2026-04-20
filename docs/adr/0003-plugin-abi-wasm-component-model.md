# ADR-0003: Plugin ABI — WASM Component Model + WIT

- **Status:** Accepted
- **Date:** 2026-04-20

## Context

Plugins are a load-bearing abstraction of this platform. Every protocol adapter, device integration, ML model, and UI tile ships as a signed plugin. Plugins must:

- Run in a capability-scoped sandbox with no ambient authority.
- Be polyglot (Rust, Go, TypeScript, Python are all first-class targets).
- Present a stable, typed, versionable interface to the host and to each other through the host.
- Be fast to load (< 10 ms cold start target) and cheap in memory (< 128 MB default cap).

Options considered:

| Option | Verdict |
|---|---|
| Raw Wasmtime C ABI (`wasmtime::Func`) | Untyped, stringly schemas, every language binding reinvents marshalling. Rejected. |
| **WASM Component Model + WIT via `wit-bindgen`** | Typed structural interfaces, official multi-language bindings, resource types, async support landing. Chosen. |
| Protobuf over stdin/stdout between host and plugin subprocess | Works today but throws away all the sandbox benefits and pays a fork-per-call cost. Acceptable only for OCI-container plugins (§ADR-TBD on container fallback). |
| Extism | Nice DX but currently less expressive than Component Model; bets on a divergent module format. |

## Decision

Plugin interfaces are defined as **WIT worlds** in `schemas/wit/`. `wit-bindgen` generates bindings for Rust, Go, TypeScript, Python (when mature).

**ABI stability guarantees:**

- WIT worlds are **versioned** (`iot:plugin-host@1.0.0`). Major version = breaking change; minor = additive only (new functions, new optional record fields).
- The host supports **one major version back** for a full major cycle (≥ 1 year). A plugin compiled against v1.x works on hosts v1.x and v2.x; on v3.x it must be recompiled.
- Additive changes are enforced in CI via `wit-diff` (or equivalent) gating PRs that touch `schemas/wit/`.
- Never reuse a removed function name within a major version.

**Wasmtime version pinning:** pinned exactly in the workspace. Bumping Wasmtime is an ADR-triggering event because plugin cold-start and memory characteristics depend on it.

**Bus payload shape is NOT part of the WIT ABI.** Bus payloads are Protobuf (see ADR-0005). The plugin calls `bus.publish(subject: string, payload: list<u8>)`, and the bytes are Protobuf-encoded by the plugin SDK.

## Consequences

- Component Model is young; expect upstream churn. We accept 1-2 wasmtime bumps per year as cost.
- Polyglot SDK maintenance: Rust and TS SDKs are first-class from M2; Go/Python follow as Component Model bindings mature.
- OCI-container plugins (for HW-access cases like SDR, serial devices) get a **separate**, more limited, stdio-based ABI. They do not use WIT. This is intentional: the container path is an escape hatch, not a peer of the primary path.
- Capability enforcement happens at the host-function boundary (host checks the plugin's declared manifest before performing the requested action). The WIT world is the menu; the manifest is the plugin's order.
