//! `iotctl` — operator admin CLI.
//!
//! `iotctl ping` is the walking-skeleton smoke test: mTLS-connect to NATS,
//! publish on a unique subject, read the message back via a matching
//! subscription, and print the round-trip time. If this works, the trust
//! store, broker config, and bus wrapper are all healthy.

use anyhow::{anyhow, Context as _, Result};
use clap::{Parser, Subcommand};
use futures::StreamExt as _;
use iot_bus::{Bus, Config as BusConfig};
use iot_observability::Config as ObsConfig;
use std::time::{Duration, Instant};

/// IoT-AtHome admin CLI.
#[derive(Debug, Parser)]
#[command(name = "iotctl", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the CLI and protocol versions.
    Version,
    /// mTLS-connect to NATS, round-trip a message, print RTT.
    /// Requires `just dev` to be running.
    Ping,
    /// Device management (W1: stubs).
    #[command(subcommand)]
    Device(DeviceCmd),
}

#[derive(Debug, Subcommand)]
enum DeviceCmd {
    /// List devices.
    List,
    /// Fetch a single device by ULID.
    Get { id: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    iot_observability::init(&ObsConfig {
        service_name: "iotctl".into(),
        service_version: env!("CARGO_PKG_VERSION").into(),
        otlp_endpoint: None,
    })?;

    let cli = Cli::parse();
    let result = match cli.command {
        Command::Version => cmd_version(),
        Command::Ping => cmd_ping().await,
        Command::Device(DeviceCmd::List) => {
            println!("device list - W1 stub (registry lands W2)");
            Ok(())
        }
        Command::Device(DeviceCmd::Get { id }) => {
            println!("device get {id} - W1 stub (registry lands W2)");
            Ok(())
        }
    };

    iot_observability::shutdown();
    result
}

fn cmd_version() -> Result<()> {
    println!("iotctl {}", env!("CARGO_PKG_VERSION"));
    println!("device schema v{}", iot_core::DEVICE_SCHEMA_VERSION);
    Ok(())
}

async fn cmd_ping() -> Result<()> {
    let cfg = BusConfig::from_env("iotctl");
    println!("connecting to {} (component={})", cfg.url, cfg.publisher);

    let bus = Bus::connect(cfg)
        .await
        .context("connect — is `just dev` running and did you `just certs`?")?;

    let unique = ulid::Ulid::new().to_string().to_lowercase();
    let subject = format!("sys.iotctl.ping.{unique}");

    // Subscribe to the wildcard so the server has to route our own publish
    // back through a subscription — proves end-to-end pub/sub, not just
    // client connectivity.
    let mut sub = bus
        .raw()
        .subscribe("sys.iotctl.ping.>".to_string())
        .await
        .context("subscribe")?;

    // Flush guarantees the subscription is registered on the server before
    // we publish — without this, the publish can beat the SUB to the broker.
    bus.raw().flush().await.context("flush subscribe")?;

    let start = Instant::now();
    bus.publish_proto(&subject, "iot.sys.v1.Ping", unique.clone().into_bytes(), None)
        .await
        .context("publish")?;

    let msg = tokio::time::timeout(Duration::from_secs(2), sub.next())
        .await
        .map_err(|_| anyhow!("no echo received within 2s"))?
        .ok_or_else(|| anyhow!("subscription closed before receiving a message"))?;
    let rtt = start.elapsed();

    if msg.subject.as_str() != subject {
        println!(
            "warning: echoed subject mismatch (sent {subject}, received {})",
            msg.subject
        );
    }

    println!("pong: subject={} bytes={} rtt={:?}", msg.subject, msg.payload.len(), rtt);
    Ok(())
}
