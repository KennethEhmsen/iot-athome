# ADR-0004: NATS Subject Taxonomy

- **Status:** Accepted
- **Date:** 2026-04-20

## Context

Every service and plugin communicates over NATS JetStream. Subject design is a one-way door: once plugins and rules depend on a subject shape, changing it requires a coordinated major-version migration across the whole fleet. The taxonomy must:

- Admit meaningful wildcards for ACLs (per-plugin NATS accounts) and the rule engine.
- Keep user-visible names (room names, user labels) out of subjects. Subjects are stable; labels are mutable.
- Be greppable and predictable.
- Be independent of payload schema version (the header carries the schema version; the subject carries identity and intent).

## Decision

### Top-level namespaces

| Prefix | Purpose |
|---|---|
| `device.<plugin_id>.<device_id>.<entity>.<leaf>` | Device telemetry, commands, availability |
| `cmd.<plugin_id>.<device_id>.<entity>` | Commands into a plugin (plugin subscribes) |
| `automation.<rule_id>.<leaf>` | Rule lifecycle + firings |
| `ml.<model_id>.<leaf>` | ML model predictions and training hooks |
| `alert.<severity>.<source>` | Human-facing alerts |
| `audit.<kind>` | Append-only audit log feed (also persisted by the audit service) |
| `sys.<component>.<leaf>` | Component health, discovery, lifecycle |

### Device leafs

- `.state` — authoritative entity state (retained-style semantics via JetStream last-msg-per-subject)
- `.event` — discrete events (button press, motion, etc.)
- `.avail` — availability (online/offline)

### Rules

- Tokens are **stable IDs** (ULID), not human names. `device.water-meter-cv.01HXXABC.water_total.state`, never `device.water-meter-cv.kitchen_meter.state`.
- **Lowercase snake_case** tokens. No dots inside tokens (dots are delimiters). No hyphens in tokens (hyphens are reserved for plugin IDs where they appear naturally).
- Wildcards used in ACLs and rule triggers: `*` (single token) and `>` (trailing).
- **Schema version is a message header**, not a subject segment. Header key: `iot-schema-version` = `"1"`, `"2"`, ...
- **Trace propagation:** every message carries a `traceparent` header (W3C Trace Context).

### Per-plugin NATS accounts

Each plugin gets its own NATS account. The account is issued:

- `PUB` permission on `device.<plugin_id>.>`, `ml.<model_id>.>` (if it owns models), and any bridging namespaces it declared.
- `SUB` permission on `cmd.<plugin_id>.>`, plus any declared `bus.subscribe` subjects in its manifest.
- `DENY` on everything else, including other plugins' namespaces.

The core issues accounts from a template; manifest ACLs are machine-generated from the plugin manifest on install.

## Consequences

- ACL configs are generated, not hand-edited. The plugin manifest is the single source of truth.
- Because the schema version is in the header, we can evolve payloads without fracturing subject space.
- Rules reference subjects, so renaming a device (changing its ULID-anchored namespace) is effectively impossible — which is correct. User-visible names are metadata.
- `audit.>` is the only subject namespace the audit service is allowed to consume exclusively; other subscribers must have `audit.read` capability.
