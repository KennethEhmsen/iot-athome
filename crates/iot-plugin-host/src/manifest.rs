//! Plugin manifest parser.
//!
//! Reads the YAML form (or equivalent JSON) described by
//! `schemas/plugin-manifest.schema.json` into strongly-typed Rust. The host
//! consults this at install time to decide whether to accept the plugin
//! (signing + capability approvals), and at load time to construct the
//! [`CapabilityMap`] the host-call enforcer checks against.
//!
//! We parse only the fields the host actively uses today. Marketplace
//! metadata (authors, license, homepage, SBOM) stays in the raw YAML until
//! someone needs to act on it.

use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::capabilities::CapabilityMap;

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("yaml: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("unsupported schema_version {0} (host supports 1)")]
    UnsupportedSchema(u32),
    #[error("unsupported runtime `{0}` (host supports: wasm-component)")]
    UnsupportedRuntime(String),
}

/// Strongly-typed subset of the plugin manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub runtime: String,
    pub entrypoint: String,
    #[serde(default)]
    pub capabilities: CapabilityMap,
    #[serde(default)]
    pub resources: Resources,
    /// Signature claim metadata. Multiple entries are allowed (e.g. one
    /// cosign-keyless + one signed-by-key claim). The plugin host doesn't
    /// trust the manifest's claims by themselves — they're informational
    /// for `iotctl plugin list` and the panel; trust comes from
    /// out-of-band cosign-style verification at install time.
    #[serde(default)]
    pub signatures: Vec<SignatureClaim>,
}

/// One element of `manifest.signatures[]`. Mirrors `cosign sign-blob` +
/// Rekor / Fulcio output: a mechanism, an OIDC identity (for keyless),
/// and a Rekor entry id.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SignatureClaim {
    /// `cosign-keyless`, `cosign-key`, `minisign`, …
    #[serde(default)]
    pub mechanism: String,
    /// OIDC subject for keyless mode (e.g. `developer@example.com`),
    /// the key fingerprint for signed-by-key, etc.
    #[serde(default)]
    pub identity: String,
    /// OIDC issuer for keyless mode (`https://github.com/login/oauth`,
    /// `https://accounts.google.com`, …).
    #[serde(default)]
    pub issuer: String,
    /// Rekor transparency-log entry UUID.
    #[serde(default)]
    pub rekor_entry: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Resources {
    #[serde(default = "default_mem_mb_max")]
    pub memory_mb_max: u64,
    #[serde(default = "default_cpu_pct_max")]
    pub cpu_pct_max: u32,
    #[serde(default)]
    pub fuel_max: u64,
}

const fn default_mem_mb_max() -> u64 {
    128
}
const fn default_cpu_pct_max() -> u32 {
    25
}

impl Manifest {
    /// Parse a manifest from a file on disk and enforce host-supported
    /// schema_version + runtime.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ManifestError> {
        let bytes = std::fs::read(path.as_ref())?;
        let m: Self = serde_yaml::from_slice(&bytes)?;
        if m.schema_version != 1 {
            return Err(ManifestError::UnsupportedSchema(m.schema_version));
        }
        if m.runtime != "wasm-component" {
            return Err(ManifestError::UnsupportedRuntime(m.runtime));
        }
        Ok(m)
    }
}

/// Apply the same validation rules [`Manifest::load`] does, on an
/// already-parsed instance. Used by the tests so they can construct a
/// Manifest from an in-memory string.
#[cfg(test)]
fn enforce(m: Manifest) -> Result<Manifest, ManifestError> {
    if m.schema_version != 1 {
        return Err(ManifestError::UnsupportedSchema(m.schema_version));
    }
    if m.runtime != "wasm-component" {
        return Err(ManifestError::UnsupportedRuntime(m.runtime));
    }
    Ok(m)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    const DEMO_ECHO: &str = r#"
schema_version: 1
id: demo-echo
name: Demo Echo
version: 0.1.0
description: Round-trips every message.
runtime: wasm-component
entrypoint: plugin.wasm
capabilities:
  bus:
    publish:
      - "device.demo-echo.>"
    subscribe:
      - "cmd.demo-echo.>"
resources:
  memory_mb_max: 32
  cpu_pct_max: 5
"#;

    #[test]
    fn parses_known_shape() {
        let m: Manifest = serde_yaml::from_str(DEMO_ECHO).expect("parse");
        assert_eq!(m.id, "demo-echo");
        assert_eq!(m.entrypoint, "plugin.wasm");
        assert_eq!(m.capabilities.bus.publish, vec!["device.demo-echo.>"]);
        assert_eq!(m.resources.memory_mb_max, 32);
    }

    #[test]
    fn rejects_unsupported_schema_version() {
        let src = DEMO_ECHO.replace("schema_version: 1", "schema_version: 99");
        let m: Manifest = serde_yaml::from_str(&src).expect("parse");
        assert!(matches!(
            super::enforce(m),
            Err(ManifestError::UnsupportedSchema(99))
        ));
    }

    #[test]
    fn rejects_unsupported_runtime() {
        let src = DEMO_ECHO.replace("runtime: wasm-component", "runtime: oci-container");
        let m: Manifest = serde_yaml::from_str(&src).expect("parse");
        assert!(matches!(
            super::enforce(m),
            Err(ManifestError::UnsupportedRuntime(_))
        ));
    }
}
