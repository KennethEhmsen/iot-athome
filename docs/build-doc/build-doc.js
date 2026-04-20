const fs = require("fs");
const {
  Document, Packer, Paragraph, TextRun, Table, TableRow, TableCell,
  Header, Footer, AlignmentType, PageOrientation, LevelFormat,
  TabStopType, TabStopPosition,
  HeadingLevel, BorderStyle, WidthType, ShadingType,
  PageNumber, PageBreak,
} = require("docx");

// ------------ helpers ------------
const FONT = "Arial";
const MONO = "Consolas";

const p = (text, opts = {}) =>
  new Paragraph({
    spacing: { after: 120, ...(opts.spacing || {}) },
    alignment: opts.alignment,
    children: [new TextRun({ text, bold: opts.bold, italics: opts.italics, size: opts.size, font: FONT })],
  });

const pMulti = (runs, opts = {}) =>
  new Paragraph({
    spacing: { after: 120, ...(opts.spacing || {}) },
    alignment: opts.alignment,
    children: runs.map(r =>
      typeof r === "string"
        ? new TextRun({ text: r, font: FONT })
        : new TextRun({ text: r.text, bold: r.bold, italics: r.italics, font: r.mono ? MONO : FONT })
    ),
  });

const h1 = (text) =>
  new Paragraph({
    heading: HeadingLevel.HEADING_1,
    pageBreakBefore: true,
    children: [new TextRun({ text, bold: true, font: FONT })],
  });

const h2 = (text) =>
  new Paragraph({
    heading: HeadingLevel.HEADING_2,
    children: [new TextRun({ text, bold: true, font: FONT })],
  });

const h3 = (text) =>
  new Paragraph({
    heading: HeadingLevel.HEADING_3,
    children: [new TextRun({ text, bold: true, font: FONT })],
  });

const bullet = (text, level = 0) =>
  new Paragraph({
    numbering: { reference: "bullets", level },
    spacing: { after: 80 },
    children: [new TextRun({ text, font: FONT })],
  });

const bulletRich = (runs, level = 0) =>
  new Paragraph({
    numbering: { reference: "bullets", level },
    spacing: { after: 80 },
    children: runs.map(r =>
      typeof r === "string"
        ? new TextRun({ text: r, font: FONT })
        : new TextRun({ text: r.text, bold: r.bold, italics: r.italics, font: r.mono ? MONO : FONT })
    ),
  });

const code = (text) => {
  const lines = text.split("\n");
  return lines.map(line => new Paragraph({
    spacing: { after: 0 },
    shading: { type: ShadingType.CLEAR, fill: "F2F2F2" },
    children: [new TextRun({ text: line || " ", font: MONO, size: 18 })],
  }));
};

// --- table helpers ---
const CELL_BORDER = { style: BorderStyle.SINGLE, size: 4, color: "BFBFBF" };
const cellBorders = { top: CELL_BORDER, bottom: CELL_BORDER, left: CELL_BORDER, right: CELL_BORDER };

const cell = (text, width, opts = {}) =>
  new TableCell({
    borders: cellBorders,
    width: { size: width, type: WidthType.DXA },
    shading: opts.header ? { type: ShadingType.CLEAR, fill: "D9E2F3" } : undefined,
    margins: { top: 80, bottom: 80, left: 120, right: 120 },
    children: (Array.isArray(text) ? text : [text]).map(t =>
      new Paragraph({
        spacing: { after: 40 },
        children: [new TextRun({ text: t, bold: !!opts.header, font: FONT, size: 20 })],
      })
    ),
  });

const buildTable = (widths, headerRow, dataRows) => {
  const total = widths.reduce((a, b) => a + b, 0);
  const rows = [];
  rows.push(new TableRow({
    tableHeader: true,
    children: headerRow.map((h, i) => cell(h, widths[i], { header: true })),
  }));
  for (const r of dataRows) {
    rows.push(new TableRow({
      children: r.map((c, i) => cell(c, widths[i])),
    }));
  }
  return new Table({
    width: { size: total, type: WidthType.DXA },
    columnWidths: widths,
    rows,
  });
};

// ------------ content ------------
const children = [];

// Title page
children.push(
  new Paragraph({
    spacing: { before: 2000, after: 200 },
    alignment: AlignmentType.CENTER,
    children: [new TextRun({ text: "IoT-AtHome", bold: true, size: 72, font: FONT })],
  }),
  new Paragraph({
    alignment: AlignmentType.CENTER,
    spacing: { after: 400 },
    children: [new TextRun({ text: "System Design & Requirements", size: 40, font: FONT })],
  }),
  new Paragraph({
    alignment: AlignmentType.CENTER,
    spacing: { after: 200 },
    children: [new TextRun({
      text: "A protocol-agnostic, ML-enhanced, voice-enabled home automation platform with a hardened plugin runtime.",
      italics: true, size: 24, font: FONT,
    })],
  }),
  new Paragraph({
    alignment: AlignmentType.CENTER,
    spacing: { before: 800 },
    children: [new TextRun({ text: "Draft  |  2026-04-20", size: 22, font: FONT })],
  }),
);

// ====================================================================
// 1. Overview
// ====================================================================
children.push(h1("1. Overview"));
children.push(p("IoT-AtHome is a home automation platform designed from a security-first, local-first posture. The trust model drives the architecture, not the other way around. The platform unifies all common home-automation protocols from sub-GHz radio up through Wi-Fi, Matter, and future standards; provides an ML subsystem for anomaly detection, disaggregation, and suggestions; ships a local voice assistant; and supports third-party extension via a signed, capability-scoped plugin runtime."));
children.push(p("The design treats every non-core component as untrusted by default. Plugins, devices, and even human sessions operate under capability-scoped permissions. The platform is fully operational without internet access; cloud services are strictly optional enhancements."));

children.push(h2("Design principles"));
children.push(bullet("Security before anything. No default passwords, mTLS internal, signed artifacts, capability sandbox."));
children.push(bullet("Local-first. Full operation offline. Cloud is opt-in, pinned, and revocable."));
children.push(bullet("Protocol-agnostic core. The core never speaks a protocol directly; protocols live behind plugin boundaries."));
children.push(bullet("Dumb edges, smart hub. Edge devices measure and act; analytics, ML, and decisions live on the hub."));
children.push(bullet("Models predict, automations decide. ML outputs are signals, not actuations; humans or explicit rules authorize control."));
children.push(bullet("Plugin composition over core features. New features land as signed plugins, not as core forks."));

// ====================================================================
// 2. Requirements
// ====================================================================
children.push(h1("2. Requirements"));

children.push(h2("2.1 Functional"));
children.push(bullet("Ingest and control across sub-GHz RF (433/868/915 MHz), Zigbee 3.0, Z-Wave, Thread/Matter, BLE and BT mesh, Wi-Fi (mDNS, SSDP, MQTT, HTTP/CoAP), IR, KNX, Modbus, with explicit room for new radios."));
children.push(bullet("Automation engine combining declarative rules with ML-driven suggestions and anomaly detection."));
children.push(bullet("Voice assistant pipeline: wake word, STT, intent routing, action dispatch, TTS, local by default."));
children.push(bullet("Plugin SDK allowing third parties to add devices, protocols, ML models, and UI tiles without forking core."));
children.push(bullet("UX surfaces: web dashboard, mobile app, local API, voice, and a dedicated Command Central wall-panel."));

