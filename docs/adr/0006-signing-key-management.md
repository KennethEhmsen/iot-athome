# ADR-0006: Signing Key Management

- **Status:** Accepted
- **Date:** 2026-04-20

## Context

The platform's security posture depends on signed artifacts: core binaries, plugins, firmware images, ML models, TUF metadata. Key management determines whether "signed" means anything. Mistakes in this area are catastrophic and not undoable without a trust-reset for every deployed user.

We target **SLSA Level 3** provenance, **ETSI EN 303 645** alignment, and **TUF** for metadata distribution.

## Decision

### Key hierarchy

```
Offline Root (hardware-backed, air-gapped YubiKey, 5y rotation)
   |-- Release Signer (hardware-backed YubiKey, 1y rotation)
   |      |-- Core Binaries signing identity
   |      |-- Plugin signing identity
   |      |-- Firmware signing identity
   |      |-- Model registry signing identity
   |-- TUF Root (offline, signs timestamp/snapshot/targets key chain)
```

### Signing mechanisms by artifact class

| Artifact | Mechanism | Transparency |
|---|---|---|
| Core CI builds (every commit) | **Sigstore `cosign` keyless** (GitHub OIDC identity → Fulcio cert → Rekor entry) | Public transparency log |
| Release binaries (tagged) | cosign signing with Release Signer key | Rekor + TUF `targets.json` |
| First-party plugins | cosign signing with Plugin key | Rekor + plugin registry TUF root |
| Third-party plugins (marketplace) | cosign keyless OR long-lived author key, both recorded in Rekor | Rekor + per-author reputation record |
| Firmware images | cosign signing with Firmware key; ESP32 verifies via Secure Boot v2 public-key hash burned at provisioning | On-device verify only |
| ML models (base) | cosign signing with Model Registry key | Rekor |
| ML models (per-household fine-tunes) | Household's locally-generated keypair; signature stored with model; hub trusts only its own key | Local audit log |

### Dev vs prod keys

- **Dev:** ephemeral per-developer `.local` keys stored in `~/.iot-athome/devkeys/`. **Panel displays a loud "UNSIGNED BUILD — development only" banner.** Dev keys are never accepted on production release artifacts.
- **CI:** Sigstore keyless. No private key material in CI storage.
- **Release / signing ceremony:** YubiKeys held offline by ≥ 2 key-holders; signing requires co-presence (M-of-N via `cosign attest --key yubikey-slot-9c` + second quorum approval in release tool).
- **Prod (deployed hub):** holds no private keys. Only trusts the anchored public keys shipped with the release, rotated via TUF.

### Rotation

- **Root**: every 5 years, or immediately on compromise.
- **Release Signer**: annually, or immediately on compromise.
- **CI identity**: per-run (keyless).
- **TUF roles**: `timestamp` daily, `snapshot` weekly, `targets` on release, `root` annually (rotation-by-threshold).

### Revocation

- TUF `targets.json` removes the bad target; hub's next poll refuses to run it.
- Rekor cross-check: the hub verifies on every install that the artifact has a valid Rekor entry and that the entry's identity matches expectations.
- User-visible revocation notice surfaces in the panel within one TUF poll interval (default 15 min).

### Dev/prod gate

- Production builds refuse to load artifacts signed with dev keys (enforced in the signature verification code path, not in configuration).

## Consequences

- First-week cost: build a **toy TUF root** and a YubiKey signing ceremony doc. Budget: 1 day during M1 W1 (per plan §7).
- All release tags are **gated on** ceremony completion. This is a feature.
- Losing a YubiKey is survivable via the quorum model; losing multiple is a user-facing trust-reset.
- Supply-chain provenance is queryable end-to-end: every byte running on a hub has a chain back to an OIDC identity or a named key-holder ceremony.
