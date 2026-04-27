# Threat model — gateway + plugin runtime + bus

**Status:** Internal threat model for M6 W3 (OWASP ASVS L2 V1.1.2
deliverable). Walks the project's data-flow diagram and applies
STRIDE per component. Like the ETSI + ASVS docs, this is
audit-ready notes, not a formal model that gets re-litigated
every sprint — the trust boundaries are stable across
M2-M6 and likely won't move materially before `v1.0.0`.

**Last updated:** M6 W3 (this commit).

## Scope

What this model covers:

* The gateway HTTP/WS surface (REST `/api/v1/*`, WebSocket
  `/stream`, `/.well-known/security.txt`).
* The panel PWA's auth flow + content-security boundary.
* The plugin install path (`iotctl plugin install` →
  cosign + SBOM gate → on-disk manifest + WASM bytes).
* The WASM plugin runtime + its capability model.
* The bus subject authorisation (NATS account/user JWT chain).
* The audit-log integrity chain.

What this model intentionally doesn't cover:

* Hardware-level attacks against the hub (cold-boot,
  side-channel, JTAG). Hub-class deployments aren't a hardware
  TPM target; physical access = compromise is acceptable for
  the home-IoT scope.
* Supply-chain attacks against the Rust toolchain itself.
  We trust rustup; mitigation is rustup's checksum verification.
* GitHub-platform compromise. We trust GH for source hosting,
  Actions, and Releases. Supply-chain via cosign + Rekor +
  SLSA provides post-compromise detection (Rekor entries are
  externally verifiable).

## Trust boundaries

```text
                 ╔═══════════════════════════╗
                 ║  PUBLIC NETWORK (untrust) ║
                 ║                           ║
                 ║  [browser: panel]   [pen-test scanner]
                 ║         │                 ║
═════════════════║═════════│═════════════════║═══════
                 ║         │                 ║   home LAN
   ╔═════════════║═════════▼═════════════════║═════════════╗
   ║             ║   [Envoy reverse proxy]   ║ TLS 1.3     ║
   ║             ╚═══════════│═══════════════╝             ║
   ║                         │                             ║
   ║       ╔═════════════════▼═══════════════════╗         ║
   ║       ║   iot-gateway (HTTP/WS frontend)    ║         ║
   ║       ║   • OIDC bearer middleware          ║         ║
   ║       ║   • REST /api/v1/* + /stream WS     ║         ║
   ║       ║   • /.well-known/security.txt       ║         ║
   ║       ╚═══╤═══════╤═══════════════╤═════════╝         ║
   ║           │       │               │                   ║
   ║      gRPC │       │ NATS (mTLS+JWT) (TimescaleDB)     ║
   ║           ▼       ▼                                   ║
   ║   [iot-registry] [NATS]    [TimescaleDB]              ║
   ║         │           │            ▲                    ║
   ║         │           │            │                    ║
   ║         └─SQLite────┘            │                    ║
   ║                     │            │                    ║
   ║      ┌──────────────┼────────────┘                    ║
   ║      │              │                                 ║
   ║      ▼              ▼                                 ║
   ║   [Mosquitto]    [iot-plugin-host]                    ║
   ║   (MQTT broker)     │                                 ║
   ║      ▲              ▼                                 ║
   ║      │       [WASM sandbox(es)]                       ║
   ║      │       • zigbee2mqtt-adapter                    ║
   ║      │       • sdr433-adapter                         ║
   ║      │       • matter-bridge (scaffold)               ║
   ║      │       • demo-echo                              ║
   ║      │       • weather-poller (scaffold)              ║
   ║      │              │                                 ║
   ║      └──────MQTT────┘                                 ║
   ║                                                       ║
   ║   ╔════════════════════════╗                          ║
   ║   ║ HUB (Pi 5 reference)   ║                          ║
   ║   ║ All processes above.   ║                          ║
   ║   ║ LUKS volume on disk.   ║                          ║
   ║   ╚════════════════════════╝                          ║
   ║                                                       ║
   ║   <==== LAN-only services beyond this point ====>     ║
   ║                                                       ║
   ║   [Z-Wave / Zigbee radios]   [SDR receiver]           ║
   ║   [Matter accessories]                                ║
   ╚═══════════════════════════════════════════════════════╝
```

