//! Shared handler state.

use iot_bus::Bus;
use iot_proto::iot::registry::v1::registry_service_client::RegistryServiceClient;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Channel;
use tonic::{Request, Status};

use crate::auth::Verifier;

/// Function pointer interceptor that stamps the task-local
/// TraceContext onto every outbound gRPC request's metadata.
///
/// The fn-pointer shape (rather than a closure) keeps the resulting
/// client type spellable + Clone/Send/Sync.
pub type TraceparentInterceptor = fn(Request<()>) -> Result<Request<()>, Status>;

/// Type alias for the intercepted registry client. Exposed so
/// handlers can name the extractor-friendly shape without threading
/// the `InterceptedService` generics.
pub type RegistryClient =
    RegistryServiceClient<InterceptedService<Channel, TraceparentInterceptor>>;

/// Interceptor body: read the current task-local TraceContext and
/// inject it as the `traceparent` metadata entry.
///
/// The task-local is populated by the gateway's `traceparent_mw`
/// middleware on inbound requests, so the registry sees the same
/// trace id as the upstream caller.
///
/// # Errors
/// Only if the formatted header fails to parse as a valid
/// `MetadataValue` — a condition that can't happen for the
/// two-hex-plus-dashes output of `TraceContext::to_header`, but we
/// surface via `Status::internal` rather than panic.
pub fn inject_traceparent(mut req: Request<()>) -> Result<Request<()>, Status> {
    if let Some(ctx) = iot_observability::traceparent::current() {
        match ctx.to_header().parse() {
            Ok(value) => {
                req.metadata_mut().insert("traceparent", value);
            }
            Err(e) => {
                return Err(Status::internal(format!(
                    "failed to stamp traceparent metadata: {e}"
                )));
            }
        }
    }
    Ok(req)
}

/// Cloned into every `State<AppState>` handler extractor.
#[derive(Clone)]
pub struct AppState {
    pub registry_client: RegistryClient,
    pub bus: Option<Bus>,
    /// OIDC bearer verifier. `None` = dev mode (auth disabled).
    pub verifier: Option<Verifier>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("bus", &self.bus.is_some())
            .field("verifier", &self.verifier.is_some())
            .field("registry_client", &"<tonic client>")
            .finish()
    }
}
