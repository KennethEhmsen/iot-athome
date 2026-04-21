//! End-to-end plugin round-trip.
//!
//! Builds (if not already) the `demo-echo` plugin to `wasm32-wasip2`,
//! loads it through the host, and invokes `init()` + `on-message()` to
//! prove the bindings + capability check wire up correctly.
//!
//! Skipped on targets / hosts without cargo + the wasip2 toolchain
//! installed (it falls back to `Ok(())`); CI has both.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;

use iot_plugin_host::capabilities::{BusCapabilities, CapabilityMap};

#[tokio::test]
async fn demo_echo_loads_and_inits() -> Result<(), Box<dyn std::error::Error>> {
    // Locate or build the demo-echo component.
    let Some(wasm_path) = locate_or_build_demo_echo() else {
        eprintln!("demo-echo.wasm not found and `cargo build` is unavailable — skipping");
        return Ok(());
    };

    let engine = iot_plugin_host::build_engine()?;
    let capabilities = CapabilityMap {
        bus: BusCapabilities {
            publish: vec!["device.demo-echo.>".into()],
            subscribe: Vec::new(),
        },
        ..Default::default()
    };

    let (mut store, plugin) =
        iot_plugin_host::load_plugin(&engine, &wasm_path, "demo-echo", capabilities).await?;

    // 1. init()
    let init_res = plugin
        .iot_plugin_host_runtime()
        .call_init(&mut store)
        .await?;
    assert!(
        init_res.is_ok(),
        "init returned app-level error: {init_res:?}"
    );

    // 2. on-message with an in-scope subject -> ok
    let ok = plugin
        .iot_plugin_host_runtime()
        .call_on_message(
            &mut store,
            "device.demo-echo.kitchen.state",
            "iot.device.v1.EntityState",
            &b"test payload".to_vec(),
        )
        .await?;
    assert!(ok.is_ok(), "on-message: {ok:?}");

    // 3. on-message with an out-of-scope subject -> capability.denied
    //    (the plugin calls bus.publish which the host rejects).
    let denied = plugin
        .iot_plugin_host_runtime()
        .call_on_message(
            &mut store,
            "device.other.kitchen.state",
            "iot.device.v1.EntityState",
            &b"test payload".to_vec(),
        )
        .await?;
    match denied {
        Err(err) => assert_eq!(err.code, "capability.denied", "{err:?}"),
        Ok(()) => panic!("expected capability.denied"),
    }

    Ok(())
}

/// Finds the pre-built `demo_echo.wasm` artifact, or runs `cargo build`
/// from the plugin directory if the wasip2 toolchain is available.
fn locate_or_build_demo_echo() -> Option<PathBuf> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)?
        .to_path_buf();
    let plugin_dir = repo_root.join("plugins").join("demo-echo");
    let expected = plugin_dir
        .join("target")
        .join("wasm32-wasip2")
        .join("debug")
        .join("demo_echo.wasm");
    if expected.exists() {
        return Some(expected);
    }
    // Try to build. Ignore failure and let the test no-op.
    let status = std::process::Command::new("cargo")
        .args(["build"])
        .current_dir(&plugin_dir)
        .status()
        .ok()?;
    status.success().then_some(expected)
}
