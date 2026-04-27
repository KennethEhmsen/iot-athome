//! Cert-rotation integration tests (M6 W2.5).
//!
//! Closes the structural surface of the cert-rotation behaviour
//! the project's runbook
//! (`docs/security/cert-rotation-test.md`) describes. Tests fall
//! into two tiers:
//!
//! * **Always-on (this file):** mint two CA chains via rcgen,
//!   verify that bus `Config`s built against each are
//!   structurally independent, that file-IO paths handle
//!   expected layouts, and that a `rustls::ClientConfig` built
//!   from CA-A's bundle rejects a leaf signed by CA-B
//!   (the core "rotation invalidates old trust" property).
//!
//! * **`#[ignore]`-gated (this file, opt-in):** spin up a NATS
//!   testcontainer with mTLS, swap the broker cert mid-test,
//!   reconnect with refreshed trust bundle, assert success.
//!   The full live-broker rotation cycle. Stubbed out in this
//!   commit since the testcontainers-NATS-with-mTLS image
//!   plumbing is its own follow-up; the structural tier
//!   covers the cert-handling correctness without it.
//!
//! ## Threat model that this exercises
//!
//! Per `docs/security/threat-model.md` § Bus, the rotation
//! property under test: a peer presenting a cert from the
//! retired CA must be rejected by clients that have rotated
//! to the new CA. The rcgen + rustls verifier path below is
//! exactly the in-process check that `Bus::connect`'s
//! `add_root_certificates` would do at handshake time —
//! verifying it here without a live broker pins the
//! correctness without paying the testcontainers cost.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::let_and_return,
    clippy::similar_names,
    clippy::struct_field_names
)]

use std::path::PathBuf;
use std::sync::Arc;

use iot_bus::Config;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, IsCa, KeyPair, KeyUsagePurpose,
};
use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, ServerName};

/// Mint a root CA + a leaf cert signed by it. Returns the
/// PEM-encoded CA cert, leaf cert, and leaf key. All
/// in-memory; the test functions write them to tempdir paths
/// when they need on-disk shapes.
struct CertChain {
    /// PEM-encoded CA self-signed cert.
    ca_pem: String,
    /// PEM-encoded leaf cert signed by the CA above.
    leaf_pem: String,
    /// PEM-encoded leaf private key (ECDSA-P256).
    leaf_key_pem: String,
}

fn mint_chain(name_prefix: &str, leaf_cn: &str) -> CertChain {
    // 1. CA keypair + self-signed cert.
    let ca_kp = KeyPair::generate().expect("ca keypair");
    let mut ca_params =
        CertificateParams::new(vec![format!("{name_prefix}-ca")]).expect("ca params");
    let mut ca_dn = DistinguishedName::new();
    ca_dn.push(rcgen::DnType::CommonName, format!("{name_prefix} test CA"));
    ca_params.distinguished_name = ca_dn;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];
    let ca_cert = ca_params.self_signed(&ca_kp).expect("ca self-signed");

    // 2. Leaf keypair + cert signed by the CA.
    let leaf_kp = KeyPair::generate().expect("leaf keypair");
    let mut leaf_params = CertificateParams::new(vec![leaf_cn.to_owned()]).expect("leaf params");
    let mut leaf_dn = DistinguishedName::new();
    leaf_dn.push(rcgen::DnType::CommonName, leaf_cn);
    leaf_params.distinguished_name = leaf_dn;
    let leaf_cert = leaf_params
        .signed_by(&leaf_kp, &ca_cert, &ca_kp)
        .expect("leaf signed");

    CertChain {
        ca_pem: ca_cert.pem(),
        leaf_pem: leaf_cert.pem(),
        leaf_key_pem: leaf_kp.serialize_pem(),
    }
}

