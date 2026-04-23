# M3 Plan — Automation Engine + Full Observability

**Starts:** post-`v0.2.0-m2` · **Target duration:** 3 weeks · **Anchor ADRs:** [0004](adr/0004-nats-subject-taxonomy.md), [0006](adr/0006-signing-key-management.md), [0008](adr/0008-error-handling.md), [0009](adr/0009-logging-and-tracing.md), [0011](adr/0011-dev-bus-auth.md), [0013](adr/0013-zigbee2mqtt-wasm-migration.md) · **Carry-overs from M2 retro:** registry host capability retirement, iot-proto split, broker-JWT bootstrap, second adapter

## Goal

The platform crosses the line from "load plugins + move bytes" to
"actually automates the home + everything is debuggable end-to-end".

Three things that make M3 tangibly better than M2 for an operator:
1. They can **write a rule** ("when temp > 25 °C in the kitchen for
   5 min, turn on the kitchen fan") and see it fire.
2. They can **see the trace** of a single zigbee button press all the
   way from sensor → adapter → bus → rule engine → action, with one
   traceparent linking every span.
3. The **panel survives reload** — state isn't ephemeral in NATS
   core; last-known values come from JetStream on first connect.

## Acceptance criterion

> From `just dev` cold, the operator writes a YAML rule via
> `iotctl rule add`, publishes a synthetic sensor value via
> `mosquitto_pub`, sees the rule fire within 200 ms with the action
> result in the audit log, and pulls up the full trace tree in Tempo
> by clicking a span-id link in the panel. The tag `v0.3.0-m3` signs +
> Rekor-logs cleanly.

## Week-by-week

### W1 — M2 rollovers + audit canonicalisation

Rollovers first so the automation work in W2 is building on a clean
base, not on scaffolding we know retires.

- [ ] **`iot-proto` split** into `iot-proto-core` (prost messages +
      subject helpers, targets wasm32-wasip2) + `iot-proto` (the core
      crate + tonic clients/services, targets host only).
  - `crates/iot-proto-core/` created. `build.rs` uses `protox` +
    `prost-build` only — no tonic.
  - `crates/iot-proto/` keeps the tonic dep, re-exports everything
    `iot-proto-core` exposes.
  - `plugins/zigbee2mqtt-adapter/src/pb.rs` deleted — the plugin
    depends on `iot-proto-core` instead. Retires the duplicated
    `EntityState`/`Ulid` redefinition M2 W4 shipped.
  - One commit, covered by the workspace's existing nextest suite.

- [ ] **Registry auto-register on bus** — retires
      `registry::upsert-device` host capability.
  - `crates/iot-registry/src/bus_watcher.rs` (new) — subscribes to
    `device.*.>` via `iot_bus::Bus::subscribe`, parses the
    `(integration, external_id)` tuple out of the subject
    (`device.<integration>.<external_id>.*.state`), upserts if
    unknown.
  - Per ADR-0013 §Consequences: once this is shipping, `registry::Host`
    impl in iot-plugin-host returns a `registry.deprecated` log line
    but still works (transitional).
  - Z2M plugin's `registry::upsert_device` call stays for M3 as a
    belt-and-braces measure; removed in M4 when the deprecation log
    has been in-place for one full milestone.

- [ ] **Broker JWT bootstrap** — uses the per-plugin nkeys M2 W3
      wrote to `<plugin_dir>/<id>/nats.nkey`. Retires ADR-0011's "one
      shared dev account".
  - `crates/iot-bus/src/account.rs` (new) — generates the broker
    operator keypair + account JWT at `just dev` time (mTLS-protected
    `/var/lib/iotathome/nats/operator.creds`).
  - `iotctl plugin install` gains a post-install step that uses the
    operator seed to mint a user JWT from the plugin's existing
    `acl.json` snapshot, writes it to `nats.creds` alongside the
    `nats.nkey`.
  - NATS server config in `deploy/compose/nats/` switched from
    `no_auth_user` to operator-mode JWT auth.
  - Existing tests keep passing — the operator seed lives in the dev
    cert mint script alongside the mTLS CA.

- [ ] **JCS (RFC 8785) canonical JSON for audit** — replaces the
      ad-hoc serde_json hash form from M1.
  - `crates/iot-audit/src/canonical.rs` replaced. Use `serde_jcs`
    crate (maintained, small). Every `append()` call canonicalises
    before hashing.
  - Migration note: existing audit logs from M1/M2 won't re-verify
    under JCS. Shipped as a version-2 log with a chain break at the
    M3 upgrade point; `AuditLog::verify()` tolerates one break if the
    file header records the bump.

### W2 — Automation engine + retention + traceparent

The heart of the milestone.

- [ ] **CEL-based rule engine** — declarative YAML rules compiled to
      a DAG, triggers → conditions → actions, idempotency keys,
      dead-letter subjects.
  - `crates/iot-automation/src/` (stubbed in M1) gets fleshed out:
    - `rule.rs` — YAML parser → intermediate `Rule { triggers,
      conditions, actions, idempotency }`. Triggers are NATS subject
      patterns (`device.zigbee2mqtt.+.temperature.state`). Conditions
      are CEL expressions over the latest `EntityState` payload. Actions
      are bus publishes (`cmd.*`), script calls, or log entries.
    - `engine.rs` — subscribes to every trigger subject, compiles
      conditions via `cel-interpreter` crate, executes actions,
      writes an audit entry per firing. Idempotency keyed on
      `(rule_id, trigger_subject, payload_hash)` against a
      short-lived cache.
    - `dlq.rs` — rules whose actions fail repeatedly land on
      `sys.automation.dlq` with full context.
  - `iotctl rule {add, list, delete, test}` subcommands — `test`
    dry-runs a rule against a synthetic payload.
  - Integration test: write a rule, publish a synthetic trigger,
    assert the action bus message lands.

- [ ] **NATS JetStream last-msg-per-subject retention** — the panel
      survives reload because state stays on the stream.
  - `deploy/compose/nats/` config adds a `DEVICE_STATE` stream with
    `retention: last-msg-per-subject` on `device.>.state`.
  - `iot-gateway` WebSocket handler fetches the last message on
    every subject a client subscribes to, emits immediately on
    connect.
  - Audit: streams only cover state, not commands (which must not
    replay).

- [ ] **OTel traceparent propagation** — every automation firing is
      a span tree.
  - `iot-observability` crate gains `inject_traceparent(headers)` +
    `extract_traceparent(headers)` helpers matching W3C traceparent.
  - `iot-bus` wires `inject_*` into every publish, `extract_*` into
    every subscribe callback.
  - `iot-gateway` + `iot-registry` pick up traceparent from inbound
    HTTP/gRPC, inject on outbound bus publishes.
  - Plugin host: the per-plugin runtime task extracts traceparent
    from `PluginCommand`, sets it as the current span's parent before
    invoking the plugin export.
  - Demo: one zigbee button press → 5-span tree in Tempo:
    `mqtt.publish → plugin.on_mqtt_message → bus.publish →
    automation.rule_fire → bus.publish(cmd)`.

### W3 — Infra + Command Central + 2nd adapter

- [ ] **Envoy + mTLS frontend** — gateway moves behind Envoy;
      registry ↔ gateway switches to mTLS via Envoy's upstream TLS.
  - `deploy/compose/envoy/envoy.yaml` — inbound :8443 → gateway
    :8080 (plaintext internal); upstream :50051 → registry gRPC
    mTLS.
  - ADR-0006's dev-cert mint script gets a new component cert for
    Envoy itself.
  - Gateway binds to `127.0.0.1` only (Envoy is the only thing
    reaching it externally).

- [ ] **TimescaleDB optional backend** — long-term time-series
      retention.
  - `crates/iot-registry/src/storage/timescale.rs` — optional
    `sqlx` compile feature; SQLite stays default.
  - `deploy/compose/timescale/` service + init SQL.
  - `iotctl storage migrate` command that can one-way-forward
    replay SQLite → Timescale.
  - Not wired into the panel retention path for M3; that's a panel
    perf optimisation for M5.

- [ ] **Command Central v1** — PWA + kiosk shell, per-person
      ephemeral auth, device cert identity, proximity wake.
  - New crate `crates/iot-command-central/` (backend: subscribes
    device state + room occupancy, serves PWA).
  - New panel route `/panel/command-central/` — full-screen kiosk
    mode when run from a pinned PWA icon.
  - Ephemeral auth: device cert minted at first launch, signed by
    the hub's CA; per-person by reading a pinned-NFC-tag / QR on
    wake.
  - Proximity wake via zigbee PIR subscription; the screen comes on
    when someone walks into the kitchen.
  - Scope is deliberately minimal for M3: state display + scene
    buttons + room-context header. Voice integration stays M5.

- [ ] **Second adapter** — either Z-Wave (via zwave-js-server
      sidecar) or 433-SDR (via rtl_433). Pick during the W1 spike
      when the mqtt + registry capability cost is concrete.
  - Same WASM plugin shape as the M2 z2m port.
  - Adapter decides what goes on `device.zwave.>` / `device.sdr433.>`.
  - Installs via `iotctl plugin install` identical to z2m.

- [ ] **`v0.3.0-m3` tag + retro.** Documents the rollover closures
      (ADR-0011 fully retired, `registry::upsert-device` capability
      deprecated, `iot-proto` split complete).

## Risks

| Risk | Spike | Resolution |
|---|---|---|
| `cel-interpreter` crate maturity / WASM compatibility | 1 day W2 | Fallback: hand-rolled subset (comparison + boolean + time window) is enough for M3 rule cases; full CEL spec catches up in M4 |
| Envoy + mTLS cert rotation | 0.5 day W3 | Reuse M1 dev-cert mint infrastructure; no runtime rotation yet (lifecycle is a user story, not a demo requirement) |
| Timescale + sqlx migration surface | 1 day W3 | SQLite stays the default; Timescale only lights up if the operator opts in |
| Command Central auth lifecycle | 2 days W3 | MVP scope: ephemeral certs, no refresh flow — re-enroll on each wake for M3, session refresh in M5 |
| Bus-driven registry auto-register race (two z2m events for the same new device) | 2h W1 | Postgres-style `INSERT ... ON CONFLICT DO NOTHING` semantics; idempotent by `(integration, external_id)` UNIQUE constraint already in place |
| JetStream storage sizing for `DEVICE_STATE` | 1h W2 | `retention: last-msg-per-subject` caps at one message per subject; for 500 devices × 10 entities that's 5000 messages — tiny |
| Second adapter slipped from M2 into M3 | — | Already absorbed in W3; if Z-Wave spec confusion happens, fall back to 433-SDR (simpler data model) |
| SLSA provenance private-repo block | resolved in M2 | `continue-on-error: true` in CI already; M6 revisits once repo goes public |

## Out of scope for M3

- Voice pipeline — M5
- NILM training loop — M5
- Firmware-inclusive plugins — M4
- Plugin marketplace UX — M6
- Full refresh-token / long-lived session auth for Command Central — M5
- Multi-hub federation — Post-M6 backlog
- Postgres migration for the registry (TimescaleDB is the interesting upgrade path; plain-Postgres is a size-of-team decision, deferred to M5)

## Definition of done

- `iotctl rule add` happy path → rule fires within 200 ms from a
  synthetic publish → action visible in audit.
- Tempo shows the 5-span tree for a zigbee button press end-to-end.
- Panel reload after 30 s of no bus activity: last-known temperature
  shows within 1 s of connect (JetStream replay).
- `iotctl plugin install` mints a real user JWT from the plugin's
  `acl.json`; plugins authenticate to NATS with their own identity
  (no more `no_auth_user`).
- ADR-0011 retires (`docs/adr/0011-dev-bus-auth.md` gets a
  status-change commit).
- Registry auto-registers a brand-new `zigbee2mqtt/` device without
  the plugin calling `registry::upsert_device`.
- Envoy fronts the gateway; panel reaches `:8443` TLS, gateway listens
  on `127.0.0.1:8080` only.
- Command Central PWA displays one room's entities + two scene buttons.
- Second adapter (Z-Wave or 433-SDR) installs + publishes `device.*`
  messages indistinguishable from z2m.
- `v0.3.0-m3` tag: sign + reproducibility green; SLSA provenance
  still advisory (tracked for M6 repo-visibility flip).
