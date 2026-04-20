//! Automation engine.
//!
//! Compiles declarative rules (YAML/JSON) into a DAG, evaluates their
//! conditions through a sandboxed CEL interpreter, and dispatches actions as
//! idempotent commands on the bus. W1 stub only — engine lands in M3.

#![forbid(unsafe_code)]

use anyhow::Result;
use serde::Deserialize;
use tracing::info;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub rules_dir: Option<String>,
}

pub async fn run(_cfg: Config) -> Result<()> {
    info!("iot-automation starting (W1 stub — rules engine lands M3)");
    tokio::signal::ctrl_c().await?;
    info!("iot-automation shutting down");
    Ok(())
}