children.push(h2("2.2 Non-functional"));
children.push(buildTable(
  [2200, 7160],
  ["Attribute", "Target"],
  [
    ["Security", "Zero-trust internal bus, signed plugins, mTLS everywhere, per-plugin capabilities, SLSA-attested builds."],
    ["Availability", "99.9% of scheduled automations fire; hub survives plugin crashes; graceful offline behavior."],
    ["Latency", "<150 ms local automation trigger to actuation; <800 ms voice intent to action."],
    ["Scale", "500 devices, 50 plugins, 100 concurrent sessions on a mid-tier SBC (Pi 5 / N100 class)."],
    ["Compliance", "ETSI EN 303 645, ISO 27001 / SOC 2 aligned controls, GDPR, OWASP ASVS L2, Matter certification."],
    ["Privacy", "Telemetry off by default; granular opt-in; one-click user-data erase including backups and derived features."],
  ]
));

children.push(h2("2.3 Constraints"));
children.push(bullet("Runs on Linux (primary), with optional containerized K3s deployment."));
children.push(bullet("Target build team is small; bias toward boring, well-supported technology."));
children.push(bullet("Must support 24/7 unattended operation."));

// ====================================================================
// 3. High-level architecture
// ====================================================================
children.push(h1("3. High-Level Architecture"));

children.push(p("The platform is a tiered architecture: client surfaces on top, an API gateway enforcing authN/Z and TLS, a set of core services communicating over a mutually-authenticated event bus (NATS JetStream), and a plugin host that runs protocol adapters, integrations, and third-party code in sandboxed runtimes."));

children.push(h2("Component diagram"));
children.push(...code(`
+-------------------------------------------------------------+
|  CLIENTS                                                    |
|  Web UI | Mobile | Voice | 3rd-party | CLI | Command Central|
+----+-----------------+-------------------------+------------+
     | HTTPS/WSS        | gRPC/TLS                | WSS + mTLS
     v                  v                          v
+-------------------------------------------------------------+
|  API GATEWAY   (OAuth2/OIDC, mTLS, rate limit, audit, WAF)  |
+----+--------------------------------------------------------+
     |
     v
+-------------------------------------------------------------+
|  CORE SERVICES                                              |
|  Device Registry | Automation Engine | Voice Orchestrator   |
|         \\              |                    /                |
|          v             v                   v                 |
|   +-------------------------------------------+              |
|   |  EVENT BUS (NATS JetStream, mTLS)         |              |
|   |  device.*  automation.*  ml.*  sys.*      |              |
|   +-------------------------------------------+              |
|          ^             ^                   ^                 |
|          |             |                   |                 |
|  ML Service        Plugin Host        Audit / OTel / SIEM   |
+----------------------+--------------------------------------+
                       |
   +------------------+-+------------------+---------------+
   v                  v                   v               v
Zigbee adapter  Z-Wave adapter  433 SDR adapter  Cloud bridges
(plugin)        (plugin)        (plugin)         (Matter, APIs)
`));

children.push(h2("Component responsibilities"));
children.push(buildTable(
  [2200, 3400, 3760],
  ["Component", "Role", "Tech (proposed)"],
  [
    ["API Gateway", "AuthN/Z, TLS termination, rate limit, audit", "Envoy + OIDC (Keycloak/Authelia)"],
    ["Device Registry", "Canonical device model, state, capabilities", "Rust/Go service + SQLite/Postgres"],
    ["Automation Engine", "Rule DAG, scheduler, idempotent executor", "Rust/Go; CEL for expressions"],
    ["Event Bus", "Pub/sub, durable streams, replay", "NATS JetStream (mTLS, per-plugin accounts)"],
    ["Plugin Host", "Sandboxed runtime, capability enforcement", "WASM (Wasmtime) primary; OCI-container fallback"],
    ["Voice Orchestrator", "Wake -> STT -> NLU -> action -> TTS", "openWakeWord, Whisper/Vosk, local LLM, Piper"],
    ["ML Service", "Feature store, inference, anomaly detection", "Python service, ONNX Runtime"],
    ["Observability", "Logs, metrics, traces, audit", "OpenTelemetry -> Loki/Prometheus/Tempo"],
  ]
));
children.push(p("Language split: core in Rust for memory safety, performance, and WASM host maturity. Plugins polyglot via the WASM Component Model. ML in Python, isolated behind a gRPC boundary."));

// ====================================================================
// 4. Deep Dive
// ====================================================================
children.push(h1("4. Deep Dive"));

children.push(h2("4.1 Device model (canonical)"));
children.push(p("One registry, many adapters. Adapters translate protocol frames into canonical entity events on the bus. Matter is treated as the schema baseline."));
children.push(...code(`{
  "id": "dev_01HXX...",              // ULID, stable
  "integration": "zigbee",           // plugin id
  "external_id": "0x00158d0001abcdef",
  "manufacturer": "Aqara",
  "model": "WSDCGQ11LM",
  "capabilities": ["temperature", "humidity", "battery"],
  "entities": [
    { "id": "...", "type": "sensor.temperature", "unit": "C", "rw": "r" }
  ],
  "rooms": ["living_room"],
  "trust_level": "verified | user_added | discovered",
  "last_seen": "2026-04-20T..."
}`));

children.push(h2("4.2 Automation engine"));
children.push(bullet("Rule format: declarative YAML/JSON compiled to a DAG. Triggers -> conditions -> actions."));
children.push(bullet("Expression language: CEL (sandboxed, typed, non-Turing-complete). Embedded Lua/Python is out of scope to limit attack surface."));
children.push(bullet("Executor: idempotency keys per trigger firing; retriable actions with exponential backoff; dead-letter on permanent failure."));
children.push(bullet("ML hook: rules may consume ml.<model>.prediction events (e.g. occupancy likely) as triggers or conditions."));

children.push(h2("4.3 Plugin system"));
children.push(p("This is the most consequential subsystem. Every extension - protocols, integrations, ML models, UI tiles, firmware - lands as a signed plugin with explicit, user-approved capabilities."));
children.push(h3("Runtime choice"));
children.push(p("WASM (Wasmtime) is the default runtime. Faster cold start (<10 ms), deterministic capability sandbox, no kernel syscall surface, smaller footprint than containers. OCI containers remain available for plugins that require Linux drivers (SDR, serial, USB)."));
children.push(h3("Manifest (signed)"));
children.push(...code(`id: zigbee2mqtt-adapter
version: 1.4.2
entrypoint: plugin.wasm
signatures:
  - keyid: ...
    sig: ...                        # Sigstore / cosign
capabilities:                       # least-privilege, user-approved
  bus.publish: ["device.zigbee.*"]
  bus.subscribe: ["cmd.zigbee.*"]
  net.outbound: ["mqtt://localhost:1883"]
  fs.read: ["/etc/iotathome/zigbee/"]
  serial: ["/dev/ttyUSB0"]          # elevated: requires user approval
resources:
  memory_mb_max: 128
  cpu_pct_max: 25`));
children.push(h3("Isolation guarantees"));
children.push(bullet("Capability-based: no ambient authority. A plugin sees only the bus subjects, files, and hardware it declared."));
children.push(bullet("Signed and pinned: cosign verification at install; rejects on signature mismatch."));
children.push(bullet("SBOM required: CycloneDX SBOM bundled; CVE scan at install and on schedule."));
children.push(bullet("Resource limits: cgroups v2 for container plugins; Wasmtime fuel/memory limits for WASM."));
children.push(bullet("Bus ACLs: NATS accounts per plugin; core mediates cross-plugin calls."));
children.push(h3("SDK languages"));
children.push(p("Rust, Go, TypeScript, Python (compiled to WASM). One schema (Protobuf) defines the host ABI."));