Trust gradient (high→low):

1. The hub's filesystem under owner-only POSIX/ACL — secrets,
   plugin manifests, audit log.
2. mTLS-authenticated bus traffic — accepted from any process
   with a valid client cert + nkey JWT.
3. MQTT broker traffic — same shape, separate cred chain.
4. Plugin WASM bytecode after cosign+SBOM verification —
   trusted to make host-call requests within its declared
   capability ACL.
5. Bus messages from authenticated plugins — trusted at the
   subject prefix the plugin's ACL allows.
6. Panel HTTPS requests with valid OIDC bearer token — trusted
   for the user-scoped authorisation the IdP grants.
7. Untrusted: any inbound HTTPS without a bearer token; any
   public-LAN traffic; any data from the public network.

## STRIDE per component

### iot-gateway (HTTP/WS frontend)

**Spoofing:** OIDC bearer middleware (`crates/iot-gateway/src/auth.rs`)
verifies JWT signatures via the IdP's JWKS — RS256 only, `none`
alg rejected, expired tokens rejected, wrong-audience rejected.
WS uses `?token=` query param because browsers can't set
Authorization on WS handshakes — it's the same bearer, validated
the same way.
*Residual risk:* IdP key compromise → mitigation is the IdP's
operator-level concern, not ours.

**Tampering:** Outbound JSON goes through serde, no template
strings. Reproducibility byte-match on releases (M6 W1) detects
post-build tampering of the binary.

**Repudiation:** Every state-changing action (`upsert_device`,
`delete_device`, plugin install) emits an audit-log entry with
the actor's identity. Hash chain (RFC 8785 JCS) makes
deletion or reordering of entries detectable.

**Information disclosure:** Errors mapped to a stable
`ApiError { code, message }` shape (`grpc_to_api`); no stack
traces or internal paths leak. Cors policy allow-lists explicit
origins only.

**Denial of service:** History endpoint clamps `limit` at 5000.
CEL eval per-message capped at 200 ms. WAF/rate-limit at the
Envoy layer is the operator's responsibility for prod
deployments. Wildcard NATS replays bound at 5000 messages
(audit Bucket 2 M3).

**Elevation of privilege:** Capability checks gate every host
call; manifest is the source of truth. URL canonicalisation on
`net.outbound` (Bucket 1) closes scheme/userinfo bypass.

### iot-plugin-host (WASM runtime)

**Spoofing:** Per-plugin nkey JWTs with `exp` (90 day default,
Bucket 2 H1) and `jti` for revocation handle. Cosign blob
signature on the WASM bytes (with `--allow-unsigned` only as a
dev escape hatch, refused in prod builds per ADR-0006).

**Tampering:** Wasmtime sandbox + capability ACL. Plugin can't
call host fns outside its manifest's `bus.publish`,
`bus.subscribe`, `mqtt.subscribe`, `net.outbound` (Bucket 1
hardened) declarations. No filesystem access beyond the
read-only manifest dir.

**Repudiation:** Every host call audit-logged with `plugin_id`
+ subject + outcome. Capability denies log explicitly.

**Information disclosure:** Each plugin runs in a wasmtime
instance with no shared memory between plugins. `secrets.read`
gates path access via the same capability model.

**Denial of service:** Plugin supervisor restarts on crash with
exponential back-off; DLQ after 5 successive crashes
(`crates/iot-plugin-host/src/supervisor.rs`). Per-call CPU
budgets via wasmtime's fuel mechanism (planned M6 W4 if it
becomes a real attack vector).

**Elevation of privilege:** Capability model is the load-bearing
mitigation. Bucket 1 audit fixes specifically targeted bypass
classes (URL parsing, request body cap, header deny-list).

### Bus (NATS) + MQTT

**Spoofing:** mTLS handshake + per-plugin user JWT validated
by NATS server against the operator-signed account JWT
(M5a W1, ADR-0011 retired).

