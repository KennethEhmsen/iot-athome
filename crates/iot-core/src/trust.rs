//! Device trust levels — mirror of `iot.device.v1.TrustLevel`.

use serde::{Deserialize, Serialize};

/// How a device came to be known to the hub.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// Seen by a scan but not yet accepted by a user.
    Discovered,
    /// Paired by a user through the wizard.
    UserAdded,
    /// Cryptographically attested (Matter certificate, signed firmware).
    Verified,
}
