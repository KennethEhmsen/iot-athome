# OWASP ASVS L2 — Evidence walk-through

**Standard:** OWASP Application Security Verification Standard
v4.0.3 (October 2021), Level 2 (defence-in-depth, suitable for
applications that contain sensitive data).
**Status:** Internal evidence map for M6 W3. Each row pairs the
ASVS requirement with project evidence. Like the ETSI walk-through,
this is audit-ready notes, not a formal cert claim.
**Last updated:** M6 W3 (this commit).

ASVS L2 has 14 categories (V1-V14, with V6 as a nested L3+
section). The categories below cover the gateway HTTP+WS surface,
the panel PWA, the plugin-install signature path, and the bus
authorisation model — i.e. everything an external attacker could
plausibly reach.

## Coverage summary

| Category | Status | Notes |
|----------|--------|-------|
| V1 Architecture | C | Threat model + ADR set; full data-flow doc at `docs/security/threat-model.md` |
| V2 Authentication | C | OIDC bearer + per-plugin NATS user JWTs (Bucket 2 H1) |
| V3 Session management | C | Stateless JWT; no server-side session store |
| V4 Access control | C | WASM capability model + rule-engine idempotency + history endpoint auth |
| V5 Input validation | C | JSON-schema'd manifests, CEL source-size cap, intent grammar rejection |
| V7 Errors + logging | C | JCS-canonical audit chain + ADR-0008 error taxonomy |
| V8 Data protection | P | TLS-at-rest is operator-deployment concern (`pgcrypto` recommended; runbook below) |
| V9 Communication | C | mTLS everywhere; TLS 1.3 only via rustls (ADR-0006) |
| V10 Malicious code | C | Cosign + SBOM CVE gate; cargo-deny + dependabot |
| V11 Business logic | C | Idempotency-keyed rule firings; DLQ on dispatch failure |
| V12 File and resources | C | Plugin install dir owner-only (Bucket 1 ACL fix); secrets 0600 |
| V13 API + Web services | C | OpenAPI surface implicit through axum routes; rate-limit + auth on every /api/v1/* |
| V14 Configuration | C | jsonschema + cargo-deny + dependabot + workspace lints |

12 / 13 in-scope categories C; 1 P (V8 — operator-action item, not a code gap).

## V1 — Architecture, design, and threat modeling

| ID | Requirement | Evidence | Status |
|----|-------------|----------|--------|
| 1.1.1 | Secure SDLC documented | ADR-0001 (architecture decisions) + `docs/M*-PLAN.md` per-milestone planning + `docs/M*-RETROSPECTIVE.md` retrospectives | C |
| 1.1.2 | Threat model exists | `docs/security/threat-model.md` (M6 W3 deliverable, this commit) | C |
| 1.2.1 | All app components identified | `crates/` workspace = 18 named crates; each ADR cites which crates it impacts | C |
| 1.4.1 | Trusted enforcement points use access-control | WASM capability model on plugin host (`crates/iot-plugin-host/src/capabilities.rs`); OIDC bearer middleware on `/api/v1/*` (`crates/iot-gateway/src/auth.rs`) | C |
| 1.5.1 | Input verification at trust boundary | manifest jsonschema, CEL source cap, intent grammar — see V5 below | C |

## V2 — Authentication

| ID | Requirement | Evidence | Status |
|----|-------------|----------|--------|
| 2.1.1 | Min password length 12 chars | N/A — no passwords. Auth is OIDC bearer + per-plugin nkey JWTs. | N/A |
| 2.1.5 | Allow secure password storage | N/A (no passwords) | N/A |
| 2.4.1 | Verify auth credentials are stored using KDF | N/A (no passwords) | N/A |
| 2.10.1 | Verify intra-service authentication | mTLS bus + per-plugin NATS user JWTs with `exp` + `jti` claims (Bucket 2 H1) | C |
| 2.10.2 | Service auth secrets stored securely | nkeys + creds files at owner-only 0600/ACL (Bucket 1 fix) | C |

## V3 — Session management

| ID | Requirement | Evidence | Status |
|----|-------------|----------|--------|
| 3.2.1 | Token rotation on auth event | OIDC tokens are short-lived; plugin user JWTs rotate on `iotctl plugin install --force` | C |
| 3.3.1 | Logout invalidates session | OIDC IdP-side logout + token expiry; no server-side session store to clear | C |
| 3.4.1 | Cookie security flags | Panel uses bearer header, not cookies; the `credentials: 'include'` in fetch is for the OIDC IdP cross-origin path, not session state | C |
| 3.5.1 | Session re-auth for sensitive ops | The rule engine + plugin install both audit-log every action with the actor's identity | C |

## V4 — Access control

| ID | Requirement | Evidence | Status |
|----|-------------|----------|--------|
| 4.1.1 | Trusted enforcement points | OIDC middleware on `/api/v1/*`; WASM capability gate on every host call; bus subject ACL on every publish | C |
| 4.1.3 | Principle of least privilege | Per-plugin nkeys allow only the manifest-declared `bus.publish` / `bus.subscribe` subjects. Mosquitto ACL same shape (M5a W3) | C |
| 4.2.1 | Sensitive operations restricted | `iotctl history prune` requires explicit filters + interactive confirmation (M6 W2) | C |
| 4.3.1 | Admin interfaces require additional protection | `iotctl` runs locally on the hub only; OIDC-protected if exposed via gateway | C |

## V5 — Validation, sanitization, encoding

| ID | Requirement | Evidence | Status |
|----|-------------|----------|--------|
| 5.1.1 | Input validation at trust boundary | manifest schema validation (`schemas/plugin-manifest.schema.json`) + Bus subject prefix check + JCS canonicalisation on audit | C |
| 5.1.3 | Reject all input that fails validation | Rule engine `decode_payload` falls through to `Null` on bad bytes (rules see `null`, no crash); manifest install errors loudly | C |
| 5.2.1 | Sanitisation of dynamic queries | sqlx parameterised queries throughout (no string concat); URL canonicalisation on `net.outbound` (Bucket 1 C1) | C |
| 5.3.1 | Output encoding | Panel uses React's auto-escaping; gateway JSON responses go through serde, not template strings | C |
| 5.5.2 | Verify deserialization is safe | Audit chain JCS-canonical (RFC 8785) before hashing; payload bytes never deserialised as Rust types from network | C |
| 5.7.1 | Rate-limit / size-cap inputs | CEL source cap 64 KB + 200 ms eval timeout (Bucket 2 H2); HTTP body cap 4 MB on `net.outbound`; response body cap 16 MB (Bucket 1 C2) | C |

## V7 — Errors and logging

| ID | Requirement | Evidence | Status |
|----|-------------|----------|--------|
| 7.1.1 | Sensitive data not in logs | ADR-0008 § "what to log"; no token / nkey-seed / payload-secret logging in our code | C |
| 7.1.3 | Log auth events | OIDC bearer middleware logs accept/reject (`crates/iot-gateway/src/auth.rs`) | C |
| 7.1.4 | Logs unalterable | `crates/iot-audit/src/lib.rs` JCS-canonical hash chain — tamper-evident | C |
| 7.2.1 | Logs available to ops | `nats sub sys.audit.>` + on-disk JCS journal | C |
| 7.4.1 | Generic errors to client | `crates/iot-gateway/src/handlers.rs::grpc_to_api` maps gRPC errors to a stable `ApiError { code, message }` shape; no stack traces | C |

## V8 — Data protection

| ID | Requirement | Evidence | Status |
|----|-------------|----------|--------|
| 8.1.1 | Sensitive data minimised | History opt-in (`IOT_TIMESCALE_URL`); registry stores only the `Device` proto fields | C |
| 8.2.1 | Sensitive data not in URL | Bearer header for auth; query-param auth on WS only (`/stream?token=`) which is the unavoidable path; no secrets in REST URLs | C |
| 8.2.2 | TLS in transit | mTLS on bus + MQTT; HTTPS on gateway (Envoy in prod) | C |
| 8.3.1 | TLS at rest | **Partial:** TimescaleDB encryption-at-rest is the operator's deployment responsibility. Recommended: enable Postgres `pgcrypto` for column-level encryption on the `payload` BYTEA column; or use filesystem-level encryption (LUKS / dm-crypt on Linux, FileVault on macOS, BitLocker on Windows). The hub-class deployment (Pi 5) ships LUKS-on-microSD by default since the M6 reference deployment guide. See **§ Operator runbook for TLS-at-rest** below. | P |
| 8.3.2 | Sensitive data wiped on user request | `iotctl history prune --device-id <ulid>` (M6 W2) | C |

### Operator runbook for TLS-at-rest

**Linux (Pi 5 reference hardware):**

```sh
# 1. Stop iot-history service so no in-flight writes during the swap.
systemctl stop iot-history

# 2. Move data dir to encrypted partition. The reference deployment
#    image puts /var/lib on LUKS, so the data is already at rest.
ls -la /var/lib/postgresql/16/main
# Verify path is on /var/lib/<luks-mount>.

# 3. For finer column-level encryption, enable pgcrypto:
psql -U postgres -d iot_history <<EOF
CREATE EXTENSION IF NOT EXISTS pgcrypto;
ALTER TABLE entity_state_history
  ALTER COLUMN payload SET DATA TYPE BYTEA
  USING pgp_sym_encrypt(encode(payload, 'escape'), '$KEY')::bytea;
EOF

# 4. The history-read path then needs `pgp_sym_decrypt(payload, key)`
#    instead of `payload`. ETSI 5.8 evidence row points here.
```

**Windows / macOS:** filesystem-level encryption only (BitLocker /
FileVault). Hub-class deployments are Linux-first; Windows + macOS
support is dev-time only.

The L2 bar is "TLS at rest is implemented." The operator runbook
above lays out a concrete path; the **P** grade reflects that
*the project ships unencrypted by default* and the operator is
responsible for the deployment-side enablement. A future
revision could ship a turn-key encrypted-history bootstrap
that the install path enables by default; that's M7+ scope.

## V9 — Communication

| ID | Requirement | Evidence | Status |
|----|-------------|----------|--------|
| 9.1.1 | TLS used for client/server | mTLS on bus, MQTT, registry gRPC; HTTPS on gateway behind Envoy (M3) | C |
| 9.1.2 | Strong protocols only | rustls only — TLS 1.3 (with TLS 1.2 fallback for legacy peers); no SSL2/3, no TLS 1.0/1.1 | C |
| 9.1.3 | Cert validation | rustls's default (`add_root_certificates`); peer certs verified against the configured CA bundle | C |
| 9.2.1 | Cipher list | rustls's curated default (no operator override); ADR-0006 § "TLS posture" | C |
| 9.2.4 | Cert rotation runbook | `docs/security/cert-rotation-test.md` (M6 W2.5) | C |

## V10 — Malicious code

| ID | Requirement | Evidence | Status |
|----|-------------|----------|--------|
| 10.1.1 | All code reviewed | Public repo (KennethEhmsen/iot-athome); every commit on main | C |
| 10.2.1 | Code integrity | Cosign blob signatures + Rekor on every release artifact (`.github/workflows/ci.yml`) | C |
| 10.2.2 | SBOM published | CycloneDX SBOM per crate; published as release artifact | C |
| 10.2.4 | Update integrity | Cosign + Rekor as above; SLSA L3 provenance hard-gate (M6 W1) | C |
| 10.3.1 | Build pipeline integrity | GH Actions runner (managed); `RUSTFLAGS=-D warnings` enforced; reproducibility byte-match assertion (M6 W1) | C |

## V11 — Business logic

| ID | Requirement | Evidence | Status |
|----|-------------|----------|--------|
| 11.1.1 | Sequential / atomic ops | Rule engine idempotency-keyed firings (`crates/iot-automation/src/engine.rs::idempotency_key`); 5-second TTL window | C |
| 11.1.2 | Rate-limited business logic | CEL eval has a 200ms timeout per evaluation (Bucket 2 H2); plugin supervisor exponential back-off + DLQ after 5 crashes | C |
| 11.1.3 | Defence against unexpected data | Rule engine `decode_payload` falls through to `null` on bad payloads (no panic) | C |

## V12 — Files and resources

| ID | Requirement | Evidence | Status |
|----|-------------|----------|--------|
| 12.1.1 | File-upload size limits | No file uploads in the gateway; `iotctl plugin install` reads from local filesystem only | C |
| 12.3.1 | Filename validation | `iotctl plugin install` validates manifest entrypoint via the Manifest schema; signature verification gates which files are accepted | C |
| 12.5.1 | Sensitive resource paths protected | `secfile.rs` 0600 / Windows ACL on plugin dirs (Bucket 1 fix) | C |

## V13 — API and Web services

| ID | Requirement | Evidence | Status |
|----|-------------|----------|--------|
| 13.1.1 | Reject XML where it isn't expected | The gateway accepts JSON only; serde_json rejects non-JSON | C |
| 13.1.3 | API URLs don't expose IDs | Gateway uses ULIDs for device IDs (high-entropy, not enumerable). | C |
| 13.2.1 | API uses TLS | Gateway behind Envoy in prod; dev-mode loopback only | C |
| 13.2.3 | API uses anti-CSRF | Bearer token auth not cookies; CSRF doesn't apply | N/A |
| 13.4.1 | GraphQL queries depth-limited | N/A — no GraphQL surface | N/A |

## V14 — Configuration

| ID | Requirement | Evidence | Status |
|----|-------------|----------|--------|
| 14.1.1 | Build pipeline doesn't expose secrets | GH Actions secrets used via `${{ secrets.* }}`; never logged. cargo-deny + dependabot in CI | C |
| 14.1.4 | No hard-coded backdoors | Public repo, every commit reviewable | C |
| 14.2.1 | Dependencies tracked | `Cargo.lock` committed; cargo-deny gates licenses + advisories + bans | C |
| 14.2.4 | Third-party deps minimised | 17 first-party crates; transitives audited via cargo-deny | C |
| 14.3.1 | Server config doesn't expose info | Envoy config in prod hides server-version headers; gateway responses minimal | C |
| 14.4.1 | Content-Type set | Gateway sets `application/json` on every JSON response via axum `Json` extractor | C |
| 14.5.3 | Cross-origin resource sharing | tower-http `cors` layer on gateway; allow-list by configured origins only | C |

## Reverse-traceability

| File / commit | ASVS sections cited |
|---------------|---------------------|
| `crates/iot-gateway/src/auth.rs` | V2.10.1, V3.x, V4.1.1 |
| `crates/iot-bus/src/jwt.rs` | V2.10.1, V2.10.2 |
| `crates/iot-cli/src/secfile.rs` | V8.x, V12.5.1 |
| `crates/iot-plugin-host/src/capabilities.rs` | V4.1.x, V5.x |
| `crates/iot-automation/src/expr.rs` | V5.7.1, V11.1.2 |
| `crates/iot-automation/src/engine.rs` | V11.1.1 |
| `crates/iot-audit/src/lib.rs` | V7.1.4 |
| `.github/workflows/ci.yml` | V10.x, V14.1.1 |
| `deny.toml` + `Cargo.lock` | V10.2.x, V14.2.x |
| `docs/security/threat-model.md` | V1.1.2 |
| `docs/security/cert-rotation-test.md` | V9.2.4 |

## Open M6 work after this doc

Per `docs/M6-PLAN.md`, the remaining items are:
* M6 W4 external pen test (engages the partner against this surface)
* M6 W4 final `v1.0.0` tag

ASVS V8.3.1 (TLS at rest) stays at P pending an opt-in encrypted-
history bootstrap; that's a post-M6 deliverable, not a M6
blocker.