**Tampering:** TLS in transit; messages bear `iot.publisher`
header so listeners can attribute origin. Audit log records
publishes for sensitive subjects.

**Repudiation:** Audit log + the broker's own log of
authentication events (operator-side observability concern).

**Information disclosure:** Per-plugin subject ACLs prevent
cross-plugin subscription (`bus.subscribe` allow-list is
manifest-derived and broker-enforced).

**Denial of service:** Wildcard replay capped at 5000 messages
(Bucket 2 M3). NATS server's own connection limits (configured
in `deploy/compose/nats/nats.conf`).

**Elevation of privilege:** A compromised plugin's nkey can't
publish on subjects outside its ACL; the broker rejects.

### TimescaleDB (history backend)

**Spoofing:** Connection authenticated via `IOT_TIMESCALE_URL`-
embedded credentials. Connection happens over the local
loopback in dev; over LAN-mTLS in prod (operator-configured).

**Tampering:** sqlx parameterised queries throughout. No
string concat into SQL. The `entity_state_history` table is
append-only-by-convention (the only write path is the bus
watcher's INSERT; only `iotctl history prune` deletes).

**Repudiation:** No per-row attribution beyond `device_id`;
acceptable because the row is itself attributable to the bus
publisher via the bus headers we capture.

**Information disclosure:** TLS-at-rest is operator-deployment
(see ASVS V8.3.1 + the runbook in `docs/security/asvs-l2.md`).
Filesystem permissions on the data dir at owner-only via the
LUKS-mount default in the reference deployment.

**Denial of service:** Connection pool max 8 (history-store
default). Hypertable partitioning bounds query cost on time
ranges.

**Elevation of privilege:** N/A — backend has no privilege
escalation path beyond "compromised account → access to one
table." Other Postgres roles aren't granted access.

### Audit log (`iot-audit`)

**Spoofing:** Every entry carries the writer's identity;
the chain's prev-hash makes after-the-fact insertion
detectable.

**Tampering:** RFC 8785 JCS canonicalisation before SHA-256;
chain breaks if any entry is altered. Detection: replay the
chain offline, compare each computed hash to the stored one.

**Repudiation:** The chain *is* the non-repudiation
mechanism. An external auditor verifying a specific chain
slice can prove it wasn't altered between two known-good
checkpoints.

**Information disclosure:** Audit entries don't carry payload
secrets — only metadata + content-hashes. ADR-0008's
"what to log" matrix is the source of truth.

**Denial of service:** Disk-bounded; the audit dir's free
space is the operator's monitoring concern.

**Elevation of privilege:** Append-only API; no operator path
to delete an entry once committed.

## Highest-priority residual risks

Three residual risks worth flagging for M6 W4's pen-test
partner SOW:

1. **OIDC IdP key compromise.** No mitigation in our scope —
   we depend on the IdP's key-rotation hygiene. Pen-test
   should verify that token-replay across IdP rotations
   doesn't accept stale tokens.

2. **Plugin manifest schema bypass.** Manifest validation
   happens client-side (`iotctl plugin install`); a malicious
   plugin author with hub-write access could skip it. The
   trust anchor is the cosign signature, which is verified
   against the pinned trust pubkey.

3. **Rekor downtime.** Cosign sign-blob can succeed without
   a Rekor entry on transient outage. Pen-test should verify
   that cosign-verify rejects unrekorred bundles.

## Out-of-scope: known-and-accepted gaps

Two gaps are documented as accepted:

* **Live cert rotation (no restart).** Documented in
  `docs/security/cert-rotation-test.md` § Current behaviour;
  workaround is the systemd-restart runbook.
* **TLS-at-rest for history.** Documented in
  `docs/security/asvs-l2.md` § V8.3.1; operator
  responsibility, not a code gap.

## Reverse-traceability

This threat model is cited from:
* `docs/security/asvs-l2.md` V1.1.2 (threat-modeling required)
* `docs/security/etsi-303-645.md` §5.6 (minimise attack surface)
