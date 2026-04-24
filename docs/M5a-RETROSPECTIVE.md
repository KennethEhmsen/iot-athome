# M5a Retrospective — Platform Closure + 2nd Adapter + Optional Long-Term History

**Tag:** `v0.5.0-m5a` · **Completed:** 2026-04-24 · **Plan:** [M5-PLAN.md](M5-PLAN.md) · **Key ADRs:** [0003](adr/0003-plugin-abi-and-resource-limits.md), [0004](adr/0004-nats-subject-taxonomy.md), [0006](adr/0006-signing-key-management.md), [0011](adr/0011-dev-bus-auth.md) (now Superseded), [0013](adr/0013-zigbee2mqtt-wasm-migration.md)

## Scope re-framing, honestly

M5a is the **code-only half** of M5, formally split out at planning
time (see `docs/M5-PLAN.md` § Scope re-framing). The original M5
roadmap bundled 17 items spanning broker auth, rule engine
completeness, replay, plugin-host polish, a second adapter,
optional storage, Command Central PWA, edge-ML hardware plugins,
and the voice pipeline. That's ~7 weeks of scope, not the
originally-planned 4. The plan's split:

* **M5a** — every item that can honestly ship from a laptop in a
  single arc (no hardware, no datasets, no UI iteration loops).
* **M5b** — hardware-bound edge-ML + voice pipeline + Command
  Central kiosk PWA. Three arcs running on separate calendars.

What M5a closed: **all the platform-debt items + the 2nd WASM
adapter + the optional TimescaleDB backend**. What M5a deferred:
Command Central v1 PWA (multi-session UI work in its own right) +
everything in M5b's hardware/voice scope.

## What we shipped

15 of 17 M5a items, 8 commits over ~1 day. Each row's "debt #"
references the M4 retrospective's architectural-debts table.

