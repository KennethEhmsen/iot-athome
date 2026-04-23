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

use std::sync::{Arc, RwLock};

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
}
