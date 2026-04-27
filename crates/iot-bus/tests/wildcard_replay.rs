//! Integration test: `Bus::last_state_wildcard` drains every subject
//! past the per-batch fetch ceiling.
//!
//! Bucket 1 audit M3 fix: the previous implementation capped
//! `max_messages` at 1024 silently, so a panel reload on a home with
//! more than 1024 distinct device entities would miss replays past
//! the cap. This test publishes 1500 distinct retained-state
//! subjects, then asserts every single one is returned by a single
//! `last_state_wildcard` call against the `device.` wildcard.
//!
//! Spins up `nats:2.10-alpine` with JetStream enabled (`-js`), no
//! TLS — the test bypasses `Bus::connect`'s mTLS path via
//! `Bus::from_client`.
//!
//! Run locally with
//! `cargo test -p iot-bus --test wildcard_replay -- --nocapture`.
//! Requires Docker / Podman / Testcontainers-compatible runtime.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use iot_bus::Bus;
use std::time::Duration;
use testcontainers::{
    core::{ContainerPort, WaitFor},
    runners::AsyncRunner,
    GenericImage, ImageExt,
};

const TOTAL_SUBJECTS: usize = 1500;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wildcard_replay_returns_every_subject_past_batch_cap(
) -> Result<(), Box<dyn std::error::Error>> {
    // Skip if the host lacks a container runtime. Mirrors the
    // detection in roundtrip.rs.
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
    let bus = Bus::from_client(client.clone(), "wildcard-replay-test");

    bus.ensure_device_state_stream().await?;

    // Publish TOTAL_SUBJECTS distinct subjects under `device.>`. Use
    // core publish with a final flush — JetStream picks them up via
    // the stream's subject filter. Each subject's payload encodes its
    // index so we can assert no collisions / off-by-ones.
    for i in 0..TOTAL_SUBJECTS {
        let subject = format!("device.test.dev{i:04}.s.state");
        let payload = format!("payload-{i:04}").into_bytes();
        client.publish(subject, payload.into()).await?;
    }
    client.flush().await?;

    // JetStream ingests asynchronously; wait until the stream's
    // message count reaches TOTAL_SUBJECTS (one message per subject
    // because `max_messages_per_subject = 1`). 30s is generous for a
    // local container — Docker on Windows is the slow path.
    let ctx = async_nats::jetstream::new(client.clone());
    let stream = ctx.get_stream("DEVICE_STATE").await?;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let mut s = stream.clone();
        let info = s.info().await?;
        if info.state.messages >= TOTAL_SUBJECTS as u64 {
            break;
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "stream only ingested {}/{} messages within deadline",
                info.state.messages, TOTAL_SUBJECTS
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // The function under test: a single call must return every
    // subject. With the loop fix in place, num_pending drains across
    // 2 batches (1024 + 476). Pre-fix this returned ~1024.
    let replays = bus.last_state_wildcard("device.>").await?;

    assert_eq!(
        replays.len(),
        TOTAL_SUBJECTS,
        "expected every published subject in the wildcard replay"
    );

    // No duplicates and no missing indexes — collect the unique
    // subject set, derive the index from the suffix, and assert the
    // full 0..TOTAL_SUBJECTS range is present.
    let mut seen: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    for (subject, payload) in &replays {
        let prefix = "device.test.dev";
        let suffix = ".s.state";
        let inner = subject
            .strip_prefix(prefix)
            .and_then(|s| s.strip_suffix(suffix))
            .ok_or_else(|| format!("unexpected subject shape: {subject}"))?;
        let idx: usize = inner.parse()?;
        assert!(seen.insert(idx), "duplicate subject for index {idx}");
        assert_eq!(payload, &format!("payload-{idx:04}").into_bytes());
    }
    assert_eq!(seen.len(), TOTAL_SUBJECTS);
    assert_eq!(*seen.iter().next().unwrap(), 0);
    assert_eq!(*seen.iter().next_back().unwrap(), TOTAL_SUBJECTS - 1);

    Ok(())
}