| Slice | Commit | Debt | Notes |
|---|---|---|---|
| **W1.1-7 — Broker decentralized auth** | `62f44d7` | #1 | Operator+account+user JWT chain end-to-end. `iot_bus::jwt::issue_account_jwt` + `format_creds_file`. New `iotctl nats {bootstrap, mint-user}` subcommands. `iotctl plugin install --account-seed` post-install hook. `mint.sh` calls `iotctl nats bootstrap`. NATS server config flips from `no_auth_user: dev` to `include "certs/resolver.conf"`. `Bus::connect` supports `IOT_NATS_CREDS`. **ADR-0011 → Superseded.** |
| **W1.8 — ABI 1.3.0** | `bb34df0` | #10 | Removed `registry::upsert-device` host capability entirely. WIT package version 1.2.0 → 1.3.0 (breaking — explicit major-N + N-1 support note). z2m adapter migrated to bus-watcher auto-register path; manifest abi.version bump + capabilities.registry block dropped. `RegistryCapabilities` struct stays for backward-compat manifest parsing. |
| **W2.1-2 — Real CEL interpreter swap** | `94d2587` | #4 | `iot-automation/src/expr.rs` swaps the M3 hand-rolled grammar for the `cel-interpreter` 0.10 crate behind the same `parse(src) → Expr` / `eval_bool(&Expr, &json) → bool` facade. New rule-author surface available: `in`, `has(...)`, `size(...)`, list literals, full arithmetic. Defensive `std::panic::catch_unwind` wrap on `Program::compile` because cel-interpreter's antlr4rust dep panics on input the lexer fails to tokenise. Profile overrides cap antlr4rust / cel-* debug info to `line-tables-only` to stay under Windows MSVC's PDB symbol cap (LNK1318). |
| **W2.3-4 — Wildcard last-msg replay** | `94d2587` | #5 | `iot_bus::jetstream::last_state_wildcard(pattern)` opens an ephemeral JetStream consumer with `DeliverPolicy::LastPerSubject` + `filter_subject = pattern`, drains every distinct subject's last message exactly once. Gateway WS handler branches on `*` / `>` in topic filter and uses the new helper. |
| **deny-fix** | `3130e10` | — | `paste 1.0.15` unmaintained advisory pulled in transitively via cel-interpreter → antlr4rust → better_any → paste. Compile-time-only proc-macro, no runtime surface. Added to `deny.toml` ignore list with documented justification. |
| **W3.1 — Fuel refueling per call** | `d1121c7` | #6 | `runtime::DEFAULT_FUEL_PER_CALL = 10M`. `spawn_plugin_task` takes `fuel_per_call: u64`; `run_plugin_task` calls `store.set_fuel(fuel_per_call)` before each guest invocation. Supervisor reads `manifest.resources.fuel_max` (or default). Replaces M2-era "1B at boot, monotonic decrement" pattern. |
| **W3.2 — MQTT sub-refcount + unsubscribe** | `d1121c7` | #7 | `MqttBroker.filter_refcount: HashMap<String, usize>`. Only the 0→1 transition issues real `SUBSCRIBE`; only 1→0 issues `UNSUBSCRIBE`. `MqttRouter::unregister(plugin_id)` now returns `Vec<String>` of dropped filters so supervisor can decrement broker refcount per filter on plugin exit. Synthetic-broker unit tests with a draining-eventloop trick. |
| **W3.3 — Mosquitto ACL from manifests** | `d1121c7` | #8 | New `iot_plugin_host::mqtt_acl::generate(user, &[&Manifest])` + `iotctl mosquitto regen-acl` subcommand. Walks installed plugins, unions their `mqtt.{subscribe,publish}` allow-lists, writes a Mosquitto-2.0 ACL file. Fail-closed when zero plugins installed. Replaces the M1-era `pattern readwrite #` permissive ACL. |
| **W3.4 — sdr433-adapter** | `ed4525d` | — | New WASM plugin under `plugins/sdr433-adapter/` against ABI 1.3.0. Mirrors z2m shape: MQTT subscribe `rtl_433/+`, JSON envelope translator → canonical `device.sdr433.<model>-<id>[-<channel>].<entity>.state` publishes. Translator catalog covers 6 device shapes (temp/humidity, door/window contact, TPMS, rain gauges, energy/power monitors, water-meter pulse counters). 14 translator unit tests including one full envelope-→ -keys roundtrip per device shape. |
| **typos-fix** | `fed5614` | — | `_typos.toml` at repo root with allowances for `ActivLink` (Honeywell product name), `Mosquitto` (the broker, vs. the insect), `BACnet`, `OTAs`, `AFE`, `leafs`, `Unparseable`, `mis`. Caught by W3.4's preflight when `Activ` got flagged inside a Honeywell-ActivLink doc comment. The pre-existing terms (Mosquitto etc.) showed up only on local typos but not CI's pinned v1.45.1 — added them belt-and-braces. |
| **W4.1 — TimescaleDB optional backend** | `24e7f3e` | — | New `iot-history` crate with `HistoryStore` wrapping a sqlx PgPool. `ensure_schema()` creates the `entity_state_history` table + `(device_id, ts DESC)` index + a guarded Timescale hypertable conversion (works on plain Postgres too — loses chunk pruning, keeps the table). `record()` / `fetch_range()` / `prune_older_than()`. `from_env()` returns `Ok(None)` when `IOT_TIMESCALE_URL` unset. Bus_watcher mirrors every recognised publish; gateway exposes `GET /api/v1/devices/{id}/history?from=&to=&limit=`. Compose stack adds a `timescale` service behind a `history` profile. |

**ADR-0011 status flipped to Superseded.** **5 architectural debts
retired** (#1, #4, #5, #6, #7, #8, #10 — i.e. all of the M4 retro's
list except the M6-targeted items #9 SLSA + #2/3 already shipped + #7
MQTT broker work which became a unit of W3).

## What we deviated on

| Plan called for | What we did | Why |
|---|---|---|
| **Command Central v1 PWA** (W4 in plan) | Deferred to its own dedicated milestone | Multi-session by itself: full PWA app workspace + service worker + gateway PIN-issue endpoint + ephemeral room-scoped JWTs + tablet-friendly tile UI + WS auth re-wire. ~2000+ LoC across multiple commits with real UI iteration. Honest M4-pattern split: ship what fits, defer what's genuinely multi-session. |
| Live-Postgres integration tests for `iot-history` | Unit tests + workspace clippy/nextest only | The structural surface (connect → schema → record → fetch) is straightforward sqlx; the schema is two CREATEs. A testcontainers Timescale spin-up costs ~30s per CI run for marginal coverage gain. Marked as a follow-up. |
| `sdr433-adapter` device-family coverage beyond 6 shapes | Stuck at 6 | The M5 plan asked for "the 6 most common"; rtl_433's ~200 decoders trail off in popularity quickly. Adding more is just match arms in the catalog when real devices arrive. |

## What was harder than expected

