//! `iotctl` — operator admin CLI.
//!
//! * `iotctl ping`: mTLS-connect to NATS, round-trip a message, print RTT.
//! * `iotctl device add <integration> <external-id> [--label ...]`:
//!   upserts a device via the registry gRPC.
//! * `iotctl device list [--integration ...] [--room ...]`: streams devices.
//! * `iotctl device get <ulid>`: fetches one device.

use anyhow::{anyhow, Context as _, Result};
use clap::{Parser, Subcommand};
use futures::StreamExt as _;
use iot_bus::{Bus, Config as BusConfig};
use iot_observability::Config as ObsConfig;
use iot_proto::iot::common::v1::Ulid as PbUlid;
use iot_proto::iot::device::v1::{Device, TrustLevel};
use iot_proto::iot::registry::v1::registry_service_client::RegistryServiceClient;
use iot_proto::iot::registry::v1::{
    DeleteDeviceRequest, GetDeviceRequest, ListDevicesRequest, UpsertDeviceRequest,
};
use std::time::{Duration, Instant};
use tonic::transport::{Channel, Endpoint};

#[derive(Debug, Parser)]
#[command(name = "iotctl", version, about)]
struct Cli {
    /// Registry gRPC endpoint (takes precedence over IOT_REGISTRY_URL).
    #[arg(
        long,
        env = "IOT_REGISTRY_URL",
        default_value = "http://127.0.0.1:50051"
    )]
    registry: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the CLI and protocol versions.
    Version,
    /// mTLS-connect to NATS, round-trip a message, print RTT.
    Ping,
    /// Device management (via registry gRPC).
    #[command(subcommand)]
    Device(DeviceCmd),
}

#[derive(Debug, Subcommand)]
enum DeviceCmd {
    /// Upsert a device. Omit --id to let the registry mint one.
    Add {
        /// Plugin integration id (e.g. `zigbee`, `demo-echo`).
        integration: String,
        /// Protocol-native identifier.
        external_id: String,
        #[arg(long)]
        id: Option<String>,
        #[arg(long, default_value = "")]
        manufacturer: String,
        #[arg(long, default_value = "")]
        model: String,
        #[arg(long, default_value = "")]
        label: String,
        #[arg(long, value_delimiter = ',')]
        rooms: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        capabilities: Vec<String>,
    },
    /// List devices.
    List {
        #[arg(long, default_value = "")]
        integration: String,
        #[arg(long, default_value = "")]
        room: String,
    },
    /// Fetch a single device by ULID.
    Get { id: String },
    /// Remove a device by ULID.
    Delete { id: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    iot_observability::init(&ObsConfig {
        service_name: "iotctl".into(),
        service_version: env!("CARGO_PKG_VERSION").into(),
        otlp_endpoint: None,
    })?;

    let cli = Cli::parse();
    let result = match &cli.command {
        Command::Version => cmd_version(),
        Command::Ping => cmd_ping().await,
        Command::Device(sub) => cmd_device(&cli.registry, sub).await,
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
        .context("connect - is `just dev` running and did you `just certs`?")?;

    let unique = ulid::Ulid::new().to_string().to_lowercase();
    let subject = format!("sys.iotctl.ping.{unique}");

    let mut sub = bus.raw().subscribe("sys.iotctl.ping.>".to_string()).await?;
    bus.raw().flush().await?;

    let start = Instant::now();
    bus.publish_proto(
        &subject,
        "iot.sys.v1.Ping",
        unique.clone().into_bytes(),
        None,
    )
    .await?;

    let msg = tokio::time::timeout(Duration::from_secs(2), sub.next())
        .await
        .map_err(|_| anyhow!("no echo received within 2s"))?
        .ok_or_else(|| anyhow!("subscription closed"))?;
    let rtt = start.elapsed();

    println!(
        "pong: subject={} bytes={} rtt={:?}",
        msg.subject,
        msg.payload.len(),
        rtt
    );
    Ok(())
}

async fn cmd_device(registry_url: &str, sub: &DeviceCmd) -> Result<()> {
    let endpoint = Endpoint::from_shared(registry_url.to_string())
        .context("invalid registry URL")?
        .timeout(Duration::from_secs(10));
    let channel: Channel = endpoint
        .connect()
        .await
        .with_context(|| format!("connect to registry at {registry_url}"))?;
    let mut client = RegistryServiceClient::new(channel);

    match sub {
        DeviceCmd::Add {
            integration,
            external_id,
            id,
            manufacturer,
            model,
            label,
            rooms,
            capabilities,
        } => {
            let device = Device {
                id: id.as_ref().map(|s| PbUlid { value: s.clone() }),
                integration: integration.clone(),
                external_id: external_id.clone(),
                manufacturer: manufacturer.clone(),
                model: model.clone(),
                label: label.clone(),
                rooms: rooms.clone(),
                capabilities: capabilities.clone(),
                entities: Vec::new(),
                trust_level: TrustLevel::UserAdded.into(),
                schema_version: iot_core::DEVICE_SCHEMA_VERSION,
                plugin_meta: Default::default(),
                last_seen: None,
            };
            let resp = client
                .upsert_device(UpsertDeviceRequest {
                    device: Some(device),
                    idempotency_key: String::new(),
                })
                .await?
                .into_inner();
            if let Some(d) = &resp.device {
                let id = d.id.as_ref().map(|u| u.value.as_str()).unwrap_or("");
                let verb = if resp.created { "created" } else { "updated" };
                println!("{verb} {id} ({})", d.integration);
            }
        }
        DeviceCmd::List { integration, room } => {
            let mut stream = client
                .list_devices(ListDevicesRequest {
                    integration: integration.clone(),
                    room: room.clone(),
                })
                .await?
                .into_inner();
            let mut n = 0usize;
            while let Some(msg) = stream.message().await? {
                if let Some(d) = msg.device {
                    let id = d.id.as_ref().map(|u| u.value.as_str()).unwrap_or("");
                    println!("{id:<28} {:<12} {}", d.integration, label_or(&d));
                    n += 1;
                }
            }
            if n == 0 {
                println!("(no devices)");
            }
        }
        DeviceCmd::Get { id } => {
            let resp = client
                .get_device(GetDeviceRequest {
                    id: Some(PbUlid { value: id.clone() }),
                })
                .await?
                .into_inner();
            let Some(d) = resp.device else {
                return Err(anyhow!("empty response"));
            };
            println!("{}", serde_json::to_string_pretty(&to_json(&d))?);
        }
        DeviceCmd::Delete { id } => {
            let resp = client
                .delete_device(DeleteDeviceRequest {
                    id: Some(PbUlid { value: id.clone() }),
                })
                .await?
                .into_inner();
            println!(
                "{}: {}",
                id,
                if resp.deleted { "deleted" } else { "not found" }
            );
        }
    }
    Ok(())
}

fn label_or(d: &Device) -> &str {
    if !d.label.is_empty() {
        &d.label
    } else if !d.model.is_empty() {
        &d.model
    } else {
        &d.external_id
    }
}

fn to_json(d: &Device) -> serde_json::Value {
    let id = d.id.as_ref().map(|u| u.value.as_str()).unwrap_or("");
    serde_json::json!({
        "id": id,
        "integration": d.integration,
        "external_id": d.external_id,
        "manufacturer": d.manufacturer,
        "model": d.model,
        "label": d.label,
        "rooms": d.rooms,
        "capabilities": d.capabilities,
        "trust_level": d.trust_level,
        "schema_version": d.schema_version,
    })
}
