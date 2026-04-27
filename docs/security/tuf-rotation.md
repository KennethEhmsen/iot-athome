# TUF metadata root rotation — paper rehearsal

**Status:** Paper exercise per `docs/M6-PLAN.md` § W4 — not a
live rotation. The project doesn't run a TUF metadata server
in production yet (post-M6 deliverable when plugin marketplace
foundations land); this doc walks through the procedure so the
first real rotation doesn't surprise anyone.
**Last updated:** M6 W4 (this commit).

---

## Why TUF

The Update Framework (TUF) is the spec for delivering signed
updates without trusting any single signing key forever.
Where cosign + Rekor (which we use today) prove that *a
specific blob was signed by a specific key at a specific
time*, TUF provides the *key-rotation framework* — how the
trust anchors themselves get updated when keys expire or
compromise.

For our scope:

* **Today (M6 W4):** No live TUF. Plugin signatures use
  cosign keyless OIDC against pinned trust pubkeys
  (`docs/adr/0006-signing-key-management.md`). Rotation
  happens at the OIDC IdP layer, not via TUF metadata.
* **Future (M7+ plugin marketplace):** The marketplace
  needs TUF for the plugin distribution channel — a remote
  registry of signed plugins served to operators who don't
  individually verify each blob. TUF's per-role keys
  (root, targets, snapshot, timestamp) provide the
  defence-in-depth.

This rehearsal documents the procedure. When the plugin
marketplace ships, this doc becomes the runbook.

## TUF role overview

For the marketplace, we'd run four key roles:

| Role | Purpose | Rotation cadence |
|------|---------|------------------|
| **Root** | The trust anchor. Signs delegations to other roles. Compromised root = compromised everything. | Yearly, or on suspected compromise |
| **Targets** | Signs the actual plugin payload metadata (cosign-signed plugin bundle hashes). | Per-release, automated |
| **Snapshot** | Signs the manifest of all targets metadata, preventing mix-and-match attacks. | Per-release, automated |
| **Timestamp** | Signs a timestamp + the current snapshot hash, preventing freeze attacks. | Hourly, automated |

The Root role is the only one that requires a manual
rotation procedure with multiple key-holders. The other
three rotate automatically as part of the release pipeline.

## Pre-conditions for the rehearsal

* Two key-holders (operator + co-owner). Single-key-holder
  setup is documented as the dev-mode path; production
  requires at least 2-of-3 to prevent single-point compromise.
* Each key-holder has:
  - A YubiKey 5 series (or equivalent FIPS 140-2 token)
    for storing their personal Root signing key.
  - A backup of their key on offline media (USB stick in
    a fireproof safe, or paper-based BIP39 mnemonic).
* The current Root metadata file (`root.json`) is published
  at a stable URL — the project's `/.well-known/tuf/root.json`
  on the gateway, mirrored on a public CDN.
* Operator has the documented signing ceremony procedure
  (this file).

## Routine rotation procedure (yearly)

**Goal:** rotate Root keys without interrupting plugin
distribution.

The Root metadata's `version` field increments on every
rotation. Clients verify the chain root.json(N) →
root.json(N+1) → ... by ensuring each new root is signed by
**both** the old key set + the new key set (transition
signature). This is what keeps the trust-chain unbroken
across rotations.

### Step 1 — Generate new keys

Each key-holder, on their own machine, runs:

```sh
# YubiKey 5 series, slot 9c (digital signature). PIN required.
ykman piv keys generate 9c --algorithm ECCP256 \
    --pin-policy ONCE --touch-policy ALWAYS \
    new-root-key.pub

# Export public key for inclusion in root.json.
ykman piv certificates export 9c new-root-key.crt
```

The private key never leaves the YubiKey. The public key +
certificate gets emailed (PGP-encrypted) to the rotation
coordinator.

### Step 2 — Build the new `root.json`

The coordinator (operator role) builds the new root metadata
via `tuf-rs` CLI:

```sh
# Pre-conditions: tuf-rs installed (cargo install tuf), all
# key-holders' new pubkeys in ./new-keys/.
tuf init --keys ./new-keys/*.pub --threshold 2 \
    --output draft-root.json
```

Open `draft-root.json` in a text editor; the operator verifies:

* `version` is current `+1`.
* `expires` is 1 year in the future.
* `keys` contains every key-holder's new pubkey (and only
  those).
* `roles.root.threshold` matches the policy (e.g. `2` for
  2-of-3 keys required to sign).
* `roles.targets.threshold` etc. match.

### Step 3 — Both old + new keys sign