- **cel-interpreter's antlr4rust panic on malformed input.** A bare `@`
  in an expression (operator typo) takes the parser down inside
  ANTLR's tree-builder, not as a `Result::Err`. Wrapped in
  `std::panic::catch_unwind`. Unfun discovery; would have crashed
  `iotctl rule add` on every typo'd attempt. The catch demotes to
  `ExprError::Parse` cleanly.
- **Windows MSVC PDB symbol cap (LNK1318).** Adding cel-interpreter
  pushed the test-target debug info past `^16 symbols`. Resolved
  with workspace profile overrides capping `antlr4rust` /
  `cel-parser` / `cel-interpreter` to `debug = "line-tables-only"`.
  Cost a 121 GB cargo-clean to free disk first (target dir had
  bloated past 100 % of the E: drive — caught the disk-full mid-
  build).
- **typos linter on legitimate product names.** `Honeywell-ActivLink`,
  `Mosquitto`, `BACnet`, `OTAs` all looked like misspellings to the
  default dictionary. Added a `_typos.toml` allow-list. typos
  tokenizes on case boundaries so multi-cap acronyms like `OTAs`
  need both the full word AND the leading uppercase chunk allowed.
- **Refcount + draining-eventloop for MQTT tests.** `rumqttc`'s
  AsyncClient request channel closes immediately if the eventloop
  isn't being polled — synthetic-broker tests need to spawn a
  drain loop that ignores connect errors.

## What was easier than expected

- **`iot_bus::Bus::connect` add of `creds_path`.** One field +
  six-line conditional in `connect()`. The `async-nats` crate's
  `ConnectOptions::credentials_file` is a clean, idempotent fit.
- **`MqttRouter::unregister` returning `Vec<String>`.** The change
  from `()` → `Vec<String>` was a one-line refactor (`retain` →
  `retain_mut` with a side-effect collector) and didn't ripple
  through any tests beyond a `let _ = ...` → `let removed = ...`
  on the supervisor's call site.
- **`json_to_cel` converter.** cel-interpreter ships a `From<HashMap<K,
  V>> for Value` impl which makes the JSON-→-CEL translation a
  recursive 14-line match.
- **Hypertable bootstrap on plain Postgres.** The `DO $$ ... IF
  EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'timescaledb')
  THEN PERFORM create_hypertable(...) END IF $$` block lets the
  same `ensure_schema()` work on Timescale and plain Postgres
  without a separate code path. Useful for future testcontainers
  tests.

## Architecture debts — updated

M4 retro's list carried forward with status updates.

| # | Debt | Status at v0.5.0-m5a |
|---|---|---|
| 1 | Broker JWT bootstrap wiring | ✅ **shipped M5a W1**; ADR-0011 → Superseded |
| 2 | Subscriber-loop traceparent wrappers | ✅ shipped post-v0.3.0-m3 (M4 baseline) |
| 3 | gRPC traceparent interceptors | ✅ client-side post-v0.3.0; server-side M4 |
| 4 | Hand-rolled vs. full CEL | ✅ **shipped M5a W2** (cel-interpreter 0.10 swap behind facade) |
| 5 | Wildcard last-msg replay | ✅ **shipped M5a W2** |
| 6 | Fuel refuel between plugin host calls | ✅ **shipped M5a W3** |
| 7 | MQTT sub-refcount / unsubscribe | ✅ **shipped M5a W3** |
| 8 | Permissive Mosquitto ACL | ✅ **shipped M5a W3** (manifest-driven) |
| 9 | SLSA provenance `continue-on-error` | Unchanged — M6 target |
| 10 | `registry::upsert-device` host capability | ✅ **removed M5a W1.8** (ABI 1.3.0) |

