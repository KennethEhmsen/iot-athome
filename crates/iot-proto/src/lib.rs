//! Wire-format types + gRPC clients/servers.
//!
//! As of M3 W1 this crate is the host-side half of a split:
//!   * `iot-proto-core` — prost messages + subject/header helpers,
//!     wasm32-wasip2 friendly. Plugins depend on that crate.
//!   * `iot-proto` (this crate) — re-exports everything from core and
//!     layers the tonic gRPC client + server generated from
//!     `schemas/iot/registry/v1/registry_service.proto`.
//!
//! Existing callers that `use iot_proto::iot::registry::v1::...` or
//! `use iot_proto::Ulid` keep working unchanged — every public name
//! from the pre-split crate still resolves at the same path.

#![forbid(unsafe_code)]

// Re-export the pure-prost + helpers half verbatim. Importantly we
// DON'T pub-use `iot_proto_core::*` because that would bring
// iot_proto_core::iot (the submodule) into scope and clash with the
// `pub mod iot` declaration below. We pull headers + subjects + the
// type shortcuts explicitly instead.
pub use iot_proto_core::headers;
pub use iot_proto_core::headers::*;
pub use iot_proto_core::subjects;
pub use iot_proto_core::{Device, Entity, ErrorEnvelope, ReadWrite, TrustLevel, Ulid};

/// Mirrors the `.proto` package tree. `iot::{common,device,bus}::v1`
/// are straight re-exports from `iot-proto-core`; `iot::registry::v1`
/// combines core's messages with this crate's locally-generated
/// tonic service client + server.
#[allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    missing_debug_implementations
)]
pub mod iot {
    pub use iot_proto_core::iot::{bus, common, device};

    pub mod registry {
        pub mod v1 {
            // Messages — owned by iot-proto-core via `extern_path` in
            // this crate's build.rs.
            pub use iot_proto_core::iot::registry::v1::*;
            // Service stubs — tonic-build output for this proto only.
            // `extern_path` keeps the generated code from redeclaring
            // the messages (which would collide with the pub-use above).
            include!(concat!(env!("OUT_DIR"), "/iot.registry.v1.rs"));
        }
    }
}

// Shortcut re-exports so existing `use iot_proto::RegistryServiceClient`
// etc. still resolve without the long package path.
pub use iot::registry::v1::registry_service_client::RegistryServiceClient;
pub use iot::registry::v1::registry_service_server::{RegistryService, RegistryServiceServer};
