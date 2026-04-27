# ADR-0015: Voice-Pipeline Architecture

- **Status:** Accepted
- **Date:** 2026-04-27
- **Anchors:** [ADR-0002](0002-async-runtime-tokio.md), [ADR-0004](0004-nats-subject-taxonomy.md), [docs/M5-PLAN.md](../M5-PLAN.md)

## Context

M5b carries the local voice-control story for IoT-AtHome. The design-doc target is:

> Wake-word + STT + closed-domain NLU + TTS, all running locally; `<800 ms` from wake-to-action; LLM fallback for free-form requests; per-household fine-tunable.

The pipeline is conceptually fixed (audio in → wake → STT → intent → action; intent acknowledgement → TTS → audio out), but **where it runs** has four real options:

| Option | Topology | Pros | Cons |
|---|---|---|---|
| **A. Host-side daemon** | Sibling binary alongside `iot-registry` / `iot-gateway` on the hub | Lowest latency; single process to deploy; reuses hub's NATS + audit infra | Tied to one physical mic location |
| **B. Panel-browser** | `MediaDevices.getUserMedia()` in the panel SPA, audio streamed to gateway over WS | Any device with the panel becomes a voice point; no extra hardware | HTTPS-only; per-device mic perm UX; +50–200 ms WS latency hits the 800 ms budget |
| **C. Command Central kiosk** | Voice runs inside the PWA kiosk shell (Pi + display + mic) | Matches the "kiosk shell" design language | Same one-location problem as A; PWA mic access is awkward; tied to display refresh cycle |
| **D. ESP32 satellites** | I²S mic on ESP32 streams PCM to host over Wi-Fi | Best acoustics; multi-room; low-cost satellites | Firmware work; bandwidth on Wi-Fi; cold-start lag |

The latency budget (`<800 ms`) is unforgiving on **B** — wake-detection alone needs <200 ms or it feels broken, and a WebSocket round-trip on a household Wi-Fi adds 30–100 ms before any inference runs. **C**'s PWA + mic story is brittle: browser autoplay policies, mic-permission expiry, kiosk-mode foregrounding under display sleep — too many failure surfaces for a voice path that has to "just work" after a sudden power cycle.

That leaves **A** and **D**. **D** is acoustically superior (room-by-room mics; better noise rejection) but adds a firmware deliverable that isn't in M5b's scope. **A** is the smallest cohesive cut that proves the pipeline works and can run on the existing hub hardware.

## Decision

**Adopt Option A: a host-side daemon for v1, satellites (Option D) for v2.**

A new workspace crate `iot-voice` ships:

1. A daemon binary (`iot-voice`) that runs alongside `iot-registry` / `iot-gateway` on the hub. Reads from a local audio source (`cpal` for the v1 mic; PCM-over-NATS for v2 satellite audio).
2. Library crate exposing the pipeline shape as traits:
   * `AudioSource` — yields 16 kHz mono `f32` audio frames.
   * `WakeDetector` — given a frame stream, fires `Wake { confidence }` when a wake word is detected.
   * `SpeechRecognizer` — transcribes the post-wake utterance window into text.
   * `IntentParser` — maps text → `Intent { domain, verb, args, raw, confidence }` using a closed-domain phrase grammar; falls back to LLM for the free-form path (out of v1 scope).
   * `IntentSink` — dispatches intents (log, NATS-publish, stdout for tests).
   * `Synthesizer` — text → audio (response path; not wired into v1 detection loop).

Each trait has a `Stub*` impl in the same module so the pipeline composes cleanly under `cargo test` without any heavy native deps. Real impls (whisper-rs / piper / openWakeWord FFI) land in subsequent commits behind cargo features so cross-platform CI (Windows MSVC + Linux glibc + macOS) doesn't fall over on a CMake bootstrap.

### Subject taxonomy

Intents publish on:

```text
command.intent.<domain>.<verb>     # request
command.intent.<domain>.<verb>/ack # acknowledgement (optional, for round-trip)
```

Where `<domain>` ∈ `{lights, scenes, sensors, power, climate, media, system}` and `<verb>` is domain-specific (`on`, `off`, `set`, `activate`, `report`, `dim`, `status`).

The rule engine (M3) already triggers on wildcard NATS subjects via `triggers: [...]` in rule YAML, so this taxonomy reuses the existing dispatch path completely. A rule like:

```yaml
id: intent-kitchen-light-on
triggers: ["command.intent.lights.kitchen.on"]
when: "true"
actions:
  - publish:
      subject: device.zigbee2mqtt.kitchen-bulb-1.switch.cmd
      payload: { action: "on" }
```

