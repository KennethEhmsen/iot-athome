//! zigbee2mqtt -> canonical bridge.
//!
//! Subscribes to `zigbee2mqtt/+` on the local MQTT broker (mTLS), translates
//! each payload into a canonical Device via [`translator`], and upserts into
//! the registry via gRPC. The registry emits bus events on state change, so
//! downstream consumers (panel, automation, ML) pick up the update without
//! the adapter ever publishing on NATS directly.

#![forbid(unsafe_code)]

pub mod translator;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use iot_proto::iot::common::v1::Ulid as PbUlid;
use iot_proto::iot::registry::v1::registry_service_client::RegistryServiceClient;
use iot_proto::iot::registry::v1::{ListDevicesRequest, UpsertDeviceRequest};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS, TlsConfiguration, Transport};
use serde::Deserialize;
use tokio::sync::Mutex;
use tonic::transport::{Channel, Endpoint};
use tracing::{debug, error, info, instrument, warn};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// MQTT broker host[:port], e.g. `mosquitto.iot.local:8884`.
    #[serde(default = "default_mqtt_host")]
    pub mqtt_host: String,

    /// CA bundle the broker's certificate chains to.
    #[serde(default = "default_ca_path")]
    pub mqtt_ca: PathBuf,

    /// Adapter client certificate (PEM).
    #[serde(default = "default_client_cert")]
    pub mqtt_cert: PathBuf,

    /// Adapter client private key (PEM).
    #[serde(default = "default_client_key")]
    pub mqtt_key: PathBuf,

    /// MQTT topic filter the adapter subscribes to.
    #[serde(default = "default_subscribe")]
    pub subscribe: String,

    /// Registry gRPC endpoint. Plaintext localhost during dev; mTLS via Envoy
    /// arrives with W3c's service-to-service cert rotation story.
    #[serde(default = "default_registry_url")]
    pub registry_url: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mqtt_host: default_mqtt_host(),
            mqtt_ca: default_ca_path(),
            mqtt_cert: default_client_cert(),
            mqtt_key: default_client_key(),
            subscribe: default_subscribe(),
            registry_url: default_registry_url(),
        }
    }
}

fn default_mqtt_host() -> String {
    "127.0.0.1:8884".into()
}
fn default_ca_path() -> PathBuf {
    PathBuf::from("./tools/devcerts/generated/ca/ca.crt")
}
fn default_client_cert() -> PathBuf {
    PathBuf::from("./tools/devcerts/generated/zigbee-adapter/zigbee-adapter.crt")
}
fn default_client_key() -> PathBuf {
    PathBuf::from("./tools/devcerts/generated/zigbee-adapter/zigbee-adapter.key")
}
fn default_subscribe() -> String {
    "zigbee2mqtt/+".into()
}
fn default_registry_url() -> String {
    "http://127.0.0.1:50051".into()
}

type IdCache = Arc<Mutex<HashMap<String, PbUlid>>>;

pub async fn run(cfg: Config) -> Result<()> {
    info!(mqtt = %cfg.mqtt_host, registry = %cfg.registry_url, "zigbee2mqtt-adapter starting");

    let channel = Endpoint::from_shared(cfg.registry_url.clone())
        .context("parse registry URL")?
        .timeout(Duration::from_secs(10))
        .connect()
        .await
        .with_context(|| format!("connect to registry at {}", cfg.registry_url))?;
    let client = RegistryServiceClient::new(channel);

    // Warm the local external_id -> ULID cache so repeat payloads update
    // instead of tripping the (integration, external_id) UNIQUE constraint.
    let id_cache = Arc::new(Mutex::new(HashMap::new()));
    warm_cache(&mut client.clone(), &id_cache).await?;

    let mqtt = connect_mqtt(&cfg)?;
    let (mqtt_client, mut eventloop) = mqtt;
    mqtt_client
        .subscribe(&cfg.subscribe, QoS::AtLeastOnce)
        .await
        .context("mqtt subscribe")?;
    info!(subscribe = %cfg.subscribe, "subscribed");

    loop {
        tokio::select! {
            evt = eventloop.poll() => match evt {
                Ok(Event::Incoming(Packet::Publish(p))) => {
                    let topic = p.topic.clone();
                    let payload = p.payload.to_vec();
                    let client = client.clone();
                    let cache = id_cache.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_message(&topic, &payload, client, cache).await {
                            warn!(topic = %topic, error = %e, "message handling failed");
                        }
                    });
                }
                Ok(Event::Incoming(Packet::ConnAck(_))) => info!("mqtt connected"),
                Ok(_) => {}
                Err(e) => {
                    error!(error = %e, "mqtt eventloop error");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            },
            _ = tokio::signal::ctrl_c() => {
                info!("shutdown signal received");
                break;
            }
        }
    }
    Ok(())
}

