//! `iot.registry.v1.RegistryService` gRPC implementation.
//!
//! Each mutating call:
//!   1. Persists to sqlx
//!   2. Appends a hash-chained audit entry
//!   3. Publishes a `device.<plugin>.<id>.state` notification on the bus
//!      (if a bus is configured)

use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use iot_audit::AuditLog;
use iot_bus::Bus;
use iot_proto::iot::registry::v1::registry_service_server::RegistryService;
use iot_proto::iot::registry::v1::{
    DeleteDeviceRequest, DeleteDeviceResponse, GetDeviceRequest, GetDeviceResponse,
    ListDevicesRequest, ListDevicesResponse, UpsertDeviceRequest, UpsertDeviceResponse,
};
use prost::Message as _;
use sqlx::SqlitePool;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use tracing::{error, info, instrument};

use crate::repo::{DeviceRepo, RepoError};

#[derive(Debug)]
pub struct RegistrySvc {
    repo: DeviceRepo,
    audit: Arc<AuditLog>,
    bus: Option<Bus>,
}

impl RegistrySvc {
    pub fn new(pool: SqlitePool, audit: AuditLog, bus: Option<Bus>) -> Self {
        Self {
            repo: DeviceRepo::new(pool),
            audit: Arc::new(audit),
            bus,
        }
    }

    async fn audit_write(&self, kind: &'static str, payload: serde_json::Value) {
        if let Err(e) = self.audit.append(kind, payload).await {
            error!(error = %e, "audit append failed");
        }
    }

    async fn bus_publish_state(&self, device: &iot_proto::Device) {
        let Some(bus) = &self.bus else { return };
        let Some(id) = device.id.as_ref().map(|u| u.value.as_str()) else {
            return;
        };
        let subject = match iot_proto::subjects::device_state(&device.integration, id, "_device") {
            Ok(s) => s,
            Err(e) => {
                error!(error = %e, "bad device subject");
                return;
            }
        };
        let bytes = device.encode_to_vec();
        if let Err(e) = bus
            .publish_proto(&subject, "iot.device.v1.Device", bytes, None)
            .await
        {
            error!(error = %e, subject, "bus publish failed");
        }
    }
}

#[tonic::async_trait]
impl RegistryService for RegistrySvc {
    type ListDevicesStream =
        Pin<Box<dyn Stream<Item = Result<ListDevicesResponse, Status>> + Send + 'static>>;

    #[instrument(skip(self, request))]
    async fn upsert_device(
        &self,
        request: Request<UpsertDeviceRequest>,
    ) -> Result<Response<UpsertDeviceResponse>, Status> {
        let req = request.into_inner();
        let device = req
            .device
            .ok_or_else(|| Status::invalid_argument("device is required"))?;
        let created_before = device.id.as_ref().is_none_or(|u| u.value.is_empty());

        let stored = self.repo.upsert(device).await.map_err(map_repo_err)?;

        let id = stored
            .id
            .as_ref()
            .map(|u| u.value.clone())
            .unwrap_or_default();
        self.audit_write(
            if created_before {
                "device.created"
            } else {
                "device.updated"
            },
            serde_json::json!({
                "id": id,
                "integration": stored.integration,
                "external_id": stored.external_id,
            }),
        )
        .await;

        self.bus_publish_state(&stored).await;

        info!(device.id = %id, "upsert_device ok");
        Ok(Response::new(UpsertDeviceResponse {
            device: Some(stored),
            created: created_before,
        }))
    }

    #[instrument(skip(self, request))]
    async fn get_device(
        &self,
        request: Request<GetDeviceRequest>,
    ) -> Result<Response<GetDeviceResponse>, Status> {
        let id = request
            .into_inner()
            .id
            .ok_or_else(|| Status::invalid_argument("id is required"))?
            .value;
        let d = self.repo.get(&id).await.map_err(map_repo_err)?;
        Ok(Response::new(GetDeviceResponse { device: Some(d) }))
    }

    #[instrument(skip(self, request))]
    async fn list_devices(
        &self,
        request: Request<ListDevicesRequest>,
    ) -> Result<Response<Self::ListDevicesStream>, Status> {
        let req = request.into_inner();
        let integ = if req.integration.is_empty() {
            None
        } else {
            Some(req.integration)
        };
        let room = if req.room.is_empty() {
            None
        } else {
            Some(req.room)
        };
        let devices = self
            .repo
            .list(integ.as_deref(), room.as_deref())
            .await
            .map_err(map_repo_err)?;

        let (tx, rx) = mpsc::channel::<Result<ListDevicesResponse, Status>>(16);
        tokio::spawn(async move {
            for d in devices {
                if tx
                    .send(Ok(ListDevicesResponse { device: Some(d) }))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        let stream = ReceiverStream::new(rx);
        Ok(Response::new(Box::pin(stream) as Self::ListDevicesStream))
    }

    #[instrument(skip(self, request))]
    async fn delete_device(
        &self,
        request: Request<DeleteDeviceRequest>,
    ) -> Result<Response<DeleteDeviceResponse>, Status> {
        let id = request
            .into_inner()
            .id
            .ok_or_else(|| Status::invalid_argument("id is required"))?
            .value;
        let deleted = self.repo.delete(&id).await.map_err(map_repo_err)?;
        if deleted {
            self.audit_write("device.deleted", serde_json::json!({ "id": id }))
                .await;
        }
        Ok(Response::new(DeleteDeviceResponse { deleted }))
    }
}

fn map_repo_err(e: RepoError) -> Status {
    match e {
        RepoError::NotFound(id) => Status::not_found(format!("device not found: {id}")),
        RepoError::Sqlx(err) => Status::internal(format!("storage: {err}")),
        RepoError::Json(err) => Status::internal(format!("serde: {err}")),
    }
}
