//! Sqlx-backed device repository.
//!
//! Keep the Row types aligned with `migrations/sqlite/*.sql`. All JSON columns
//! round-trip through `serde_json::Value`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use iot_proto::iot::device::v1::{Device, Entity, ReadWrite, TrustLevel};
use sqlx::{Row as _, SqlitePool};
use thiserror::Error;
use tracing::instrument;

#[derive(Debug, Error)]
pub enum RepoError {
    #[error("sqlx: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("device not found: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, RepoError>;

#[derive(Debug, Clone)]
pub struct DeviceRepo {
    pool: SqlitePool,
}

impl DeviceRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Upsert is long because it replaces the entire child-row set for rooms,
    /// capabilities, and entities. Splitting would obscure the transaction
    /// boundary; the allow stays scoped to this fn.
    #[allow(clippy::too_many_lines)]
    #[instrument(skip(self, device), fields(device.id = %device_id(&device)))]
    pub async fn upsert(&self, mut device: Device) -> Result<Device> {
        if device_id(&device).is_empty() {
            device.id = Some(iot_proto::iot::common::v1::Ulid {
                value: ulid::Ulid::new().to_string(),
            });
        }
        let id = device_id(&device);
        let now = Utc::now();

        let meta_json = serde_json::to_string(&device.plugin_meta)?;
        let trust = trust_level_str(device.trust_level);

        let mut tx = self.pool.begin().await?;

        let existing: Option<(String,)> = sqlx::query_as("SELECT id FROM devices WHERE id = ?")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await?;

        if existing.is_some() {
            sqlx::query(
                "UPDATE devices SET integration=?, external_id=?, manufacturer=?, model=?,
                    label=?, trust_level=?, schema_version=?, plugin_meta_json=?,
                    last_seen=?, updated_at=?
                 WHERE id=?",
            )
            .bind(&device.integration)
            .bind(&device.external_id)
            .bind(&device.manufacturer)
            .bind(&device.model)
            .bind(&device.label)
            .bind(trust)
            .bind(i64::from(device.schema_version))
            .bind(&meta_json)
            .bind(now.to_rfc3339())
            .bind(now.to_rfc3339())
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query(
                "INSERT INTO devices (
                    id, integration, external_id, manufacturer, model, label,
                    trust_level, schema_version, plugin_meta_json,
                    last_seen, created_at, updated_at
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&device.integration)
            .bind(&device.external_id)
            .bind(&device.manufacturer)
            .bind(&device.model)
            .bind(&device.label)
            .bind(trust)
            .bind(i64::from(device.schema_version))
            .bind(&meta_json)
            .bind(now.to_rfc3339())
            .bind(now.to_rfc3339())
            .bind(now.to_rfc3339())
            .execute(&mut *tx)
            .await?;
        }

        // Rooms — replace the set.
        sqlx::query("DELETE FROM device_rooms WHERE device_id = ?")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        for room in &device.rooms {
            sqlx::query("INSERT INTO device_rooms (device_id, room) VALUES (?, ?)")
                .bind(&id)
                .bind(room)
                .execute(&mut *tx)
                .await?;
        }

        // Capabilities — replace the set.
        sqlx::query("DELETE FROM device_capabilities WHERE device_id = ?")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        for cap in &device.capabilities {
            sqlx::query("INSERT INTO device_capabilities (device_id, capability) VALUES (?, ?)")
                .bind(&id)
                .bind(cap)
                .execute(&mut *tx)
                .await?;
        }

        // Entities — replace the set.
        sqlx::query("DELETE FROM entities WHERE device_id = ?")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        for e in &device.entities {
            let eid = entity_id(e);
            let meta = serde_json::to_string(&e.meta)?;
            sqlx::query(
                "INSERT INTO entities (id, device_id, type, unit, rw, device_class, meta_json)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&eid)
            .bind(&id)
            .bind(&e.r#type)
            .bind(&e.unit)
            .bind(rw_str(e.rw))
            .bind(&e.device_class)
            .bind(&meta)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        self.get(&id).await
    }

    #[instrument(skip(self))]
    pub async fn get(&self, id: &str) -> Result<Device> {
        let row = sqlx::query(
            "SELECT id, integration, external_id, manufacturer, model, label,
                    trust_level, schema_version, plugin_meta_json, last_seen
             FROM devices WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| RepoError::NotFound(id.to_owned()))?;

        let device_base = row_to_device(&row)?;
        let id_s = device_base
            .id
            .as_ref()
            .map(|u| u.value.clone())
            .unwrap_or_default();

        let rooms = sqlx::query("SELECT room FROM device_rooms WHERE device_id = ?")
            .bind(&id_s)
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|r| r.try_get::<String, _>("room").unwrap_or_default())
            .collect();

        let caps = sqlx::query("SELECT capability FROM device_capabilities WHERE device_id = ?")
            .bind(&id_s)
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|r| r.try_get::<String, _>("capability").unwrap_or_default())
            .collect();

        let entity_rows = sqlx::query(
            "SELECT id, type, unit, rw, device_class, meta_json
             FROM entities WHERE device_id = ?",
        )
        .bind(&id_s)
        .fetch_all(&self.pool)
        .await?;

        let mut entities = Vec::with_capacity(entity_rows.len());
        for r in &entity_rows {
            entities.push(row_to_entity(r)?);
        }

        Ok(Device {
            rooms,
            capabilities: caps,
            entities,
            ..device_base
        })
    }

    #[instrument(skip(self))]
    pub async fn list(&self, integration: Option<&str>, room: Option<&str>) -> Result<Vec<Device>> {
        let ids: Vec<String> = match (integration, room) {
            (Some(i), Some(r)) => {
                sqlx::query(
                    "SELECT d.id FROM devices d
                 JOIN device_rooms dr ON dr.device_id = d.id
                 WHERE d.integration = ? AND dr.room = ?
                 ORDER BY d.created_at DESC",
                )
                .bind(i)
                .bind(r)
                .fetch_all(&self.pool)
                .await?
            }
            (Some(i), None) => {
                sqlx::query("SELECT id FROM devices WHERE integration = ? ORDER BY created_at DESC")
                    .bind(i)
                    .fetch_all(&self.pool)
                    .await?
            }
            (None, Some(r)) => {
                sqlx::query(
                    "SELECT d.id FROM devices d
                 JOIN device_rooms dr ON dr.device_id = d.id
                 WHERE dr.room = ?
                 ORDER BY d.created_at DESC",
                )
                .bind(r)
                .fetch_all(&self.pool)
                .await?
            }
            (None, None) => {
                sqlx::query("SELECT id FROM devices ORDER BY created_at DESC")
                    .fetch_all(&self.pool)
                    .await?
            }
        }
        .into_iter()
        .map(|row| row.try_get::<String, _>("id").unwrap_or_default())
        .collect();

        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            out.push(self.get(&id).await?);
        }
        Ok(out)
    }