This is the load-bearing step. The new `root.json` must be
signed by:

* All required old keys (transition signature, proves
  authorisation by the previous trust anchor).
* All required new keys (proves the new keys are now active).

Each key-holder, with both their old and new YubiKeys
available:

```sh
# Sign with old key (transition signature).
tuf sign --root draft-root.json --key-slot 9c \
    --key-id <old-key-fingerprint> \
    --signature-output sig-old-<keyholder>.sig

# Sign with new key.
tuf sign --root draft-root.json --key-slot 9c \
    --key-id <new-key-fingerprint> \
    --signature-output sig-new-<keyholder>.sig
```

Signatures are emailed (in clear; signatures aren't secret)
to the coordinator.

### Step 4 — Aggregate + publish

```sh
tuf aggregate --root draft-root.json \
    --signatures ./sigs/*.sig \
    --output root-v(N+1).json

# Verify the chain locally before publishing.
tuf verify-chain ./root-v(N).json ./root-v(N+1).json
```

If `verify-chain` succeeds, publish:

```sh
# Replace the served root.json with the new version.
cp root-v(N+1).json /var/lib/tuf/root.json
# Reload the gateway's static-asset path (or restart Envoy
# if that's the serving layer).
systemctl reload iot-gateway
```

Clients that fetch `root.json` now get the new version. The
old root keys are formally retired; their signatures on
future targets metadata are no longer valid.

### Step 5 — Audit

```sh
iotctl audit emit --type tuf.rotation.complete \
    --raw "version=$(N+1) keyholders=<comma-list>"
```

The audit-chain entry is the operator-side record of the
rotation event.

## Incident rotation (compromise-driven)

The procedure is the same as the routine path **except**:

* The compromised key-holder generates the new key
  immediately (same Step 1).
* All other key-holders co-sign the new root via the
  transition signature.
* The compromised key-holder's old key is explicitly
  revoked: the old key's pubkey is still listed in the new
  `root.json`'s `keys` map but is removed from
  `roles.root.keyids`. This is TUF's "revoke without
  forgetting" mechanism — verifiers reject signatures from
  the revoked key going forward, but the cryptographic
  evidence of past activity (on prior root.jsons) remains
  intact.
* Audit emits `tuf.rotation.incident` with the compromised
  key-holder's id + a reason field.

## Disaster scenarios

### All keys lost simultaneously

If every key-holder loses their hardware token (fire,
theft, etc.) and no offline backup is recoverable:

* The trust chain is **permanently broken**. Clients trusting
  the lost root keys will refuse all future updates.
* Recovery requires every client to fetch a fresh
  `root.json` from a side-channel (e.g., a manual download
  from the project's GitHub releases, signed by the
  project's regular release key — which is *not* a TUF
  root key).
* This is why the threshold is 2-of-3: a single key-holder's
  loss is recoverable without breaking the chain.

### Root metadata file lost

If `root.json` is deleted from the serving path but the
keys are intact:

* Re-publish the latest `root.json` from any key-holder's
  local copy.
* Verify-chain against the previous version (which clients
  cached) before publishing.
* No rotation needed; the keys themselves are unchanged.

### Targets / snapshot / timestamp key compromise

These are automated keys; rotation is inline with the
release pipeline:

* Generate a new automated key.
* Co-sign a new `targets.json` (or `snapshot.json` / 
  `timestamp.json`) with the old + new automated keys plus
  one Root signature delegating to the new key.
* Publish.

The Root role doesn't itself rotate in this scenario;
just the delegation chain.

## Test schedule

Rehearsal cadence (when live TUF ships in M7+):

* **Quarterly:** dry-run the routine rotation against a
  shadow `root.json` (not the live one). Verify
  `tuf verify-chain` succeeds end-to-end.
* **Annually:** real rotation against the live `root.json`.
  Audit-log entry; release notes mention the version bump.

## Out-of-scope clarifications

* **Cosign keys today.** This doc is forward-looking. The
  current cosign keyless flow (M2 W3 / ADR-0006) doesn't
  use TUF; the trust anchor is the GitHub OIDC issuer +
  Rekor's append-only log.
* **Plugin author keys.** Each plugin author signs their
  own bundle with their own cosign key; TUF root rotation
  doesn't affect their per-plugin signing keys.
* **Hub-binary rotation.** The hub itself ships from
  GitHub Releases with cosign keyless; no TUF dependency.

## Reverse-mapping

This doc is cited from:
* `docs/security/threat-model.md` — out-of-scope items
* `docs/M6-PLAN.md` § W4 — paper exercise deliverable
