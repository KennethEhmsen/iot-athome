//! End-to-end integration test for the MQTT broker dispatcher (M2 W4).
//!
//! Spins `eclipse-mosquitto:2` in a testcontainer with an inline
//! anonymous-enabling config, connects `MqttBroker` to it plaintext
//! (no mTLS — we're testing the dispatch pipe, not the handshake),
//! registers a fake plugin tx with the router, publishes a message
//! through the broker itself, and asserts the tx received exactly the
//! expected `PluginCommand::OnMqttMessage`.
//!
//! Covers the full pipeline end-to-end without a wasm plugin in the
//! loop:
//!
//!     broker.publish → Mosquitto → broker.eventloop.poll → router.dispatch → tx.send
//!
//! Run locally with `cargo test -p iot-plugin-host --test mqtt_broker -- --nocapture`.
//! Requires a Docker-compatible runtime on PATH (same as the iot-bus
//! roundtrip test).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::time::Duration;

use iot_plugin_host::mqtt::{MqttBroker, MqttBrokerConfig, MqttRouter};
use iot_plugin_host::runtime::PluginCommand;
use testcontainers::{
    core::{ContainerPort, WaitFor},
    runners::AsyncRunner,
    GenericImage, ImageExt,
};
use tokio::sync::mpsc;

/// Launch a Mosquitto 2.x broker with anonymous access on port 1883.
///
/// The upstream `eclipse-mosquitto:2` image refuses to start without
/// an explicit config (2.0 tightened the default from earlier
/// versions); we override the entrypoint to write a minimal
/// `listener 1883 + allow_anonymous true` config at startup and then
/// exec mosquitto against it. Ugly but keeps the test hermetic.
async fn start_mosquitto() -> (testcontainers::ContainerAsync<GenericImage>, String, u16) {
    let inline_conf = "listener 1883 0.0.0.0\nallow_anonymous true\n";
    let container = GenericImage::new("eclipse-mosquitto", "2.0.18")
        .with_exposed_port(ContainerPort::Tcp(1883))
        .with_wait_for(WaitFor::message_on_stderr("mosquitto version"))
        .with_entrypoint("sh")
        .with_cmd(vec![
            "-c".to_string(),
            format!(
                "printf '{}' > /m.conf && exec mosquitto -c /m.conf",
                inline_conf.replace('\n', "\\n")
            ),
        ])
        .start()
        .await
        .expect("start mosquitto");

    let host = container.get_host().await.expect("host").to_string();
    let port = container.get_host_port_ipv4(1883).await.expect("port");
    (container, host, port)
}

#[tokio::test]
async fn broker_delivers_published_message_to_registered_plugin() {
    // Skip when no container runtime is available — matches the
    // convention from `iot-bus/tests/roundtrip.rs`.
    if std::env::var_os("DOCKER_HOST").is_none()
        && !std::path::Path::new("/var/run/docker.sock").exists()
    {
        eprintln!("no container runtime — skipping");
        return;
    }

    let (_container, host, port) = start_mosquitto().await;

    // Build the broker + router. Plaintext for test speed — the TLS
    // path is exercised by the z2m adapter's M1 tests and by the
    // `mqtt_options_reads_tls_material` unit test.
    let router = MqttRouter::new();
    let cfg = MqttBrokerConfig {
        host: host.clone(),
        port,
        client_id: "iot-plugin-host-test".into(),
        tls: None,
    };
    let broker = MqttBroker::connect(cfg, router.clone())
        .await
        .expect("connect broker");

    // Pretend we're a plugin: register a mailbox with the router +
    // ask the broker to forward messages for a filter to us.
    let (tx, mut rx) = mpsc::channel::<PluginCommand>(8);
    router.register("fake-plugin", "sensors/+/temp", tx);
    broker
        .subscribe_filter("sensors/+/temp")
        .await
        .expect("subscribe filter");

    // Give the broker a beat to actually wire up the subscription
    // before we publish. Without this, the publish can race the
    // SUBSCRIBE packet and the broker drops the message on the floor.
    tokio::time::sleep(Duration::from_millis(250)).await;

    broker
        .publish("sensors/kitchen/temp", b"21.5", false)
        .await
        .expect("publish");

    // The eventloop task should route this to our mailbox within a
    // few hundred ms; 5s gives plenty of slack under CI load.
    let cmd = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for dispatch")
        .expect("channel closed without message");

    match cmd {
        PluginCommand::OnMqttMessage { topic, payload } => {
            assert_eq!(topic, "sensors/kitchen/temp");
            assert_eq!(payload, b"21.5");
        }
        other => panic!("expected OnMqttMessage, got {other:?}"),
    }

    // A non-matching topic must NOT arrive (no further messages within
    // a short settle window).
    broker
        .publish("sensors/kitchen/humid", b"45", false)
        .await
        .expect("publish non-matching");
    let maybe = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
    assert!(
        maybe.is_err(),
        "expected no match on sensors/kitchen/humid but got: {maybe:?}"
    );
}
