//! JSON-facing device shapes.
//!
//! The gateway's REST/JSON surface is intentionally its own type, not a
//! serde-derived view of the Protobuf types. This gives us:
//!
//!   * A stable public API that can evolve independently of the wire schema.
//!   * Freedom to strip fields prost-style enums generate as raw i32s.
//!   * A single place to translate timestamps and ULIDs to readable strings.
//!
//! Conversions to/from `iot_proto::Device` live here so handlers stay terse.

use chrono::{DateTime, TimeZone, Utc};
use iot_proto::iot::common::v1::Ulid as PbUlid;
use iot_proto::iot::device::v1::{Device as PbDevice, Entity as PbEntity, ReadWrite, TrustLevel};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeviceJson {
    /// ULID. Omitted on upsert to let the registry mint a fresh one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub integration: String,
    #[serde(default)]
    pub external_id: String,
    #[serde(default)]
    pub manufacturer: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub rooms: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub entities: Vec<EntityJson>,
    #[serde(default = "default_trust_level")]
    pub trust_level: String,
    #[serde(default)]
    pub plugin_meta: BTreeMap<String, String>,
    /// RFC3339. Empty on upsert.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_seen: String,
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EntityJson {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default)]
    pub unit: String,
    #[serde(default = "default_rw")]
    pub rw: String,
    #[serde(default)]
    pub device_class: String,
    #[serde(default)]
    pub meta: BTreeMap<String, String>,
}

fn default_trust_level() -> String {
    "user_added".into()
}
fn default_rw() -> String {
    "read".into()
}
fn default_schema_version() -> u32 {
    iot_core::DEVICE_SCHEMA_VERSION
}

// ---------- JSON -> Protobuf ----------

impl From<DeviceJson> for PbDevice {
    fn from(j: DeviceJson) -> Self {
        Self {
            id: j.id.map(|v| PbUlid { value: v }),
            integration: j.integration,
            external_id: j.external_id,
            manufacturer: j.manufacturer,
            model: j.model,
            label: j.label,
            capabilities: j.capabilities,
            entities: j.entities.into_iter().map(Into::into).collect(),
            rooms: j.rooms,
            trust_level: trust_level_from_str(&j.trust_level).into(),
            schema_version: j.schema_version,
            plugin_meta: j.plugin_meta.into_iter().collect(),
            last_seen: None,
        }
    }
}

impl From<EntityJson> for PbEntity {
    fn from(j: EntityJson) -> Self {
        Self {
            id: j.id.map(|v| PbUlid { value: v }),
            r#type: j.type_,
            unit: j.unit,
            rw: rw_from_str(&j.rw).into(),
            device_class: j.device_class,
            meta: j.meta.into_iter().collect(),
        }
    }
}

// ---------- Protobuf -> JSON ----------

impl From<PbDevice> for DeviceJson {
    fn from(p: PbDevice) -> Self {
        Self {
            id: p.id.map(|u| u.value),
            integration: p.integration,
            external_id: p.external_id,
            manufacturer: p.manufacturer,
            model: p.model,
            label: p.label,
            rooms: p.rooms,
            capabilities: p.capabilities,
            entities: p.entities.into_iter().map(Into::into).collect(),
            trust_level: trust_level_str(p.trust_level).into(),
            plugin_meta: p.plugin_meta.into_iter().collect(),
            last_seen: p
                .last_seen
                .and_then(|ts| {
                    Utc.timestamp_opt(ts.seconds, u32::try_from(ts.nanos).unwrap_or(0))
                        .single()
                })
                .map(|dt: DateTime<Utc>| dt.to_rfc3339())
                .unwrap_or_default(),
            schema_version: p.schema_version,
        }
    }
}

impl From<PbEntity> for EntityJson {
    fn from(p: PbEntity) -> Self {
        Self {
            id: p.id.map(|u| u.value),
            type_: p.r#type,
            unit: p.unit,
            rw: rw_str(p.rw).into(),
            device_class: p.device_class,
            meta: p.meta.into_iter().collect(),
        }
    }
}

// ---------- Enum <-> string ----------

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