children.push(h2("4.4 Protocols & adapters"));
children.push(buildTable(
  [2800, 3600, 2960],
  ["Band / Standard", "Stack", "Hardware"],
  [
    ["433/868/915 MHz", "rtl_433 + SDR adapter, or CC1101 module", "RTL-SDR v4, CC1101"],
    ["Zigbee 3.0", "zigbee-herdsman (or Rust equivalent)", "Sonoff Dongle-E, ConBee III"],
    ["Z-Wave", "Z-Wave JS (containerized plugin)", "Aeotec Z-Stick 7"],
    ["Thread / Matter", "OpenThread Border Router + chip-tool", "nRF52840 dongle"],
    ["BLE / Bluetooth Mesh", "BlueZ via D-Bus", "Built-in or USB BT 5.x"],
    ["Wi-Fi LAN", "mDNS, SSDP, MQTT, HTTP/CoAP discovery", "n/a"],
    ["MQTT 3.1.1 / 5.0", "Embedded Mosquitto/EMQX + client plugin", "n/a (broker is local)"],
    ["IR", "LIRC / Broadlink", "USB-UIRT, Broadlink Pro"],
    ["Industrial", "KNX/IP, Modbus TCP/RTU, BACnet", "Gateways"],
    ["Future", "Adapter template; new bands = new plugin", "e.g. LoRa/LoRaWAN, UWB"],
  ]
));
children.push(p("Design rule: the core never speaks a protocol directly. Protocols live behind the plugin boundary - that is what makes 'all protocols, including future ones' actually maintainable."));

children.push(h2("4.5 Voice assistant"));
children.push(p("Pipeline: openWakeWord (local) -> Whisper-small or Vosk (local STT) -> intent router (local NLU for closed-domain: lights, scenes, sensors) -> fallback to local LLM (Llama-3.1-8B-Instruct Q4) for open-domain -> action dispatched as a normal automation trigger -> Piper TTS."));
children.push(bullet("Wake word and STT run locally. Cloud STT is opt-in, per-user, with explicit consent and audit trail."));
children.push(bullet("Voice data is ephemeral (RAM only) unless user opts into training corpus. If enabled: encrypted at rest, user-owned key."));
children.push(bullet("Barge-in and continuous-listen are off by default."));

children.push(h2("4.6 ML service"));
children.push(p("Shipped use cases:"));
children.push(bullet("Anomaly detection - per-device time-series (Isolation Forest or autoencoder)."));
children.push(bullet("Occupancy prediction - room-level, multi-sensor fusion."));
children.push(bullet("Energy forecasting - next 24 hours per circuit."));
children.push(bullet("Automation suggestions - frequent-pattern mining on event logs; suggested rules require human approval before activation."));
children.push(p("Serving: ONNX Runtime. Models come from a signed model registry (same cosign pipeline as plugins). Models never auto-actuate - they publish predictions; the automation engine decides."));
children.push(p("Training: local-first with a federated option. No raw telemetry leaves the house unless the user opts in and the destination is cryptographically pinned."));

children.push(h2("4.7 Data model & storage"));
children.push(buildTable(
  [2400, 3800, 3160],
  ["Data", "Store", "Retention"],
  [
    ["Device registry, config", "SQLite (small) / Postgres (large)", "forever"],
    ["Event stream", "NATS JetStream", "30d hot"],
    ["Time-series", "TimescaleDB or VictoriaMetrics", "1y default, configurable"],
    ["ML features", "Parquet on local disk", "per policy"],
    ["Audit log", "Append-only, hash-chained (S3/Glacier optional)", "1y min, immutable"],
    ["Secrets", "age-encrypted file or Vault", "n/a"],
  ]
));

// ====================================================================
// 5. MQTT addendum
// ====================================================================
children.push(h1("5. MQTT"));
children.push(p("MQTT is first-class, with a clear separation between its two roles: south-facing adapter (devices talk to us) and north-facing integration point (external systems talk to us). Both use the same protocol, same broker, different ACL scopes."));

children.push(h2("5.1 Embedded broker"));
children.push(bullet("Ships with the hub. Mosquitto by default; EMQX option for clustering / higher scale."));
children.push(bullet("Listeners: 1883 disabled by default, 8883 (TLS) and 8884 (mTLS for devices) enabled, 9001 (WSS)."));
children.push(bullet("Zigbee2MQTT, Tasmota, ESPHome, Shelly, Theengs (BLE->MQTT), rtl_433->MQTT all land here via their own adapter plugins."));

children.push(h2("5.2 Topic convention"));
children.push(...code(`iot/<plugin_id>/<device_id>/state
iot/<plugin_id>/<device_id>/cmd
iot/<plugin_id>/<device_id>/availability`));
children.push(p("Plugins may bridge foreign trees (e.g. zigbee2mqtt/#) into their own namespace. Bridging is a declared capability, not ambient."));

children.push(h2("5.3 North-facing integration"));
children.push(bullet("Read-only mirror topic home/events/# for NodeRED, Grafana, scripts, SCADA."));
children.push(bullet("Command ingress home/cmd/# requires a per-client mTLS cert with topic-scoped ACL."));

children.push(h2("5.4 Plugin capability additions"));
children.push(...code(`capabilities:
  mqtt.subscribe: ["zigbee2mqtt/#", "tasmota/+/tele/#"]
  mqtt.publish:   ["iot/zigbee/+/cmd"]
  mqtt.bridge:    ["external-broker://..."]   # elevated`));

children.push(h2("5.5 MQTT security"));
children.push(bullet("No anonymous listener. allow_anonymous false is hard-coded; config validator rejects overrides."));
children.push(bullet("mTLS for devices where firmware supports it; password+TLS is the fallback, not the default."));
children.push(bullet("Per-client ACLs generated from plugin manifests; wildcard subscribes require explicit user approval."));
children.push(bullet("Payload size and rate limits per client; stops a compromised sensor from DoS-ing the bus."));
children.push(bullet("Retained messages are treated as state -> included in the audit log; clearing is a first-class admin action."));
children.push(bullet("MQTT 5 features used: shared subscriptions for HA plugin pairs, message expiry, reason codes."));
children.push(bullet("Bridging to external brokers is an elevated capability; requires pinned CA and direction-restricted topics."));

// ====================================================================
// 6. Command Central
// ====================================================================
children.push(h1("6. Command Central"));
children.push(p("A dedicated, always-on control surface: wall-mounted tablet, kitchen touchscreen, or desktop display. Different hardware posture (shared, sometimes public-facing), different threat model, different interaction patterns than the web UI."));

children.push(h2("6.1 Hardware profiles"));
children.push(buildTable(
  [2400, 3400, 3560],
  ["Profile", "Example", "Notes"],
  [
    ["Wall panel", "8-13 inch PoE tablet (Fire, Lenovo, custom RPi+DSI)", "Primary target. Kiosk mode, auto-wake on motion."],
    ["Kitchen/hallway display", "10 inch HDMI touch + Pi 4/5", "Shared household device."],
    ["Dedicated HMI", "Industrial panel PC", "Garages/workshops; ruggedized."],
    ["Desktop", "Browser on any PC", "Same app, windowed."],
    ["TV dashboard", "Chromecast / FireTV cast target", "Read-only status wall variant."],
  ]
));

children.push(h2("6.2 Tech"));
children.push(bullet("PWA-first (React + TypeScript + Tailwind, Vite build) wrapped in a minimal kiosk shell: Electron for x86; WebView + WPE/Cog on RPi; Fully Kiosk Browser for Android tablets."));
children.push(bullet("Offline-first: IndexedDB cache + service worker; panel still shows last state and queues commands when the core is unreachable."));
children.push(bullet("Realtime: subscribes to device.*, alert.*, scene.* on the bus via the WebSocket gateway. p95 paint-to-reality under 250 ms."));
children.push(bullet("No REST polling. Stream or nothing."));

