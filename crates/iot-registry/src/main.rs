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
    let cfg: Config = iot_config::load(&paths).unwrap_or(Config {
        listen: "127.0.0.1:50051".into(),
        database_url: "sqlite::memory:".into(),
    });

    let result = iot_registry::run(cfg).await;
    iot_observability::shutdown();
    result
}
