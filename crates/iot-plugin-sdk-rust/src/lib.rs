//! Plugin SDK (Rust).
//!
//! Plugins compile to a WASM Component targeting the `iot:plugin-host@1.0.0`
//! WIT world (see `schemas/wit/` — landing M2). This crate is the
//! Rust-facing ergonomic layer: re-exports of shared types, a small prelude,
//! and utilities for building canonical bus payloads.
//!
//! # W1 status
//!
//! The WIT world + generated bindings land in M2. For W1 this crate exists so
//! that downstream plugin crates can declare the dependency and compile as
//! stubs without picking up a cyclic path-reference to unfinished host code.

#![forbid(unsafe_code)]

pub use iot_core::{DeviceId, TrustLevel, DEVICE_SCHEMA_VERSION};
pub use iot_proto::{headers, subjects};

/// Canonical prelude plugin authors should `use iot_plugin_sdk::prelude::*;`
/// once the SDK is real.
pub mod prelude {
    pub use crate::{headers, subjects, DeviceId, TrustLevel, DEVICE_SCHEMA_VERSION};
}
