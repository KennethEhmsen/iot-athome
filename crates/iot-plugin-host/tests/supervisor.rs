//! Integration tests for the supervisor + per-plugin runtime task.
//!
//! These exercise the real load + spawn + init + shutdown pipeline
//! against the demo-echo .wasm — no mocking on the Wasmtime side. The
//! plumbing these tests validate:
//!   * `supervise()` respects an existing `.dead-lettered` marker and
//!     exits cleanly without loading the plugin.
//!   * A plugin that's been asked to shut down (via `Shutdown` command)
//!     returns from `run_plugin_task` with `Ok(())`, and the supervisor
//!     treats that as a clean shutdown (returns `Ok(())` immediately,
//!     no restart).
//!
//! The "plugin crashes and gets dead-lettered" path is asserted at the
//! unit level (`supervisor::tests` — CrashTracker + DLQ marker
//! roundtrip) rather than here, because producing a genuine Wasmtime
//! trap from an integration test requires a bespoke trapping .wasm
//! fixture that's out of scope for this commit.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use iot_plugin_host::runtime::{spawn_plugin_task, PluginCommand};
use iot_plugin_host::supervisor::{self, DLQ_MARKER_FILENAME};
use iot_plugin_host::{build_engine, load_plugin_dir, HostBindings};

/// Rebuild/locate the demo-echo .wasm + copy it into the manifest's
/// `entrypoint` slot. Mirrors the helper in `roundtrip.rs`.
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

#[tokio::test]
async fn supervisor_skips_dead_lettered_install() {
    // Stage a fake install dir with the DLQ marker. `supervise()` must
    // bail out before even trying to load the manifest — we'd know if
    // it didn't, because there's no real manifest here.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(DLQ_MARKER_FILENAME), "prior crash").unwrap();

    let engine = build_engine().expect("build engine");
    supervisor::supervise(engine, dir.path().to_path_buf(), HostBindings::default())
        .await
        .expect("DLQ'd plugin supervision returns Ok immediately");
}

#[tokio::test]
async fn plugin_task_cleanly_shuts_down_on_command() {
    // Load the real demo-echo, spawn its task, send Shutdown, confirm
    // the task returns Ok(()). This exercises `run_plugin_task`'s init
    // path + its Shutdown handling — the two most common non-crash exits.
    let Some(plugin_dir) = locate_or_build_demo_echo() else {
        // Build wasm32-wasip2 target isn't installed — skip rather than fail.
        eprintln!("skipping: can't locate or build demo-echo.wasm");
        return;
    };

    let engine = build_engine().expect("build engine");
    let (store, plugin, manifest) = load_plugin_dir(&engine, &plugin_dir, HostBindings::default())
        .await
        .expect("load demo-echo");
    assert_eq!(manifest.id, "demo-echo");

    let handle = spawn_plugin_task(manifest.id.clone(), store, plugin);

    // Send Shutdown. The task's init() runs before the loop polls, so
    // shutdown actually arrives after init completes. That's fine —
    // the mpsc queue buffers it.
    handle
        .tx
        .send(PluginCommand::Shutdown)
        .await
        .expect("send shutdown");

    // The task must exit cleanly within a reasonable window. Without a
    // timeout, a bug that leaves the task hung would wedge the test
    // runner.
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), handle.join)
        .await
        .expect("task exited within timeout")
        .expect("task join succeeded");
    assert!(
        outcome.is_ok(),
        "expected clean shutdown, got crash: {outcome:?}"
    );
}
