//! HTTP + WebSocket gateway.
//!
//! Behind Envoy. Speaks REST on `/api/v1/*`, WS on `/stream`, health on
//! `/healthz`. Forwards REST to the registry via gRPC; `/stream` bridges
//! selected NATS subjects out to browser clients as JSON.
//!
//! OIDC bearer-token validation lands in W3b. In W3a the gateway trusts
//! its upstream (iotctl over localhost, or Envoy once certs land).

#![forbid(unsafe_code)]

pub mod auth;
pub mod handlers;
pub mod json;
pub mod state;
pub mod stream;
pub mod tracing_mw;

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context as _, Result};
use axum::middleware;
use axum::routing::get;
use axum::Router;
use iot_proto::iot::registry::v1::registry_service_client::RegistryServiceClient;
use serde::Deserialize;
use tokio::net::TcpListener;
use tonic::transport::Endpoint;
use tracing::info;

use crate::auth::{OidcConfig, Verifier};
use crate::tracing_mw::traceparent_mw;

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

    /// Optional OIDC bearer-token verification. When absent the gateway
    /// runs in dev mode (no auth). When present, every `/api/v1/*` and
    /// `/stream` request must carry a valid RS256 JWT from `issuer_url`.
    #[serde(default)]
    pub oidc: Option<OidcConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            registry_url: default_registry_url(),
            bus: None,
            oidc: None,
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

    // `with_interceptor` stamps the task-local TraceContext onto the
    // outbound gRPC metadata; the gateway's `traceparent_mw`
    // populates that task-local on every inbound request, so the
    // registry sees the same trace id (M3 post-v0.3.0 follow-up).
    let registry_client: state::RegistryClient = RegistryServiceClient::with_interceptor(
        channel,
        state::inject_traceparent as state::TraceparentInterceptor,
    );

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

    let verifier = cfg.oidc.clone().map(|o| {
        info!(issuer = %o.issuer_url, aud = %o.audience, "OIDC bearer verification enabled");
        Verifier::new(o)
    });

    let state = state::AppState {
        registry_client,
        bus,
        verifier,
    };

    // REST routes under /api/v1 are gated by Bearer header. Middleware is
    // a no-op when state.verifier is None (dev mode).
    let rest = Router::new()
        .route("/api/v1/version", get(handlers::version))
        .route(
            "/api/v1/devices",
            get(handlers::list_devices).post(handlers::upsert_device),
        )
        .route(
            "/api/v1/devices/{id}",
            get(handlers::get_device).delete(handlers::delete_device),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::bearer_middleware,
        ));

    let app = Router::new()
        .route("/healthz", get(handlers::health))
        // `/stream` self-authenticates via ?token= (WS handshakes can't set
        // Authorization headers on the browser side).
        .route("/stream", get(stream::stream_handler))
        .merge(rest)
        // Applied to every route — extracts inbound W3C traceparent
        // (or mints a fresh root) and scopes the handler in a
        // TraceContext so bus publishes from the handler inherit it.
        .layer(middleware::from_fn(traceparent_mw))
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