…is the same shape as today's device-state rules. No new dispatch code, no new YAML schema.

### Library boundaries

The `iot-voice` crate stays pure-Rust at v1 scaffold time:

* No `iot-bus` dep yet — `IntentSink` is a trait, the NATS-publish impl wires through to `iot-bus` from a thin adapter at daemon-binary level. Keeps unit tests fast and the trait shape independent of the broker.
* No audio crate yet — `AudioSource` is a trait, `cpal` lands in commit 2 behind a `cpal` feature. Tests use `StubAudioSource` (PCM samples from a `Vec<f32>`).
* No model deps yet — `WakeDetector` / `SpeechRecognizer` are traits, real impls land in commits 3–4 with their own features (`whisper-rs`, `oww-rs` or FFI fallback).

This forward-staging keeps each commit independently verifiable and avoids the M2 "WIT-bindgen + wasmtime + nkeys + base64 all in one commit" landing-pad problem.

### Latency targets

The `<800 ms` budget breaks down as:

* Wake detection: 100–150 ms typical, budget 200 ms.
* Endpoint detection (silence after wake): 200–400 ms.
* STT inference: 100–200 ms on small Whisper, budget 300 ms.
* NLU + bus dispatch: <50 ms.
* Buffer = ~150 ms.

If any stage exceeds budget consistently, the daemon emits a `voice.latency.exceeded` audit-event (M3 W1.4 hash chain) so operators see the trend rather than chasing an intermittent feel-bug.

## Consequences

### Positive
* Reuses existing rule engine; no new dispatch path.
* Trait-shaped pipeline lets each model swap independently (Whisper → Vosk; openWakeWord → Porcupine).
* Pure-Rust scaffold compiles + tests on every CI matrix without CMake.
* Latency telemetry is a first-class audit-event, not a post-hoc metric.

### Negative
* Single mic location until satellites ship (M5c/M6).
* Real model impls bring CMake / FFI / native deps — CI matrix gets more complex once we wire them in.
* Closed-domain grammar will lag rule-author needs ("dim the kitchen lights to 30%" with quantifier extraction is non-trivial); LLM fallback comes online in M5b W3+.

### Revisit when
* M6: satellite firmware lands → AudioSource implementations expand.
* M5b W3: STT model size vs. Pi 5 thermal sustains real bench numbers.
* Voice-on-mobile becomes a user need → Option B re-enters consideration as a *secondary* path, not the primary.

## Implementation log

This ADR's decisions land incrementally. Each row points at the
commit that implemented the corresponding piece.

| Slice | What | Commit |
|-------|------|--------|
| W2 | Library scaffold (traits + Stub impls) | `5411d96` |
| W3 | Daemon binary + NATS sink | `e5e4396` |
| W5 | Rule engine consumes `command.intent.>` | `96cb28e` |
| W4a | Real audio capture via `cpal` (feature: `mic`) | `77491b1` |
| W4c | Real STT via `whisper-rs` (feature: `stt-whisper`) | `893e45a` |
| W4b | Energy-VAD wake detector (pure-Rust) | `2551597` |
| W4b.5 | Phrase-specific wake-word via rustpotter (feature: `wake-phrase`) | `ee7ad3f` |
| W4d | Real TTS via `piper` binary shell-out | `b8a960b` |

### Build-prereq ladder

Default builds stay pure-Rust (no native deps). Each real-impl
feature adds one prereq class:

| Feature | Crate | Build prereqs |
|---------|-------|---------------|
| (default) | — | rustup toolchain |
| `mic` (cpal) | iot-voice | system audio libs (alsa-dev on Linux; built-in on Win/macOS) |
| `wake-phrase` (rustpotter) | iot-voice | none (pure Rust); operator supplies an `.rpw` model file |
| `stt-piper` (TTS, always-on) | iot-voice | none at build time; runtime needs the `piper` binary on PATH + a voice `.onnx` model. Operator downloads pre-built piper from rhasspy/piper releases (no Rust build deps — sidestepped the CMake cost via shell-out). |
| `stt-whisper` | iot-voice | **CMake + Clang** on PATH (whisper.cpp build script) |

Operators on Windows install the toolchain once with
`choco install cmake llvm`; macOS via `brew install cmake`;
Debian / Ubuntu via `apt install cmake clang`. The first compile
of whisper.cpp takes ~3 min; subsequent incremental builds are
fast.
