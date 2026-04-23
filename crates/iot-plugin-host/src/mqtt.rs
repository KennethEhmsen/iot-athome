//! MQTT broker bridge (M2 W4, part 1 — router only).
//!
//! Plugins that declare `capabilities.mqtt.subscribe` want inbound
//! messages from a topic filter; the host owns one broker connection
//! and fans out inbound messages to every plugin whose registered
//! filter matches. This module is the routing table that decides
//! *which* plugins get *which* topics — the piece that has no
//! rumqttc dependency and can be unit-tested exhaustively.
//!
//! The actual rumqttc client + eventloop that feeds this router
//! lands in the next commit. Until it's wired, plugin subscriptions
//! register successfully but no broker messages flow.
//!
//! Ownership:
//! ```text
//!                        ┌── MqttRouter ──────────────┐
//!  broker eventloop ──► │   subscriptions: Vec<{     │
//!      .dispatch(topic, │     plugin_id, filter, tx  │
//!                payload)│   }>                       │
//!                        └─── fan out via tx.send ────┘
//!                                │ OnMqttMessage
//!                                ▼
//!                          PluginTask (owns Store)
//! ```

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context as _, Result};
use rumqttc::{
    AsyncClient, Event, EventLoop, MqttOptions, Packet, QoS, TlsConfiguration, Transport,
};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::capabilities::matches_mqtt_topic;
use crate::runtime::PluginCommand;

/// Single registration: "this plugin wants inbound messages matching
/// `filter`, deliver via `tx`".
#[derive(Debug, Clone)]
struct Subscription {
    plugin_id: String,
    filter: String,
    tx: mpsc::Sender<PluginCommand>,
}

/// Routing table + fan-out for inbound MQTT messages.
///
/// Cheap to clone (it's an `Arc` wrapper around the shared state),
/// intended to live inside `HostBindings::mqtt` and be reused across
/// every plugin in the host process.
#[derive(Debug, Clone, Default)]
pub struct MqttRouter {
    inner: Arc<RwLock<Vec<Subscription>>>,
}