children.push(h2("6.3 Auth model"));
children.push(p("A wall tablet is physically accessible to anyone in the house, including guests. Treating it as a logged-in admin browser is how home automation systems leak. The panel itself has an identity; the person in front of it authenticates separately and ephemerally."));
children.push(buildTable(
  [2600, 6760],
  ["Layer", "Mechanism"],
  [
    ["Device identity", "mTLS cert provisioned at enrollment; revocable from core"],
    ["Default session", "Household view: read + low-risk actions (lights, scenes, music)"],
    ["Elevated actions", "PIN, NFC tag, BLE phone proximity, or voice+PIN - choose per user"],
    ["High-risk actions", "Always elevated: unlock doors, disarm alarm, admin settings"],
    ["Session timeout", "Elevated session expires on inactivity (default 60s) and on step-away (presence sensor)"],
    ["Admin", "Never on panel. Admin is a separate OIDC scope reachable only from the web UI."],
  ]
));

children.push(h2("6.4 Presence & ambient"));
children.push(bullet("Wake on approach (radar like LD2410, PIR, or BLE RSSI); display off otherwise (privacy + screen life)."));
children.push(bullet("Ambient mode when idle: clock, weather, next calendar event, quiet alerts. No controls visible - prevents shoulder-surfed taps."));
children.push(bullet("Identity-aware UI (opt-in): surface your scenes and calendar when the hub recognizes you."));

children.push(h2("6.5 Plugin contribution"));
children.push(p("Tiles are plugin-provided. A plugin ships its own tile components as a signed WASM+JS bundle - same signing chain as backend plugins. Tiles run in a sandboxed iframe with manifest-declared data and action scopes."));
children.push(...code(`ui:
  tiles:
    - id: solar-live
      bundle: tiles/solar.wasm.js
      permissions:
        data: ["energy.*"]
        actions: ["scene.*"]
      size: ["2x1", "4x2"]`));

children.push(h2("6.6 Security specifics"));
children.push(bullet("CSP: default-src 'self'; connect-src 'self' wss://gateway.local; frame-src 'self' - no third-party anything."));
children.push(bullet("Certificate pinning to the local CA."));
children.push(bullet("Screen privacy: sensitive data (PINs, private-room cameras) requires elevated session; ambient never shows them."));
children.push(bullet("Tamper detection: panel reports app hash + OS integrity to the core; drift = quarantine."));
children.push(bullet("Audit: every action logged with panel ID + (if elevated) user ID."));
children.push(bullet("Guest mode: big, obvious toggle that narrows the UI to household view; survives reboot until explicitly disabled."));

children.push(h2("6.7 Offline behavior"));
children.push(bullet("Last-known state stays visible; commands queue locally."));
children.push(bullet("Safety-critical commands (locks, alarm, garage) do not queue - live confirmation required. Better to fail loud than fire late."));
children.push(bullet("Reconnect reconciles via CRDT merge; conflicts resolve last-writer-wins with audit trail."));

// ====================================================================
// 7. Water Usage plugin
// ====================================================================
children.push(h1("7. Water Usage - ESP32-CAM + TFLite Micro"));
children.push(p("Read a household water meter with a camera and on-device ML, publish canonical consumption events, detect leaks and anomalies via the ML service."));

children.push(h2("7.1 Check for simpler alternatives first"));
children.push(bullet("Pulse output (reed switch / S0) on the meter - use that. 1 pulse = 1 L or 10 L, no CV needed."));
children.push(bullet("M-Bus / wM-Bus smart meter - use the 868 MHz adapter plugin."));
children.push(bullet("Legacy mechanical or dial-only - this is where the camera earns its keep."));
children.push(p("The integration ships all three variants; users should not do CV when a reed switch exists."));

children.push(h2("7.2 Hardware"));
children.push(buildTable(
  [2200, 3800, 3360],
  ["Part", "Choice", "Why"],
  [
    ["MCU + camera", "ESP32-S3-CAM (OV2640 or OV5640)", "S3 has vector instructions + more PSRAM -> real TFLM perf."],
    ["Lighting", "IR LED ring (850 nm) + IR-pass filter; or white LED on PIR", "Meter cupboards are dark; IR avoids visual pollution."],
    ["Mounting", "3D-printed shroud aligned to meter face", "Calibration stability is the #1 accuracy factor."],
    ["Power", "5 V USB or PoE splitter", "Batteries kill duty cycle and accuracy."],
  ]
));

children.push(h2("7.3 On-device pipeline"));
children.push(...code(`Capture (QVGA 320x240 grayscale, 1 fps scheduled)
  |
  v
Preprocess: warp to calibrated ROI, normalize, contrast stretch
  |
  v
Digit locator: tiny detector OR fixed crops from calibration
  |
  v
Per-digit classifier: CNN ~80 KB, 10 classes + "rolling"
  |
  v
Reading assembler: handles wheel-rolling ambiguity, monotonic constraint
  |
  v
Confidence check -- low --> buffer raw crop + upload to hub
  | high
  v
Publish MQTT  iot/water/<id>/reading  { m3: 00142.783, conf: 0.97, ts }`));
children.push(p("Why per-digit, not whole-number regression: digit wheels rotate and mid-roll looks like 'between 3 and 4'. A whole-number regressor hides that. A classifier with a 'rolling' class plus a monotonic post-filter behaves correctly."));

children.push(h2("7.4 Models"));
children.push(buildTable(
  [2800, 1800, 2200, 2560],
  ["Model", "Size", "Target", "Notes"],
  [
    ["digit_classifier_v1.tflite", "~80 KB int8", "Per-digit wheel", "10 digits + rolling class"],
    ["dial_regressor_v1.tflite", "~120 KB int8", "Analog needle dials", "Angle regression, 0-360 -> fraction"],
  ]
));
children.push(p("Both int8-quantized with representative-dataset calibration. ESP-DL or TFLite Micro runtime - ship TFLM first, add ESP-DL behind a build flag."));

children.push(h2("7.5 Calibration flow"));
children.push(bulletRich([{ text: "1. " }, "User mounts the camera and opens the calibration wizard on the panel."]));
children.push(bulletRich([{ text: "2. " }, "User drags corners of the meter face -> perspective transform matrix saved."]));
children.push(bulletRich([{ text: "3. " }, "User drags rectangles over each digit wheel -> per-digit ROIs saved."]));
children.push(bulletRich([{ text: "4. " }, "User enters current meter reading -> offset anchor for monotonic check."]));
children.push(bulletRich([{ text: "5. " }, "Hub ships the calibration bundle back to the ESP32, persisted in NVS."]));
children.push(p("Calibration is a config artifact, not firmware. Users can recalibrate without reflashing."));

children.push(h2("7.6 Training loop"));
children.push(bullet("ESP32 uploads low-confidence crops (<0.9) to the hub - small, opt-in - with the assembler's best-guess label."));
children.push(bullet("User reviews mislabelled ones in the web UI during the first two weeks (onboarding task: 'label 20 images')."));
children.push(bullet("Hub fine-tunes a per-household model on top of the base, quantizes, signs, OTAs to the ESP32."));
children.push(bullet("Base model stays in the signed model registry; per-household deltas use the same signing chain."));
children.push(p("Privacy: crops never leave the house unless the user opts into public dataset contribution."));

children.push(h2("7.7 Canonical entities"));
children.push(...code(`{
  "integration": "water-meter-cv",
  "entities": [
    { "id": "water.total",        "type": "sensor", "unit": "m3",   "class": "total_increasing" },
    { "id": "water.flow_rate",    "type": "sensor", "unit": "L/min" },
    { "id": "water.reading_conf", "type": "sensor", "unit": "%" },
    { "id": "water.leak_suspect", "type": "binary_sensor" }
  ]
}`));

