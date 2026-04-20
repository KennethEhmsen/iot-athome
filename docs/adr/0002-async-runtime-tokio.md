# ADR-0002: Async Runtime — Tokio

- **Status:** Accepted
- **Date:** 2026-04-20

## Context

Every Rust service in this project is network-bound: NATS JetStream client, Axum HTTP gateway, tonic gRPC (intra-service), sqlx (Postgres/SQLite), Wasmtime async hosting for plugins. An async runtime choice is load-bearing and painful to swap later because ecosystem crates pin to a specific runtime through traits and trait objects.

Options considered:

| Runtime | Notes |
|---|---|
| **tokio** | De-facto standard. All target crates (axum, tonic, sqlx, async-nats, wasmtime) target it. Wide library ecosystem. |
| async-std | Effectively in maintenance mode. Diverges from std and the rest of the ecosystem. |
| smol | Minimalist, excellent executor design, but NATS/Axum/tonic target tokio. Mixing is possible via compat shims but adds surface area. |
| glommio | Thread-per-core. Interesting for high-throughput TSDB ingestion. Doesn't fit a multi-service hub on a Pi-class machine. |

## Decision

**Tokio, single runtime, full feature flag.** Every service uses `#[tokio::main(flavor = "multi_thread")]`. Library crates depend on `tokio` with **only the features they use**, never `full`.

Runtime version: pin in the workspace `Cargo.toml` `[workspace.dependencies]`; bump deliberately.

## Consequences

- Plugin host (Wasmtime async) can share the runtime with service code without cross-runtime shenanigans.
- Spawning conventions: service-owned background work uses `tokio::spawn`; plugin-invoked work uses a plugin-scoped `JoinSet` so cancellation on plugin unload is trivial.
- Accept that blocking work (sync SQLite, signature verification, image preprocessing on the hub) must go through `tokio::task::spawn_blocking`. Track blocking-pool saturation via a tracing span.
- If a future component genuinely needs thread-per-core (e.g. a streaming TSDB writer), it runs in its own process with its own runtime — not inside the core.
