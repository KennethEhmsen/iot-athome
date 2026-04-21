//! Tracing bootstrap.
//!
//! Every service calls [`init`] on the first line of `main`. See ADR-0009.
//!
//! W2 ships JSON logs via `tracing-subscriber`. OpenTelemetry OTLP export
//! wires in alongside the first cross-service call that actually carries a
//! trace (M3); keeping it out of the build now avoids chasing the
//! `opentelemetry-otlp` API churn for zero immediate value.

#![forbid(unsafe_code)]

use thiserror::Error;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::EnvFilter;

/// Configuration for observability bootstrap.
#[derive(Debug, Clone)]
pub struct Config {
    /// Logical service name. Injected as a `service.name` field on every log
    /// event once the real OTel layer lands.
    pub service_name: String,
    /// Service version (semver string).
    pub service_version: String,
    /// OTLP endpoint URL. Currently ignored (logged, not wired).
    pub otlp_endpoint: Option<String>,
}

/// Errors from [`init`] that the operator must resolve.
#[derive(Debug, Error)]
pub enum InitError {
    #[error("RUST_LOG filter is invalid: {0}")]
    BadFilter(#[from] tracing_subscriber::filter::ParseError),
}

/// Initialise tracing.
///
/// Safe to call once per process. Calling twice is a bug that is logged and
/// otherwise ignored.
///
/// # Errors
/// Returns [`InitError::BadFilter`] only if the `RUST_LOG` env var is set to
/// something unparseable.
pub fn init(cfg: &Config) -> Result<(), InitError> {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .map_err(InitError::BadFilter)?;

    let fmt_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(false);

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .try_init()
        .ok();

    tracing::info!(
        service.name = %cfg.service_name,
        service.version = %cfg.service_version,
        otlp_endpoint = ?cfg.otlp_endpoint,
        "observability initialised"
    );

    Ok(())
}

/// Flush any batched spans. No-op until OTel is re-wired in M3.
pub fn shutdown() {}