children.push(h2("7.8 Anomaly detection (ML service)"));
children.push(buildTable(
  [2400, 3200, 3760],
  ["Detection", "Method", "Signal"],
  [
    ["Slow leak", "Night-hours baseline (2-5 AM), 14-day median", "leak_suspect when baseline > 0 for 3 consecutive nights"],
    ["Burst / open tap", "Hampel filter on flow_rate", "Immediate push alert + optional auto-shutoff scene"],
    ["Monotonic violation", "Reading decreased", "Low confidence -> re-inference upload"],
  ]
));

children.push(h2("7.9 Security (ESP32 baseline)"));
children.push(bullet("Secure Boot v2 + Flash Encryption enabled at first flash."));
children.push(bullet("Per-device X.509 cert provisioned at enrollment (QR-code pairing via the panel or phone)."));
children.push(bullet("mTLS to the MQTT broker, pinned to the home CA. No internet egress by default."));
children.push(bullet("Signed OTA, A/B partitions, rollback on boot-fail. Same cosign chain as core plugins and models."));
children.push(bullet("SBOM for firmware registered with the hub's CVE watcher."));
children.push(bullet("Network isolation: enrollment wizard nudges the IoT VLAN."));
children.push(bullet("No telnet, no serial-over-wifi, no debug backdoors. Debug builds refuse enrollment."));
children.push(bullet("Crops only leave the device to the hub; 'send any image to cloud' is never a default."));

// ====================================================================
// 8. Mains Power
// ====================================================================
children.push(h1("8. Mains Power - 3-phase ESP32"));
children.push(p("3-phase main incoming power monitoring with CT clamps and voltage taps. Combined with the heating plugin, this is the backbone of the platform's energy intelligence: real power, power factor, harmonics, imbalance, NILM disaggregation, and COP."));

children.push(h2("8.1 Hard prerequisite: electrician + regulation"));
children.push(bullet("Installation by a qualified electrician. Not negotiable. Part P (UK), NEN 1010, NF C 15-100, NEC."));
children.push(bullet("Non-invasive clamps only on the current side - no cutting conductors."));
children.push(bullet("Fused, isolated voltage taps. Galvanic isolation between HV side and ESP32 (AC-AC transformer or isolated AFE)."));
children.push(bullet("DIN-rail enclosure, IP-rated, placed next to the consumer unit."));
children.push(bullet("Firmware disables operation until a commissioning signature is present (installer credentials + date)."));
children.push(p("The plugin UI's first screen is 'call an electrician'. Not decoration - product posture."));

children.push(h2("8.2 Sensor + AFE choice"));
children.push(p("Do not build this on ESP32's built-in ADC - noisy, non-linear, temperature-drifts. A dedicated metering IC is the right abstraction."));
children.push(buildTable(
  [3000, 6360],
  ["Option", "When"],
  [
    ["ATM90E32AS / ATM90E36A (recommended)", "Production. 3-phase metering IC, SPI. Computes V_rms, I_rms, P/Q/S, PF, frequency, THD in hardware. Revenue-grade."],
    ["ADE9000 / ADE7953", "Higher-end alternative, more headroom, higher cost."],
    ["ESP32 ADC + raw CTs", "Prototyping only. Skip for anything claimed as accurate."],
  ]
));

children.push(h2("8.3 Sensor topology"));
children.push(buildTable(
  [2000, 3000, 4360],
  ["Sensor", "Purpose", "Part"],
  [
    ["CT clamp", "Current (I)", "SCT-013-000 100A (budget); YHDC SCT-006 or Magnelab SCT-0750 (accurate)"],
    ["Voltage tap", "Real V for P/PF", "Small AC-AC transformer (ZMPT101B or UL-rated step-down) with fused primary"],
  ]
));
children.push(p("Plus: neutral CT (4th) strongly recommended - catches phase imbalance and leakage currents. Choose CT rating to match incoming breaker."));

children.push(h2("8.4 What gets measured"));
children.push(...code(`Per phase (L1, L2, L3):
  V_rms        [V]
  I_rms        [A]
  P_real       [W]    signed (+import / -export)
  Q_reactive   [VAr]
  S_apparent   [VA]
  PF           [-]
  Frequency    [Hz]
  THD_v, THD_i [%]

Totals / derived:
  P_total       [W]
  energy_import [kWh]  monotonic, persisted NVS + hub checkpoint
  energy_export [kWh]  for solar / net-metering
  imbalance     [%]    from 4th CT

Events:
  sag, swell, outage, phase_loss, overcurrent_phase_X`));
children.push(p("Energy counters must be monotonic and survive reboots. Stored in NVS with wear-leveling + periodic checkpoint to the hub. Never reset without explicit, audited user action."));

children.push(h2("8.5 Pipeline"));
children.push(...code(`ATM90E32 (SPI) --> ESP32-S3
                     |
                     +- 1 Hz: per-phase V/I/P/Q/S/PF/F
                     +- 10 Hz: P_total (burst detection)
                     +- events: sag/swell/outage (immediate)
                     +- 5 min: harmonics snapshot, energy checkpoint
                             |
                             v  mTLS MQTT
              iot/power/<id>/phase/L1   {V,I,P,Q,S,PF,F,ts}
              iot/power/<id>/phase/L2   ...
              iot/power/<id>/phase/L3   ...
              iot/power/<id>/totals     {P,E_in,E_out,imbalance,ts}
              iot/power/<id>/event      {type, phase, ts, magnitude}
              iot/power/<id>/harmonics  {phase, THD_v, THD_i, bins[]}`));
children.push(p("Edge stays dumb. All ML, disaggregation, alerting - on the hub."));

children.push(h2("8.6 ML / analytics"));
children.push(buildTable(
  [2600, 3300, 3460],
  ["Model", "What it does", "Approach"],
  [
    ["NILM (disaggregation)", "Identify individual appliances from aggregate", "Event-based detector + signature classifier; NILMTK / seq2point; per-household fine-tune"],
    ["Anomaly detection", "Something new draws power at 3 AM", "Per-hour baselines + Hampel + isolation forest"],
    ["Appliance-failure prediction", "Motor bearing wear via harmonic drift", "THD + reactive-power trend; long-horizon, low-urgency"],
    ["Solar self-consumption", "Shift flexible loads to export windows", "Rule-composition suggestions, user approves"],
    ["Demand forecasting", "Next-24h total P for tariff optimization", "Gradient-boosted regressor on time, weather, occupancy"],
    ["Phase balancing advisor", "'Move your EV charger to L3'", "Imbalance integral over 30 days"],
  ]
));

children.push(h2("8.7 Capacity guardian (deterministic)"));
children.push(p("The automation engine needs to know service capacity and any contractual limits. At rule compile time, any rule turning on a known-large load is checked against declared capacity and a 15-minute forecast. Conflicts surface as design-time warnings."));
children.push(p("Runtime: a capacity guardian subscribes to power.total.P at 10 Hz and sheds load in user-configured priority order when trending toward overload. Safety-critical logic is deterministic, not ML-driven."));

children.push(h2("8.8 Security additions (beyond ESP32 baseline)"));
children.push(bullet("Tamper detection: accelerometer logs and alerts on enclosure movement."));
children.push(bullet("Write-protected commissioning: CT ratio, voltage-tap transform, service capacity, installer signature are set once and require a physical button + hub-side admin action to change."));
children.push(bullet("No remote firmware rollback to pre-commissioning versions; signed version floor."));
children.push(bullet("Air-gap default: no outbound internet; hub is its only peer."));

