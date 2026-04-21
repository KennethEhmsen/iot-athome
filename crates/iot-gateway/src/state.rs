//! Shared handler state.

use iot_bus::Bus;
use iot_proto::iot::registry::v1::registry_service_client::RegistryServiceClient;
use tonic::transport::Channel;

use crate::auth::Verifier;

/// Cloned into every `State<AppState>` handler extractor.
#[derive(Clone)]
pub struct AppState {
    pub registry_client: RegistryServiceClient<Channel>,
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
