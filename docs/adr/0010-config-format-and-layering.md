# ADR-0010: Configuration — Layered TOML + Env, Schema-Validated

- **Status:** Accepted
- **Date:** 2026-04-20

## Context

The platform has many configuration surfaces: service endpoints, TLS certificate paths, database URLs, NATS connection, feature flags, plugin install paths, hardware-specific overrides. Configuration mistakes are a common cause of subtle outages. Operators in users' homes are not necessarily experts; a bad config must fail loudly, not silently.

## Decision

### File format

**TOML.** Human-writable, comment-friendly, well-specified, round-trippable, no YAML footguns.

### Layering

Configuration is composed from the following layers, each overriding the previous:

1. **`/etc/iotathome/default.toml`** — shipped with the package. Read-only to the operator. Contains sensible defaults.
2. **`/etc/iotathome/local.toml`** — admin overrides. Sourced from `deploy/` during install.
3. **`/etc/iotathome/conf.d/*.toml`** — drop-in fragments, sorted lexicographically. Plugins land their own fragments here.
4. **`/var/lib/iotathome/state.toml`** — runtime-written state (enrollment tokens, discovered device hints). The service writes this; operators don't edit it.
5. **Environment variables** prefixed `IOT_` — highest precedence; intended for container/systemd overrides. Nested keys use `__` as separator (`IOT_BUS__URL=nats://...`).

Loader: **[`figment`](https://docs.rs/figment/)** handles layering natively.

### Schema validation

- Every service ships a **JSON Schema** for its config section: `crates/<service>/config.schema.json`.
- On service startup, the loaded merged config is validated against all applicable schemas.
- **Validation failure = refuse to start**, with a single-error-message-per-issue output (not a pile of nested JSONPath). Use a humanized error formatter.
- Unknown fields are **warnings** in dev, **errors** in production (set by a config key `strict_unknown = true` in `/etc/iotathome/default.toml`).

### Secrets

- **No secrets in config files.** Secrets are referenced by name:
  - `nats_credentials = { from = "file", path = "/etc/iotathome/secrets/nats.creds" }`
  - `keycloak_client_secret = { from = "env", name = "IOT_KEYCLOAK_CLIENT_SECRET" }`
  - `vault_token = { from = "systemd_credential", name = "vault-token" }`
- Secret files must have mode 0600 and be owned by the service user; the loader checks and refuses to start otherwise.

### Config versioning

- Top-level `config_version` integer in every config file. Loader refuses to boot on a config whose version is newer than the code understands.
- Upgrades bump `config_version` and emit a migration note in release notes.

### Examples

Every service's repo folder contains a `config.example.toml` with all keys and their defaults, heavily commented. CI verifies `config.example.toml` is a valid superset of `config.schema.json` defaults.

### Dev loop

- `just dev` writes a synthesized config from `deploy/compose/dev.toml` into each service's runtime directory.
- Hot-reload: services listen for `SIGHUP`; non-reloadable keys (e.g. listener ports) log a warning and keep the old value.

## Consequences

- Operators get one format (TOML) and one mental model (layered).
- Automation (systemd, Ansible, K3s ConfigMaps) drops into `conf.d/` or env without touching packaged defaults.
- Schema validation eliminates a class of silent mis-config bugs.
- Adding a config key is an additive change; removing one is breaking and bumps `config_version`.