// ====================================================================
// 9. Heating Flow/Return
// ====================================================================
children.push(h1("9. Heating Flow/Return Temps"));
children.push(p("Two temperature probes on the heating loop: flow (water leaving the boiler / heat pump into the house) and return. Publish T_flow, T_return, delta T. When combined with the power meter plugin, derive COP in real time."));

children.push(h2("9.1 Signals and meaning"));
children.push(buildTable(
  [2600, 6760],
  ["Signal", "Meaning"],
  [
    ["delta T (flow - return)", "Heating system health. Design delta T: ~5-7 K heat pumps, ~15-20 K gas boilers. Drift indicates balance or flow issue."],
    ["T_flow vs outdoor", "Validates weather-compensation curve. Over-spec = wasted energy."],
    ["delta T + flow rate", "Actual thermal kWh delivered. Without flow, delta T is qualitative only."],
    ["Thermal kWh / Electrical kWh", "COP - the number heat pump owners actually care about."],
    ["Short-cycling pattern", "Heat pump cycling too often -> sizing / control issue."],
    ["Defrost detection", "Transient inverted delta T is normal; frequent / long = icing."],
  ]
));

children.push(h2("9.2 Sensor choice"));
children.push(buildTable(
  [1800, 3200, 2400, 1960],
  ["Tier", "Sensor", "Accuracy", "Use"],
  [
    ["Good enough", "DS18B20 (1-Wire, waterproof probe)", "+/-0.5 K; ~+/-0.2 K matched pair", "Gas boilers at 15-20 K delta T"],
    ["Accurate", "PT1000 + MAX31865 (SPI, 15-bit)", "+/-0.15 K; +/-0.05 K matched pair", "Required for heat pump COP"],
    ["Overkill", "Class A PT100 + bridge", "Revenue-grade", "Skip"],
  ]
));
children.push(p("Default to PT1000 matched pair, factory-calibrated together. A 0.5 K error on a 5 K delta T is 10% error on thermal output - wrecks COP."));

children.push(h2("9.3 Mounting"));
children.push(bullet("Strap-on (non-invasive): sensor clamped to copper pipe with thermal paste, covered with insulation sleeve. DIY-friendly. Requires the insulation - un-insulated reads 2-4 K low."));
children.push(bullet("Immersion pocket (plumber required): probe in thermowell, direct water contact. ~0.1 K better, much more stable."));
children.push(p("Calibration at commissioning: both probes in the same bucket of water -> capture offset -> store per-probe correction in NVS. Do this before installing on the pipe. Skipping this is why most DIY heat-pump COP claims are wrong."));

children.push(h2("9.4 Hardware integration"));
children.push(bullet("Option A (preferred): daughter-board on the power-meter ESP32-S3 - 2x MAX31865 over SPI. Same enclosure, commissioning, cert, mTLS session."));
children.push(bullet("Option B (standalone): separate ESP32-S3 near the heating system. Same firmware family and security baseline."));

children.push(h2("9.5 Pipeline"));
children.push(...code(`PT1000 --> MAX31865 --> SPI --> ESP32
PT1000 --> MAX31865 --> SPI ---'
                                 |
                                 +- 1 Hz: T_flow, T_return, delta T
                                 +- event: short-cycle (3 on-off within 10 min)
                                 +- event: defrost (delta T inverted > 60s)
                                 +- 5 min: checkpoint to hub
                                       |
                                       v  mTLS MQTT
                           iot/heating/<id>/flow    {T, ts}
                           iot/heating/<id>/return  {T, ts}
                           iot/heating/<id>/delta   {delta_T, ts}
                           iot/heating/<id>/event   {type, ts, details}`));

children.push(h2("9.6 Canonical entities"));
children.push(...code(`{
  "integration": "heating-loop",
  "entities": [
    { "id": "heating.T_flow",         "type": "sensor", "unit": "C" },
    { "id": "heating.T_return",       "type": "sensor", "unit": "C" },
    { "id": "heating.delta_T",        "type": "sensor", "unit": "K" },
    { "id": "heating.short_cycle",    "type": "binary_sensor" },
    { "id": "heating.defrost_active", "type": "binary_sensor" }
  ]
}`));

children.push(h2("9.7 Optional flow rate -> thermal kWh"));
children.push(bullet("Ultrasonic clamp-on (Grundfos VFS, Sika): non-invasive, +/-2% with good coupling."));
children.push(bullet("Inline turbine / impeller: plumber, +/-1%."));
children.push(bullet("Read from heat-pump Modbus: most modern heat pumps expose flow; use the Modbus adapter."));
children.push(p("If present: publish heating.flow_rate and heating.thermal_power. Thermal power = flow_rate x rho x c_p x delta T. Glycol mix configured at commissioning."));

children.push(h2("9.8 COP cross-plugin"));
children.push(p("The ML service subscribes to both heating.thermal_power and power.heat_pump.P, and publishes:"));
children.push(bullet("heating.cop.instant (1 Hz, rolling 60s average)"));
children.push(bullet("heating.cop.daily (kWh_thermal / kWh_electrical, daily)"));
children.push(bullet("heating.cop.seasonal (SCOP rolling 12 months)"));
children.push(p("The power-meter plugin must know which circuit is the heat pump - a commissioning checkbox, no new glue code. If sub-metering is absent, fallback = total minus baseline (noisy, clearly labelled). This is the architectural test: a cross-plugin feature requires no bespoke integration, only a subscription."));

children.push(h2("9.9 Automation hooks"));
children.push(...code(`- trigger: heating.delta_T < 2 K for 10 min AND heating.flow_rate > 0
  then: notify "Heating system not transferring heat - check pump or air lock"

- trigger: heating.short_cycle == true
  then: notify "Heat pump short-cycling - check sizing or buffer tank"

- trigger: heating.cop.daily < weather_adjusted_expected * 0.85 for 3 days
  then: notify "Heat pump efficiency dropped - service recommended"

- trigger: heating.T_return < 2 C
  then: activate scene "freeze_protection"`));

// ====================================================================
// 10. Security architecture
// ====================================================================
children.push(h1("10. Security Architecture"));

children.push(h2("10.1 Trust model"));
children.push(bullet("Core = TCB. Everything else is untrusted until proven otherwise (signed + capability-scoped)."));
children.push(bullet("User = sovereign. Keys held locally. Cloud is optional."));

children.push(h2("10.2 Controls (ETSI EN 303 645 + OWASP ASVS L2)"));
children.push(buildTable(
  [3200, 6160],
  ["Control", "Implementation"],
  [
    ["No default passwords", "First-boot wizard forces strong credentials + 2FA"],
    ["Secure update", "Signed, atomic, A/B partitions, rollback; TUF metadata"],
    ["Vuln disclosure", ".well-known/security.txt, public PGP key, 90-day SLA"],
    ["Keep software updated", "Auto-update staged rollout; CVE watcher per SBOM"],
    ["Secure communication", "mTLS everywhere internal; TLS 1.3 external; HSTS"],
    ["Minimize attack surface", "No default open ports; admin on loopback / VPN only"],
    ["Software integrity", "Measured boot where HW supports; signed binaries; cosign"],
    ["Personal data protection", "Encryption at rest (LUKS + per-table); user-held keys for exports"],
    ["Resilience to outages", "Full local operation; cached credentials; offline ML"],
    ["Telemetry", "Off by default; granular opt-in; local-only audit always on"],
    ["Delete user data", "One-click full erase, including backups and derived features"],
    ["Secure install", "Reproducible builds; SLSA L3 target"],
  ]
));

