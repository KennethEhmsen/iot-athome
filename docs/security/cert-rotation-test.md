# Cert rotation — threat model, behaviour, runbook, test plan (M6 W2.5)

Closes the ETSI EN 303 645 §5.5 partial ("Communicate securely")
in `docs/security/etsi-303-645.md`. The standard wants a clear
answer to: *what happens when the CA or server cert is rotated
mid-deployment?* This doc is that answer plus the test surface
that pins the behaviour.

## Threats

Three concrete rotation scenarios are in scope:

1. **Routine rotation.** Operator pre-emptively rotates the CA
   on a yearly cadence. No incident; no urgency.
2. **CA compromise.** The dev CA's private key was disclosed
   (e.g. accidentally pushed to a public git repo). Rotation
   is *now*; the old CA must stop being trusted within minutes.
3. **Cert expiry.** The dev CA shipped with a 1-year validity;
   the operator forgot to rotate; clients start refusing the
   server post-expiry.

Out of scope:

* Server cert pinning / HPKP-style certificate-pinning. The
  project relies on CA-trust-anchor validation, not pinned
  end-entity certs. Pinning is a post-M6 hardening item.
* Hardware security module (HSM)-backed CA. Hub-class
  deployments don't have an HSM; the CA's private key sits on
  the hub's filesystem under `tools/devcerts/generated/` with
  POSIX `chmod 0600` (or Windows ACL post-Bucket-1).

## Current behaviour: process-restart required

The bus client (`iot_bus::Bus`) loads CA + client certs at
`Bus::connect` time via `async_nats::ConnectOptions`. The
underlying `add_root_certificates(path)` reads the file once
and bakes the trust bundle into the rustls config. On
reconnect after a transient network failure, async-nats reuses
the *in-memory* config — it does **not** re-read the cert
file from disk.

Consequence: a CA rotation requires the bus client process to
restart. Operators get this for free under systemd's standard
unit configuration (`Restart=on-failure` + a SIGHUP handler
that re-execs), but the test surface must reflect the
limitation explicitly so future contributors don't assume
live-reload.

A future revision could add a file-watch-driven config reload
(notify-rs + tokio_stream) to support live rotation; that's
post-M6 scope. Documenting now so the limitation surfaces in
ETSI evidence rather than being implicit.

## Operator runbook — clean rotation

The "no-incident routine rotation" path:

```sh
# 1. Mint a new CA + leaf certs alongside the existing ones.
#    The dev script `tools/devcerts/mint.sh` accepts a
#    `--rotate` flag (M6 W2.5); without it, regeneration
#    overwrites in place.
bash tools/devcerts/mint.sh --rotate

# 2. Concatenate old + new CA into a transitional trust bundle
#    so existing connections don't drop while servers swap.
cat tools/devcerts/generated/ca/ca.crt \
    tools/devcerts/generated/ca/ca.crt.new \
  > tools/devcerts/generated/ca/ca.transitional.crt

# 3. Update services to use the transitional bundle. Restart in
#    rolling order (broker first, then registry / gateway, then
#    plugins). Each service comes back trusting both old + new.
IOT_DEV_CERTS_ROOT=/path/to/generated systemctl restart \
    iot-bus iot-registry iot-gateway

# 4. After all services are on the transitional bundle, swap the
#    leaf certs to the new ones.
mv tools/devcerts/generated/server.crt.new \
   tools/devcerts/generated/server.crt
systemctl restart nats-server  # leaf cert change requires its own restart

# 5. Once all clients are reconnected against the new leaf,
#    drop the old CA.
mv tools/devcerts/generated/ca/ca.crt.new \
   tools/devcerts/generated/ca/ca.crt
systemctl restart iot-bus iot-registry iot-gateway
```

Total downtime per service: one restart cycle (~5 s for
iot-registry / iot-gateway, ~2 s for the bus reconnect).
The transitional-bundle phase prevents any "trust gap"
window where some clients still trust the old CA but the
server has moved.

## Operator runbook — incident rotation (CA compromise)

When the old CA must stop being trusted *now*:

