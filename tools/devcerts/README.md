# Dev Certificates

Local-only mTLS certs for the dev loop. See [ADR-0006](../../docs/adr/0006-signing-key-management.md) for the real signing hierarchy.

## Usage

```bash
just certs         # mint (idempotent)
just certs-reset   # wipe and re-mint
```

The script generates:

- `generated/ca/` — dev root CA.
- `generated/<component>/` — per-service key + cert (NATS, Mosquitto, Gateway, Registry, Envoy, Panel, client).

All SANs include `localhost` and `127.0.0.1` so dev tools work without `/etc/hosts` fuss. Component DNS names resolve via docker-compose's service names.

## What this is NOT

- **Not** production keys — ADR-0006 describes a YubiKey-held hierarchy with Sigstore + TUF. None of that runs here.
- **Not** acceptable in release artifacts — the dev/prod gate is enforced in the signature verification code.
- **Not** checked in — `generated/` is `.gitignore`d.

## Trust

Adding `generated/ca/ca.crt` to your OS trust store makes the dev loop frictionless (no curl `-k`, no browser warnings on the panel). Example:

- macOS: `sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain tools/devcerts/generated/ca/ca.crt`
- Linux (Debian/Ubuntu): copy to `/usr/local/share/ca-certificates/iot-athome-dev.crt` and `sudo update-ca-certificates`.
- Windows: `certutil -addstore -f ROOT tools\devcerts\generated\ca\ca.crt` (Admin PowerShell).

Remove the cert when you stop working on the project.
