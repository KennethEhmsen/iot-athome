//! Device Registry service.
//!
//! Persists the canonical device record (ADR-0005) in SQLite, serves it over
//! gRPC (the `iot.registry.v1.RegistryService`), emits state events on NATS,
//! and hash-chains every mutation into the audit log.

#![forbid(unsafe_code)]

pub mod bus_watcher;
pub mod repo;
pub mod service;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use iot_audit::AuditLog;
use iot_proto::iot::registry::v1::registry_service_server::RegistryServiceServer;
use serde::Deserialize;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tonic::transport::Server;
use tracing::info;

/// Service configuration. Deserialized via `iot-config` (ADR-0010).
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// gRPC listen address.
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,

    /// SQLite database URL. Example: `sqlite://state/registry.db`.
    /// The file is created if missing.
    #[serde(default = "default_db")]
    pub database_url: String,

    /// Where the hash-chained audit log is appended.
    #[serde(default = "default_audit_path")]
    pub audit_path: PathBuf,

    /// NATS connection (optional — if `None`, registry runs without bus
    /// publishing; gRPC CRUD still works). Relevant for unit-test runs.
    #[serde(default)]
    pub bus: Option<iot_bus::Config>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            database_url: default_db(),
            audit_path: default_audit_path(),
            bus: None,
        }
    }
}

fn default_listen() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 50051))
}

fn default_db() -> String {
    "sqlite://var/registry.db".into()
}

fn default_audit_path() -> PathBuf {
    PathBuf::from("var/registry.audit.jsonl")
}

/// Run the registry service. Returns on SIGINT / Ctrl-C.
pub async fn run(cfg: Config) -> Result<()> {
    info!(listen = %cfg.listen, db = %cfg.database_url, "iot-registry starting");

    // Create the SQLite database on-demand; apply migrations.
    let connect_opts = cfg
        .database_url
        .parse::<SqliteConnectOptions>()
        .context("parse database_url")?
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(connect_opts)
        .await
        .context("connect sqlite")?;

    sqlx::migrate!("./migrations/sqlite")
        .run(&pool)
        .await
        .context("run migrations")?;
    info!("migrations applied");

    if let Some(parent) = cfg.audit_path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    let audit = AuditLog::open(&cfg.audit_path)
        .await
        .context("open audit log")?;

    let bus = if let Some(bus_cfg) = cfg.bus.clone() {
        match iot_bus::Bus::connect(bus_cfg).await {
            Ok(b) => {
                info!("connected to bus");
                Some(b)
            }
            Err(e) => {
                tracing::warn!(error = %e, "bus connect failed — running without bus publishing");
                None
            }
        }
    } else {
        None
    };

    let repo = repo::DeviceRepo::new(pool.clone());
    let svc = service::RegistrySvc::new(pool, audit, bus.clone());
    let server = RegistryServiceServer::new(svc);

    // Spawn the bus watcher alongside the gRPC server. This is the
    // mechanism that retires M2's `registry::upsert-device` host
    // capability — plugins will start publishing state with their
    // native id in the subject and the registry notices on its own
    // (see ADR-0013 §Consequences). Skipped if the host was started
    // without a bus; gRPC-only test setups stay clean.
    if let Some(bus_for_watcher) = bus {
        let watcher = bus_watcher::BusWatcher::new(bus_for_watcher, repo);
        tokio::spawn(async move {
            if let Err(e) = watcher.run().await {
                tracing::error!(
                    error = %format!("{e:#}"),
                    "bus watcher exited with error"
                );
            }
        });
        info!("bus watcher spawned");
    } else {
        info!("no bus configured — auto-register-on-bus disabled");
    }

    info!("gRPC server listening");
    Server::builder()
        .trace_fn(|_| tracing::info_span!("registry.rpc"))
        .add_service(server)
        .serve_with_shutdown(cfg.listen, async {
            let _ = tokio::signal::ctrl_c().await;
            info!("shutdown signal received");
        })
        .await
        .context("serve")?;

    Ok(())
}
