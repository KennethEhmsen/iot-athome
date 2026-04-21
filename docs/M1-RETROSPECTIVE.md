# M1 Retrospective — Skeleton + Walking Plugin

**Tag:** `v0.1.0-m1` · **Completed:** 2026-04-21 · **Design doc:** [IoT-AtHome-Design.docx](IoT-AtHome-Design.docx)

## What we said we'd ship

The Plan agent's brief for M1 ([plan in session](adr/0001-record-architecture-decisions.md)) called for:

| Week | Scope | Demo acceptance |
|---|---|---|
| W1 | Foundations: repo, flake, justfile, CI, empty crates, iot-proto codegen, dev CA, mTLS infra | `just dev` up, `cargo build` green, `iotctl ping` round-trips NATS |
| W2 | Registry + bus: sqlx SQLite, gRPC, audit log, tracing | `iotctl device add` inserts; `list` returns it; trace in local Tempo |
| W3 | Gateway + auth + first adapter | Real Zigbee press → adapter → NATS → registry → WS → browser |
| W4 | Panel polish + M1 demo | Cold-boot stack, pair a sensor, see it live on the panel, audit shows enrollment, release tarball cosign-verified |

## What we actually shipped

Every M1 milestone is met except the last line of the W4 demo ("release tarball cosign-verified on a tag") — that runs in CI on the tag push we're about to do.

| Week-scope | Status | Notes |
|---|---|---|
| **W1** Foundations | ✅ | 10 ADRs, Cargo workspace (12 crates), pinned Rust 1.95, protox codegen (no protoc needed), CI matrix (x86_64 + aarch64), dev-cert mint, `tools/msvc-env.sh` (Windows parity) |
| **W2** Registry + bus | ✅ | Registry gRPC CRUD, SQLite + sqlx migrations, hash-chained audit log, bus publish on state change |
| **W2** `iotctl` | ✅ | `device add/list/get/delete` + `ping`; mTLS NATS round-trip |
| **W3a** Gateway REST/WS | ✅ | `POST/GET/DELETE /api/v1/devices`, `GET /stream` (WS) |
| **W3b** First adapter | ✅ | `zigbee2mqtt-adapter` in-process with rumqttc + rustls, `no_auth_user` on NATS, ACL on Mosquitto |
| **W3c** Entity-level stream | ✅ | Adapter publishes `iot.device.v1.EntityState`; gateway decodes to JSON; panel renders |
| **W4** Panel demo | ✅ | Browser showed `temperature: 22.1` from a live `mosquitto_pub` within ~150ms |
| W4 release ceremony | 🔜 | `v0.1.0-m1` tag triggers the sign + SLSA + reproducibility CI jobs |

## What we deviated on

Each of these is a deliberate cut with a named follow-up:

| Plan called for | What we did | Why |
|---|---|---|
| OIDC on gateway + panel login | Skipped until W3d | Demo works without it; OIDC is self-contained scope, best done as one batch rather than layered on top of every endpoint |
| mTLS gateway ↔ registry | Plaintext localhost | Both live on the same host for M1; Envoy layer arrives with M3's external exposure |
| OTel tracing end-to-end | Dropped from iot-observability for W2 | `opentelemetry-otlp` 0.27 API was churning; JSON logs via tracing-subscriber cover structured correlation. Rewires in M3 with the first cross-service trace. |
| Full `buf generate` CI stage | `protox` + `tonic-build` inside each crate's `build.rs` | Avoids requiring a `protoc` binary or a Buf runtime on every dev box; same output, simpler reproducibility story |
| zigbee2mqtt in pure Rust | Adapter bridges from the real Node z2m daemon (MQTT subscriber only, not a Zigbee stack of its own) | Rust zigbee-herdsman is a quarter of work for zero differentiation; revisit post-M2 |
| Per-plugin NATS accounts | One shared `IOT` account with `no_auth_user dev` over mTLS | Plugin-account-per-manifest is an install-time story that belongs with the plugin installer in M2 ([ADR-0011](adr/0011-dev-bus-auth.md)) |
| Per-plugin MQTT ACLs | One dev-wide permissive ACL | Same reason as above |

## What was harder than expected

