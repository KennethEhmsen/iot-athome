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

/// Query string for `GET /api/v1/devices/{id}/history`.
///
/// `from` / `to` are RFC 3339 timestamps; both default to "open"
/// (epoch / now()) when omitted, so a bare query returns the most
/// recent `limit` rows. `limit` is clamped at 5000 server-side to
/// keep response sizes bounded.
#[derive(Debug, Deserialize, Default)]
pub struct HistoryQuery {
    /// Inclusive start. Default: epoch (1970-01-01T00:00:00Z).
    #[serde(default)]
    pub from: Option<String>,
    /// Inclusive end. Default: now().
    #[serde(default)]
    pub to: Option<String>,
    /// Max rows. Default + cap: 5000.
    #[serde(default)]
    pub limit: Option<i64>,
}

/// One row in the `GET /devices/{id}/history` response. `payload_b64`
/// is the original message bytes (Protobuf-encoded EntityState for
/// state subjects); the panel decodes per its own needs.
#[derive(Debug, Serialize)]
pub struct HistoryRowJson {
    pub device_id: String,
    pub subject: String,
    /// RFC 3339 capture timestamp (UTC, microsecond precision).
    pub at: String,
    pub payload_b64: String,
}

#[derive(Debug, Serialize)]
pub struct HistoryResponse {
    pub rows: Vec<HistoryRowJson>,
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

/// `GET /api/v1/devices/{id}/history?from=&to=&limit=` — fetch
/// rows from the optional TimescaleDB-backed history store
/// (M5a W4.1).
///
/// Returns:
/// * `200` with `HistoryResponse { rows: [...] }` on success.
/// * `503` with code `history.disabled` when the host wasn't started
///   with `IOT_TIMESCALE_URL` (history is opt-in; no surprise empty
///   responses).
/// * `400` for malformed `from` / `to` timestamps.
/// * `502` for any storage-layer error (the panel surfaces these
///   as "history backend unavailable" rather than retrying blindly).
#[instrument(skip(state))]
pub async fn get_device_history(
    State(state): State<AppState>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<HistoryQuery>,
) -> Result<Json<HistoryResponse>, (StatusCode, Json<ApiError>)> {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    use chrono::{DateTime, TimeZone as _, Utc};

    let Some(history) = state.history.as_ref() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError {
                code: "history.disabled".into(),
                message: "history backend not configured (set IOT_TIMESCALE_URL)".into(),
            }),
        ));
    };

    let from = match q.from.as_deref() {
        Some(s) => DateTime::parse_from_rfc3339(s)
            .map_err(|e| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(ApiError {
                        code: "history.bad_from".into(),
                        message: format!("invalid `from`: {e}"),
                    }),
                )
            })?
            .with_timezone(&Utc),
        None => Utc.timestamp_opt(0, 0).single().unwrap_or_else(Utc::now),
    };
    let to = match q.to.as_deref() {
        Some(s) => DateTime::parse_from_rfc3339(s)
            .map_err(|e| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(ApiError {
                        code: "history.bad_to".into(),
                        message: format!("invalid `to`: {e}"),
                    }),
                )
            })?
            .with_timezone(&Utc),
        None => Utc::now(),
    };
    // Clamp limit at 5000; default 500 if unset. Keeps responses
    // bounded for the panel's "last N events" tile pattern.
    let limit = q.limit.unwrap_or(500).clamp(1, 5_000);

    let rows = history
        .fetch_range(&id, from, to, limit)
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                Json(ApiError {
                    code: "history.query_failed".into(),
                    message: format!("{e}"),
                }),
            )
        })?;

    let rows: Vec<HistoryRowJson> = rows
        .into_iter()
        .map(|r| HistoryRowJson {
            device_id: r.device_id,
            subject: r.subject,
            at: r.ts.to_rfc3339(),
            payload_b64: B64.encode(&r.payload),
        })
        .collect();

    Ok(Json(HistoryResponse { rows }))
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
