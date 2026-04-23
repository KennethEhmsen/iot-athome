//! NATS JetStream client wrapper.
//!
//! Wraps `async-nats` to enforce the conventions from ADR-0004 and ADR-0009:
//!
//! * mTLS connect by default (no plaintext option in this API).
//! * Every publish carries a `traceparent` header.
//! * Every publish carries `iot-schema-version` and `iot-type` headers.
//! * Subjects are built via `iot_proto::subjects::*`, never ad-hoc.
//!
//! W1 scope: connection + typed publish. Full JetStream consumer ergonomics
//! land in M2 once services genuinely need durable subscriptions.

#![forbid(unsafe_code)]

pub mod jwt;

use async_nats::HeaderMap;
use iot_proto::headers::{
    CONTENT_TYPE, CONTENT_TYPE_PROTOBUF, IOT_PUBLISHER, IOT_SCHEMA_VERSION, IOT_TYPE, TRACEPARENT,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::instrument;

/// Errors from the bus wrapper.
#[derive(Debug, Error)]
pub enum BusError {
    #[error("connect: {0}")]
    Connect(#[from] async_nats::ConnectError),
    #[error("publish: {0}")]
    Publish(#[from] async_nats::PublishError),
    #[error("missing mTLS cert path: {0}")]
    MissingCerts(&'static str),
}

/// Connection configuration. mTLS is mandatory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// NATS URL, e.g. `tls://nats.iot.local:4222`.
    pub url: String,
    /// PEM-encoded CA bundle to trust for the server.
    pub ca_path: std::path::PathBuf,
    /// PEM client certificate.
    pub client_cert_path: std::path::PathBuf,
    /// PEM client private key.
    pub client_key_path: std::path::PathBuf,
    /// Publisher identity (service or plugin id).
    pub publisher: String,
}

impl Config {
    /// Build a config from conventional environment variables, falling back
    /// to dev-cert paths relative to the current working directory.
    ///
    /// Variables consulted:
    ///
    /// * `IOT_NATS_URL` (default `tls://127.0.0.1:4222`)
    /// * `IOT_DEV_CERTS_ROOT` (default `./tools/devcerts/generated`)
    /// * `IOT_BUS_COMPONENT` — subdir under the certs root for this caller's
    ///   client cert. Defaults to `client` (the shared dev client identity).
    #[must_use]
    pub fn from_env(publisher: impl Into<String>) -> Self {
        let dev_root = std::env::var("IOT_DEV_CERTS_ROOT")
            .unwrap_or_else(|_| "./tools/devcerts/generated".into());
        let component = std::env::var("IOT_BUS_COMPONENT").unwrap_or_else(|_| "client".into());
        let dev = std::path::PathBuf::from(&dev_root);
        Self {
            url: std::env::var("IOT_NATS_URL").unwrap_or_else(|_| "tls://127.0.0.1:4222".into()),
            ca_path: dev.join("ca").join("ca.crt"),
            client_cert_path: dev.join(&component).join(format!("{component}.crt")),
            client_key_path: dev.join(&component).join(format!("{component}.key")),
            publisher: publisher.into(),
        }
    }
}

/// The main bus handle.
#[derive(Debug, Clone)]
pub struct Bus {
    client: async_nats::Client,
    publisher: String,
}

impl Bus {
    /// Connect using mTLS.
    #[instrument(skip(cfg), fields(publisher = %cfg.publisher, url = %cfg.url))]
    pub async fn connect(cfg: Config) -> Result<Self, BusError> {
        let client = async_nats::ConnectOptions::new()
            .add_root_certificates(cfg.ca_path.clone())
            .add_client_certificate(cfg.client_cert_path.clone(), cfg.client_key_path.clone())
            .require_tls(true)
            .name(cfg.publisher.clone())
            .connect(&cfg.url)
            .await?;

        Ok(Self {
            client,
            publisher: cfg.publisher,
        })
    }

    /// Publish a Protobuf-encoded payload. Headers are populated automatically.
    ///
    /// `iot_type` is the fully-qualified Protobuf type name, e.g.
    /// `"iot.device.v1.EntityEvent"`. `traceparent` is taken from the caller's
    /// ambient tracing span; callers that need to propagate a specific trace
    /// context can pass it in `extra_headers`.
    #[instrument(skip(self, payload, extra_headers), fields(subject = %subject))]
    pub async fn publish_proto(
        &self,
        subject: &str,
        iot_type: &str,
        payload: Vec<u8>,
        extra_headers: Option<HeaderMap>,
    ) -> Result<(), BusError> {
        let mut headers = extra_headers.unwrap_or_default();
        headers.insert(IOT_PUBLISHER, self.publisher.as_str());
        headers.insert(
            IOT_SCHEMA_VERSION,
            iot_core::DEVICE_SCHEMA_VERSION.to_string().as_str(),
        );
        headers.insert(IOT_TYPE, iot_type);
        headers.insert(CONTENT_TYPE, CONTENT_TYPE_PROTOBUF);

        if headers.get(TRACEPARENT).is_none() {
            if let Some(tp) = current_traceparent() {
                headers.insert(TRACEPARENT, tp.as_str());
            }
        }

        self.client
            .publish_with_headers(subject.to_owned(), headers, payload.into())
            .await?;
        Ok(())
    }

    /// Direct access to the underlying client for advanced paths (JetStream
    /// consumers, KV, object store). Use sparingly — prefer typed wrappers.
    #[must_use]
    pub fn raw(&self) -> &async_nats::Client {
        &self.client
    }
}

/// Best-effort extraction of the active W3C traceparent from the tracing
/// subscriber. Returns `None` if no compatible OTel layer is installed.
///
/// Integrations that require propagation can always override by passing an
/// explicit header.
fn current_traceparent() -> Option<String> {
    // The real implementation plumbs `tracing_opentelemetry::OpenTelemetrySpanExt`
    // to pull the current context and serialize it to traceparent form. Kept as
    // a placeholder for W1 so the API shape is stable; the mechanism lands in
    // M2 alongside the first consumer.
    None
}
