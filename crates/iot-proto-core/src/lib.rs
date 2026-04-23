//! Wire-format Protobuf types (generated from `schemas/`) + hand-written
//! helpers — split out from `iot-proto` in M3 W1 so WASM plugins can
//! consume it (no `tonic` transitive).
//!
//! The sibling crate `iot-proto` re-exports everything here and layers
//! the gRPC service client / server generation on top. Host-side callers
//! that need the gRPC client depend on `iot-proto`; plugins + other
//! pure-message consumers depend on `iot-proto-core`.

#![forbid(unsafe_code)]

pub mod headers;
pub mod subjects;

pub use headers::*;

/// Generated Protobuf types. Kept in a module tree that mirrors the
/// `.proto` package paths so `iot-proto`'s re-exports line up.
#[allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    missing_debug_implementations
)]
pub mod iot {
    pub mod common {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/iot.common.v1.rs"));
        }
    }
    pub mod device {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/iot.device.v1.rs"));
        }
    }
    pub mod bus {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/iot.bus.v1.rs"));
        }
    }
    pub mod registry {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/iot.registry.v1.rs"));
        }
    }
}

// Shortcuts for the most-used message names. `iot-proto` re-exports
// the same set for backwards compat with pre-M3 consumers.
pub use iot::common::v1::{ErrorEnvelope, Ulid};
pub use iot::device::v1::{Device, Entity, ReadWrite, TrustLevel};
