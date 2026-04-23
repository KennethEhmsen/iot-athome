//! Minimal wire-compatible subset of the iot-proto message types the
//! plugin emits on the bus.
//!
//! We can't just depend on `iot-proto` because its build graph pulls
//! `tonic` → `hyper` → native socket APIs, none of which target
//! `wasm32-wasip2`. So the plugin re-declares the few messages it
//! actually encodes, keeping field numbers in lockstep with
//! `schemas/iot/device/v1/entity_event.proto` and
//! `schemas/iot/common/v1/ids.proto`. Protobuf is just field tags +
//! wire types, so byte-identical output is achievable without sharing
//! generated code.
//!
//! Consumers on the bus side (panel, automation) decode these with
//! the real `iot-proto` types — the wire bytes are the contract.

use prost::Message;

/// Mirror of `iot.common.v1.Ulid`. One tagged string field.
#[derive(Clone, PartialEq, Message)]
pub struct Ulid {
    #[prost(string, tag = "1")]
    pub value: prost::alloc::string::String,
}

/// Mirror of `iot.device.v1.EntityState`. Field numbers match the
/// `.proto` exactly — `schema_version` stays at tag 15.
#[derive(Clone, PartialEq, Message)]
pub struct EntityState {
    #[prost(message, optional, tag = "1")]
    pub device_id: Option<Ulid>,
    #[prost(message, optional, tag = "2")]
    pub entity_id: Option<Ulid>,
    #[prost(message, optional, tag = "3")]
    pub value: Option<prost_types::Value>,
    #[prost(message, optional, tag = "4")]
    pub at: Option<prost_types::Timestamp>,
    #[prost(uint32, tag = "15")]
    pub schema_version: u32,
}

/// Fully-qualified Protobuf type name used in the `iot-type` bus
/// header. Must match what the real iot-proto crate produces on the
/// decoder side.
pub const ENTITY_STATE_TYPE: &str = "iot.device.v1.EntityState";
