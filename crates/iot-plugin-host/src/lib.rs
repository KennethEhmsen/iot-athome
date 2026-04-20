//! Plugin host.
//!
//! Supervises WASM Component plugins via Wasmtime, enforcing manifest-declared
//! capabilities at every host call. A parallel code path supervises
//! OCI-container plugins (hardware-access escape hatch, see ADR-0003) — not
//! present in W1.

#![forbid(unsafe_code)]

use anyhow::Result;
use serde::Deserialize;
use tracing::info;
use wasmtime::{Config as WtConfig, Engine};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    /// Directory scanned for installed plugins.
    #[serde(default = "default_plugin_dir")]
    pub plugin_dir: String,
}

fn default_plugin_dir() -> String {
    "/var/lib/iotathome/plugins".into()
}

pub async fn run(_cfg: Config) -> Result<()> {
    let mut wt = WtConfig::new();
    wt.wasm_component_model(true);
    wt.async_support(true);
    // `consume_fuel` gives us per-plugin CPU metering (see ADR-0003 §resources).
    wt.consume_fuel(true);

    let _engine = Engine::new(&wt)?;
    info!("iot-plugin-host starting (W1 stub — engine built, no plugins loaded)");

    tokio::signal::ctrl_c().await?;
    info!("iot-plugin-host shutting down");
    Ok(())
}
