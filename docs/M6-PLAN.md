# M6 Plan — Hardening + Certification Prep

**Starts:** post-`v0.6.0-m5b` · **Target duration:** 4 weeks
**Anchor ADRs:** [0006](adr/0006-signing-key-management.md), [0008](adr/0008-error-handling.md), [0009](adr/0009-logging-and-tracing.md)
**Outcome:** `v1.0.0` — first GA release.

## Why M6 exists

The functional surface stabilised at M5b: plugin runtime, rule
engine, voice loop, history, panel UI, decentralized broker auth.
M6 doesn't add features — it makes the existing surface
**defensible** under external review. The deliverables map to the
external standards the project aims to clear:

* **ETSI EN 303 645** — IoT consumer cybersecurity baseline (EU
  reference standard, voluntary but increasingly required by
  member states).
* **OWASP ASVS L2** — verification standard for the gateway HTTP
  + WebSocket surface.
* **SLSA L3** — supply-chain attestation level (signed
  provenance + isolated build + non-falsifiable build pipeline).

None of these are external-audit pass/fail at M6 — that's the
post-M6 commercial cert arc. M6 is "we'd survive an audit
tomorrow." The pen test catches what we missed; the
checklist walk-throughs document where we already comply; the
SLSA / repro work hardens the release pipeline that M2 set up
with `continue-on-error: true`.

## Acceptance criterion

> From a fresh clone with no operator config: an external auditor
> running the ETSI EN 303 645 checklist fills in every row.
> ASVS L2 verification produces a written report citing
> evidence per requirement. The release pipeline produces SLSA
> v1.0 provenance attestations + reproducibility byte-match
> assertions on every artifact, with the workflow gates set to
> `continue-on-error: false`. A plain-text vulnerability disclosure
> path exists at `https://<host>/.well-known/security.txt` + a
> repo-root `SECURITY.md`. A pen-test partner has filed a written
> report; every High/Medium finding has an issue ticket with an
> owner. `v1.0.0` signs + Rekor-logs cleanly. Tag-time CI billing
> reset to fully-paid (the M5a workaround retires).

## Week-by-week

### W1 — Vulnerability disclosure + SLSA hard-gate

The cheap-but-load-bearing infra. Done first so external
researchers can already file disclosures while the rest of M6 runs.

- [ ] **`SECURITY.md` at repo root** — disclosure policy, 90-day
  embargo, PGP key fingerprint, primary contact email. Mirrors
  `github.com/<repo>/security/policy` UX. Lands at this commit
  (M6 W0 prep): scaffolds the doc with placeholder contact.
- [ ] **`/.well-known/security.txt`** — RFC 9116 format, served
  by the gateway as a static endpoint at `/.well-known/security.txt`
  (no auth, no CORS, plain text). Encoded fields:
  `Contact:`, `Expires:`, `Encryption:`, `Preferred-Languages:`,
  `Canonical:`. Lands at this commit (M6 W0 prep): the policy file
  exists; the gateway endpoint comes in W1.
- [ ] **PGP key generation + publication** — operator-side
  ceremony; the public key uploads to keys.openpgp.org +
  appears in the security.txt's `Encryption:` field.
- [ ] **SLSA hard-gate** — flip `continue-on-error: true` →
  `false` in `.github/workflows/ci.yml`'s SLSA provenance step.
  **BLOCKED** on a product decision: at the v0.6.0-m5b release-
  ceremony attempt, the attestation API rejected the call with
  "Feature not available for user-owned private repositories"
  (run 25005026705). To unblock, pick one:
  * `gh repo edit KennethEhmsen/iot-athome --visibility public`
    — flips the repo to public; attestation API works
    immediately; W1 closes by removing the
    `continue-on-error: true` line.
  * Upgrade the GitHub plan to one with private-repo
    attestations (Team / Enterprise tier).
  The M2-era assumption "the repo will go public in M5a" was
  incorrect; the repo stayed private. Until either choice
  above, this step ships as a warning, with cosign + Rekor
  carrying the signature story.
- [ ] **Reproducibility byte-match** — the M2 reproducibility job
  builds twice + diffoscope-diffs; M6 adds a hard `exit 1` if
  diffoscope reports any difference. Today the job's `success`
  exit gates only on diffoscope's run-success, not its diff
  cleanliness.

### W2 — ETSI EN 303 645 walk-through

The standard has 13 provisions + 33 sub-requirements. Each gets
a row in `docs/security/etsi-303-645.md` with: requirement,
evidence (file path / commit / test), status (compliant / partial
/ N/A / open). Most rows already have evidence from M1-M5b work;
M6 W2 is documentation, not implementation.

Provisions targeted:
* **5.1 No universal default passwords** — covered by
  ADR-0011's per-plugin nkeys + the M5a W3 mosquitto ACL gen.
* **5.2 Implement a means to manage reports of vulnerabilities**
  — the SECURITY.md + security.txt from W1.
* **5.3 Keep software updated** — the cosign-signed release
  pipeline + the SBOM CVE gate at install time
  (`iotctl plugin install`).
* **5.4 Securely store sensitive security parameters** — the
  M5a-Bucket-2 Windows ACL fix on plugin-secret files +
  POSIX 0600.
* **5.5 Communicate securely** — mTLS everywhere; documented
  rejection-on-cert-rotate test (W2 deliverable).
* **5.6 Minimize exposed attack surfaces** — the closed-domain
  voice grammar (M5b W2), the bus subject taxonomy gate, the
  WASM capability model.
