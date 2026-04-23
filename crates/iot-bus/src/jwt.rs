//! NATS decentralized-auth JWT minter (M3 W1.3).
//!
//! Mints the user-tier JWTs an operator needs to hand out when the
//! broker is running in `nats-server --auth=operator-jwt` mode. This
//! module is the **cryptographic** half of the broker-JWT-bootstrap
//! story — pure, no I/O, unit-testable.
//!
//! The second half (wiring up the NATS server config + the
//! `iotctl plugin install` post-install step that uses this to mint
//! a `.creds` file from the plugin's existing `nats.nkey` seed + its
//! `acl.json` snapshot) lands in a follow-up commit. ADR-0011 retires
//! fully when both halves are shipping.
//!
//! JWT format (NATS v2 decentralized auth):
//!
//! ```text
//! base64url(header) "." base64url(payload) "." base64url(signature)
//!   header  = {"typ":"JWT","alg":"ed25519-nkey"}
//!   payload = {"iss": issuer_public, "sub": subject_public, "iat": ...,
//!              "name": ..., "nats": { "pub": {...}, "sub": {...},
//!                                     "type": "user", "version": 2 }}
//!   signature = ed25519_sign(issuer_seed, "header.payload")
//! ```
//!
//! The issuer for user JWTs is an *account* nkey; for operator-signed
//! account JWTs it's the *operator* nkey. This module covers user
//! JWTs only — account JWT signing arrives with the server-config
//! work.

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Errors from JWT minting or parsing.
#[derive(Debug, thiserror::Error)]
pub enum JwtError {
    #[error("nkeys: {0}")]
    Nkeys(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("signature verification failed")]
    BadSignature,
    #[error("malformed JWT: {0}")]
    Malformed(&'static str),
    #[error("base64: {0}")]
    Base64(#[from] base64::DecodeError),
}

impl From<nkeys::error::Error> for JwtError {
    fn from(e: nkeys::error::Error) -> Self {
        Self::Nkeys(e.to_string())
    }
}

/// Per-user pub/sub allow-list. Mirrors the NATS "permissions" claim
/// shape. `allow: ["foo.>"]` follows NATS wildcard rules.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserAcl {
    /// Subjects the user is permitted to publish on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_pub: Vec<String>,
    /// Subjects the user is permitted to subscribe to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_sub: Vec<String>,
}

/// NATS user-claims payload. Only the fields we actually populate —
/// NATS tolerates unknown fields + missing optional fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserClaims {
    /// Issuer (account or operator public key — `A…` / `O…` encoding).
    pub iss: String,
    /// Subject (user public key — `U…` encoding).
    pub sub: String,
    /// Issued-at unix seconds.
    pub iat: u64,
    /// Human-readable name (plugin id, user name, …).
    pub name: String,
    /// NATS-specific claim block.
    pub nats: UserNatsClaims,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserNatsClaims {
    /// Pub permissions.
    #[serde(rename = "pub")]
    pub publish: Permissions,
    /// Sub permissions.
    #[serde(rename = "sub")]
    pub subscribe: Permissions,
    /// Claim type; NATS 2.x uses `"user"` here.
    #[serde(rename = "type")]
    pub type_: String,
    /// Claim schema version (2 at time of writing).
    pub version: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Permissions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
}

/// Issue a NATS user JWT.
///
/// * `issuer` — the **account** nkey that signs this user. Its
///   `public_key()` goes into `iss`.
/// * `subject_public` — the **user** nkey's public key (already
///   derived, so this fn doesn't need the user's seed).
/// * `name` — display name; operationally the plugin id.
/// * `acl` — pub/sub allow-list (passed straight from the
///   `<plugin_dir>/<id>/acl.json` snapshot `iotctl plugin install`
///   wrote).
/// * `iat` — issued-at as unix seconds. Caller provides so tests
///   can pin timestamps.
///
/// # Errors
/// Serialisation of the payload, or signing failure from the nkeys
/// crate.
pub fn issue_user_jwt(
    issuer: &nkeys::KeyPair,
    subject_public: &str,
    name: &str,
    acl: &UserAcl,
    iat: u64,
) -> Result<String, JwtError> {
    let claims = UserClaims {
        iss: issuer.public_key(),
        sub: subject_public.to_owned(),
        iat,
        name: name.to_owned(),
        nats: UserNatsClaims {
            publish: Permissions {
                allow: acl.allow_pub.clone(),
            },
            subscribe: Permissions {
                allow: acl.allow_sub.clone(),
            },
            type_: "user".to_owned(),
            version: 2,
        },
    };

    // Header + payload, each base64url(no-pad)-encoded.
    let header = br#"{"typ":"JWT","alg":"ed25519-nkey"}"#;
    let header_b64 = B64URL.encode(header);
    let payload = serde_json::to_vec(&claims)?;
    let payload_b64 = B64URL.encode(payload);

    // Sign "header.payload" with the account seed — ed25519 under
    // the hood per the nkeys wire contract.
    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = issuer.sign(signing_input.as_bytes())?;
    let sig_b64 = B64URL.encode(sig);

    Ok(format!("{signing_input}.{sig_b64}"))
}

/// Parse + verify a user JWT against `issuer`'s public key. Returns
/// the claims on success.
///
/// # Errors
/// `BadSignature` when the signature doesn't verify under `issuer`;
/// `Malformed` for the wrong number of `.`-segments; `Json` /
/// `Base64` for payload decode errors.
pub fn verify_user_jwt(issuer: &nkeys::KeyPair, token: &str) -> Result<UserClaims, JwtError> {
    let mut segs = token.split('.');
    let header_b64 = segs.next().ok_or(JwtError::Malformed("missing header"))?;
    let payload_b64 = segs.next().ok_or(JwtError::Malformed("missing payload"))?;
    let sig_b64 = segs
        .next()
        .ok_or(JwtError::Malformed("missing signature"))?;
    if segs.next().is_some() {
        return Err(JwtError::Malformed("extra segment"));
    }

    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = B64URL.decode(sig_b64)?;
    issuer
        .verify(signing_input.as_bytes(), &sig)
        .map_err(|_| JwtError::BadSignature)?;

    let payload = B64URL.decode(payload_b64)?;
    let claims: UserClaims = serde_json::from_slice(&payload)?;
    Ok(claims)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn fixed_acl() -> UserAcl {
        UserAcl {
            allow_pub: vec!["device.demo-echo.>".into()],
            allow_sub: vec!["cmd.demo-echo.>".into()],
        }
    }

    #[test]
    fn jwt_roundtrip_verifies() {
        let account = nkeys::KeyPair::new_account();
        let user = nkeys::KeyPair::new_user();

        let token = issue_user_jwt(
            &account,
            &user.public_key(),
            "demo-echo",
            &fixed_acl(),
            1_700_000_000,
        )
        .expect("issue");

        let claims = verify_user_jwt(&account, &token).expect("verify");
        assert_eq!(claims.iss, account.public_key());
        assert_eq!(claims.sub, user.public_key());
        assert_eq!(claims.name, "demo-echo");
        assert_eq!(claims.iat, 1_700_000_000);
        assert_eq!(claims.nats.type_, "user");
        assert_eq!(claims.nats.version, 2);
        assert_eq!(claims.nats.publish.allow, vec!["device.demo-echo.>"]);
        assert_eq!(claims.nats.subscribe.allow, vec!["cmd.demo-echo.>"]);
    }

    #[test]
    fn jwt_from_wrong_issuer_fails() {
        let correct = nkeys::KeyPair::new_account();
        let impostor = nkeys::KeyPair::new_account();
        let user = nkeys::KeyPair::new_user();

        let token = issue_user_jwt(
            &correct,
            &user.public_key(),
            "demo-echo",
            &fixed_acl(),
            1_700_000_000,
        )
        .unwrap();

        let err = verify_user_jwt(&impostor, &token).unwrap_err();
        assert!(matches!(err, JwtError::BadSignature), "{err:?}");
    }

    #[test]
    fn malformed_rejections() {
        let account = nkeys::KeyPair::new_account();
        // Missing signature segment.
        assert!(matches!(
            verify_user_jwt(&account, "eyJhbGci.eyJpc3Mi"),
            Err(JwtError::Malformed(_))
        ));
        // Missing payload segment.
        assert!(matches!(
            verify_user_jwt(&account, "eyJhbGci"),
            Err(JwtError::Malformed(_))
        ));
        // Empty string.
        assert!(matches!(
            verify_user_jwt(&account, ""),
            Err(JwtError::Malformed(_))
        ));
    }

    #[test]
    fn claims_roundtrip_json() {
        // A round-trip through serde_json + our own types gives back
        // equivalent claims — guards us against someone changing a
        // field name mid-flight without updating the NATS-wire match.
        let claims = UserClaims {
            iss: "AACME".into(),
            sub: "UUSR".into(),
            iat: 1,
            name: "x".into(),
            nats: UserNatsClaims {
                publish: Permissions {
                    allow: vec!["a.>".into()],
                },
                subscribe: Permissions::default(),
                type_: "user".into(),
                version: 2,
            },
        };
        let s = serde_json::to_string(&claims).unwrap();
        // Check on-wire field names match NATS conventions.
        assert!(s.contains(r#""pub":{"allow":["a.>"]}"#));
        assert!(s.contains(r#""type":"user""#));
        assert!(s.contains(r#""version":2"#));
    }
}
