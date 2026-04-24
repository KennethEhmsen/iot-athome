//! `iotctl mosquitto …` — Mosquitto helper subcommands (M5a W3 — debt #8).
//!
//! Currently one verb: `regen-acl`. Walks the plugin install dir,
//! reads each plugin's `manifest.yaml`, and writes a Mosquitto 2.0
//! ACL file authorising the host's MQTT user the union of every
//! plugin's `mqtt.subscribe` + `mqtt.publish` allow-list.
//!
//! Manual operator workflow:
//!
//! ```sh
//! iotctl mosquitto regen-acl \
//!   --user iot-plugin-host \
//!   --plugin-dir /var/lib/iotathome/plugins \
//!   --out deploy/compose/mosquitto/acl.conf
//! docker compose -f deploy/compose/dev-stack.yml \
//!   exec mosquitto kill -HUP 1
//! ```
//!
//! Auto-regenerate-on-install hook is a follow-up polish; for now
//! the operator runs this after each `iotctl plugin install` /
//! `uninstall`.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::Subcommand;

use iot_plugin_host::manifest::Manifest;
use iot_plugin_host::mqtt_acl;

#[derive(Debug, Subcommand)]
pub enum MosquittoCmd {
    /// Regenerate the Mosquitto ACL file from the union of every
    /// installed plugin's `mqtt.{subscribe,publish}` allow-list.
    ///
    /// The output is fail-closed: when zero plugins are installed
    /// the file grants the user no topics at all (Mosquitto 2.0
    /// denies absent an explicit `topic` rule). Operators should
    /// `kill -HUP` Mosquitto after writing the new file so the
    /// broker re-reads it.
    RegenAcl {
        /// Mosquitto username to authorise (typically the host's
        /// mTLS cert CN — Mosquitto's `use_identity_as_username
        /// true` maps the cert subject to a username at handshake
        /// time).
        #[arg(long, default_value = "iot-plugin-host")]
        user: String,
        /// Plugin install root.
        #[arg(
            long,
            env = "IOT_PLUGIN_DIR",
            default_value = "/var/lib/iotathome/plugins"
        )]
        plugin_dir: PathBuf,
        /// ACL output path.
        #[arg(long, default_value = "deploy/compose/mosquitto/acl.conf")]
        out: PathBuf,
    },
}

/// Dispatch `iotctl mosquitto …`.
///
/// # Errors
/// Surfaces filesystem and manifest-parse errors with enough context
/// for an operator to fix the underlying problem.
pub fn run(cmd: &MosquittoCmd) -> Result<()> {
    match cmd {
        MosquittoCmd::RegenAcl {
            user,
            plugin_dir,
            out,
        } => regen_acl(user, plugin_dir, out),
    }
}

fn regen_acl(user: &str, plugin_dir: &std::path::Path, out: &std::path::Path) -> Result<()> {
    let manifests = load_installed_manifests(plugin_dir)?;
    let count = manifests.len();
    let body = mqtt_acl::generate(user, &manifests.iter().collect::<Vec<_>>());
    fs::write(out, &body).with_context(|| format!("write {}", out.display()))?;
    println!(
        "wrote ACL for user `{user}` covering {count} plugin(s) → {}",
        out.display()
    );
    Ok(())
}

/// Walk `plugin_dir` for every subdirectory containing `manifest.yaml`
/// and parse each. Plugins that fail to parse are surfaced as warnings
/// but don't abort regeneration — we'd rather emit an ACL covering the
/// healthy plugins than refuse to update at all.
fn load_installed_manifests(plugin_dir: &std::path::Path) -> Result<Vec<Manifest>> {
    if !plugin_dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in
        fs::read_dir(plugin_dir).with_context(|| format!("read_dir {}", plugin_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let manifest_path = entry.path().join("manifest.yaml");
        if !manifest_path.is_file() {
            continue;
        }
        match Manifest::load(&manifest_path) {
            Ok(m) => out.push(m),
            Err(e) => tracing::warn!(
                path = %manifest_path.display(),
                error = %e,
                "skipping plugin: manifest parse failed"
            ),
        }
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn write_manifest(dir: &std::path::Path, id: &str, subs: &[&str], pubs: &[&str]) {
        use std::fmt::Write as _;
        let plugin_dir = dir.join(id);
        fs::create_dir_all(&plugin_dir).unwrap();
        let mqtt_block = if subs.is_empty() && pubs.is_empty() {
            String::new()
        } else {
            let mut s = String::from("  mqtt:\n");
            if !subs.is_empty() {
                s.push_str("    subscribe:\n");
                for f in subs {
                    let _ = writeln!(s, "      - \"{f}\"");
                }
            }
            if !pubs.is_empty() {
                s.push_str("    publish:\n");
                for f in pubs {
                    let _ = writeln!(s, "      - \"{f}\"");
                }
            }
            s
        };
        let yaml = format!(
            "schema_version: 1\n\
             id: {id}\n\
             version: 0.1.0\n\
             runtime: wasm-component\n\
             entrypoint: plugin.wasm\n\
             capabilities:\n{mqtt_block}\n"
        );
        fs::write(plugin_dir.join("manifest.yaml"), yaml).unwrap();
    }

    #[test]
    fn regen_acl_covers_every_installed_plugin() {
        let td = tempfile::tempdir().unwrap();
        write_manifest(td.path(), "z2m", &["zigbee2mqtt/+"], &["zigbee2mqtt/cmd/+"]);
        write_manifest(td.path(), "sdr", &["rtl_433/+"], &[]);
        let out = td.path().join("acl.conf");

        regen_acl("iot-plugin-host", td.path(), &out).expect("regen");

        let body = fs::read_to_string(&out).unwrap();
        assert!(body.contains("user iot-plugin-host"));
        assert!(body.contains("topic read  zigbee2mqtt/+"));
        assert!(body.contains("topic write zigbee2mqtt/cmd/+"));
        assert!(body.contains("topic read  rtl_433/+"));
    }

    #[test]
    fn regen_acl_on_empty_dir_emits_deny_closed_file() {
        let td = tempfile::tempdir().unwrap();
        let out = td.path().join("acl.conf");
        regen_acl("iot-plugin-host", td.path(), &out).expect("regen");

        let body = fs::read_to_string(&out).unwrap();
        assert!(body.contains("user iot-plugin-host"));
        assert!(!body.contains("topic "), "no topic rules → deny: {body}");
    }

    #[test]
    fn regen_acl_skips_dirs_without_manifest() {
        let td = tempfile::tempdir().unwrap();
        // Plugin with manifest.
        write_manifest(td.path(), "real-plugin", &["a/+"], &[]);
        // Junk dir without manifest — must not break the run.
        fs::create_dir_all(td.path().join("garbage-dir")).unwrap();
        // File at the top level — also not a plugin.
        fs::write(td.path().join("README.md"), "hi").unwrap();

        let out = td.path().join("acl.conf");
        regen_acl("iot-plugin-host", td.path(), &out).expect("regen");

        let body = fs::read_to_string(&out).unwrap();
        assert!(body.contains("topic read  a/+"));
        assert!(body.contains("# plugin: real-plugin"));
        assert!(!body.contains("garbage-dir"));
    }
}
