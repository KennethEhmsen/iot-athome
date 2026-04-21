# IoT-AtHome

A protocol-agnostic, ML-enhanced, voice-enabled home automation platform with a hardened plugin runtime. Local-first. Security before anything.

> **Status: W1 foundation scaffolding.** No code runs yet. The repo holds the design, the architectural decisions, and the skeleton a developer needs to start contributing.

## What this is

- **Every home protocol** behind a plugin boundary — 433/868/915 MHz, Zigbee, Z-Wave, Matter/Thread, BLE, Wi-Fi (MQTT/mDNS/SSDP/CoAP), IR, KNX, Modbus — and the plugin model ensures new protocols are additions, not forks.
- **Capability-sandboxed plugins.** Signed, resource-limited, least-privilege. Nothing runs with ambient authority.
- **ML subsystem** for anomaly detection, disaggregation (NILM), occupancy, suggestions — predictions only, never actuation without a rule.
- **Local voice** — wake, STT, NLU, TTS on-device. Cloud is opt-in, pinned, revocable.
- **Command Central** — dedicated wall-panel UX with per-person ephemeral auth, not shared-login.
- **Edge ML reference plugins** for water-meter CV, 3-phase mains power, and heating flow/return COP.

Read the full design: **[docs/IoT-AtHome-Design.docx](docs/IoT-AtHome-Design.docx)**.

## Repo layout

```
iot-athome/
├── Cargo.toml              # Rust workspace
├── flake.nix               # Nix dev shell (everything pinned)
├── justfile                # Canonical task runner
├── rust-toolchain.toml     # Pinned Rust
├── .github/workflows/      # CI
├── crates/                 # Rust services + libraries
├── plugins/                # First-party signed plugins (WASM / OCI)
├── services/ml/            # Python + ONNX Runtime (gRPC)
├── panel/                  # PWA (React/TS/Vite) + kiosk shell
├── firmware/               # ESP32 (PlatformIO / ESP-IDF)
├── schemas/                # Protobuf (device/bus/registry) + plugin manifest JSON Schema
├── deploy/compose/         # Dev infra (NATS, Mosquitto, Envoy, Keycloak, OTel)
├── tools/                  # Dev tooling (cert mint, CI helpers)
└── docs/
    ├── IoT-AtHome-Design.docx     # Locked design
    ├── adr/                        # Architecture Decision Records
    └── runbooks/
```

## Zero to dev in 15 minutes

On Linux or macOS with Nix installed:

```bash
git clone <this-repo>
cd iot-athome

# 1) Open the pinned dev shell. First run downloads toolchains; later runs are instant.
nix develop

# 2) Mint the local dev CA + component certs (ADR-0006: these are DEV ONLY).
just certs

# 3) Boot the local infra (NATS, Mosquitto, Keycloak, Envoy, Tempo/Loki/Prom/Grafana).
just dev

# 4) Lint + build everything that exists so far.
just lint
just build

# 5) Tail logs / browse Grafana at http://localhost:3000 (admin/admin, anon view enabled).
just dev-logs
```

On Windows: use **WSL2** with Nix inside it. The Rust workspace compiles natively on Windows, but the `just dev` target assumes Linux because of systemd / cert-path conventions. Development of the panel alone works cross-platform via `pnpm -C panel`.

## Decisions, documented

Every load-bearing choice is an ADR in [docs/adr/](docs/adr/):

| # | Decision |
|---|----------|
| [0001](docs/adr/0001-record-architecture-decisions.md) | Record architecture decisions |
| [0002](docs/adr/0002-async-runtime-tokio.md) | Async runtime — Tokio |
| [0003](docs/adr/0003-plugin-abi-wasm-component-model.md) | Plugin ABI — WASM Component Model + WIT |
| [0004](docs/adr/0004-nats-subject-taxonomy.md) | NATS subject taxonomy |
| [0005](docs/adr/0005-canonical-device-schema-versioning.md) | Canonical device schema versioning |
| [0006](docs/adr/0006-signing-key-management.md) | Signing key management |
| [0007](docs/adr/0007-database-migrations-forward-only.md) | Database migrations — forward-only |
| [0008](docs/adr/0008-error-handling.md) | Error handling |
| [0009](docs/adr/0009-logging-and-tracing.md) | Logging and tracing |
| [0010](docs/adr/0010-config-format-and-layering.md) | Config format and layering |
| [0011](docs/adr/0011-dev-bus-auth.md) | Dev-mode bus auth — mTLS-only, single IOT account (supersedes part of ADR-0004 in dev) |

## Milestones

| ID | Name (duration) | Scope |
|----|-----------------|-------|
| **M1** | Skeleton (4 wks) | Bus + registry + gateway + auth + one real Zigbee adapter + web stub. **Current milestone.** |
| M2 | Plugin SDK (4 wks) | Wasmtime host + capability model + 3 adapters (Z-Wave, 433 SDR, Matter). |
| M3 | Automation + Obs (3 wks) | CEL rule engine, full OTel, audit log. |
| M3.5 | Command Central v1 (3 wks) | PWA + kiosk wrapper + core tiles + device-cert enrollment. |
| M4 | ML + edge-ML reference plugins (4 wks) | First ML models, water-meter CV, 3-phase power, heating flow/return. |
| M4b | COP derivation | Cross-plugin ML subscription (power × heating → COP). |
| M5 | Voice (4 wks) | Wake/STT/NLU/TTS closed-domain; NILM pipeline. |
| M6 | Hardening / cert prep (3 wks) | Pen test, ETSI 303 645 checklist, vuln disclosure program. |

## Contributing

Before you write code:

1. Read the design doc — it's the contract.
2. Read the ADRs — they pre-answer most "why" questions.
3. Changes that touch schemas, plugin ABI, signing, or auth **require a new ADR in the same PR**.

## License

Apache-2.0 OR MIT — dual-licensed.
