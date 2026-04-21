//! Integration test: bus envelope round-trip against a real NATS server.
//!
//! The test spins up `nats:2.10-alpine` in a container (no TLS) and exercises
//! the header convention documented in ADR-0004 and ADR-0009. It does NOT go
//! through [`iot_bus::Bus`] — that path requires dev certs, which are minted
//! only in the CI integration stage and in `just dev`. A follow-up test in M2
//! will cover the full mTLS path with container-minted certs.
//!
//! Run locally with `cargo test -p iot-bus --test roundtrip -- --nocapture`.
//! Requires Docker / Podman / Testcontainers-compatible runtime.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use futures::StreamExt as _;
use iot_proto::headers::{IOT_PUBLISHER, IOT_SCHEMA_VERSION, IOT_TYPE};
use std::time::Duration;
use testcontainers::{
    core::{ContainerPort, WaitFor},
    runners::AsyncRunner,
    GenericImage, ImageExt,
};

#[tokio::test]
async fn header_envelope_roundtrips() -> Result<(), Box<dyn std::error::Error>> {
    // Skip if the host lacks a container runtime. Detected via the env var
    // testcontainers itself sets, falling back to a docker socket check.
    if std::env::var_os("DOCKER_HOST").is_none()
        && !std::path::Path::new("/var/run/docker.sock").exists()
    {
        eprintln!("no container runtime — skipping");
        return Ok(());
    }

    let container = GenericImage::new("nats", "2.10-alpine")
        .with_exposed_port(ContainerPort::Tcp(4222))
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .with_cmd(vec!["-js".to_string(), "-DV".to_string()])
        .start()
        .await?;

    let host = container.get_host().await?;
    let port = container.get_host_port_ipv4(4222).await?;
    let url = format!("nats://{host}:{port}");

    let client = async_nats::connect(&url).await?;
    let mut sub = client.subscribe("test.envelope.>".to_string()).await?;
    client.flush().await?;

    let mut headers = async_nats::HeaderMap::new();
    headers.insert(IOT_PUBLISHER, "integration-test");
    headers.insert(IOT_SCHEMA_VERSION, "1");
    headers.insert(IOT_TYPE, "iot.device.v1.EntityEvent");

    client
        .publish_with_headers("test.envelope.roundtrip", headers, b"hello".to_vec().into())
        .await?;
    client.flush().await?;

    let msg = tokio::time::timeout(Duration::from_secs(5), sub.next())
        .await?
        .ok_or("subscription closed")?;

    assert_eq!(msg.subject.as_str(), "test.envelope.roundtrip");
    assert_eq!(msg.payload.as_ref(), b"hello");

    let got_headers = msg.headers.as_ref().expect("headers present");
    let as_string = |k: &str| got_headers.get(k).map(ToString::to_string);
    assert_eq!(as_string(IOT_PUBLISHER), Some("integration-test".into()));
    assert_eq!(as_string(IOT_SCHEMA_VERSION), Some("1".into()));
    assert_eq!(
        as_string(IOT_TYPE),
        Some("iot.device.v1.EntityEvent".into())
    );

    Ok(())
}
