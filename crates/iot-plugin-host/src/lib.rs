//! Plugin host.
//!
//! Supervises WASM Component plugins via Wasmtime, enforcing manifest-declared
//! capabilities at every host call. A parallel code path supervises
//! OCI-container plugins (hardware-access escape hatch, see ADR-0003) — not
//! present in M2.

#![forbid(unsafe_code)]

pub mod capabilities;
pub mod component;
pub mod manifest;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use iot_audit::AuditLog;
use iot_bus::Bus;
use serde::Deserialize;
use tracing::info;
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config as WtConfig, Engine, Store};

use crate::capabilities::CapabilityMap;
use crate::component::{Plugin, PluginState};
use crate::manifest::Manifest;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    /// Directory scanned for installed plugins.
    #[serde(default = "default_plugin_dir")]
    pub plugin_dir: String,
}

fn default_plugin_dir() -> String {
    "/var/lib/iotathome/plugins".into()
}

/// Runtime bindings the host feeds into every loaded plugin. Both are
/// optional so unit / offline tests can exercise the capability path
/// without a live NATS server or on-disk audit log.
#[derive(Debug, Clone, Default)]
pub struct HostBindings {
    pub bus: Option<Bus>,
    pub audit: Option<Arc<AuditLog>>,
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

/// Load a plugin from its install directory.
///
/// The directory must contain:
///   * `manifest.yaml`  — see schemas/plugin-manifest.schema.json
///   * `<manifest.entrypoint>` — the .wasm component (usually `plugin.wasm`)
///
/// Capabilities + resource limits come straight from the manifest. The
/// returned `Store` is already fueled and the component instantiated; the
/// caller invokes exports via the returned [`Plugin`] handle.
pub async fn load_plugin_dir(
    engine: &Engine,
    plugin_dir: impl AsRef<Path>,
    bindings: HostBindings,
) -> Result<(Store<PluginState>, Plugin, Manifest)> {
    let plugin_dir = plugin_dir.as_ref();
    let manifest = Manifest::load(plugin_dir.join("manifest.yaml"))
        .with_context(|| format!("load manifest from {}", plugin_dir.display()))?;
    let wasm_path = plugin_dir.join(&manifest.entrypoint);
    let (store, plugin) = load_plugin(
        engine,
        &wasm_path,
        &manifest.id,
        manifest.capabilities.clone(),
        bindings,
    )
    .await?;
    Ok((store, plugin, manifest))
}

/// Lower-level loader: explicit `wasm_path` + already-parsed capabilities.
///
/// Callers that need manifest-aware install logic use [`load_plugin_dir`]
/// instead. This entry point exists for tests and for the install path
/// that's already parsed the manifest for signing / SBOM checks.
pub async fn load_plugin(
    engine: &Engine,
    wasm_path: impl AsRef<Path>,
    plugin_id: &str,
    capabilities: CapabilityMap,
    bindings: HostBindings,
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
        bus: bindings.bus,
        audit: bindings.audit,
        wasi: wasmtime_wasi::WasiCtxBuilder::new()
            .inherit_stderr()
            .build(),
        table: wasmtime::component::ResourceTable::default(),
    };
    let mut store = Store::new(engine, state);
    // Fuel is budgeted per host call in a later milestone; 1 billion is
    // plenty for instantiation + a few host calls.
    store.set_fuel(1_000_000_000).context("set fuel")?;

    let plugin = Plugin::instantiate_async(&mut store, &component, &linker)
        .await
        .context("instantiate component")?;
    Ok((store, plugin))
}

/// Walk `plugin_dir` for child directories each containing a `manifest.yaml`.
/// Returns their absolute paths. Used by [`run`] at startup.
pub fn discover_plugins(plugin_dir: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
    let dir = plugin_dir.as_ref();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.path().join("manifest.yaml").exists() {
            out.push(entry.path());
        }
    }
    Ok(out)
}

pub async fn run(cfg: Config) -> Result<()> {
    let engine = build_engine()?;
    let found = discover_plugins(&cfg.plugin_dir)?;
    info!(
        plugin_dir = %cfg.plugin_dir,
        discovered = found.len(),
        "iot-plugin-host starting"
    );
    // Real plugin supervision (spawning + watchdog) arrives in W4. For W2
    // the binary loads + logs the plugins it found.
    for dir in &found {
        match load_plugin_dir(&engine, dir, HostBindings::default()).await {
            Ok((_store, _plugin, m)) => {
                info!(plugin = %m.id, version = %m.version, "loaded");
            }
            Err(e) => tracing::warn!(error = %e, dir = %dir.display(), "load failed"),
        }
    }
    tokio::signal::ctrl_c().await?;
    info!("iot-plugin-host shutting down");
    Ok(())
}