/// Write a `CertChain` into the on-disk layout `iot_bus::Config`
/// expects under `IOT_DEV_CERTS_ROOT`. Returns the root tempdir
/// (so the caller can build a `Config` against it without
/// spreading `tempfile::TempDir` ownership through the tests).
fn write_chain_to_tempdir(chain: &CertChain) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    // CA bundle at <root>/ca/ca.crt.
    let ca_dir = root.join("ca");
    std::fs::create_dir_all(&ca_dir).expect("mkdir ca/");
    std::fs::write(ca_dir.join("ca.crt"), &chain.ca_pem).expect("write ca.crt");

    // Client cert at <root>/client/client.{crt,key} — the
    // default `IOT_BUS_COMPONENT=client` layout.
    let client_dir = root.join("client");
    std::fs::create_dir_all(&client_dir).expect("mkdir client/");
    std::fs::write(client_dir.join("client.crt"), &chain.leaf_pem).expect("write client.crt");
    std::fs::write(client_dir.join("client.key"), &chain.leaf_key_pem).expect("write client.key");

    dir
}

#[test]
fn two_independent_ca_chains_produce_independent_configs() {
    // The core rotation invariant: building a Config for CA-B
    // doesn't accidentally reuse CA-A's path or trust state.
    let chain_a = mint_chain("ca-a", "iot-bus-test-a.local");
    let chain_b = mint_chain("ca-b", "iot-bus-test-b.local");

    let dir_a = write_chain_to_tempdir(&chain_a);
    let dir_b = write_chain_to_tempdir(&chain_b);

    let cfg_a = build_config_against(dir_a.path(), "test-a");
    let cfg_b = build_config_against(dir_b.path(), "test-b");

    // Paths point at different tempdirs.
    assert!(cfg_a.ca_path.starts_with(dir_a.path()));
    assert!(cfg_b.ca_path.starts_with(dir_b.path()));
    assert_ne!(cfg_a.ca_path, cfg_b.ca_path);
    assert_ne!(cfg_a.client_cert_path, cfg_b.client_cert_path);

    // Both CA paths exist + are non-empty PEM.
    let ca_a_bytes = std::fs::read(&cfg_a.ca_path).expect("read ca-a");
    let ca_b_bytes = std::fs::read(&cfg_b.ca_path).expect("read ca-b");
    assert!(ca_a_bytes.starts_with(b"-----BEGIN CERTIFICATE-----"));
    assert!(ca_b_bytes.starts_with(b"-----BEGIN CERTIFICATE-----"));
    // The two CAs are actually different bytes — proves rcgen
    // isn't producing identical CAs across calls (would
    // invalidate the rotation test).
    assert_ne!(ca_a_bytes, ca_b_bytes);
}

#[test]
fn rustls_verifier_built_against_ca_a_rejects_leaf_signed_by_ca_b() {
    // The cert-rotation correctness property: a verifier
    // configured to trust ONLY CA-A must reject a server cert
    // signed by CA-B. This is the in-process check
    // `Bus::connect`'s rustls handshake performs at runtime;
    // verifying it here pins the correctness without a live
    // broker.
    let chain_a = mint_chain("ca-a", "server-a.local");
    let chain_b = mint_chain("ca-b", "server-b.local");

    // Build a rustls verifier trusting only CA-A.
    let verifier = build_verifier_for(&chain_a.ca_pem);

    // Try to verify CA-B's leaf cert against the CA-A-only
    // verifier. Should fail.
    let leaf_b_der = pem_cert_to_der(&chain_b.leaf_pem);
    let server_name = ServerName::try_from("server-b.local").expect("server name");
    let result = verifier.verify_server_cert(
        &leaf_b_der,
        &[],
        &server_name,
        &[],
        rustls::pki_types::UnixTime::now(),
    );
    assert!(
        result.is_err(),
        "verifier trusting CA-A wrongly accepted leaf signed by CA-B"
    );

    // And the inverse: CA-A's own leaf verifies cleanly against
    // CA-A's verifier (sanity — proves the verifier isn't
    // rejecting everything).
    let leaf_a_der = pem_cert_to_der(&chain_a.leaf_pem);
    let server_name_a = ServerName::try_from("server-a.local").expect("server name a");
    let ok = verifier.verify_server_cert(
        &leaf_a_der,
        &[],
        &server_name_a,
        &[],
        rustls::pki_types::UnixTime::now(),
    );
    assert!(
        ok.is_ok(),
        "verifier trusting CA-A rejected its own leaf: {ok:?}"
    );
}

