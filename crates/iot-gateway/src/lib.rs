//! HTTP + WebSocket gateway.
//!
//! The gateway lives behind Envoy (see `deploy/compose/envoy/envoy.yaml`).
//! Envoy handles TLS termination + OIDC + audit; this binary speaks business
//! endpoints: REST over `/api/v1/*`, a streaming WS at `/stream`, and health
//! probes on `/healthz`.

#![forbid(unsafe_code)]

use anyhow::Result;
use axum::{routing::get, Router};
use serde::Deserialize;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing::info;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: default_listen(),
        }
    }
}

fn default_listen() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 8081))
}

pub async fn run(cfg: Config) -> Result<()> {
    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/v1/version", get(version));

    info!(listen = %cfg.listen, "iot-gateway starting (W1 stub — endpoints land W3)");

    let listener = TcpListener::bind(cfg.listen).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

async fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