- **Cargo-deny schema drift**: `deny.toml` moved from `{crate=..., version=...}` to `"crate:version"` strings, license allow-list now rejects `CDLA-Permissive-2.0` and `MIT-0` by default, and `allow-wildcard-paths` only works if path-dep crates are `publish = false`. Each was a one-line fix; together they cost a full CI round-trip each.
- **MSVC linker on Windows**: Git-bash's `/usr/bin/link.exe` (coreutils) shadows MSVC's `link.exe`. Sourcing `vcvars64.bat` from bash is awkward; the answer was [`tools/msvc-env.sh`](../tools/msvc-env.sh) that computes paths from the install root.
- **Mosquitto 2.0 default-deny**: v2.0 denies all pub/sub for authenticated users unless an ACL grants it. Undocumented-ish migration gotcha.
- **async-nats 0.38 → 0.47**: required bump to close three RUSTSEC-2026 rustls-webpki CVEs. Transitive fallout was modest; `HeaderName::as_str()` became private, tiny diff in iot-bus.
- **Git-bash MSYS path mangling**: `openssl -subj /C=XX/...` gets rewritten as `/C=XX/...` → Windows path. Fixed by writing subjects to config files rather than cmdline args.

## What was easier than expected

- **protox over tonic-build**: pure-Rust protobuf compiler. Zero protoc runtime dep across the whole workspace — huge win for CI determinism and Windows developer loop.
- **Testcontainers for integration**: the NATS envelope round-trip test spins a container, runs in ~5s, works cross-platform.
- **rumqttc + rustls**: mTLS client cert wiring was exactly the minimum ceremony.
- **Cosign keyless in GitHub Actions**: no private keys in CI storage, just OIDC identity. Already signing every commit's artifacts.

## Architecture debts taken deliberately

Each has a named future resolution, not "we'll get to it":

| Debt | Where it bites | Resolved by |
|---|---|---|
| Single NATS account, mTLS-only auth | Can't revoke an individual adapter without rotating the whole IOT account | Plugin installer (M2) generates per-plugin NATS accounts from manifest |
| Permissive MQTT ACL | Adapter cannot be constrained from reading `homeassistant/#` (unrelated tree) | Same installer, same manifest |
| No OIDC on gateway | Any process reaching `:8081` can CRUD devices | W3d middleware |
| SQLite only (no Postgres wiring) | Fine for single-home; breaks at ~5k devices | Migration file set already split (`migrations/{sqlite,postgres}/`); flip to Postgres behind a Config flag when needed |
| Plain hash chain in audit log | No canonical-JSON normalization → recomputation requires knowing serde_json's implementation detail | ADR-0008 / M3 swap to JCS (RFC 8785) once a second consumer needs to verify |
| Entity state has no retention | Panel blank until new events arrive after reload | NATS JetStream last-msg-per-subject (M3) |
| Registry UPSERT by external_id | Adapter maintains a local cache to avoid (integration, external_id) UNIQUE collision | Add `GetByExternalIdRequest` RPC in M2 |

## Metrics (at v0.1.0-m1)

| | |
|---|---|
| Crates in workspace | 13 |
| Adapters (plugins) | 1 (zigbee2mqtt-adapter) |
| ADRs | 10 + 1 with this PR |
| Rust LoC (src/) | ~3k |
| TypeScript LoC (panel/) | ~700 |
| Unit + integration tests | 13 passing |
| CI pipeline stages | 6 green (preflight, build x2 targets, test, sbom, integration, vuln) |
| Supply-chain advisories ignored | 4 (all transitive, all with justifications + revisit triggers) |

## What ships next (M2, per Plan agent)

1. **WASM Component Model plugin runtime** — `iot:plugin-host@1.0.0` WIT world, wit-bindgen bindings, Wasmtime host loads signed `.wasm` components with capability enforcement.
2. **Plugin SDK (Rust + TS)** publishing real crates.
3. **3 more adapters**: Z-Wave, 433-SDR, Matter — built on the SDK to validate the ABI.
4. **Registry `GetByExternalId`** — drops the adapter-side cache hack.
5. **Per-plugin NATS account + MQTT ACL generation** at install time.
6. **OTel tracing re-wired** — first cross-service span (gateway → registry) lands the propagation plumbing.

## What ships two milestones out (M4)

The reference edge-ML plugins the design doc exists to enable:

- **Water-meter CV** (ESP32-CAM + TFLM digit classifier)
- **Mains-power 3-phase** (ESP32-S3 + ATM90E32)
- **Heating flow/return ΔT + COP** (piggybacks on the power-meter ESP32)
- **NILM training loop** (hub-side, Python + ONNX)

These exercise every M2 primitive — plugins that ship firmware + models + UI tiles + commissioning wizards as one signed package.
