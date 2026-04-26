//! End-to-end plugin round-trip.
//!
//! Builds (if not already) the `demo-echo` plugin to `wasm32-wasip2`,
//! loads it through the host **via its manifest**, and invokes `init()` +
//! `on_message()` to prove the full M2-W2 chain:
//!
//!   - manifest.yaml parsed into CapabilityMap
//!   - host-call capability check fires on out-of-scope subjects
//!   - `plugin.denied` audit entry appended + hash-chain intact
//!   - (bus publish on allowed subjects is a no-op here — no NATS in
//!     this test; `iot-bus/tests/roundtrip.rs` already covers that path)
//!
//! Skipped when `cargo` or the wasip2 toolchain is unavailable at test
//! time. CI installs both.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;

use iot_plugin_host::HostBindings;

#[tokio::test]
async fn demo_echo_manifest_load_init_deny_audit() -> Result<(), Box<dyn std::error::Error>> {
    let Some(plugin_dir) = locate_or_build_demo_echo() else {
        eprintln!("demo-echo not found and `cargo build` unavailable — skipping");
        return Ok(());
    };

    let audit_path = std::env::temp_dir().join(format!(
        "iot-plugin-host-audit-{}.jsonl",
        std::process::id()
    ));
    if audit_path.exists() {
        std::fs::remove_file(&audit_path).ok();
    }
    let audit = Arc::new(iot_audit::AuditLog::open(&audit_path).await?);

    let engine = iot_plugin_host::build_engine()?;
    let (mut store, plugin, manifest) = iot_plugin_host::load_plugin_dir(
        &engine,
        &plugin_dir,
        HostBindings {
            bus: None,
            audit: Some(audit.clone()),
            mqtt: None,
            registry: None,
            http: None,
        },
    )
    .await?;
    assert_eq!(manifest.id, "demo-echo");
    // The manifest's allow-list drove the CapabilityMap — not the test.
    assert!(
        manifest
            .capabilities
            .bus
            .publish
            .iter()
            .any(|p| p.starts_with("device.demo-echo")),
        "manifest declares device.demo-echo.>"
    );

    // 1. init()
    plugin
        .iot_plugin_host_runtime()
        .call_init(&mut store)
        .await?
        .expect("init returned an app-level error");

    // 2. on-message with an in-scope subject -> ok
    plugin
        .iot_plugin_host_runtime()
        .call_on_message(
            &mut store,
            "device.demo-echo.kitchen.state",
            "iot.device.v1.EntityState",
            &b"test payload".to_vec(),
        )
        .await?
        .expect("in-scope publish should succeed");

    // 3. on-message with an out-of-scope subject -> capability.denied.
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

    // 4. Audit log captured the deny and the hash chain verifies.
    audit.verify().await?;
    let audit_text = std::fs::read_to_string(&audit_path)?;
    let denied_entries: Vec<_> = audit_text
        .lines()
        .filter(|l| l.contains("plugin.denied"))
        .collect();
    assert_eq!(denied_entries.len(), 1, "exactly one plugin.denied entry");
    assert!(denied_entries[0].contains("device.other.kitchen.state"));
    assert!(denied_entries[0].contains("demo-echo"));

    std::fs::remove_file(&audit_path).ok();
    Ok(())
}

/// Finds the demo-echo plugin directory, building the wasm component if
/// needed, and symlinks / copies it to the manifest's `entrypoint` name.
fn locate_or_build_demo_echo() -> Option<PathBuf> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)?
        .to_path_buf();
    let plugin_dir = repo_root.join("plugins").join("demo-echo");
    let built = plugin_dir
        .join("target")
        .join("wasm32-wasip2")
        .join("debug")
        .join("demo_echo.wasm");
    if !built.exists() {
        let status = std::process::Command::new("cargo")
            .args(["build"])
            .current_dir(&plugin_dir)
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
    }
    // The manifest says `entrypoint: plugin.wasm`. Copy the freshly-built
    // artifact into place (cheap; a symlink would also work on Unix).
    let dst = plugin_dir.join("plugin.wasm");
    if !dst.exists() || file_newer(&built, &dst) {
        std::fs::copy(&built, &dst).ok()?;
    }
    Some(plugin_dir)
}

fn file_newer(a: &std::path::Path, b: &std::path::Path) -> bool {
    let ma = std::fs::metadata(a).and_then(|m| m.modified()).ok();
    let mb = std::fs::metadata(b).and_then(|m| m.modified()).ok();
    matches!((ma, mb), (Some(a), Some(b)) if a > b)
}
