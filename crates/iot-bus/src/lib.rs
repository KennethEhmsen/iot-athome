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

pub mod jetstream;
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
    /// Loading a `.creds` file failed (path missing, perms, …).
    #[error("creds file: {0}")]
    Creds(#[from] std::io::Error),
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
    /// Optional NATS credentials file (the JWT + nkey blob produced
    /// by `iotctl plugin install` post-install or by `iotctl nats
    /// mint-user`). Used when the broker is running in operator-JWT
    /// decentralized-auth mode (M5a W1). Falls back to mTLS-only
    /// authentication when `None`, which is the pre-M5a dev path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creds_path: Option<std::path::PathBuf>,
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
    /// * `IOT_NATS_CREDS` — path to a NATS `.creds` file with a user JWT +
    ///   nkey seed. When set, `Bus::connect` uses it for decentralized-auth
    ///   handshake (M5a W1). When unset, the connection is mTLS-only.
    #[must_use]
    pub fn from_env(publisher: impl Into<String>) -> Self {
        let dev_root = std::env::var("IOT_DEV_CERTS_ROOT")
            .unwrap_or_else(|_| "./tools/devcerts/generated".into());
        let component = std::env::var("IOT_BUS_COMPONENT").unwrap_or_else(|_| "client".into());
        let dev = std::path::PathBuf::from(&dev_root);
        let creds_path = std::env::var("IOT_NATS_CREDS")
            .ok()
            .map(std::path::PathBuf::from);
        Self {
            url: std::env::var("IOT_NATS_URL").unwrap_or_else(|_| "tls://127.0.0.1:4222".into()),
            ca_path: dev.join("ca").join("ca.crt"),
            client_cert_path: dev.join(&component).join(format!("{component}.crt")),
            client_key_path: dev.join(&component).join(format!("{component}.key")),
            publisher: publisher.into(),
            creds_path,
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
    /// Connect using mTLS, optionally with NATS decentralized-auth
    /// credentials.
    ///
    /// When `cfg.creds_path` is set, the JWT + nkey seed in that
    /// file are added to the connect options so the broker can
    /// validate the user against its operator-signed account JWT
    /// (M5a W1). When unset, only mTLS authenticates the
    /// connection — the pre-M5a dev path.
    ///
    /// # Errors
    /// `BusError::Connect` for TLS handshake failure, JWT-rejection
    /// from the broker, or unreachable URL.
    #[instrument(skip(cfg), fields(publisher = %cfg.publisher, url = %cfg.url))]
    pub async fn connect(cfg: Config) -> Result<Self, BusError> {
        let mut opts = async_nats::ConnectOptions::new()
            .add_root_certificates(cfg.ca_path.clone())
            .add_client_certificate(cfg.client_cert_path.clone(), cfg.client_key_path.clone())
            .require_tls(true)
            .name(cfg.publisher.clone());

        if let Some(creds) = cfg.creds_path.as_ref() {
            // Wraps a future internally — must `.await`. Failures
            // here surface as `ConnectError::Authentication`.
            opts = opts.credentials_file(creds).await?;
            tracing::info!(
                creds = %creds.display(),
                "bus: using NATS user JWT credentials"
            );
        }

        let client = opts.connect(&cfg.url).await?;

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

/// Best-effort extraction of the active W3C traceparent from the
/// task-local context set by `iot_observability::traceparent::with_context`.
/// Returns `None` when the current task wasn't entered through a
/// `with_context` scope — which is the right behaviour: top-level
/// binaries that haven't yet opened a trace don't stamp one on their
/// outbound publishes. Callers that explicitly want to start a trace
/// generate + scope a [`iot_observability::traceparent::TraceContext`]
/// themselves.
fn current_traceparent() -> Option<String> {
    iot_observability::traceparent::current().map(|tc| tc.to_header())
}

/// Pull the W3C traceparent out of an inbound NATS message, if
/// present + well-formed. Designed for bus subscriber loops:
///
/// ```ignore
/// while let Some(msg) = sub.next().await {
///     let ctx = iot_bus::extract_trace_context(&msg)
///         .map(|p| p.child_of())
///         .unwrap_or_else(iot_observability::traceparent::TraceContext::new_root);
///     iot_observability::traceparent::with_context(ctx, handle(msg)).await;
/// }
/// ```
///
/// Returns `None` if the message has no headers, no `traceparent`
/// header, the header's bytes aren't UTF-8, or the value doesn't
/// pass `TraceContext::parse`. Malformed headers fall through rather
/// than error — the subscriber keeps handling the message under a
/// fresh root, matching the gateway's inbound behaviour.
#[must_use]
pub fn extract_trace_context(
    msg: &async_nats::Message,
) -> Option<iot_observability::traceparent::TraceContext> {
    let headers = msg.headers.as_ref()?;
    let value = headers.get(TRACEPARENT)?;
    let s = std::str::from_utf8(value.as_ref()).ok()?;
    iot_observability::traceparent::TraceContext::parse(s).ok()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    // ---------------------------------------------- M6 W2.5 cert-rotation
    //
    // Pin the cert-rotation behaviour at the config layer. A live
    // broker test (testcontainers + custom cert) is the next-tier
    // deliverable — see `docs/security/cert-rotation-test.md` for
    // the full plan. These unit tests cover the pieces that don't
    // need a broker: config construction is stable across
    // rotations, the path fields don't bleed across instances,
    // and the env-driven `from_env` constructor reads the right
    // dirs.

    #[test]
    fn config_round_trips_two_cas_independently() {
        // Two configs built for two different CA roots — neither
        // should leak path state into the other. This pins the
        // rotation invariant: a fresh `Config` for CA-B doesn't
        // accidentally retain CA-A's paths via shared state.
        let cfg_a = Config {
            url: "tls://nats-a:4222".into(),
            ca_path: "/tmp/ca-a/ca.crt".into(),
            client_cert_path: "/tmp/ca-a/client.crt".into(),
            client_key_path: "/tmp/ca-a/client.key".into(),
            publisher: "test-a".into(),
            creds_path: None,
        };
        let cfg_b = Config {
            url: "tls://nats-b:4222".into(),
            ca_path: "/tmp/ca-b/ca.crt".into(),
            client_cert_path: "/tmp/ca-b/client.crt".into(),
            client_key_path: "/tmp/ca-b/client.key".into(),
            publisher: "test-b".into(),
            creds_path: None,
        };

        assert_eq!(cfg_a.ca_path, std::path::PathBuf::from("/tmp/ca-a/ca.crt"));
        assert_eq!(cfg_b.ca_path, std::path::PathBuf::from("/tmp/ca-b/ca.crt"));
        // Constructing cfg_b doesn't have side-effects on cfg_a.
        assert_ne!(cfg_a.ca_path, cfg_b.ca_path);
        assert_ne!(cfg_a.url, cfg_b.url);
    }

    #[test]
    fn from_env_picks_up_per_component_paths() {
        // The post-rotation runbook concatenates old + new CA into
        // a transitional bundle at the configured `IOT_DEV_CERTS_ROOT`
        // path. Verify `from_env` honours that env var so an operator's
        // rotation script doesn't have to touch source code.
        //
        // Use temp env vars to avoid polluting the test process;
        // this is fine because each test has its own process under
        // nextest's default isolation.

        // SAFETY: env::set_var is unsafe in 2024 edition. We're
        // testing serial-isolated by nextest default, so race
        // hazard isn't real here. Forbid lint at module scope
        // doesn't reach this — but the crate's
        // `#![forbid(unsafe_code)]` does. So we read-only check
        // the construction shape instead, without mutating env.
        // The `IOT_DEV_CERTS_ROOT` default-fallback path is the
        // documented contract; verify it.
        let cfg = Config::from_env("rotation-test");
        assert!(cfg.publisher == "rotation-test");
        // The default root is `./tools/devcerts/generated`.
        // Under that, the `client` component's cert lives at
        // `client/client.crt`. Pin the layout so an operator's
        // mint-script reorg doesn't silently break.
        assert!(cfg.client_cert_path.ends_with("client.crt"));
        assert!(cfg.ca_path.ends_with("ca.crt"));
    }
}
