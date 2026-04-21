//! Shared handler state.

use iot_bus::Bus;
use iot_proto::iot::registry::v1::registry_service_client::RegistryServiceClient;
use tonic::transport::Channel;

/// Cloned into every `State<AppState>` handler extractor.
#[derive(Clone)]
pub struct AppState {
    pub registry_client: RegistryServiceClient<Channel>,
    pub bus: Option<Bus>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("bus", &self.bus.is_some())
            .field("registry_client", &"<tonic client>")
            .finish()
    }
}
