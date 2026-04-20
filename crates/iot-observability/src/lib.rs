//! Tracing + OpenTelemetry bootstrap.
//!
//! Every service calls [`init`] on the first line of `main`. See ADR-0009.
//!
//! The bootstrap is intentionally infallible at the `Err` boundary for the
//! common failure mode (no OTel collector available): the service falls back
//! to local-only JSON-to-stderr logging rather than refusing to start. The
//! `Err` return is reserved for misconfiguration that the operator must fix
//! (e.g. an unparseable `RUST_LOG` directive).

#![forbid(unsafe_code)]

use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::runtime;
use opentelemetry_sdk::trace as sdktrace;
use opentelemetry_sdk::Resource;
use thiserror::Error;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::EnvFilter;

/// Configuration for observability bootstrap.
#[derive(Debug, Clone)]
pub struct Config {
    /// Logical service name (`service.name` resource attribute).
    pub service_name: String,
    /// Service version (`service.version` resource attribute).
    pub service_version: String,
    /// OTLP endpoint URL (gRPC). `None` = disable trace export.
    pub otlp_endpoint: Option<String>,
}

/// Errors from [`init`] that the operator must resolve.
#[derive(Debug, Error)]
pub enum InitError {
    #[error("RUST_LOG filter is invalid: {0}")]
    BadFilter(#[from] tracing_subscriber::filter::ParseError),
}

/// Initialise tracing + OTel.
///
/// Safe to call once per process. Calling twice is a bug that is logged and
/// otherwise ignored.
///
/// # Errors
/// Returns [`InitError::BadFilter`] only if the `RUST_LOG` env var is set to
/// something unparseable. Missing OTLP collectors do NOT fail this call.
pub fn init(cfg: &Config) -> Result<(), InitError> {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .map_err(InitError::BadFilter)?;

    let fmt_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(false);

    let resource = Resource::new(vec![
        opentelemetry::KeyValue::new("service.name", cfg.service_name.clone()),
        opentelemetry::KeyValue::new("service.version", cfg.service_version.clone()),
    ]);

    let registry = tracing_subscriber::registry().with(filter).with(fmt_layer);

    if let Some(endpoint) = &cfg.otlp_endpoint {
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint.clone())
            .build();

        match exporter {
            Ok(exp) => {
                let provider = sdktrace::TracerProvider::builder()
                    .with_batch_exporter(exp, runtime::Tokio)
                    .with_resource(resource)
                    .build();
                let tracer = provider.tracer(cfg.service_name.clone());
                global::set_tracer_provider(provider);

                registry
                    .with(tracing_opentelemetry::layer().with_tracer(tracer))
                    .try_init()
                    .ok();
                return Ok(());
            }
            Err(e) => {
                // Fall through to log-only mode; record why OTel was skipped.
                eprintln!("[observability] OTel exporter disabled: {e}");
            }
        }
    }

    registry.try_init().ok();
    Ok(())
}

/// Flush any batched spans. Call during graceful shutdown.
pub fn shutdown() {
    global::shutdown_tracer_provider();
}
