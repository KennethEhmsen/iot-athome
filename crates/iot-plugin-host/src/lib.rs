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
pub mod mqtt;
pub mod mqtt_acl;
pub mod runtime;
pub mod supervisor;

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
    /// MQTT broker this host connects to on startup. When present,
    /// plugins declaring `capabilities.mqtt.*` get a live broker
    /// handle; when absent, those plugins still load but their
    /// `mqtt::subscribe` / `publish` calls degrade to dry-run logs
    /// (the capability check still enforces). See ADR-0013.
    #[serde(default)]
    pub mqtt: Option<crate::mqtt::MqttBrokerConfig>,
}

fn default_plugin_dir() -> String {
    "/var/lib/iotathome/plugins".into()
}

/// Runtime bindings the host feeds into every loaded plugin. All are
/// optional so unit / offline tests can exercise the capability path
/// without a live NATS server, on-disk audit log, or MQTT broker.
#[derive(Debug, Clone, Default)]
pub struct HostBindings {
    pub bus: Option<Bus>,
    pub audit: Option<Arc<AuditLog>>,
    /// Shared MQTT broker handle (per-host-process) — owns the rumqttc
    /// client and the `MqttRouter` that fans inbound messages out to
    /// plugins. Construct via `MqttBroker::connect(...)` at host
    /// startup; pass a clone of the `Arc` into every `load_plugin_dir`
    /// call.
    pub mqtt: Option<Arc<crate::mqtt::MqttBroker>>,
    /// gRPC channel to the registry service. M2-era plugins reached
    /// it via the `registry::upsert-device` host capability (ABI
    /// 1.2.0); that capability went away in ABI 1.3.0 (M5a W1) — the
    /// iot-registry bus-watcher (M3 W1.2) auto-registers devices
    /// from `device.>` publishes instead. The field stays for
    /// host-internal use (per-plugin admin RPCs in future) and may
    /// drop entirely in a later major.
    pub registry: Option<tonic::transport::Channel>,
    /// Shared HTTP client for the ABI 1.4.0 `net::http` host import.
    /// One per host process — `reqwest::Client` is internally
    /// `Arc<...>` and reuses its connection pool across clones.
    /// `None` in unit-test setups (the capability check still
    /// enforces; the call degrades to `net.unconfigured`).
    pub http: Option<reqwest::Client>,
}

/// Default request timeout for the ABI 1.4.0 `net::http` host import.
///
/// Per-plugin overrides land in a later minor; for now every outbound
/// HTTP call shares this ceiling so a misbehaving target can't pin a
/// plugin task indefinitely.
pub const DEFAULT_NET_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Build the shared outbound HTTP client used by the `net::http` host
/// import. Configured to:
///   * reject automatic redirects — a 3xx is surfaced verbatim so the
///     plugin (and its capability allow-list) governs the next URL.
///   * apply a [`DEFAULT_NET_TIMEOUT`] cap to every request.
///
/// Automatic decompression (gzip / brotli / deflate) is implicitly off:
/// the workspace `reqwest` is built with `default-features = false`, so
/// those decoder features are never enabled. Plugins that want
/// compressed responses set `Accept-Encoding` explicitly and decode the
/// body themselves.
///
/// rustls (workspace `reqwest` feature `rustls-tls`) is the only TLS
/// backend; OpenSSL is banned by ADR-0006 / `deny.toml`.
pub fn build_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(DEFAULT_NET_TIMEOUT)
        .build()
        .context("build outbound HTTP client")
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
    crate::component::iot::plugin_host::mqtt::add_to_linker::<_, HasSelf<PluginState>>(
        &mut linker,
        |s| s,
    )
    .context("link mqtt host")?;
    // ABI 1.4.0 (M5a follow-up): `net::http` host import — outbound
    // HTTP gated by manifest `capabilities.net.outbound` URL-prefix
    // allow-list. Strictly additive; 1.3.0 plugins that import
    // bus/log/mqtt only continue to load.
    crate::component::iot::plugin_host::net::add_to_linker::<_, HasSelf<PluginState>>(
        &mut linker,
        |s| s,
    )
    .context("link net host")?;
    // ABI 1.3.0 (M5a W1) removed the `registry::upsert-device`
    // capability — no `registry::add_to_linker` to call here. The
    // PluginState `registry` channel field remains for future per-
    // plugin admin RPCs.

    let state = PluginState {
        id: plugin_id.to_owned(),
        capabilities,
        bus: bindings.bus,
        audit: bindings.audit,
        mqtt: bindings.mqtt,
        registry: bindings.registry,
        http: bindings.http,
        // `self_tx` is filled in by `spawn_plugin_task` after the
        // mpsc channel is created — we can't know it here because
        // load_plugin is the synchronous side of construction.
        self_tx: None,
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

    // Connect the shared MQTT broker up-front (before any plugins
    // spawn), so every plugin's init() that calls `mqtt::subscribe`
    // sees a live router + client. `None` is a valid outcome — hosts
    // with no MQTT-speaking plugins skip the broker entirely and the
    // capability impls quietly degrade to dry-run.
    let mqtt = if let Some(mqtt_cfg) = &cfg.mqtt {
        let router = crate::mqtt::MqttRouter::new();
        info!(host = %mqtt_cfg.host, port = mqtt_cfg.port, "connecting MQTT broker");
        match crate::mqtt::MqttBroker::connect(mqtt_cfg.clone(), router).await {
            Ok(broker) => Some(broker),
            Err(e) => {
                // Don't abort the whole host on MQTT failure — non-MQTT
                // plugins are still useful. Log loud and let the
                // capability calls degrade.
                tracing::error!(
                    error = %format!("{e:#}"),
                    "MQTT broker connect failed — continuing without broker"
                );
                None
            }
        }
    } else {
        info!("no MQTT broker configured — mqtt.* host calls will dry-run");
        None
    };

    // Build the shared outbound HTTP client (ABI 1.4.0). Per-call
    // capability checks gate which plugins can reach which URL prefixes;
    // the client itself is process-wide so a noisy plugin can't starve
    // a quiet one of connection-pool slots beyond reqwest's defaults.
    // No redirects (a 3xx is surfaced verbatim — plugins decide), no
    // automatic decompression (plugins opt back in via headers if they
    // care), 10 s default timeout per request.
    let http = match build_http_client() {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::error!(
                error = %format!("{e:#}"),
                "outbound HTTP client init failed — net.http calls will return net.unconfigured"
            );
            None
        }
    };

    // Per-plugin supervisor tasks. Each one owns its restart loop, its
    // CrashTracker, and eventually its DLQ marker. The host binary's
    // role is just to spawn them and wait for ctrl-c.
    let bindings = HostBindings {
        mqtt,
        http,
        ..HostBindings::default()
    };
    let mut supervisor_tasks = Vec::with_capacity(found.len());
    for dir in found {
        let engine = engine.clone();
        let bindings = bindings.clone();
        let dir_for_log = dir.clone();
        supervisor_tasks.push(tokio::spawn(async move {
            if let Err(e) = supervisor::supervise(engine, dir, bindings).await {
                tracing::error!(
                    dir = %dir_for_log.display(),
                    error = %format!("{e:#}"),
                    "supervisor exited with error"
                );
            }
        }));
    }

    tokio::signal::ctrl_c().await?;
    info!("iot-plugin-host shutting down");
    for task in supervisor_tasks {
        task.abort();
    }
    Ok(())
}
