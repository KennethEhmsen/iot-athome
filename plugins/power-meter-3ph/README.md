# Mains Power — 3-phase plugin

**Status:** M4 scaffold. `manifest.yaml` validates against
`schemas/plugin-manifest.schema.json` today. Firmware, host plugin, and UI
tiles land in M4 (see [M4-PLAN.md](../../docs/M4-PLAN.md) when it's written).

## What this plugin is

A **plugin package**, not a single binary. It bundles four artifacts:

```
power-meter-3ph/
├── manifest.yaml            # identity + capabilities + signing (this dir, today)
├── plugin.wasm              # hub-side component (M4)
├── firmware/                # ESP32-S3 firmware (M4)
│   ├── power-meter-fw.bin   # signed + Secure-Boot-v2 ready
│   └── firmware.sbom.json
├── models/                  # ML artifacts (M4)
│   ├── nilm.onnx            # disaggregation base model
│   └── nilm.meta.json
├── tiles/                   # Command Central UI (M4)
│   ├── power-live.wasm.js
│   ├── power-phases.wasm.js
│   ├── power-disaggregation.wasm.js
│   └── power-commissioning.wasm.js
└── sbom.cdx.json
```

The plugin installer (M2) unpacks the bundle, verifies the cosign signature
over every file, checks the SBOMs for CVEs, and registers the plugin's
capabilities with NATS/Mosquitto.

## Hardware + firmware boundary

The ESP32-S3 in the consumer unit is **not a peer** of the hub plugin — it's
the plugin's **downstream sensor node**. The ESP32:

1. Runs the signed firmware shipped in the plugin bundle.
2. Connects to the hub's Mosquitto over mTLS (separate `power-adapter` cert,
   minted by `mint.sh` on install).
3. Publishes `iot/power/<id>/phase/L1` (etc.) over MQTT with the fields
   the ATM90E32AS produces.

The hub plugin (`plugin.wasm`):

1. Subscribes to `iot/power/+/#` via the host's MQTT capability.
2. Translates each message to canonical entities (§3.1 of the design).
3. Upserts the Device with the registry.
4. Publishes per-entity `EntityState` events on the bus.
5. Runs the capacity-guardian algorithm when fed capacity + live total.
6. Forwards timeseries to the ML service for NILM training.

## Why this shape is the test of the architecture

A successful M4 ship proves the plugin model can carry a **full edge
product**: firmware, hub runtime, ML model, UI, commissioning wizard,
security posture, all in one signed package. If water-meter-cv and
power-meter-3ph both land cleanly, the design's "new home protocol =
one plugin away" promise is paid off.

## Safety notes (belong here, not buried later)

This plugin CANNOT be installed by an unelevated user. The manifest marks
`requires_installer_cert: true`; the commissioning wizard requires a
credentialed electrician to sign the wiring record before the first
`upsert_device` succeeds.

The capacity-guardian capability is `policy.capacity_guardian: true` —
**elevated**. It can trigger load-shed actions via the automation
engine. Refer to ADR-0003 §capabilities for the approval gate.

## What ships with a real M4 release

Tracking this as a checklist in `docs/M4-PLAN.md` (TBD). Seed items:

- [ ] Firmware: ESP-IDF project under `firmware/power-meter-fw/` with
      Secure Boot v2, flash encryption, per-device cert provisioning.
- [ ] Signed OTA with A/B partitions; cosign-verified at boot.
- [ ] Hub plugin: canonical entity catalog, capacity guardian,
      short-cycle/sag/swell/imbalance detectors.
- [ ] NILM training pipeline (Python, services/ml) using seq2point as
      the base model; per-household fine-tune pathway.
- [ ] UI tiles.
- [ ] Commissioning wizard with electrician sign-off flow.
- [ ] Documentation for installers (non-Claude-Code audience).
