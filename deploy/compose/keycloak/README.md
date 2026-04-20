# Keycloak (dev realm)

Imports the `iotathome` realm on first boot via `--import-realm`.

## What's seeded

| Entity | Value |
|---|---|
| Realm | `iotathome` |
| URL | `http://localhost:8080/realms/iotathome` |
| Admin console | `http://localhost:8080/` (admin / admin) |
| Password policy | 12+ chars, 1 digit, 1 upper, 1 special, not-username |
| Realm roles | `iot-admin`, `iot-operator`, `iot-user`, `iot-guest` |
| Groups | `Administrators`, `Operators`, `Household` |
| Seeded users | `admin`, `operator`, `user` — all with password `DevPass!234` |

## OIDC clients

- **iot-panel** — public client (PWA), PKCE S256, redirect URIs cover `localhost:5173` (Vite dev), `localhost:8443` (Envoy), and `panel.iot.local`.
- **iot-gateway** — bearer-only resource server used by `iot-gateway` to verify tokens.
- **iot-cli** — public native client with PKCE; loopback redirect for `iotctl login`.

## Reset

```bash
just dev-nuke    # wipes the keycloak-data volume; next `just dev` re-imports
```

## What's NOT in here

Everything production. ADR-0006 describes the real identity hierarchy (YubiKey-held roots, Sigstore keyless CI signing, TUF metadata). This realm file is dev-only and never ships with a release.