```sh
# 1. Mint the new CA + leaf certs.
bash tools/devcerts/mint.sh --force

# 2. Move the old CA out of the trust store immediately.
mv tools/devcerts/generated/ca/ca.crt /var/incident/ca.compromised.crt

# 3. Replace with the new CA + new leaf cert.
mv tools/devcerts/generated/ca/ca.crt.new tools/devcerts/generated/ca/ca.crt
mv tools/devcerts/generated/server.crt.new tools/devcerts/generated/server.crt

# 4. Bounce all services. Old in-memory configs are stuck on the
#    compromised CA; the restart is what gets clean state.
systemctl restart nats-server iot-registry iot-gateway iot-bus

# 5. Re-mint per-plugin nkeys + creds (if the compromised CA
#    signed the operator JWT chain too).
iotctl nats bootstrap --force
for d in /var/lib/iotathome/plugins/*/; do
  iotctl plugin install --force "$d" --account-seed=...
done

# 6. Audit-log the incident.
iotctl audit emit --type incident.cert-compromise \
    --raw "old-ca-fingerprint=$(openssl x509 -fingerprint -in /var/incident/ca.compromised.crt)"
```

Total downtime: ~30 s for the broker bounce + 60-90 s for
re-issuing plugin creds. The audit-log entry is the
tamper-evident record of what happened.

## Test plan

Integration test at `crates/iot-bus/tests/cert_rotation.rs`
(testcontainers-gated, future deliverable). Pseudo-code:

```rust
#[tokio::test]
async fn cert_rotation_via_restart_recovers() {
    // 1. Mint CA-A + server cert signed by CA-A.
    let ca_a = mint_ca("ca-a");
    let server_cert_a = mint_leaf(&ca_a, "nats");
    let nats_a = start_nats_with_cert(server_cert_a).await;

    // 2. Connect bus client trusting CA-A. Verify round-trip.
    let bus = Bus::connect(cfg_with_ca(&ca_a, &nats_a.url())).await?;
    bus.publish_proto(...).await?;

    // 3. Stop nats, mint CA-B, restart with new cert.
    drop(nats_a);
    let ca_b = mint_ca("ca-b");
    let server_cert_b = mint_leaf(&ca_b, "nats");
    let nats_b = start_nats_with_cert(server_cert_b).await;

    // 4. Reconnect bus client with refreshed trust bundle.
    //    The drop+reconnect is the "process restart" simulation.
    drop(bus);
    let bus = Bus::connect(cfg_with_ca(&ca_b, &nats_b.url())).await?;
    bus.publish_proto(...).await?;

    // 5. Assert: stale-CA reconnect is REJECTED.
    let stale = Bus::connect(cfg_with_ca(&ca_a, &nats_b.url())).await;
    assert!(matches!(stale, Err(BusError::Connect(_))));
}
```

Test runs under `--features integration-tests` (matches the
existing iot-history pattern). On a stock CI runner without
Docker, the test self-skips with a clear log message.

## Unit-level pinning

For the cases that don't need a live broker, the unit test in
`crates/iot-bus/src/lib.rs::tests::config_round_trips_two_cas`
covers:

* A `Config` for CA-A round-trips through serde correctly.
* A second `Config` for CA-B, built post-rotation, holds the
  new path without the old one bleeding through.
* Both configs reject invalid CA paths early (file-not-found
  surfaces as `BusError::Connect`).

## Status

* **Documentation:** ✅ This file (M6 W2.5).
* **Operator runbooks:** ✅ Two scenarios above.
* **Structural integration test:** ✅
  `crates/iot-bus/tests/cert_rotation.rs` — three always-on
  tests covering: (1) two independent CA chains via rcgen
  produce independent Configs; (2) a rustls verifier trusting
  CA-A correctly rejects a leaf signed by CA-B (the rotation
  correctness property — same code path
  `Bus::connect`'s rustls handshake takes); (3) Config
  construction is lazy-IO (an about-to-rotate path doesn't
  error on construction, allowing atomic operator-side
  preparation).
* **Live-broker integration test:** ⏳ Stubbed at
  `cert_rotation.rs::live_rotation_via_testcontainers`,
  `#[ignore]`-gated. Blocked on testcontainers-rs 0.27's
  NATS module being the plain-tcp variant — mTLS requires a
  custom Image impl mounting cert + server.conf (~150 lines
  of test scaffolding). Follow-up.
* **Live-reload (no restart) capability:** ✗ Out of scope;
  documented as a post-M6 hardening item.

## Reverse-mapping

ETSI EN 303 645 §5.5 — *Communicate securely*. This doc is
cited in `docs/security/etsi-303-645.md` as the evidence that
turns the §5.5 row from P → C. The remaining "not live-reload"
limitation is captured explicitly there; an external auditor
asking "what about CA rotation" gets a complete answer
including its known limits.
