//! HTTP + WebSocket gateway.
//!
//! Behind Envoy. Speaks REST on `/api/v1/*`, WS on `/stream`, health on
//! `/healthz`. Forwards REST to the registry via gRPC; `/stream` bridges
//! selected NATS subjects out to browser clients as JSON.
//!
//! OIDC bearer-token validation lands in W3b. In W3a the gateway trusts
//! its upstream (iotctl over localhost, or Envoy once certs land).

#![forbid(unsafe_code)]

pub mod handlers;
pub mod json;
pub mod state;
pub mod stream;

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context as _, Result};
use axum::routing::get;
use axum::Router;
use iot_proto::iot::registry::v1::registry_service_client::RegistryServiceClient;
use serde::Deserialize;
use tokio::net::TcpListener;
use tonic::transport::Endpoint;
use tracing::info;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,

    /// Where to reach `iot-registry` over gRPC. Plaintext localhost by
    /// default; mTLS story lands with Envoy wiring.
    #[serde(default = "default_registry_url")]
    pub registry_url: String,

    /// Optional NATS connection for the `/stream` endpoint.
    #[serde(default)]
    pub bus: Option<iot_bus::Config>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            registry_url: default_registry_url(),
            bus: None,
        }
    }
}

fn default_listen() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 8081))
}

fn default_registry_url() -> String {
    "http://127.0.0.1:50051".into()
}

pub async fn run(cfg: Config) -> Result<()> {
    info!(listen = %cfg.listen, registry = %cfg.registry_url, "iot-gateway starting");

    let channel = Endpoint::from_shared(cfg.registry_url.clone())
        .context("invalid registry URL")?
        .timeout(Duration::from_secs(10))
        .connect()
        .await
        .with_context(|| format!("connect to registry at {}", cfg.registry_url))?;

    let registry_client = RegistryServiceClient::new(channel);

    let bus = if let Some(bus_cfg) = cfg.bus {
        match iot_bus::Bus::connect(bus_cfg).await {
            Ok(b) => {
                info!("connected to bus");
                Some(b)
            }
            Err(e) => {
                tracing::warn!(error = %e, "bus connect failed — /stream will return an error");
                None
            }
        }
    } else {
        None
    };

    let state = state::AppState {
        registry_client,
        bus,
    };

    let app = Router::new()
        .route("/healthz", get(handlers::health))
        .route("/api/v1/version", get(handlers::version))
        .route(
            "/api/v1/devices",
            get(handlers::list_devices).post(handlers::upsert_device),
        )
        .route(
            "/api/v1/devices/{id}",
            get(handlers::get_device).delete(handlers::delete_device),
        )
        .route("/stream", get(stream::stream_handler))
        .with_state(state);

    let listener = TcpListener::bind(cfg.listen).await?;
    info!("listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