* **5.7 Ensure software integrity** — cosign blob verification
  on plugin install + Rekor lookup (M2 W3).
* **5.8 Ensure that personal data is secure** — history is
  opt-in (`IOT_TIMESCALE_URL`); deletion via `iotctl history
  prune` (W2 deliverable).
* **5.9 Make systems resilient to outages** — the supervisor
  back-off + DLQ.
* **5.10 Examine system telemetry data** — the JCS-canonical
  audit chain.
* **5.11 Make it easy for users to delete user data** —
  `iotctl history prune --device-id <id>` (W2 deliverable).
* **5.12 Make installation and maintenance of devices easy** —
  `just dev` + `iot-voice send` smoke tests + the LOCAL-CI doc.
* **5.13 Validate input data** — the manifest-derived ACL
  enforcement + CEL source size cap + intent grammar rejection.

Open W2 deliverables (the rows that need NEW evidence):
* `docs/security/cert-rotation-test.md` + an integration test that
  rotates the dev CA mid-run and asserts client reconnects.
* `iotctl history prune --device-id <id>` subcommand.
* `iotctl history prune --before <rfc3339>` (already exists?
  audit during W2; if not, ship).

### W3 — OWASP ASVS L2 verification

The gateway is the only HTTP/WS surface that faces an
untrusted-or-semi-trusted network. ASVS L2 verification scopes:

* **V1 (Architecture)** — the existing ADR set covers most of
  this. W3 adds a written threat model document
  (`docs/security/threat-model.md`) walking the gateway's
  data-flow diagram + STRIDE per component.
* **V2 (Authentication)** — OIDC bearer + per-plugin NATS user
  JWTs (M5a W1, Bucket 2 H1). Verify the W3d OIDC validation
  rejects `none`-alg JWTs, expired tokens, wrong-audience tokens.
* **V3 (Session management)** — the gateway's session model is
  per-request (stateless JWT). No server-side session store to
  invalidate.
* **V4 (Access control)** — the M2 capability model on the
  plugin host, the rule engine's idempotency-keyed firing, the
  history endpoint's auth gate.
* **V5 (Input validation)** — JSON-schema'd bus subjects,
  manifest jsonschema, CEL source-size cap (Bucket 2 H2).
* **V7 (Errors and logging)** — JCS audit chain (M3 W1.4) +
  ADR-0008 error taxonomy.
* **V8 (Data protection)** — TimescaleDB encryption-at-rest
  (W3 deliverable: document the operator's responsibility +
  recommended Postgres pgcrypto extension).
* **V9 (Communication)** — mTLS + TLS-1.3-only enforcement
  (verify cipher list).
* **V14 (Configuration)** — the `iot-config` jsonschema + cargo
  deny + dependabot wiring.

W3 produces `docs/security/asvs-l2.md` — the per-requirement
evidence matrix, similar to W2's ETSI doc.

### W4 — Pen test + final tag

- [ ] **External pen test partner engagement.** Two-week scoped
  engagement; SOW covers the gateway HTTP/WS surface, the panel
  PWA, the plugin-install signature path, the bus subject
  authorisation. Candidate partners (chosen at W4 start):
  * Trail of Bits (Rust + supply-chain heritage)
  * NCC Group (consumer-IoT track record)
  * Cure53 (web-PWA + auth specialty)
- [ ] **Findings remediation.** Every High / Medium finding
  becomes a tracked issue with an owner + a milestone. Lows go
  to a follow-up triage pass.
- [ ] **Reproducibility rehearsal.** Two key-holders run the
  release ceremony from independent boxes; signatures match.
  Rekor entries cross-verify.
- [ ] **TUF metadata root rotation rehearsal** — paper exercise,
  not live. `docs/security/tuf-rotation.md` walks through the
  process so the next rotation doesn't surprise anyone.
- [ ] `v1.0.0` tag.

## Risk register

| Risk | Mitigation |
|------|------------|
| Pen-test partner availability slips W4 | Engage Q2 ahead of M6 start. SOW signed before W1. |
| ETSI 303 645 row turns up an unfixable gap | Surface during W2 walk; either fix in M6.5 or document as a known limitation in `v1.0.0`'s release notes. |
| SLSA hard-gate trips on a transient billing block (the M5a workaround scenario) | Pre-fund GH Actions billing for the entire M6 window before W4. |
| Repro byte-match fails on Windows (cargo + MSVC link timestamps) | M2 reproducibility job runs Linux-only; document Linux as the reproducibility-target platform. Windows builds are bit-for-bit-not-verified. |
| Disclosure email spam | Plus-tag the `security@` address; route to a low-volume mailbox. |

## Out of scope (post-M6)

* **Matter certification.** CSA membership + test-lab partner +
  6-month timeline. M6's work makes us *credible* for the
  certification; the paperwork itself is its own arc.
* **Multi-home / tenancy (CRDT federation).** Design open;
  not user-requested.
* **EU Cyber Resilience Act compliance audit** (effective 2027).
  The SBOM + disclosure posture should already satisfy; the
  formal audit is a 2026Q4 task.
* **LLM-as-agent for automation composition.** Needs an extended
  capability model that's its own design effort.

## Dependencies on M6 from later milestones

* **Plugin marketplace** — needs M6 W4 signing infrastructure +
  the public disclosure path. Marketplace lands as M7+.
* **Public release / app-store-style distribution** — `v1.0.0`
  is the prerequisite. The post-tag commercial track starts only
  after M6 closes.
