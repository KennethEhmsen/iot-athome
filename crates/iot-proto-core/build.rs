//! Build script: compile every `.proto` in `schemas/` into prost message
//! types. `tonic` service codegen (the gRPC client / server traits) stays
//! in the `iot-proto` wrapper crate next door — this crate is the
//! wasm32-wasip2-friendly half of the split.
//!
//! Uses [`protox`] so no `protoc` binary is required on the build box.

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
    prost_build::Config::new().compile_fds(file_descriptors)?;

    Ok(())
}