#[test]
fn config_pointing_at_missing_ca_path_is_constructible_but_path_is_invalid() {
    // `Config` construction is filesystem-touching at connect
    // time, not constructor time — so a Config with a bogus
    // path *builds* but fails at `Bus::connect`. Pin the
    // contract so a future refactor doesn't accidentally make
    // construction blocking.
    let cfg = Config {
        url: "tls://localhost:4222".into(),
        ca_path: PathBuf::from("/this/does/not/exist/ca.crt"),
        client_cert_path: PathBuf::from("/this/does/not/exist/client.crt"),
        client_key_path: PathBuf::from("/this/does/not/exist/client.key"),
        publisher: "rotation-test".into(),
        creds_path: None,
    };

    assert_eq!(cfg.publisher, "rotation-test");
    assert!(!cfg.ca_path.exists());
    // Confirms the lazy-IO contract: building a Config for an
    // about-to-be-rotated CA path doesn't error on construction
    // even before the operator has dropped the new file in
    // place. Operator can prepare the rotation atomically:
    // mint new CA → write to new path → rebuild Config with
    // new path → drop in-memory bus client + reconnect.
}

#[tokio::test]
#[ignore = "needs a NATS testcontainer with mTLS server.conf — full live-broker rotation cycle"]
async fn live_rotation_via_testcontainers() {
    // Future deliverable per `docs/security/cert-rotation-test.md`.
    // The test plan:
    //   1. Mint CA-A + server cert via rcgen.
    //   2. Spin a NATS testcontainer mounting CA-A's server cert
    //      + a custom server.conf that requires_tls + verifies
    //      client certs against CA-A.
    //   3. Build Config trusting CA-A; Bus::connect; publish
    //      round-trip.
    //   4. Stop container.
    //   5. Mint CA-B + new server cert.
    //   6. Restart container with CA-B's bundle.
    //   7. Build new Config trusting CA-B; Bus::connect against
    //      the new container; publish round-trip.
    //   8. Verify Config trusting CA-A but pointing at the new
    //      container's URL fails at TLS handshake.
    //
    // Blockers:
    //   * testcontainers-rs 0.27's NATS image module is the
    //     plain-tcp variant. mTLS requires a custom Image impl
    //     that mounts the server cert + a custom server.conf.
    //     ~150 lines of test scaffolding; out of this commit's
    //     scope.
    //   * The rotation step (4-6) requires either a fresh
    //     container or a NATS reload-config feature; the
    //     simpler path is fresh containers per phase, accepting
    //     the ~5 s setup cost twice.
    //
    // When this lands, it closes the test-plan section in
    // cert-rotation-test.md from "stubbed" to "shipped".
    panic!("see docstring; implementation pending testcontainers NATS mTLS plumbing");
}

// --- helpers -------------------------------------------------

/// Build an `iot_bus::Config` against a tempdir that contains
/// the standard `ca/`, `client/` layout. Mirrors what
/// `Config::from_env` does, except points at the explicit dir
/// rather than reading `IOT_DEV_CERTS_ROOT`.
fn build_config_against(root: &std::path::Path, publisher: &str) -> Config {
    Config {
        url: "tls://localhost:4222".into(),
        ca_path: root.join("ca").join("ca.crt"),
        client_cert_path: root.join("client").join("client.crt"),
        client_key_path: root.join("client").join("client.key"),
        publisher: publisher.into(),
        creds_path: None,
    }
}

/// Build a rustls `ServerCertVerifier` that trusts only `ca_pem`.
/// Mirrors what `async_nats::ConnectOptions::add_root_certificates`
/// does at handshake time — same trust-store construction; same
/// `WebPkiServerVerifier`.
fn build_verifier_for(ca_pem: &str) -> Arc<dyn ServerCertVerifier> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let mut roots = rustls::RootCertStore::empty();
    let cert_der = pem_cert_to_der(ca_pem);
    roots.add(cert_der).expect("add CA to roots");

    rustls::client::WebPkiServerVerifier::builder(Arc::new(roots))
        .build()
        .expect("build verifier")
}

/// Convert a PEM-encoded cert to a DER-encoded `CertificateDer`.
fn pem_cert_to_der(pem: &str) -> CertificateDer<'static> {
    let mut reader = pem.as_bytes();
    let mut iter = rustls_pemfile::certs(&mut reader);
    let cert = iter
        .next()
        .expect("at least one cert in PEM")
        .expect("valid PEM cert");
    cert
}
