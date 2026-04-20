//! Shared domain primitives for IoT-AtHome.
//!
//! Keep this crate small and free of runtime dependencies. Anything that needs
//! the network, the filesystem, or an async runtime belongs elsewhere.

#![forbid(unsafe_code)]

pub mod id;
pub mod trust;

pub use id::DeviceId;
pub use trust::TrustLevel;

/// Schema version this build of the core speaks natively.
/// Mirrors the `schema_version` Protobuf field. See ADR-0005.
pub const DEVICE_SCHEMA_VERSION: u32 = 1;
