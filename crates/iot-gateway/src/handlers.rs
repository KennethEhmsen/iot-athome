//! HTTP handlers for `/api/v1/devices`.
//!
//! Each handler forwards to the registry gRPC service and translates errors
//! to a JSON `ErrorEnvelope` with a stable code + trace id.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use iot_proto::iot::common::v1::Ulid as PbUlid;
use iot_proto::iot::registry::v1::{
    DeleteDeviceRequest, GetDeviceRequest, ListDevicesRequest, UpsertDeviceRequest,
};
use serde::{Deserialize, Serialize};
use tracing::instrument;

use crate::json::DeviceJson;
use crate::state::AppState;

// ---------- Response shapes ----------

#[derive(Debug, Serialize)]
pub struct UpsertResponse {
    pub device: DeviceJson,
    /// `true` if the device was newly inserted, `false` on update.
    pub created: bool,
}

#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub devices: Vec<DeviceJson>,
}

#[derive(Debug, Serialize)]
pub struct DeleteResponse {
    pub deleted: bool,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct ListQuery {
    #[serde(default)]
    pub integration: String,
    #[serde(default)]
    pub room: String,
}

// ---------- Handlers ----------

#[instrument]
pub async fn health() -> &'static str {
    "ok"
}

#[instrument]
pub async fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[instrument(skip(state, body))]
pub async fn upsert_device(
    State(state): State<AppState>,
    Json(body): Json<DeviceJson>,
) -> Result<Json<UpsertResponse>, (StatusCode, Json<ApiError>)> {
    let mut client = state.registry_client.clone();
    let resp = client
        .upsert_device(UpsertDeviceRequest {
            device: Some(body.into()),
            idempotency_key: String::new(),
        })
        .await
        .map_err(|e| grpc_to_api(&e))?
        .into_inner();

    let device = resp
        .device
        .ok_or_else(|| internal("registry returned empty device"))?
        .into();
    Ok(Json(UpsertResponse {
        device,
        created: resp.created,
    }))
}

#[instrument(skip(state))]
pub async fn list_devices(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ListQuery>,
) -> Result<Json<ListResponse>, (StatusCode, Json<ApiError>)> {
    let mut client = state.registry_client.clone();
    let mut stream = client
        .list_devices(ListDevicesRequest {
            integration: q.integration,
            room: q.room,
        })
        .await
        .map_err(|e| grpc_to_api(&e))?
        .into_inner();

    let mut devices = Vec::new();
    while let Some(msg) = stream.message().await.map_err(|e| grpc_to_api(&e))? {
        if let Some(d) = msg.device {
            devices.push(DeviceJson::from(d));
        }
    }
    Ok(Json(ListResponse { devices }))
}

#[instrument(skip(state))]
pub async fn get_device(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<DeviceJson>, (StatusCode, Json<ApiError>)> {
    let mut client = state.registry_client.clone();
    let resp = client
        .get_device(GetDeviceRequest {
            id: Some(PbUlid { value: id }),
        })
        .await
        .map_err(|e| grpc_to_api(&e))?
        .into_inner();
    let device = resp
        .device
        .ok_or_else(|| internal("registry returned empty device"))?;
    Ok(Json(DeviceJson::from(device)))
}

#[instrument(skip(state))]
pub async fn delete_device(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<DeleteResponse>, (StatusCode, Json<ApiError>)> {
    let mut client = state.registry_client.clone();
    let resp = client
        .delete_device(DeleteDeviceRequest {
            id: Some(PbUlid { value: id }),
        })
        .await
        .map_err(|e| grpc_to_api(&e))?
        .into_inner();
    Ok(Json(DeleteResponse {
        deleted: resp.deleted,
    }))
}

// ---------- Error translation ----------

fn grpc_to_api(status: &tonic::Status) -> (StatusCode, Json<ApiError>) {
    let http = match status.code() {
        tonic::Code::NotFound => StatusCode::NOT_FOUND,
        tonic::Code::InvalidArgument => StatusCode::BAD_REQUEST,
        tonic::Code::PermissionDenied => StatusCode::FORBIDDEN,
        tonic::Code::Unauthenticated => StatusCode::UNAUTHORIZED,
        tonic::Code::FailedPrecondition => StatusCode::CONFLICT,
        _ => StatusCode::BAD_GATEWAY,
    };
    let code = format!("registry.{}", status.code().description().replace(' ', "_"));
    (
        http,
        Json(ApiError {
            code,
            message: status.message().to_owned(),
        }),
    )
}

fn internal(msg: &str) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            code: "gateway.internal".into(),
            message: msg.into(),
        }),
    )
}
