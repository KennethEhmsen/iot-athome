# ADR-0008: Error Handling Strategy

- **Status:** Accepted
- **Date:** 2026-04-20

## Context

Rust's error handling is a choice, not a default. Inconsistent choices (mixing `Box<dyn Error>`, `anyhow::Error`, and bespoke enums across crates) produce error stories that lose information at boundaries. Across services and plugin ABIs, stringly-typed errors become untraceable and unprogrammable.

## Decision

### Rule of thumb

- **Library crates** (everything in `crates/` that is a `lib`): **`thiserror`** with explicit per-crate error enums.
- **Binary crates** (service `main.rs`, `iotctl`): **`anyhow::Result`** at the top level. Enum errors from libs convert via `?`; context is added at each layer with `.context("what we were trying to do")`.
- **Over-the-wire errors** (gRPC status, bus error events, plugin-host↔plugin return values): a single **`iot.proto.ErrorEnvelope`** Protobuf message. Never serialize `anyhow` strings into wire messages.

### `ErrorEnvelope`

```proto
message ErrorEnvelope {
  string code = 1;          // stable, greppable (e.g. "registry.device.not_found")
  string message = 2;       // human-readable, not for programmatic branching
  map<string, string> detail = 3;  // structured fields
  string trace_id = 4;      // W3C trace_id for cross-service correlation
  google.protobuf.Timestamp at = 5;
}
```

Codes are **dotted-namespace, stable strings**. Adding a new code is backward-compat; removing or renaming is breaking.

### At service boundaries

- Each service defines a `map_error(e: ServiceError) -> ErrorEnvelope` function. This is the *only* place errors are translated. Compile-time enforcement via `From<ServiceError> for ErrorEnvelope`.
- gRPC status codes: well-known mapping (NotFound, PermissionDenied, FailedPrecondition, ...) in addition to the envelope.

### Tracing integration

- Every error path logs at `error!` level via the `tracing` crate. The log event includes:
  - `error.code`
  - `error.detail` (structured)
  - automatic `trace_id` / `span_id` from the ambient span.
- `tracing::error!` is the *only* error log channel. No `eprintln!`, no `log::error!`.

### Panics

- Panics in service code are treated as bugs. All top-level spawned tasks are wrapped in `AbortOnPanic` (service restart via systemd / supervisor); panic payloads are reported via a dedicated crash-report bus event, but the process does not attempt to self-recover.
- Plugins cannot panic the host: the Wasmtime guest trap surfaces as an error, not a host panic.

### Forbidden patterns

- `.unwrap()` and `.expect()` in library code (clippy-gated except in `main.rs` and tests).
- `Box<dyn Error>` in public APIs.
- `String` as an error type.
- Stringly-typed error codes (`if err.to_string().contains("not found")`).

## Consequences

- Every error has a stable code that's searchable across logs, alerts, and dashboards.
- Error-handling surface grows predictably with the codebase; new devs learn one pattern.
- Slight boilerplate cost (explicit `From` impls, error enums per crate). Worth it for a 5+ year system.
- Operational dashboards key on `error.code` as the primary dimension. Alert rules are precise.