#[instrument(skip(client, cache), fields(topic))]
async fn handle_message(
    topic: &str,
    payload: &[u8],
    mut client: RegistryServiceClient<Channel>,
    cache: IdCache,
) -> Result<()> {
    let Some(friendly) = translator::friendly_name_from_topic(topic) else {
        debug!("ignoring non-zigbee topic");
        return Ok(());
    };

    let mut translated =
        translator::translate(friendly, payload).with_context(|| format!("translate {topic}"))?;

    // Attach the cached ULID (if any) so the registry takes the update
    // branch rather than attempting an insert that would violate the
    // (integration, external_id) UNIQUE constraint.
    let cached_id = { cache.lock().await.get(friendly).cloned() };
    if let Some(id) = cached_id {
        translated.device.id = Some(id);
    }

    let resp = client
        .upsert_device(UpsertDeviceRequest {
            device: Some(translated.device),
            idempotency_key: String::new(),
        })
        .await
        .with_context(|| format!("upsert {friendly}"))?
        .into_inner();

    if let Some(d) = resp.device {
        if let Some(id) = d.id {
            cache.lock().await.insert(friendly.to_owned(), id);
        }
    }

    if resp.created {
        info!(device = %friendly, "registered new device");
    }
    Ok(())
}

async fn warm_cache(client: &mut RegistryServiceClient<Channel>, cache: &IdCache) -> Result<()> {
    let mut stream = client
        .list_devices(ListDevicesRequest {
            integration: "zigbee2mqtt".into(),
            room: String::new(),
        })
        .await?
        .into_inner();
    let mut n = 0usize;
    while let Some(msg) = stream.message().await? {
        if let Some(d) = msg.device {
            if let Some(id) = d.id {
                cache.lock().await.insert(d.external_id, id);
                n += 1;
            }
        }
    }
    info!(cached = n, "id cache warmed from registry");
    Ok(())
}

fn connect_mqtt(cfg: &Config) -> Result<(AsyncClient, rumqttc::EventLoop)> {
    let ca = std::fs::read(&cfg.mqtt_ca)
        .with_context(|| format!("read CA {}", cfg.mqtt_ca.display()))?;
    let cert = std::fs::read(&cfg.mqtt_cert)
        .with_context(|| format!("read cert {}", cfg.mqtt_cert.display()))?;
    let key = std::fs::read(&cfg.mqtt_key)
        .with_context(|| format!("read key {}", cfg.mqtt_key.display()))?;

    let (host, port) = parse_host_port(&cfg.mqtt_host)?;

    let mut opts = MqttOptions::new("iot-zigbee2mqtt-adapter", host, port);
    opts.set_keep_alive(Duration::from_secs(30));
    opts.set_transport(Transport::Tls(TlsConfiguration::Simple {
        ca,
        alpn: None,
        client_auth: Some((cert, key)),
    }));

    Ok(AsyncClient::new(opts, 64))
}

fn parse_host_port(s: &str) -> Result<(String, u16)> {
    let (h, p) = s
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("mqtt_host missing :port"))?;
    Ok((h.to_owned(), p.parse()?))
}
