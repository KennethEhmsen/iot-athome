use anyhow::Result;
use iot_observability::Config as ObsConfig;
use iot_registry::Config;

#[tokio::main]
async fn main() -> Result<()> {
    iot_observability::init(&ObsConfig {
        service_name: "iot-registry".into(),
        service_version: env!("CARGO_PKG_VERSION").into(),
        otlp_endpoint: std::env::var("IOT_OTLP_ENDPOINT").ok(),
    })?;

    let paths = iot_config::Paths::default();
    let cfg: Config = iot_config::load(&paths).unwrap_or_default();

    let result = iot_registry::run(cfg).await;
    iot_observability::shutdown();
    result
}
