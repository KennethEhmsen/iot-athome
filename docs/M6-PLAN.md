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
- [x] **SLSA hard-gate** ✅ closed post-v0.6.0-m5b. The repo
  flipped to public on 2026-04-27, which unblocked GitHub's
  attestation API (it refuses user-owned private repos).
  `continue-on-error: true` is now removed from the SLSA-
  provenance step in `.github/workflows/ci.yml`. The next tag
  bump exercises the hard-gate live; if the attestation step
  fails, `sign` fails, and `publish` (which depends on it)
  doesn't fire. ADR-0006's "M6 hard-gate" item closes.

  Reverse-path: if the repo flips back to private (or moves to
  a plan without private-repo attestations), restore
  `continue-on-error: true` and reopen this entry.
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

W2 deliverables status:
* [x] `docs/security/etsi-303-645.md` — 13-row evidence table
  with provision-by-provision citations, coverage summary,
  reverse-traceability map. ✅ Shipped W2.
* [x] `iotctl history prune --device-id <id>` subcommand. ✅
  Shipped W2.
* [x] `iotctl history prune --before <rfc3339>` flag. ✅ Shipped
  W2. Both filters compose AND-wise via
  `prune_for_device(device_id, cutoff: Option)` on the
  underlying `HistoryStore`.
* [x] `docs/security/cert-rotation-test.md` — threat model,
  operator runbooks (routine + incident), test plan,
  unit-pinning. ✅ Shipped W2.5.
* [ ] Live broker integration test that rotates the dev CA
  mid-run. Stubbed in cert-rotation-test.md; the
  testcontainers+cert plumbing lands in a follow-up.

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

W3 deliverables status:
* [x] `docs/security/asvs-l2.md` — per-requirement evidence
  matrix covering V1-V14 (with V6 marked L3-only), coverage
  table (12 C / 1 P / 0 O across in-scope categories),
  TLS-at-rest operator runbook, reverse-traceability map.
  ✅ Shipped W3.
* [x] `docs/security/threat-model.md` — STRIDE walk on the
  gateway, plugin-host, bus, history, and audit-log
  components, with trust-gradient table, residual-risk
  callouts for the M6 W4 pen-test SOW, and accepted gaps
  (live cert reload, TLS-at-rest). ✅ Shipped W3.

### W4 — Pen test + final tag

- [x] **Statement of Work template** — `docs/security/pentest-sow.md`
  ✅ Shipped W4 prep. Pre-fill ready for the partner contract;
  partner-specific terms (price, schedule, named consultants)
  plug into § 9.
- [x] **Partner candidate matrix** — `docs/security/pentest-partners.md`
  ✅ Shipped W4 prep. Six-axis scoring against three named
  candidates (Trail of Bits, NCC Group, Cure53) plus open-pool
  alternatives. Recommendation: NCC Group for the balanced
  axis-weight fit; Trail of Bits as technical-depth backup;
  Cure53 if scope tightens to S1 + S2 only.
- [x] **Pre-pen-test self-audit checklist** —
  `docs/security/pre-pentest-checklist.md` ✅ Shipped W4 prep.
  10 sections covering code health, dep health, static
  analysis, dynamic surface, plugin install, bus + auth,
  audit log, CI integrity, runtime hardening, doc completeness.
  Maintainer signs off before sending the partner SOW.
- [x] **TUF metadata root rotation rehearsal** —
  `docs/security/tuf-rotation.md` ✅ Shipped W4 prep. Paper
  exercise; live TUF ships post-M6 with the plugin marketplace.
  Documents routine + incident rotation, disaster scenarios,
  test schedule.
- [ ] **External pen test partner engagement.** Two-week scoped
  engagement against the SOW above. Pending: RFQ to candidates,
  quote comparison, signature, kickoff.
- [ ] **Findings remediation.** Every High / Medium finding
  becomes a tracked issue with an owner + a milestone. Lows go
  to a follow-up triage pass.
- [ ] **Reproducibility rehearsal.** Two key-holders run the
  release ceremony from independent boxes; signatures match.
  Rekor entries cross-verify.
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