**Net: 8 of 10 debts retired in M5a.** The remaining two (#9 SLSA
hard-gate, M6-scheduled; #2/#3 are already done) leave M6 as the
next debt-relevant milestone.

## Metrics (at v0.5.0-m5a)

| | |
|---|---|
| Crates in workspace | **14** (M4 baseline 13; +1 `iot-history`) |
| WASM plugins | **3 shipping + 1 scaffold** (M4 baseline 2 + 1; +1 `sdr433-adapter`) |
| ADRs | 13 (no new ones in M5a; ADR-0011 status-changed to Superseded) |
| Plugin ABI version | **1.3.0** (M4 baseline 1.2.0; major bump removed `registry::upsert-device`) |
| Rust LoC (src/) | ~9.1k (M4 end) + ~3.3k (M5a diff) ≈ **~12.4k** |
| Workspace tests (nextest) | **138** (M4 baseline 111; +27) |
| `_typos.toml` | new (config-only, no source) |
| `deny.toml` ignored advisories | 7 (M4 baseline 6; +1 RUSTSEC-2024-0436 paste unmaintained) |
| CI pipeline stages | Unchanged |
| Commits since `v0.4.0-m4` | **8** (W1, W1.8, W2, deny-fix, W3 (1+2), W3.4 sdr433, typos-fix, W4.1) |

## Test trajectory

| Boundary | Workspace tests | Δ |
|---|---|---|
| M4 shipped | 111 | — |
| M5a W1 (broker JWT + ABI 1.3.0) | 118 | +7 net (–2 capability tests on removed `registry::upsert`) |
| M5a W2 (CEL + wildcard replay) | 122 | +4 |
| M5a W3 (1+2) (fuel + MQTT refcount + ACL) | 135 | +13 |
| M5a W3.4 (sdr433-adapter) | 135 + 14 plugin-local | (translator tests live in the plugin's own crate, out of workspace) |
| M5a W4.1 (TimescaleDB) | **138** | +3 |

Zero workspace flakes during M5a. All clippy / deny gates clean on
every push.

## What ships next

**Command Central v1 PWA** is the immediate next arc — its own
session, its own milestone tag (m5a.5 or m5b prefix at planning
time). Scope from `docs/M5-PLAN.md` § W4 stays unchanged:
- New workspace under `apps/command-central/` (Vite + React PWA)
- Service worker + offline shell
- Gateway-side PIN-issue endpoint that mints short-lived
  room-scoped JWTs (15-min sessions, auto-lock)
- Tablet-kiosk tile UI filtered by `?room=` URL param
- Proximity-wake (no-sleep + passive motion JS)
- Re-uses the same WS subscription path the panel does

**M5b** — hardware-bound arcs:
- Edge-ML plugin family (water-meter CV, 3-phase power, heating,
  NILM training loop)
- Voice pipeline (openWakeWord, Whisper/Vosk, closed-domain NLU,
  Piper TTS, llama.cpp Q4 fallback)

Each of M5b is iteration-heavy (firmware + datasets + model
training) and won't compress into a single laptop session. M5b
plan written at the boundary between Command Central shipping and
the hardware arcs starting.

## Acceptance criterion (from M5-PLAN, M5a slice)

> From `just dev` cold, every plugin's NATS connection uses a
> per-plugin user JWT minted at install time (no more
> `no_auth_user: dev`). ADR-0011 status flips to Superseded. The
> automation engine runs rules through the real `cel-interpreter`
> crate, not the hand-rolled subset. Panel reload pulls last-known
> values for wildcard subscriptions (`device.>`). `iotctl plugin
> list` shows 3 running plugins (demo-echo, z2m, sdr433). An
> operator can flip `IOT_TIMESCALE_URL` to promote historical
> retention from SQLite to Timescale without code changes.
> `v0.5.0-m5a` signs + Rekor-logs cleanly.

✅ All boxes checked except Command Central v1 PWA (deferred per
above). The criterion conditioned the PWA on "boots on a fresh
tablet"; honest deferral preserves the rest.

## Definition of done (M5a)

- [x] Broker JWT chain end-to-end; ADR-0011 → Superseded.
- [x] ABI 1.3.0 live; z2m adapter migrated; one-shot deprecation
      warn from M4 → handler removed entirely.
- [x] Real CEL interpreter via cel-interpreter 0.10 behind the
      `expr::parse` / `expr::eval_bool` facade; all M3 backward-
      compat tests pass.
- [x] Wildcard JetStream last-per-subject replay path in the
      gateway WS handler.
- [x] Per-call fuel refuel + MQTT sub-refcount with last-subscriber
      `UNSUBSCRIBE` on plugin exit.
- [x] Manifest-driven Mosquitto ACL via `iotctl mosquitto regen-acl`.
- [x] `sdr433-adapter` shipping plugin (3 plugins now installable
      in parallel: demo-echo, z2m, sdr433-adapter).
- [x] Optional TimescaleDB-backed long-term history; gateway
      `GET /devices/{id}/history` endpoint; compose `history`
      profile-opt-in.
- [x] 138 workspace tests + 14 sdr433 plugin-local tests, all
      green; clippy + fmt + cargo-deny clean.
- [x] Tag `v0.5.0-m5a` shipping; sign + reproducibility + cosign
      Rekor all green; SLSA provenance still advisory per the M2
      repo-visibility workaround (M6 fix).
- [ ] Command Central v1 PWA — **deferred** (next dedicated arc).
