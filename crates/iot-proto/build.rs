//! Build script: compile all `.proto` files into `$OUT_DIR`.
//!
//! Uses [`protox`] (pure-Rust Protobuf compiler) so we never need a `protoc`
//! binary on the build machine — works identically on Linux, macOS, Windows,
//! and in CI without special setup.

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

    // Rebuild when any .proto changes.
    println!("cargo:rerun-if-changed={}", schemas.display());
    for p in &proto_files {
        println!("cargo:rerun-if-changed={}", p.display());
    }

    let file_descriptors = protox::compile(&proto_files, [&schemas])?;

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_fds(file_descriptors)?;

    Ok(())
}
