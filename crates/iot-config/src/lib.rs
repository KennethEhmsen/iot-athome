//! Layered configuration loader.
//!
//! See ADR-0010. Layers, in precedence order (lowest first):
//!
//! 1. `/etc/iotathome/default.toml` (shipped)
//! 2. `/etc/iotathome/local.toml` (admin)
//! 3. `/etc/iotathome/conf.d/*.toml` (drop-ins, lexicographic)
//! 4. `/var/lib/iotathome/state.toml` (runtime-written)
//! 5. `IOT_*` environment variables (double-underscore splits nested keys)
//!
//! Deserialisation is generic over the service's own config struct.
//! Validation against an embedded JSON Schema is recommended for every
//! service to catch typos and type drift at boot.

#![forbid(unsafe_code)]

use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use serde::de::DeserializeOwned;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Errors from [`load`].
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("figment: {0}")]
    Figment(#[from] figment::Error),
    #[error("schema validation failed: {0}")]
    Schema(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Override of the default file paths. Handy for tests.
#[derive(Debug, Clone)]
pub struct Paths {
    pub default_toml: PathBuf,
    pub local_toml: PathBuf,
    pub conf_d: PathBuf,
    pub state_toml: PathBuf,
    /// Environment variable prefix. Defaults to `"IOT_"`.
    pub env_prefix: String,
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            default_toml: PathBuf::from("/etc/iotathome/default.toml"),
            local_toml: PathBuf::from("/etc/iotathome/local.toml"),
            conf_d: PathBuf::from("/etc/iotathome/conf.d"),
            state_toml: PathBuf::from("/var/lib/iotathome/state.toml"),
            env_prefix: "IOT_".into(),
        }
    }
}

/// Load and deserialize a typed config struct from the layered sources.
///
/// Missing files are silently skipped (they are layered, not required).
/// Unparseable files ARE errors.
pub fn load<T: DeserializeOwned>(paths: &Paths) -> Result<T, ConfigError> {
    let mut figment = Figment::new();

    if paths.default_toml.exists() {
        figment = figment.merge(Toml::file(&paths.default_toml));
    }
    if paths.local_toml.exists() {
        figment = figment.merge(Toml::file(&paths.local_toml));
    }
    if paths.conf_d.is_dir() {
        for entry in read_conf_d(&paths.conf_d)? {
            figment = figment.merge(Toml::file(entry));
        }
    }
    if paths.state_toml.exists() {
        figment = figment.merge(Toml::file(&paths.state_toml));
    }

    figment = figment.merge(Env::prefixed(&paths.env_prefix).split("__"));

    Ok(figment.extract()?)
}

/// Validate an in-memory JSON value against a JSON Schema.
///
/// Call this after [`load`] once you have the serde-parsed struct — round-trip
/// it through `serde_json::to_value` and feed the result in. Yes, it's a
/// round-trip; it is cheap and catches structural mistakes the service would
/// otherwise hit only at use.
pub fn validate(schema: &serde_json::Value, data: &serde_json::Value) -> Result<(), ConfigError> {
    let validator =
        jsonschema::validator_for(schema).map_err(|e| ConfigError::Schema(e.to_string()))?;
    let result: Result<(), _> = validator.validate(data);
    match result {
        Ok(()) => Ok(()),
        Err(err) => Err(ConfigError::Schema(err.to_string())),
    }
}

fn read_conf_d(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("toml"))
        .collect();
    files.sort();
    Ok(files)
}
