# ADR-0009: Logging and Tracing — tracing + OpenTelemetry

- **Status:** Accepted
- **Date:** 2026-04-20

## Context

The design doc states: "a failing automation is a debuggable span tree, not a log grep." Meeting that promise requires disciplined instrumentation from day one. Retrofitting tracing is expensive and almost never complete.

## Decision

### Instrumentation

- **`tracing` crate** as the single observability API in every Rust service.
- **`tracing-opentelemetry`** bridges to OTLP/gRPC exporter.
- Default local dev backend: **Tempo** (traces) + **Loki** (logs) + **Prometheus** (metrics) via docker-compose. Production: OTLP-compatible collector of the operator's choice; default configured for self-hosted Tempo/Loki/Prometheus on the same hub.

### Log shape

- **All logs JSON**, emitted via `tracing-subscriber` with a JSON formatter.
- Mandatory fields on every event:
  - `timestamp` (RFC 3339)
  - `level`
  - `target` (Rust module path)
  - `trace_id` and `span_id` (when an ambient span exists)
  - `service.name`
  - `service.version`
- Events inside a span automatically inherit span fields.

### Trace propagation

- **W3C Trace Context** is the wire format.
- Every NATS bus message carries a `traceparent` header (set by the publisher's active span).
- Subscribers extract the parent context and create a consumer span with the `span.kind=consumer`.
- gRPC calls use `tonic`'s tracing middleware with W3C propagation.
- HTTP ingress (Axum) extracts `traceparent` from headers and starts a server span.
- Plugin host passes `traceparent` into plugin calls via an explicit WIT parameter; the plugin SDK pushes it as the ambient trace context for the plugin's own operations.

### Naming and conventions

- **Span names** follow OpenTelemetry semantic conventions where applicable (`rpc.<service>.<method>`, `messaging.nats.consume`, `db.query`).
- **Field names** use dot-namespaced keys (`device.id`, `plugin.id`, `automation.rule_id`). Prefer consistent names over descriptive ones.
- **Levels:**
  - `error!` — something broke that operators must see.
  - `warn!` — something unusual but handled.
  - `info!` — state transitions, service lifecycle, correlation points.
  - `debug!` — developer-useful detail; off by default in production.
  - `trace!` — very verbose; only enabled by targeted directive.

### Dynamic control

- Default log level: `info`, configurable via `RUST_LOG` using `tracing-subscriber`'s `EnvFilter` syntax.
- **Hot-reload:** services listen for `SIGHUP` (Linux) or a specific NATS subject (`sys.<component>.log_filter`) to change filters without restart.

### Metrics

- `metrics` crate with the OTel exporter.
- Metric naming: `iot_<component>_<unit>_<suffix>` (`iot_bus_messages_published_total`, `iot_registry_device_count`).
- Histograms use OTel's exponential/native histogram encoding.

### Plugin observability

- Plugins have access to a **scoped tracer** via a WIT-defined host call. They cannot configure the exporter or change global state.
- Plugin spans are parented to the host call that invoked the plugin.

## Consequences

- Every service boots with `init_tracing()` as its first line. This is enforced by a lint (clippy restriction rule) on `main.rs` files.
- JSON logs are not human-friendly by default. Developers use `jq` or a pretty-printer (`just logs | iot-log-pretty`). A local-dev `console` output mode is available via feature flag but never in prod.
- Propagating `traceparent` across the bus is mandatory, not optional. A publisher that forgets breaks trace continuity; CI integration test exercises the path to catch regressions.
