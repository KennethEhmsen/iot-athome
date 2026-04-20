//! Device Registry library.
//!
//! Exposes a `run()` entry point called by the thin `main.rs`. Keeping the
//! service's logic in a library lets integration tests spawn it in-process
//! without shell gymnastics.

#![forbid(unsafe_code)]

use anyhow::Result;
use serde::Deserialize;
use tracing::info;

/// Service configuration. Deserialized via `iot-config` from the layered
/// sources described in ADR-0010.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Where to listen for gRPC/HTTP admin traffic, e.g. `127.0.0.1:50051`.
    #[serde(default = "default_listen")]
    pub listen: String,

    /// Database URL. `sqlite://...` for small deployments, `postgres://...`
    /// for large. See ADR-0007 for migration semantics.
    #[serde(default = "default_db")]
    pub database_url: String,
}

fn default_listen() -> String {
    "127.0.0.1:50051".into()
}

fn default_db() -> String {
    "sqlite:///var/lib/iotathome/registry.db".into()
}

/// Run the registry service to completion. Returns on graceful shutdown.
pub async fn run(_cfg: Config) -> Result<()> {
    // W1 stub: prove the wiring compiles; real implementation lands in W2.
    info!("iot-registry starting (W1 stub — no-op)");

    tokio::signal::ctrl_c().await?;
    info!("iot-registry shutting down");
    Ok(())
}
