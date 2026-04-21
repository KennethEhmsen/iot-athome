# ADR-0011: Dev-Mode Bus Auth — mTLS-only, Single IOT Account

- **Status:** Accepted
- **Date:** 2026-04-21
- **Deciders:** IoT-AtHome core team
- **Context milestone:** shipped in M1; replaced during M2 plugin installer work

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
