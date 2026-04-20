# ADR-0005: Canonical Device Schema Versioning

- **Status:** Accepted
- **Date:** 2026-04-20

## Context

The canonical device model (see `schemas/device.proto`) is consumed by every service, every plugin, the panel, and eventually thousands of deployed devices. The schema WILL evolve: Matter adds device types, new sensors ship, capabilities grow. A naive "break-and-upgrade" strategy is untenable for a platform that runs in users' homes.

## Decision

### Wire format

**Protobuf (proto3), generated per language via `buf`.** Field numbers are irrevocable.

### Versioning rules

1. **Proto3 additive rules are the baseline.** New optional fields are always safe; unknown fields pass through unchanged.
2. **Never reuse a removed field tag.** Removed fields go into `reserved` blocks. Enforced via `buf breaking` in CI against `main`.
3. **Major versions exist**, and are expressed as **separate `.proto` files in separate packages** (`iot.device.v1`, `iot.device.v2`), not by renaming fields in place. This follows Google's AIP-181.
4. **Bus message envelope carries the schema version** as header `iot-schema-version`. Producers stamp it; consumers route by it.
5. The **Device Registry is the single upcaster**: when a consumer requests v2 and the stored record is v1, the registry upcasts on read. Plugins never see multiple versions simultaneously.

### Compatibility window

- **Registry supports the latest major + one previous major** for a full major cycle (minimum 12 months).
- A deprecation runs ≥ 6 months before the previous major is dropped. Deprecation is announced via an alert in the panel ("plugin X emits v1 events; support ends 2027-Q3").

### Forbidden changes (always break)

- Renaming a field.
- Changing a field's type (including `int32` → `int64`).
- Changing an enum value's numeric tag.
- Making a previously-optional field required via presence expectations in consumers.

### Tooling

- `buf` for linting, breaking-change detection, and codegen.
- `buf.yaml` + `buf.gen.yaml` committed; CI runs `buf breaking --against '.git#branch=main'` on every PR that touches `schemas/`.

## Consequences

- We pay upfront tax on discipline (no field renames, no tag reuse) and earn smooth upgrades forever.
- Plugin authors see one Device API per major version. Upcasting cost lives in the registry, not in every plugin.
- Storage cost: the registry stores the raw v1 bytes + an upcast cache. Upcast is deterministic so the cache is optional.
- Panel consumes the latest major only. The registry fronts older producers.
