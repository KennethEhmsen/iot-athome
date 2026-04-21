//! Plugin host.
//!
//! Supervises WASM Component plugins via Wasmtime, enforcing manifest-declared
//! capabilities at every host call. A parallel code path supervises
//! OCI-container plugins (hardware-access escape hatch, see ADR-0003) — not
//! present in M2.

#![forbid(unsafe_code)]

pub mod capabilities;
pub mod component;

use std::path::Path;

use anyhow::{Context as _, Result};
use serde::Deserialize;
use tracing::info;
use wasmtime::component::{Component, Linker};
use wasmtime::{Config as WtConfig, Engine, Store};

use crate::capabilities::CapabilityMap;
use crate::component::{Plugin, PluginState};
use wasmtime::component::HasSelf;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    /// Directory scanned for installed plugins.
    #[serde(default = "default_plugin_dir")]
    pub plugin_dir: String,
}

fn default_plugin_dir() -> String {
    "/var/lib/iotathome/plugins".into()
}

/// Construct a Wasmtime Engine preconfigured for the Component Model + async
/// + fuel-based CPU metering.
pub fn build_engine() -> Result<Engine> {
    let mut wt = WtConfig::new();
    wt.wasm_component_model(true);
    wt.async_support(true);
    // `consume_fuel` gives us per-plugin CPU metering (see ADR-0003 §resources).
    wt.consume_fuel(true);
    Engine::new(&wt).context("build wasmtime engine")
}

/// Load a component from disk, instantiate it under a fresh Store, and
/// return both halves so the caller can invoke exports.
pub async fn load_plugin(
    engine: &Engine,
    wasm_path: impl AsRef<Path>,
    plugin_id: &str,
    capabilities: CapabilityMap,
) -> Result<(Store<PluginState>, Plugin)> {
    let bytes = std::fs::read(wasm_path.as_ref())
        .with_context(|| format!("read plugin {}", wasm_path.as_ref().display()))?;
    let component = Component::from_binary(engine, &bytes).context("parse component")?;

    // `HasSelf<PluginState>` tells wasmtime that PluginState itself carries
    // the Host-trait impls. See wasmtime::component::HasSelf docs.
    let mut linker = Linker::<PluginState>::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker).context("link wasi p2")?;
    crate::component::iot::plugin_host::bus::add_to_linker::<_, HasSelf<PluginState>>(
        &mut linker,
        |s| s,
    )
    .context("link bus host")?;
    crate::component::iot::plugin_host::log::add_to_linker::<_, HasSelf<PluginState>>(
        &mut linker,
        |s| s,
    )
    .context("link log host")?;

    let state = PluginState {
        id: plugin_id.to_owned(),
        capabilities,
        wasi: wasmtime_wasi::WasiCtxBuilder::new()
            .inherit_stderr()
            .build(),
        table: wasmtime::component::ResourceTable::default(),
    };
    let mut store = Store::new(engine, state);
    // Fuel is budgeted per host call in M2 W2; plenty for instantiation.
    store.set_fuel(1_000_000_000).context("set fuel")?;

    let plugin = Plugin::instantiate_async(&mut store, &component, &linker)
        .await
        .context("instantiate component")?;
    Ok((store, plugin))
}

pub async fn run(_cfg: Config) -> Result<()> {
    let _engine = build_engine()?;
    info!("iot-plugin-host starting — engine ready, no plugins loaded (M2 W1)");
    tokio::signal::ctrl_c().await?;
    info!("iot-plugin-host shutting down");
    Ok(())
}