    #[instrument(skip(self))]
    pub async fn delete(&self, id: &str) -> Result<bool> {
        let r = sqlx::query("DELETE FROM devices WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }
}

// ------------ helpers ------------

fn device_id(d: &Device) -> String {
    d.id.as_ref().map(|u| u.value.clone()).unwrap_or_default()
}

fn entity_id(e: &Entity) -> String {
    if let Some(u) = &e.id {
        if !u.value.is_empty() {
            return u.value.clone();
        }
    }
    ulid::Ulid::new().to_string()
}

fn rw_str(rw: i32) -> &'static str {
    match ReadWrite::try_from(rw).unwrap_or(ReadWrite::Unspecified) {
        ReadWrite::Write => "write",
        ReadWrite::ReadWrite => "read_write",
        ReadWrite::Read | ReadWrite::Unspecified => "read",
    }
}

fn rw_from_str(s: &str) -> ReadWrite {
    match s {
        "write" => ReadWrite::Write,
        "read_write" => ReadWrite::ReadWrite,
        _ => ReadWrite::Read,
    }
}

fn trust_level_str(t: i32) -> &'static str {
    match TrustLevel::try_from(t).unwrap_or(TrustLevel::Unspecified) {
        TrustLevel::Discovered => "discovered",
        TrustLevel::Verified => "verified",
        TrustLevel::UserAdded | TrustLevel::Unspecified => "user_added",
    }
}

fn trust_level_from_str(s: &str) -> TrustLevel {
    match s {
        "discovered" => TrustLevel::Discovered,
        "verified" => TrustLevel::Verified,
        _ => TrustLevel::UserAdded,
    }
}

fn row_to_device(row: &sqlx::sqlite::SqliteRow) -> Result<Device> {
    let id_s: String = row.try_get("id")?;
    let last_seen_s: String = row.try_get("last_seen")?;
    let plugin_meta_json: String = row.try_get("plugin_meta_json")?;
    let plugin_meta: BTreeMap<String, String> = serde_json::from_str(&plugin_meta_json)?;
    let last_seen: DateTime<Utc> = match DateTime::parse_from_rfc3339(&last_seen_s) {
        Ok(d) => d.with_timezone(&Utc),
        Err(_) => Utc::now(),
    };

    Ok(Device {
        id: Some(iot_proto::iot::common::v1::Ulid { value: id_s }),
        integration: row.try_get("integration")?,
        external_id: row.try_get("external_id")?,
        manufacturer: row.try_get("manufacturer")?,
        model: row.try_get("model")?,
        label: row.try_get("label")?,
        trust_level: trust_level_from_str(&row.try_get::<String, _>("trust_level")?).into(),
        schema_version: u32::try_from(row.try_get::<i64, _>("schema_version")?).unwrap_or(1),
        plugin_meta: plugin_meta.into_iter().collect(),
        last_seen: Some(prost_types::Timestamp {
            seconds: last_seen.timestamp(),
            nanos: i32::try_from(last_seen.timestamp_subsec_nanos()).unwrap_or(0),
        }),
        capabilities: Vec::new(),
        entities: Vec::new(),
        rooms: Vec::new(),
    })
}

fn row_to_entity(row: &sqlx::sqlite::SqliteRow) -> Result<Entity> {
    let id_s: String = row.try_get("id")?;
    let meta_json: String = row.try_get("meta_json")?;
    let meta: BTreeMap<String, String> = serde_json::from_str(&meta_json)?;
    Ok(Entity {
        id: Some(iot_proto::iot::common::v1::Ulid { value: id_s }),
        r#type: row.try_get("type")?,
        unit: row.try_get("unit")?,
        rw: rw_from_str(&row.try_get::<String, _>("rw")?).into(),
        device_class: row.try_get("device_class")?,
        meta: meta.into_iter().collect(),
    })
}