children.push(h2("10.3 Supply chain"));
children.push(bullet("Reproducible builds, SLSA provenance, CycloneDX SBOMs per artifact, cosign signatures, TUF update metadata."));
children.push(bullet("Plugin marketplace requires: signed author identity, SBOM, automated scan pass, human review for serial / raw-socket / elevated capabilities."));

children.push(h2("10.4 Threat model highlights"));
children.push(bullet("Malicious plugin: contained by capability sandbox + bus ACL + resource limits; worst case is its own subject tree."));
children.push(bullet("Compromised cloud partner: isolated to bridge plugin; core state untouched; credentials revocable."));
children.push(bullet("RF spoofing (433 MHz): replay attacks assumed; 433 sensors never used as sole input for security-critical automations. Engine enforces via capability tags."));
children.push(bullet("Voice injection (ultrasonic/laser): high-impact intents require confirmation; optional PIN."));

// ====================================================================
// 11. APIs
// ====================================================================
children.push(h1("11. APIs"));
children.push(bullet("External: REST + WebSocket (OpenAPI 3.1), OAuth2/OIDC with PKCE, scoped tokens. Optional GraphQL read layer for UI."));
children.push(bullet("Internal: gRPC over mTLS service-to-service; NATS for events."));
children.push(bullet("Plugin ABI: Protobuf-defined, versioned, backward-compatible for a full major cycle."));
children.push(h2("REST examples"));
children.push(...code(`POST /api/v1/automations
GET  /api/v1/devices?room=kitchen
POST /api/v1/devices/{id}/commands   { "entity": "...", "action": "...", "value": ... }
WS   /api/v1/stream?topics=device.*`));

// ====================================================================
// 12. Scale & Reliability
// ====================================================================
children.push(h1("12. Scale & Reliability"));
children.push(bullet("Vertical first. Target is a single home. A Pi 5 with NVMe handles the stated load."));
children.push(bullet("Horizontal option: core services are stateless behind the registry / DB; HA pair via keepalived or K3s. Zigbee / Z-Wave coordinators are stateful hardware - plugin supports active-passive with takeover."));
children.push(h2("Failure domains"));
children.push(bullet("Plugin crash -> supervisor restart with backoff; event bus queues buffer commands."));
children.push(bullet("Core crash -> systemd restart; state rehydrated from registry + JetStream replay."));
children.push(bullet("Power loss -> UPS signaling plugin triggers graceful-shutdown automations."));
children.push(h2("Observability"));
children.push(p("Every event carries a trace ID. A failing automation is a debuggable span tree, not a log grep."));

// ====================================================================
// 13. Trade-offs (consolidated)
// ====================================================================
children.push(h1("13. Trade-off Analysis"));

children.push(h2("13.1 Core architecture"));
children.push(buildTable(
  [2400, 2400, 2400, 2160],
  ["Decision", "Alternative", "Why chosen", "What it costs"],
  [
    ["WASM plugin runtime (default)", "Containers everywhere", "Faster, smaller, stronger sandbox", "HW-access plugins still need containers -> two runtimes"],
    ["Rust core", "Go or Python", "Memory safety + perf + WASM host maturity", "Smaller contributor pool early"],
    ["NATS JetStream", "Kafka / Redis Streams", "Lightweight, mTLS accounts, runs on SBC", "Smaller ecosystem"],
    ["CEL for rules", "Embedded Python / Lua", "Sandboxed, typed, no RCE surface", "Power users may want loops"],
    ["Local-first ML", "Cloud ML", "Privacy, offline, no recurring cost", "Smaller models, slower training"],
    ["Matter as schema baseline", "Custom schema", "Future-proof, interop", "Some early devices need shims"],
    ["Own voice stack", "Alexa / Google", "Privacy, offline", "Worse open-domain accuracy"],
  ]
));

children.push(h2("13.2 MQTT"));
children.push(buildTable(
  [2400, 2400, 2400, 2160],
  ["Decision", "Alternative", "Why", "Cost"],
  [
    ["MQTT as adapter, not primary bus", "MQTT everywhere internally", "NATS has better auth / multi-tenant; no retained-msg footguns", "Two messaging systems"],
    ["Bundle Mosquitto by default", "BYO broker", "Works out of the box", "Larger install"],
    ["ACLs from plugin manifest", "Hand-edit mosquitto.conf", "Single source of truth", "Manifest schema grows"],
  ]
));

children.push(h2("13.3 Command Central"));
children.push(buildTable(
  [2400, 2400, 2400, 2160],
  ["Decision", "Alternative", "Why", "Cost"],
  [
    ["PWA + kiosk shell", "Native apps per platform", "One codebase, fast iteration, good offline", "Lockdown work per OS"],
    ["Device cert + ephemeral user auth", "Shared panel login", "Matches physical reality of shared panels", "More UX work"],
    ["Tiles in sandboxed iframes", "Freeform plugin React", "Prevents escalation", "Constrained API"],
    ["WebRTC cameras, local signalling", "MJPEG / cloud relay", "Low latency, no cloud dep", "WebRTC setup complexity"],
  ]
));

children.push(h2("13.4 Water meter"));
children.push(buildTable(
  [2400, 2400, 2400, 2160],
  ["Decision", "Alternative", "Why", "Cost"],
  [
    ["ESP32-S3 + TFLM on-device", "RTSP camera -> hub inference", "Offline, low bandwidth, no video leaves cupboard", "Lower model ceiling"],
    ["Per-digit classifier", "End-to-end regressor", "Handles rolling digits correctly", "Calibration required"],
    ["On-device low-conf upload", "Always upload", "Bandwidth + privacy", "Some mislabels never caught"],
    ["Per-household fine-tune", "One-size-fits-all", "Handles unusual meters", "Small training infra on hub"],
    ["IR lighting default", "White LED", "Works in closed cupboards", "IR-pass filter needed"],
  ]
));

children.push(h2("13.5 Mains power"));
children.push(buildTable(
  [2400, 2400, 2400, 2160],
  ["Decision", "Alternative", "Why", "Cost"],
  [
    ["Dedicated metering IC (ATM90E32)", "ESP32 ADC + DSP firmware", "Revenue-grade out of the box", "+$3 BOM, IC lock-in"],
    ["Voltage taps per phase", "Assume nominal 230 V", "Real P, PF, harmonics, sag/swell", "Electrician install"],
    ["Neutral CT (4th)", "Skip", "Imbalance + leakage detection", "+1 CT ~$10"],
    ["All analytics on hub", "Edge ML for disaggregation", "Hub has compute + retraining loop", "Needs hub reachable; MQTT buffers outages"],
    ["Deterministic capacity guardian", "ML-driven load shed", "Safety-critical must be predictable", "Less 'clever'"],
    ["Tamper accelerometer", "Skip", "Consumer-unit tamper sensitivity", "+$1 BOM"],
  ]
));

children.push(h2("13.6 Heating"));
children.push(buildTable(
  [2400, 2400, 2400, 2160],
  ["Decision", "Alternative", "Why", "Cost"],
  [
    ["PT1000 + MAX31865 default", "DS18B20 default", "10x better delta T, required for COP", "Higher BOM + SPI"],
    ["Strap-on default mount", "Immersion default", "DIY install covers 90% of users", "~0.1-0.3 K worse"],
    ["Piggyback on power-meter ESP32", "Always separate", "Fewer devices / certs / enclosures", "Couples two plugins' hardware"],
    ["Flow sensor optional", "Required", "delta T alone still diagnostic", "COP gated on flow - disclosed"],
    ["COP via ML-service subscription", "Bake COP into one plugin", "Validates plugin architecture", "Depends on circuit labelling"],
  ]
));

