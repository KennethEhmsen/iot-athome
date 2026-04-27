# ADR-0011: Dev-Mode Bus Auth — mTLS-only, Single IOT Account

- **Status:** Superseded by [M5a W1] — broker decentralized auth shipped 2026-04-24
- **Date:** 2026-04-21 (accepted) · 2026-04-24 (superseded)
- **Deciders:** IoT-AtHome core team
- **Context milestone:** shipped in M1; cryptographic minter shipped M3 W1.3; bootstrap wiring shipped M5a W1

## Context

ADR-0004 specified per-plugin NATS accounts, each issued from a template by "the registry" (conceptually — plugin installer in practice) with PUB/SUB ACLs derived from the plugin manifest. That infrastructure lands with the WASM plugin runtime in M2.

M1 ships adapters as standalone systemd-shaped services, not via an installer. We needed an authentication model that:

- Works today without the plugin installer.
- Does not leak dev credentials into future production configs.
- Does not require rewriting the adapter side when M2 lands.

## Decision

In M1, the dev NATS server (see [`deploy/compose/nats/nats.conf`](../../deploy/compose/nats/nats.conf)) runs with:

```
accounts {
  SYS { users: [{ user: "sys", password: "sys" }] }      # admin only
  IOT {
    jetstream: enabled
    users: [{ user: "dev" }]                             # no password
  }
}
system_account: SYS
no_auth_user: dev                                        # ← the key
```

The `no_auth_user: dev` directive means a client that presents a valid certificate (our mTLS `verify: true` gate already held this) but no password is auto-mapped to the `IOT/dev` user. Effectively: **mTLS is the authentication factor, the account + username are infrastructure.**

Every service (`iot-registry`, `iot-gateway`, `zigbee2mqtt-adapter`) connects with its per-component client cert and no credentials. Bus ACLs are the single permissive `>` inherited from the `IOT` account.

## Consequences

- **M1 works without the plugin installer.** No blocker for any demo.
- **Revocation is coarse**: losing a single adapter's cert means re-minting the dev CA (for everyone), not just revoking that identity. Acceptable because the dev CA is ephemeral per-workstation.
- **Production deployments must switch to per-plugin accounts.** This decision deliberately leaves `no_auth_user` on in the dev compose only; the production packaging story (M3+) writes `accounts { ... }` + per-plugin creds from the plugin-installer state at install time.
- **Client code stays unchanged** when M2 arrives: `iot_bus::Bus::connect` already takes only mTLS cert paths; adding credentials is a field addition, not an API change.

## Alternatives considered

- **Per-plugin accounts with static `deploy/compose` creds.** Every adapter gets a hand-authored `creds` file. Works but ossifies configuration that the installer will regenerate anyway.
- **`authorization: { token: "dev" }` as a shared secret.** Simpler than accounts, but weaker than mTLS since any client presenting the token is trusted. Drops the "identity by cert" property the rest of the platform relies on.
- **Disable auth in dev entirely.** Drift risk: prod config would diverge from dev in a security-critical way.

## Supersession trigger

This ADR is retired (status → Superseded) when ADR-0004's per-plugin account plan is implemented in the plugin installer — target M2 end.

## Superseded

The retirement landed in **M5a W1** (post-`v0.4.0-m4`), later than the originally-planned M2-end target. Two halves shipped in sequence:

1. **Cryptographic half** (M3 W1.3) — `iot_bus::jwt::issue_user_jwt` minter + `UserAcl` + claim types, with unit-test coverage proving NATS-wire JWTs verify under the issuing account key. This was usable as a library but not yet wired into install or runtime.

2. **Bootstrap wiring half** (M5a W1) — what made it actually retire ADR-0011:
   - `iot_bus::jwt` extended with `issue_account_jwt` + `format_creds_file` so the operator → account → user trust chain mints end-to-end.
   - `iotctl nats bootstrap` generates an operator + account keypair pair, signs an account JWT, and writes a `resolver.conf` snippet ready for the broker to `include`.
   - `tools/devcerts/mint.sh` invokes `iotctl nats bootstrap` so `just dev`'s cert-mint pass also produces the JWT trust root.
   - `iotctl plugin install`'s post-install hook reads the freshly-written `nats.nkey` + `acl.json`, mints a user JWT against the account seed (when `IOT_NATS_ACCOUNT_SEED` is set), and writes `nats.creds` next to them.
   - `deploy/compose/nats/nats.conf` switched from `accounts { ... } no_auth_user: dev` to `include "certs/resolver.conf"` — the operator pubkey + memory-resolved account JWT are loaded from the generated snippet. No more shared `dev` user.
   - `iot_bus::Config` gained a `creds_path` option (driven by `IOT_NATS_CREDS` env var); `Bus::connect` calls `async_nats::ConnectOptions::credentials_file` when set, falling back to mTLS-only when unset.

The post-supersession dev path is therefore: `mint.sh` produces both mTLS bundle + JWT trust root → `just dev` boots the compose stack with the broker in operator-JWT mode → `iotctl plugin install` mints a per-plugin user JWT for each plugin → the runtime connects with mTLS + JWT.

The two-step retirement (crypto then wiring) is reflected in the M5 plan's M5a/M5b split — a pattern the M4 retro flagged (don't ship "scaffolds claiming to be plugins") and one we deliberately repeated for honesty rather than backdating the ADR closure.

### User JWT expiry + host-side rotation (Bucket 1 audit H1, post-`v0.5.0-m5a`)

The first-cut M5a minter emitted user JWTs with no `exp` claim — leaked `nats.creds` files were valid for the lifetime of the issuing account keypair (months / years). The Bucket 1 audit closure adds:

- `iot_bus::jwt::issue_user_jwt` always populates `exp = iat + validity_seconds` (default 24 h) and a random `jti` (ULID, sets up the option for a later revocation-list ADR without committing the impl yet).
- `iot_bus::jwt::verify_user_jwt` enforces `exp` against a caller-supplied `now`. Tokens with `exp == 0` (already-on-disk legacy creds from the M5a path) keep verifying so the upgrade is non-breaking.
- `iotctl nats mint-user --validity-seconds <N>` and `iotctl plugin install --validity-seconds <N>` (env: `IOT_NATS_CREDS_VALIDITY`) plumb the lifetime end-to-end. The install path writes a `nats.creds.expiry` sidecar carrying the unix-seconds expiry, so the host-side refresh poller can decide when to re-mint without parsing the JWT body.
- `iot_plugin_host::creds_refresh` is a periodic task (default 60 s poll) that walks every plugin install dir, reads the expiry sidecar, and re-mints fresh creds (same nkey, same ACL, new JWT) when within 1 h of expiry. Configured via `nats_account_seed` in the host config; absent = no rotation, install-time validity holds. The refreshed file is rewritten in place; the plugin's next reconnect — organic from a broker-side eviction, or via the supervisor's restart loop — picks it up. There is no plugin-side "reload creds" signal yet (a future ABI question if and when expiry windows tighten enough that brokers evict before reconnect).

**Operator/account JWT lifetime stays static.** The trust root the broker preloads from `resolver.conf` is rotated only via `iotctl nats bootstrap --force`, which is destructive (invalidates every previously-minted user JWT). That's the right behaviour for a single-host model: an automatic operator-key rotation on a box where the operator+account+broker all share the same machine buys nothing and trades reliable uptime for moving parts. Cluster-wide synchronised expiry windows are out of scope until there is a cluster.

Distributed revocation (CRL-style) is also out of scope; the `jti` claim is the seam a future ADR can hook into when there's a real revocation event to handle. Until then, the rotation cadence + the destructive `bootstrap --force` are sufficient.