impl MqttRouter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `plugin_id` to receive messages whose topic matches
    /// `filter`. A plugin can register multiple filters — each is an
    /// independent subscription.
    pub fn register(
        &self,
        plugin_id: impl Into<String>,
        filter: impl Into<String>,
        tx: mpsc::Sender<PluginCommand>,
    ) {
        let sub = Subscription {
            plugin_id: plugin_id.into(),
            filter: filter.into(),
            tx,
        };
        #[allow(clippy::unwrap_used)] // RwLock poisoning only on panic; fine to surface.
        self.inner.write().unwrap().push(sub);
    }

    /// Drop every subscription for `plugin_id`. Called by the
    /// supervisor when a plugin task exits (clean shutdown or DLQ) so
    /// the closed `tx` doesn't linger and every dispatch doesn't have
    /// to prune it.
    pub fn unregister(&self, plugin_id: &str) {
        #[allow(clippy::unwrap_used)]
        self.inner
            .write()
            .unwrap()
            .retain(|s| s.plugin_id != plugin_id);
    }

    /// Deliver an inbound `(topic, payload)` pair to every registered
    /// subscription whose filter matches, dropping closed-mpsc entries
    /// in the process. Returns the number of plugin tasks that
    /// *successfully received* the message — useful for metrics /
    /// logging, not for correctness.
    pub async fn dispatch(&self, topic: &str, payload: &[u8]) -> usize {
        // Snapshot the subscriptions under a brief read lock; send
        // without holding the lock so a slow plugin's mailbox doesn't
        // wedge every other plugin's registration.
        let matches: Vec<Subscription> = {
            #[allow(clippy::unwrap_used)]
            let guard = self.inner.read().unwrap();
            guard
                .iter()
                .filter(|s| matches_mqtt_topic(&s.filter, topic))
                .cloned()
                .collect()
        };

        let mut delivered = 0usize;
        let mut dead: Vec<(String, String)> = Vec::new();
        for sub in matches {
            let cmd = PluginCommand::OnMqttMessage {
                topic: topic.to_owned(),
                payload: payload.to_owned(),
            };
            if sub.tx.send(cmd).await.is_ok() {
                delivered += 1;
            } else {
                tracing::warn!(
                    plugin = %sub.plugin_id,
                    filter = %sub.filter,
                    topic,
                    "plugin mailbox closed — dropping subscription"
                );
                dead.push((sub.plugin_id.clone(), sub.filter.clone()));
            }
        }

        // Prune closed senders so we don't try again next dispatch.
        if !dead.is_empty() {
            #[allow(clippy::unwrap_used)]
            let mut guard = self.inner.write().unwrap();
            guard.retain(|s| {
                !dead
                    .iter()
                    .any(|(pid, filt)| *pid == s.plugin_id && *filt == s.filter)
            });
        }
        delivered
    }

    /// Current count of registered subscriptions. Used by tests; also
    /// handy for `iotctl plugin list --verbose` in a later slice.
    #[must_use]
    pub fn len(&self) -> usize {
        #[allow(clippy::unwrap_used)]
        self.inner.read().unwrap().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ---------------------------------------------------------- broker bridge

/// mTLS material for an MQTT broker connection. Matches the three-file
/// layout the dev CA mint script produces (`just certs`).
#[derive(Debug, Clone, Deserialize)]
pub struct MqttTlsConfig {
    pub ca: PathBuf,
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// Per-process MQTT broker configuration. All fields default-off so a
/// host without any MQTT plugins pays nothing.
#[derive(Debug, Clone, Deserialize)]
pub struct MqttBrokerConfig {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_client_id")]
    pub client_id: String,
    /// Enables `Transport::Tls` with the given certificate material.
    /// Omit for plaintext localhost dev; always populate for real
    /// deployments (ADR-0006).
    #[serde(default)]
    pub tls: Option<MqttTlsConfig>,
}

const fn default_port() -> u16 {
    8884
}
fn default_client_id() -> String {
    "iot-plugin-host".into()
}

/// A connected MQTT broker — owns the `rumqttc::AsyncClient` for
/// outbound publishes + subscribes, plus an [`MqttRouter`] that the
/// eventloop task feeds with inbound messages.
///
/// Cheap to clone the `Arc<MqttBroker>` handle; the broker itself is
/// a singleton per host process.
#[derive(Debug)]
pub struct MqttBroker {
    client: AsyncClient,
    router: MqttRouter,
}

impl MqttBroker {
    /// Connect to the broker, spawn the eventloop task, return an
    /// `Arc<MqttBroker>` ready for plugins to use.
    ///
    /// The eventloop task runs forever, dispatching every inbound
    /// `Publish` event through `router.dispatch(topic, payload)`. On
    /// protocol errors it logs and reconnects after a 2 s back-off —
    /// matching what the M1 z2m adapter did standalone.
    ///
    /// # Errors
    /// Returns on invalid TLS material (cert files missing / malformed)
    /// or socket-level issues rumqttc surfaces at construction. The
    /// actual broker handshake happens asynchronously inside the
    /// eventloop task, so a wrong password won't show up here — watch
    /// the task's `mqtt eventloop error` logs.
    //
    // Not actually async today — rumqttc's `AsyncClient::new` is
    // synchronous and the handshake is deferred into the eventloop.
    // Keeping the `async` keyword preserves the call-site shape
    // (`.await`-ready for future versions that *do* need to wait for
    // ConnAck before returning), which is cheap and means the z2m
    // migration doesn't have to touch this line twice.
    #[allow(clippy::unused_async)]
    pub async fn connect(cfg: MqttBrokerConfig, router: MqttRouter) -> Result<Arc<Self>> {
        let opts = mqtt_options(&cfg).context("build MQTT options")?;
        let (client, eventloop) = AsyncClient::new(opts, 64);
        spawn_eventloop(eventloop, router.clone());
        Ok(Arc::new(Self { client, router }))
    }

    /// Router handle for plugins calling `mqtt::subscribe` — they
    /// register their mailbox with the router, the broker subscribes
    /// the underlying filter so inbound messages actually arrive.
    #[must_use]
    pub fn router(&self) -> &MqttRouter {
        &self.router
    }

    /// Tell the broker to forward messages matching `filter` to us.
    /// Plugin-side capability + router registration happens *before*
    /// this call inside `mqtt::Host::subscribe`; this method is the
    /// last link in the chain.
    ///
    /// Duplicate subscriptions are broker-side no-ops — we don't
    /// dedupe on the host side for simplicity.
    ///
    /// # Errors
    /// Propagates `rumqttc::ClientError` — channel-full or shutting
    /// down conditions on the AsyncClient's request queue.
    pub async fn subscribe_filter(&self, filter: &str) -> Result<()> {
        self.client
            .subscribe(filter, QoS::AtLeastOnce)
            .await
            .with_context(|| format!("broker subscribe {filter}"))
    }

    /// Publish on the broker at QoS 1.
    ///
    /// # Errors
    /// Propagates `rumqttc::ClientError` (same conditions as above).
    pub async fn publish(&self, topic: &str, payload: &[u8], retain: bool) -> Result<()> {
        self.client
            .publish(topic, QoS::AtLeastOnce, retain, payload.to_vec())
            .await
            .with_context(|| format!("broker publish {topic}"))
    }
}

fn mqtt_options(cfg: &MqttBrokerConfig) -> Result<MqttOptions> {
    let mut opts = MqttOptions::new(&cfg.client_id, &cfg.host, cfg.port);
    opts.set_keep_alive(Duration::from_secs(30));
    if let Some(tls) = &cfg.tls {
        let ca =
            std::fs::read(&tls.ca).with_context(|| format!("read MQTT CA {}", tls.ca.display()))?;
        let cert = std::fs::read(&tls.cert)
            .with_context(|| format!("read MQTT cert {}", tls.cert.display()))?;
        let key = std::fs::read(&tls.key)
            .with_context(|| format!("read MQTT key {}", tls.key.display()))?;
        opts.set_transport(Transport::Tls(TlsConfiguration::Simple {
            ca,
            alpn: None,
            client_auth: Some((cert, key)),
        }));
    }
    Ok(opts)
}

/// Detached task that owns the rumqttc `EventLoop` and routes inbound
/// messages into the shared `MqttRouter`. Reconnects after transient
/// errors with a 2 s backoff.
fn spawn_eventloop(mut eventloop: EventLoop, router: MqttRouter) {
    tokio::spawn(async move {
        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Packet::Publish(p))) => {
                    router.dispatch(&p.topic, &p.payload).await;
                }
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    tracing::info!("mqtt broker connected");
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::error!(error = %e, "mqtt eventloop error — retrying in 2s");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    });
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn router_with(plugins: &[(&str, &str)]) -> (MqttRouter, Vec<mpsc::Receiver<PluginCommand>>) {
        let router = MqttRouter::new();
        let mut rxs = Vec::new();
        for (plugin_id, filter) in plugins {
            let (tx, rx) = mpsc::channel(8);
            router.register(*plugin_id, *filter, tx);
            rxs.push(rx);
        }
        (router, rxs)
    }

    #[tokio::test]
    async fn dispatch_delivers_to_exact_match() {
        let (router, mut rxs) = router_with(&[("p1", "sensors/kitchen/temp")]);
        let delivered = router.dispatch("sensors/kitchen/temp", b"21.5").await;
        assert_eq!(delivered, 1);

        let cmd = rxs[0].try_recv().expect("p1 received");
        match cmd {
            PluginCommand::OnMqttMessage { topic, payload } => {
                assert_eq!(topic, "sensors/kitchen/temp");
                assert_eq!(payload, b"21.5");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_respects_plus_wildcard() {
        let (router, mut rxs) = router_with(&[("p1", "sensors/+/temp")]);
        router.dispatch("sensors/kitchen/temp", b"x").await;
        router.dispatch("sensors/livingroom/temp", b"y").await;
        router.dispatch("sensors/livingroom/humid", b"z").await; // shouldn't match
        router.dispatch("other/kitchen/temp", b"a").await; // shouldn't match

        let mut received = Vec::new();
        while let Ok(cmd) = rxs[0].try_recv() {
            if let PluginCommand::OnMqttMessage { topic, .. } = cmd {
                received.push(topic);
            }
        }
        assert_eq!(
            received,
            vec!["sensors/kitchen/temp", "sensors/livingroom/temp"]
        );
    }

    #[tokio::test]
    async fn dispatch_respects_hash_wildcard() {
        let (router, mut rxs) = router_with(&[("p1", "zigbee2mqtt/#")]);
        router.dispatch("zigbee2mqtt/kitchen-temp", b"x").await;
        router
            .dispatch("zigbee2mqtt/devices/ieee/state", b"y")
            .await;
        router.dispatch("other/topic", b"z").await; // no match

        let mut received = Vec::new();
        while let Ok(cmd) = rxs[0].try_recv() {
            if let PluginCommand::OnMqttMessage { topic, .. } = cmd {
                received.push(topic);
            }
        }
        assert_eq!(
            received,
            vec!["zigbee2mqtt/kitchen-temp", "zigbee2mqtt/devices/ieee/state"]
        );
    }

    #[tokio::test]
    async fn dispatch_fans_out_to_multiple_subscribers() {
        let (router, mut rxs) = router_with(&[
            ("p1", "sensors/+/temp"),
            ("p2", "sensors/+/temp"),
            ("p3", "sensors/kitchen/#"),
        ]);
        let delivered = router.dispatch("sensors/kitchen/temp", b"21.5").await;
        assert_eq!(delivered, 3, "all three filters match");

        for rx in &mut rxs {
            assert!(rx.try_recv().is_ok(), "each subscriber got the message");
        }
    }

    #[tokio::test]
    async fn dispatch_prunes_closed_mailboxes() {
        let router = MqttRouter::new();
        let (tx_live, _rx_live) = mpsc::channel(8);
        let (tx_dead, rx_dead) = mpsc::channel(8);
        router.register("p-live", "sensors/#", tx_live);
        router.register("p-dead", "sensors/#", tx_dead);
        drop(rx_dead); // close the dead plugin's mailbox

        let delivered = router.dispatch("sensors/anything", b"x").await;
        assert_eq!(delivered, 1, "only p-live received");
        assert_eq!(router.len(), 1, "p-dead's subscription pruned");
    }

    #[test]
    fn unregister_removes_all_filters_for_plugin() {
        let router = MqttRouter::new();
        let (tx1, _rx1) = mpsc::channel(8);
        let (tx2, _rx2) = mpsc::channel(8);
        router.register("p1", "a/b", tx1.clone());
        router.register("p1", "c/d", tx1);
        router.register("p2", "e/f", tx2);
        assert_eq!(router.len(), 3);

        router.unregister("p1");
        assert_eq!(router.len(), 1);
    }

    // -------------------------------------------------------- broker config

    #[test]
    fn mqtt_options_plaintext_when_no_tls() {
        let cfg = MqttBrokerConfig {
            host: "127.0.0.1".into(),
            port: 1883,
            client_id: "test".into(),
            tls: None,
        };
        // Should build without reading any cert files.
        let _ = mqtt_options(&cfg).expect("opts");
    }

    #[test]
    fn mqtt_options_reads_tls_material() {
        let dir = tempfile::tempdir().unwrap();
        let ca = dir.path().join("ca.crt");
        let cert = dir.path().join("client.crt");
        let key = dir.path().join("client.key");
        std::fs::write(&ca, b"-----BEGIN CERTIFICATE-----\n").unwrap();
        std::fs::write(&cert, b"-----BEGIN CERTIFICATE-----\n").unwrap();
        std::fs::write(&key, b"-----BEGIN PRIVATE KEY-----\n").unwrap();

        let cfg = MqttBrokerConfig {
            host: "mosquitto.iot.local".into(),
            port: 8884,
            client_id: "iot-plugin-host".into(),
            tls: Some(MqttTlsConfig { ca, cert, key }),
        };
        // Builds; actual TLS validation happens at connect-time inside
        // rumqttc's eventloop, which is beyond this unit's scope.
        let _ = mqtt_options(&cfg).expect("tls opts");
    }

    #[test]
    fn mqtt_options_surfaces_missing_tls_file() {
        let cfg = MqttBrokerConfig {
            host: "mosquitto.iot.local".into(),
            port: 8884,
            client_id: "iot-plugin-host".into(),
            tls: Some(MqttTlsConfig {
                ca: "/does/not/exist/ca.crt".into(),
                cert: "/nope/client.crt".into(),
                key: "/nope/client.key".into(),
            }),
        };
        let err = mqtt_options(&cfg).unwrap_err();
        assert!(format!("{err:#}").contains("read MQTT"), "got: {err:#}");
    }
}