// ====================================================================
// 14. Milestones
// ====================================================================
children.push(h1("14. Milestones"));
children.push(buildTable(
  [1400, 3000, 4960],
  ["ID", "Name (duration)", "Scope"],
  [
    ["M1", "Skeleton (4 wks)", "Core bus + registry + API gateway + auth + Zigbee adapter + web UI stub. Security baseline (mTLS, signing, SBOM) from day one."],
    ["M2", "Plugin SDK (4 wks)", "WASM host, capability model, published SDK + 3 adapters (Z-Wave, 433 SDR, Matter)."],
    ["M3", "Automation + Observability (3 wks)", "CEL rule engine, OpenTelemetry end-to-end, audit log."],
    ["M3.5", "Command Central v1 (3 wks)", "PWA shell, kiosk wrapper (RPi + Fire tablet), core tiles, device cert enrollment, PIN elevation, offline cache."],
    ["M4", "ML anomalies + edge-ML reference plugins (4 wks)", "First two ML models + approval flow. Water meter, mains power (measurement + capacity guardian), heating flow/return plugins ship as reference integrations."],
    ["M4b", "COP derivation", "Cross-plugin ML-service subscription wiring; requires power meter deployed and heat-pump circuit labelled."],
    ["M5", "Voice (4 wks)", "Wake + STT + NLU + TTS; closed-domain intents only. NILM training pipeline and disaggregation tile extension."],
    ["M6", "Hardening + cert prep (3 wks)", "Pen test, ETSI 303 645 checklist, public vuln disclosure program."],
  ]
));

// ====================================================================
// 15. Revisit-later
// ====================================================================
children.push(h1("15. Revisit-later"));

children.push(h2("15.1 Platform"));
children.push(bullet("Multi-home / tenancy: current design is single-home. Federation via CRDT-based sync before users accumulate pain."));
children.push(bullet("Plugin marketplace: signed + scanned is table stakes; revocation UX and abandoned-plugin policy are the hard parts."));
children.push(bullet("LLM in the loop: if the local LLM composes automations as an agent, extend the capability model to the LLM - it should not be more privileged than a plugin."));
children.push(bullet("Regulatory drift: EU Cyber Resilience Act (CRA) takes effect 2027. SBOM + update + vuln-disclosure commitments should already comply; re-audit annually."));
children.push(bullet("Storage tiering: TimescaleDB on a Pi is fine at 500 devices; at 5000+ the registry / TSDB split becomes real and needs a migration story."));

children.push(h2("15.2 MQTT"));
children.push(bullet("Sparkplug B for industrial users, once the user base asks."));
children.push(bullet("MQTT over QUIC (EMQX supports it) once clients do; better for mobile / flaky links."));

children.push(h2("15.3 Command Central"));
children.push(bullet("Gesture + gaze control (kitchen/bathroom - wet/greasy hands), via the same radar sensors used for presence."));
children.push(bullet("Multi-panel handoff: start a timer on the kitchen panel, walk to the living room, see it pick up there. Needs CRDT sync solid."));
children.push(bullet("E-ink secondary displays for always-on info without light pollution in bedrooms."));

children.push(h2("15.4 Water"));
children.push(bullet("Electric meter + gas meter with the same pipeline - mostly a new training set. Make meter-cv generic, ship three integrations off it."));
children.push(bullet("Two-camera redundancy for high-value meters (cross-check)."));
children.push(bullet("ESP32-P4 when mature - dedicated camera pipeline, more NN perf."));
children.push(bullet("Audio-based leak detection (ultrasonic mics on pipes) - separate plugin."));

children.push(h2("15.5 Mains power"));
children.push(bullet("Phase-angle-aware NILM: per-phase disaggregation; materially better, needs more data."));
children.push(bullet("Dynamic tariff integration (Tibber, Octopus Agile, Nord Pool) as a hub-side plugin publishing 'cheap now' signals."));
children.push(bullet("Grid-service participation (demand response, V2G) - regulated, opt-in, country-specific."));
children.push(bullet("Sub-metering (per-circuit CTs) - different plugin, same firmware stack, 6-12 CTs. Makes NILM nearly unnecessary."));
children.push(bullet("Safety interlock across domains: capacity guardian + EV + solar coordinating via formal plugin-to-plugin negotiation - worth a dedicated design pass once two exist."));

children.push(h2("15.6 Heating"));
children.push(bullet("Per-zone return temps (multi-zone systems): same plugin, more probes, declared at commissioning."));
children.push(bullet("Refrigerant-circuit sensors (evaporator / condenser) - invasive, service-level installs only."));
children.push(bullet("Hydraulic separator detection with buffer tank: delta T across primary vs secondary loops differs. Topology selector in commissioning wizard."));
children.push(bullet("Gas meter correlation: pairing with a gas-meter plugin (same CV meter-reading pattern as water) gives combustion efficiency estimates."));

// ------------ document assembly ------------
const doc = new Document({
  creator: "IoT-AtHome",
  title: "IoT-AtHome System Design & Requirements",
  description: "Full system design and requirements document",
  styles: {
    default: { document: { run: { font: FONT, size: 22 } } },
    paragraphStyles: [
      { id: "Heading1", name: "Heading 1", basedOn: "Normal", next: "Normal", quickFormat: true,
        run: { size: 36, bold: true, font: FONT, color: "1F3864" },
        paragraph: { spacing: { before: 360, after: 200 }, outlineLevel: 0 } },
      { id: "Heading2", name: "Heading 2", basedOn: "Normal", next: "Normal", quickFormat: true,
        run: { size: 28, bold: true, font: FONT, color: "2F5496" },
        paragraph: { spacing: { before: 240, after: 120 }, outlineLevel: 1 } },
      { id: "Heading3", name: "Heading 3", basedOn: "Normal", next: "Normal", quickFormat: true,
        run: { size: 24, bold: true, font: FONT, color: "2E74B5" },
        paragraph: { spacing: { before: 160, after: 80 }, outlineLevel: 2 } },
    ],
  },
  numbering: {
    config: [
      { reference: "bullets",
        levels: [
          { level: 0, format: LevelFormat.BULLET, text: "\u2022", alignment: AlignmentType.LEFT,
            style: { paragraph: { indent: { left: 720, hanging: 360 } } } },
          { level: 1, format: LevelFormat.BULLET, text: "\u25E6", alignment: AlignmentType.LEFT,
            style: { paragraph: { indent: { left: 1440, hanging: 360 } } } },
        ],
      },
    ],
  },
  sections: [{
    properties: {
      page: {
        size: { width: 12240, height: 15840 },
        margin: { top: 1440, right: 1440, bottom: 1440, left: 1440 },
      },
    },
    headers: {
      default: new Header({ children: [new Paragraph({
        alignment: AlignmentType.RIGHT,
        children: [new TextRun({ text: "IoT-AtHome \u2014 System Design", size: 18, font: FONT, color: "7F7F7F" })],
      })] }),
    },
    footers: {
      default: new Footer({ children: [new Paragraph({
        alignment: AlignmentType.CENTER,
        children: [
          new TextRun({ text: "Page ", size: 18, font: FONT, color: "7F7F7F" }),
          new TextRun({ children: [PageNumber.CURRENT], size: 18, font: FONT, color: "7F7F7F" }),
          new TextRun({ text: " of ", size: 18, font: FONT, color: "7F7F7F" }),
          new TextRun({ children: [PageNumber.TOTAL_PAGES], size: 18, font: FONT, color: "7F7F7F" }),
        ],
      })] }),
    },
    children,
  }],
});

Packer.toBuffer(doc).then(buffer => {
  fs.writeFileSync("IoT-AtHome-Design.docx", buffer);
  console.log("OK: wrote IoT-AtHome-Design.docx (" + buffer.length + " bytes)");
});
