//! Stable identifiers.
//!
//! Per ADR-0004, every device, plugin, user, and rule is identified by a ULID.
//! We wrap the upstream `ulid::Ulid` in a newtype per domain so the type system
//! prevents mixing a `DeviceId` with a `RuleId`.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// Errors produced while parsing a `DeviceId` from a string.
#[derive(Debug, Error)]
pub enum IdParseError {
    #[error("invalid ulid: {0}")]
    Invalid(#[from] ulid::DecodeError),
}

/// Stable device identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceId(pub ulid::Ulid);

impl DeviceId {
    #[must_use]
    pub fn new() -> Self {
        Self(ulid::Ulid::new())
    }
}

impl Default for DeviceId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for DeviceId {
    type Err = IdParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(ulid::Ulid::from_string(s)?))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_string() {
        let id = DeviceId::new();
        let parsed: DeviceId = id.to_string().parse().expect("valid ulid");
        assert_eq!(id, parsed);
    }
}
