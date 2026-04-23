//! Build script: emit the tonic gRPC client + server traits for
//! `iot.registry.v1.RegistryService`. Message types are owned by
//! `iot-proto-core` (next door); we point tonic at them via
//! `extern_path` so the generated service code uses those definitions
//! instead of regenerating its own (which would cause orphan-impl
//! collisions).
//!
//! M3 W1: this crate shrank from "everything prost + tonic" to
//! "tonic services on top of iot-proto-core's prost messages".
//! Nothing links tonic unless it consumes this crate — wasm plugins
//! depend on iot-proto-core directly.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let schemas = manifest_dir.join("..").join("..").join("schemas");

    let proto_files: Vec<PathBuf> = [
        "iot/common/v1/ids.proto",
        "iot/common/v1/error.proto",
        "iot/device/v1/device.proto",
        "iot/device/v1/entity_event.proto",
        "iot/bus/v1/envelope.proto",
        "iot/registry/v1/registry_service.proto",
    ]
    .iter()
    .map(|p| schemas.join(p))
    .collect();

    println!("cargo:rerun-if-changed={}", schemas.display());
    for p in &proto_files {
        println!("cargo:rerun-if-changed={}", p.display());
    }

    let file_descriptors = protox::compile(&proto_files, [&schemas])?;

    // tonic_build still needs to see the whole descriptor set (so it
    // can generate server + client stubs for the service), but we
    // redirect every message type reference at iot-proto-core's copies
    // via `extern_path`. Result: the only code in this crate's OUT_DIR
    // is tonic client + server traits.
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .extern_path(".iot.common.v1", "::iot_proto_core::iot::common::v1")
        .extern_path(".iot.device.v1", "::iot_proto_core::iot::device::v1")
        .extern_path(".iot.bus.v1", "::iot_proto_core::iot::bus::v1")
        .extern_path(".iot.registry.v1", "::iot_proto_core::iot::registry::v1")
        .compile_fds(file_descriptors)?;

    Ok(())
}
