//! NATS decentralized-auth JWT minter (M3 W1.3 + M5a W1).
//!
//! Mints the operator-, account-, and user-tier JWTs the NATS server
//! needs when running in `nats-server` with an `operator:` + memory
//! resolver configuration. This module is the **cryptographic** half
//! of the broker-JWT-bootstrap story — pure, no I/O, unit-testable.
//!
//! The second half (wiring up the NATS server config + the
//! `iotctl plugin install` post-install step that uses this to mint
//! a `.creds` file from the plugin's existing `nats.nkey` seed + its
//! `acl.json` snapshot, plus `tools/devcerts/mint.sh` producing the
//! operator/account keypairs) lands alongside — ADR-0011 retires
//! fully at M5a W1.
//!
//! JWT format (NATS v2 decentralized auth):
//!
//! ```text
//! base64url(header) "." base64url(payload) "." base64url(signature)
//!   header  = {"typ":"JWT","alg":"ed25519-nkey"}
//!   payload = {"iss": issuer_public, "sub": subject_public, "iat": ...,
//!              "name": ..., "nats": { … claim block keyed on "type" }}
//!   signature = ed25519_sign(issuer_seed, "header.payload")
//! ```
//!
//! * **Operator → Account**: operator nkey signs; claim type `account`.
//!   Account JWTs are what the NATS server config's
//!   `resolver_preload` map contains.
//! * **Account → User**: account nkey signs; claim type `user`. User
//!   JWTs are what plugins hand the server on connect (via the
//!   `.creds` file format from [`format_creds_file`]).

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
    /// User JWT's `exp` claim is in the past relative to `now`. Bucket
    /// 1 audit H1 closure — pre-fix, leaked `nats.creds` files were
    /// usable for the lifetime of the account keypair. The minter now
    /// always populates `exp`; verifiers reject expired tokens.
    #[error("JWT expired (exp={exp}, now={now})")]
    Expired { now: u64, exp: u64 },
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
///
/// `exp` and `jti` are serialised only when non-default so JWTs minted
/// by older builds (no expiry, no revocation id) still round-trip cleanly
/// through [`verify_user_jwt`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserClaims {
    /// Issuer (account or operator public key — `A…` / `O…` encoding).
    pub iss: String,
    /// Subject (user public key — `U…` encoding).
    pub sub: String,
    /// Issued-at unix seconds.
    pub iat: u64,
    /// Expiry as unix seconds. `0` = no expiry (legacy / pre-Bucket-1
    /// path). Always populated by [`issue_user_jwt`] going forward.
    /// NATS server enforces this on connect; [`verify_user_jwt`]
    /// enforces locally so the host can reject stale creds before
    /// even attempting to authenticate.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub exp: u64,
    /// Random JWT ID. Sets up the revocation-list option a future ADR
    /// will define — every JWT carries a unique handle so a CRL-style
    /// store can deny a single token without rotating the issuing
    /// account key. Empty = legacy.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub jti: String,
    /// Human-readable name (plugin id, user name, …).
    pub name: String,
    /// NATS-specific claim block.
    pub nats: UserNatsClaims,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(x: &u64) -> bool {
    *x == 0
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

/// NATS account-claims payload. Minimum surface we need for dev
/// bootstrap: type + version + (optional) JetStream limits. The
/// server accepts unknown fields + missing optional fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountClaims {
    /// Operator public key (`O…`).
    pub iss: String,
    /// Account public key (`A…`).
    pub sub: String,
    /// Issued-at unix seconds.
    pub iat: u64,
    /// Display name (e.g. `"IOT"`).
    pub name: String,
    /// NATS-specific claim block.
    pub nats: AccountNatsClaims,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountNatsClaims {
    /// JetStream + connection limits. `-1` means unlimited.
    #[serde(default)]
    pub limits: AccountLimits,
    /// Claim type — always `"account"`.
    #[serde(rename = "type")]
    pub type_: String,
    /// Claim schema version — 2 at time of writing.
    pub version: u32,
}

/// Account-level limits. Defaults are "permissive dev" — unlimited
/// subs/payload/connections plus 256 MB memory + 2 GB disk JetStream
/// quota, mirroring the `deploy/compose/nats/nats.conf` settings.
///
/// Prod deployments override these through the mint script; the
/// account JWT the memory resolver preloads is the source of truth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountLimits {
    /// Max number of subscriptions; `-1` = unlimited.
    pub subs: i64,
    /// Max publish payload bytes; `-1` = unlimited.
    pub payload: i64,
    /// Max in-flight data bytes; `-1` = unlimited.
    pub data: i64,
    /// Max imports; `-1` = unlimited.
    pub imports: i64,
    /// Max exports; `-1` = unlimited.
    pub exports: i64,
    /// Allow wildcard exports.
    pub wildcards: bool,
    /// Max concurrent connections; `-1` = unlimited.
    pub conn: i64,
    /// Max leafnode connections; `-1` = unlimited.
    pub leaf: i64,
    /// JetStream in-memory store quota (bytes).
    pub mem_storage: i64,
    /// JetStream on-disk store quota (bytes).
    pub disk_storage: i64,
    /// Max JetStream streams; `-1` = unlimited.
    pub streams: i64,
    /// Max JetStream consumers; `-1` = unlimited.
    pub consumer: i64,
}

impl Default for AccountLimits {
    fn default() -> Self {
        // Mirrors deploy/compose/nats/nats.conf's single-account
        // jetstream quota. Adjust for prod via the mint script.
        Self {
            subs: -1,
            payload: -1,
            data: -1,
            imports: -1,
            exports: -1,
            wildcards: true,
            conn: -1,
            leaf: -1,
            mem_storage: 256 * 1024 * 1024,
            disk_storage: 2 * 1024 * 1024 * 1024,
            streams: -1,
            consumer: -1,
        }
    }
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
/// * `exp` — expiry as unix seconds. `0` skips the claim (legacy
///   no-expiry behaviour, kept on the API for the operator/account
///   trust-root path which still rotates only via destructive
///   `iotctl nats bootstrap --force`). Real plugin creds always set
///   this — see Bucket 1 audit H1.
/// * `jti` — random JWT id, used as the revocation-list handle if
///   one is later added. Empty = skip the claim.
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
    exp: u64,
    jti: &str,
) -> Result<String, JwtError> {
    let claims = UserClaims {
        iss: issuer.public_key(),
        sub: subject_public.to_owned(),
        iat,
        exp,
        jti: jti.to_owned(),
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

/// Issue a NATS account JWT.
///
/// The issuer is the **operator** keypair (its public key is in
/// `iss`). The subject is the **account** public key (already
/// derived, so this fn doesn't take the account seed).
///
/// The returned JWT is what the NATS server's `resolver_preload`
/// map stores — i.e. `resolver_preload: { "<account_pub>": "<jwt>" }`.
///
/// # Errors
/// Serialisation of the payload, or signing failure from the nkeys
/// crate.
pub fn issue_account_jwt(
    operator: &nkeys::KeyPair,
    account_public: &str,
    name: &str,
    limits: AccountLimits,
    iat: u64,
) -> Result<String, JwtError> {
    let claims = AccountClaims {
        iss: operator.public_key(),
        sub: account_public.to_owned(),
        iat,
        name: name.to_owned(),
        nats: AccountNatsClaims {
            limits,
            type_: "account".to_owned(),
            version: 2,
        },
    };

    let header = br#"{"typ":"JWT","alg":"ed25519-nkey"}"#;
    let header_b64 = B64URL.encode(header);
    let payload = serde_json::to_vec(&claims)?;
    let payload_b64 = B64URL.encode(payload);

    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig = operator.sign(signing_input.as_bytes())?;
    let sig_b64 = B64URL.encode(sig);

    Ok(format!("{signing_input}.{sig_b64}"))
}

/// Format a NATS credentials-file blob containing a user JWT + user
/// seed, readable by `nats-server` + `async-nats` +
/// [`ConnectOptions::credentials_file`].
///
/// The format is the de-facto `nsc` / NATS CLI convention — two
/// labelled PEM-ish blocks with a human-readable warning between:
///
/// ```text
/// -----BEGIN NATS USER JWT-----
/// <jwt>
/// ------END NATS USER JWT------
///
/// ************************* IMPORTANT *************************
/// NKEY Seed printed below can be used to sign and prove identity.
/// NKEYs are sensitive and should be treated as secrets.
///
/// -----BEGIN USER NKEY SEED-----
/// <seed>
/// ------END USER NKEY SEED------
///
/// *************************************************************
/// ```
///
/// [`ConnectOptions::credentials_file`]: https://docs.rs/async-nats/latest/async_nats/struct.ConnectOptions.html#method.credentials_file
#[must_use]
pub fn format_creds_file(user_jwt: &str, user_seed: &str) -> String {
    format!(
        "\
-----BEGIN NATS USER JWT-----
{user_jwt}
------END NATS USER JWT------

************************* IMPORTANT *************************
NKEY Seed printed below can be used to sign and prove identity.
NKEYs are sensitive and should be treated as secrets.

-----BEGIN USER NKEY SEED-----
{user_seed}
------END USER NKEY SEED------

*************************************************************
"
    )
}

/// Parse + verify an account JWT against the `operator`'s public key.
///
/// # Errors
/// `BadSignature` when the signature doesn't verify under `operator`;
/// `Malformed` for the wrong number of `.`-segments; `Json` /
/// `Base64` for payload decode errors.
pub fn verify_account_jwt(
    operator: &nkeys::KeyPair,
    token: &str,
) -> Result<AccountClaims, JwtError> {
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
    operator
        .verify(signing_input.as_bytes(), &sig)
        .map_err(|_| JwtError::BadSignature)?;

    let payload = B64URL.decode(payload_b64)?;
    let claims: AccountClaims = serde_json::from_slice(&payload)?;
    Ok(claims)
}

/// Parse + verify a user JWT against `issuer`'s public key. Returns
/// the claims on success.
///
/// `now` is the unix-seconds wall-clock used for the `exp` check.
/// Tokens with `exp == 0` (legacy / pre-Bucket-1 mints) are accepted
/// regardless — that lane is documented as "no expiry" by the NATS v2
/// spec and we stay compatible with already-on-disk creds. Tokens
/// with a non-zero `exp` are rejected with [`JwtError::Expired`] when
/// `now >= exp`. Pass `0` to skip the freshness check entirely (used
/// by tests that only care about signature validity).
///
/// # Errors
/// `BadSignature` when the signature doesn't verify under `issuer`;
/// `Expired` when the JWT's `exp` is in the past relative to `now`;
/// `Malformed` for the wrong number of `.`-segments; `Json` /
/// `Base64` for payload decode errors.
pub fn verify_user_jwt(
    issuer: &nkeys::KeyPair,
    token: &str,
    now: u64,
) -> Result<UserClaims, JwtError> {
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
    // Freshness check. `now == 0` is the test-only escape hatch — real
    // callers pass `SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs()`.
    if claims.exp != 0 && now != 0 && now >= claims.exp {
        return Err(JwtError::Expired {
            now,
            exp: claims.exp,
        });
    }
    Ok(claims)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn fixed_acl() -> UserAcl {
        UserAcl {
            allow_pub: vec!["device.demo-echo.>".into()],
            allow_sub: vec!["cmd.demo-echo.>".into()],
        }
    }

    const IAT: u64 = 1_700_000_000;
    const EXP: u64 = 1_700_086_400; // IAT + 24 h
    const JTI: &str = "01HV5Z3M9G7Q8P3N5K6X7Y8Z9A";

    #[test]
    fn jwt_roundtrip_verifies() {
        let account = nkeys::KeyPair::new_account();
        let user = nkeys::KeyPair::new_user();

        let token = issue_user_jwt(
            &account,
            &user.public_key(),
            "demo-echo",
            &fixed_acl(),
            IAT,
            EXP,
            JTI,
        )
        .expect("issue");

        let claims = verify_user_jwt(&account, &token, IAT + 60).expect("verify");
        assert_eq!(claims.iss, account.public_key());
        assert_eq!(claims.sub, user.public_key());
        assert_eq!(claims.name, "demo-echo");
        assert_eq!(claims.iat, IAT);
        assert_eq!(claims.exp, EXP);
        assert_eq!(claims.jti, JTI);
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
            IAT,
            EXP,
            JTI,
        )
        .unwrap();

        let err = verify_user_jwt(&impostor, &token, IAT + 60).unwrap_err();
        assert!(matches!(err, JwtError::BadSignature), "{err:?}");
    }

    #[test]
    fn malformed_rejections() {
        let account = nkeys::KeyPair::new_account();
        // Missing signature segment.
        assert!(matches!(
            verify_user_jwt(&account, "eyJhbGci.eyJpc3Mi", 0),
            Err(JwtError::Malformed(_))
        ));
        // Missing payload segment.
        assert!(matches!(
            verify_user_jwt(&account, "eyJhbGci", 0),
            Err(JwtError::Malformed(_))
        ));
        // Empty string.
        assert!(matches!(
            verify_user_jwt(&account, "", 0),
            Err(JwtError::Malformed(_))
        ));
    }

    #[test]
    fn expired_jwt_rejected() {
        // exp at IAT + 24h; verify a second past that → Expired.
        let account = nkeys::KeyPair::new_account();
        let user = nkeys::KeyPair::new_user();
        let token = issue_user_jwt(
            &account,
            &user.public_key(),
            "demo-echo",
            &fixed_acl(),
            IAT,
            EXP,
            JTI,
        )
        .unwrap();

        let err = verify_user_jwt(&account, &token, EXP).unwrap_err();
        match err {
            JwtError::Expired { now, exp } => {
                assert_eq!(now, EXP);
                assert_eq!(exp, EXP);
            }
            other => panic!("expected Expired, got {other:?}"),
        }

        // And one second further — same outcome, different `now`.
        let err2 = verify_user_jwt(&account, &token, EXP + 1).unwrap_err();
        assert!(matches!(err2, JwtError::Expired { .. }));
    }

    #[test]
    fn pre_expiry_jwt_accepted() {
        // Right up to the second before exp the token verifies.
        let account = nkeys::KeyPair::new_account();
        let user = nkeys::KeyPair::new_user();
        let token = issue_user_jwt(
            &account,
            &user.public_key(),
            "demo-echo",
            &fixed_acl(),
            IAT,
            EXP,
            JTI,
        )
        .unwrap();
        // exp - 1 is still inside the window.
        let claims = verify_user_jwt(&account, &token, EXP - 1).expect("not yet expired");
        assert_eq!(claims.exp, EXP);
    }

    #[test]
    fn legacy_no_exp_jwt_accepted() {
        // exp == 0 means "no expiry"; verify still succeeds at any
        // wall-clock — keeps already-on-disk creds from older builds
        // valid through the upgrade.
        let account = nkeys::KeyPair::new_account();
        let user = nkeys::KeyPair::new_user();
        let token = issue_user_jwt(
            &account,
            &user.public_key(),
            "demo-echo",
            &fixed_acl(),
            IAT,
            0,
            "",
        )
        .unwrap();
        let claims = verify_user_jwt(&account, &token, 9_999_999_999).expect("no-exp accepts");
        assert_eq!(claims.exp, 0);
        assert_eq!(claims.jti, "");
    }

    #[test]
    fn jti_roundtrips_through_serde() {
        let account = nkeys::KeyPair::new_account();
        let user = nkeys::KeyPair::new_user();
        let jti = "01HV5Z3M9G7Q8P3N5K6X7Y8Z9B";
        let token = issue_user_jwt(
            &account,
            &user.public_key(),
            "demo-echo",
            &fixed_acl(),
            IAT,
            EXP,
            jti,
        )
        .unwrap();
        let claims = verify_user_jwt(&account, &token, IAT).expect("verify");
        assert_eq!(claims.jti, jti);
    }

    #[test]
    fn account_jwt_roundtrip_verifies() {
        let operator = nkeys::KeyPair::new_operator();
        let account = nkeys::KeyPair::new_account();

        let token = issue_account_jwt(
            &operator,
            &account.public_key(),
            "IOT",
            AccountLimits::default(),
            1_700_000_000,
        )
        .expect("issue");

        let claims = verify_account_jwt(&operator, &token).expect("verify");
        assert_eq!(claims.iss, operator.public_key());
        assert_eq!(claims.sub, account.public_key());
        assert_eq!(claims.name, "IOT");
        assert_eq!(claims.nats.type_, "account");
        assert_eq!(claims.nats.version, 2);
        assert!(claims.nats.limits.wildcards);
        assert_eq!(claims.nats.limits.mem_storage, 256 * 1024 * 1024);
    }

    #[test]
    fn account_jwt_from_wrong_operator_fails() {
        let correct = nkeys::KeyPair::new_operator();
        let impostor = nkeys::KeyPair::new_operator();
        let account = nkeys::KeyPair::new_account();

        let token = issue_account_jwt(
            &correct,
            &account.public_key(),
            "IOT",
            AccountLimits::default(),
            1_700_000_000,
        )
        .unwrap();

        let err = verify_account_jwt(&impostor, &token).unwrap_err();
        assert!(matches!(err, JwtError::BadSignature), "{err:?}");
    }

    #[test]
    fn creds_file_contains_both_blocks() {
        let creds = format_creds_file("jwt.header.sig", "SUAABCDE");
        assert!(creds.contains("-----BEGIN NATS USER JWT-----"));
        assert!(creds.contains("jwt.header.sig"));
        assert!(creds.contains("------END NATS USER JWT------"));
        assert!(creds.contains("-----BEGIN USER NKEY SEED-----"));
        assert!(creds.contains("SUAABCDE"));
        assert!(creds.contains("------END USER NKEY SEED------"));
        // End marker line also closes the file cleanly.
        assert!(creds.ends_with("*************************************************************\n"));
    }

    #[test]
    fn full_bootstrap_chain() {
        // Operator → signs account JWT → account → signs user JWT →
        // user connects. Mirrors the production trust path.
        let operator = nkeys::KeyPair::new_operator();
        let account = nkeys::KeyPair::new_account();
        let user = nkeys::KeyPair::new_user();

        let account_jwt = issue_account_jwt(
            &operator,
            &account.public_key(),
            "IOT",
            AccountLimits::default(),
            IAT,
        )
        .expect("account issue");
        verify_account_jwt(&operator, &account_jwt).expect("account verify");

        let user_jwt = issue_user_jwt(
            &account,
            &user.public_key(),
            "demo-echo",
            &fixed_acl(),
            IAT,
            EXP,
            JTI,
        )
        .expect("user issue");
        verify_user_jwt(&account, &user_jwt, IAT + 60).expect("user verify");

        // A user JWT minted by the account should NOT verify under the
        // operator — isolation between the two trust tiers.
        assert!(matches!(
            verify_user_jwt(&operator, &user_jwt, IAT + 60),
            Err(JwtError::BadSignature)
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
            exp: 86_401,
            jti: "01HV5Z3M9G7Q8P3N5K6X7Y8Z9C".into(),
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
        assert!(s.contains(r#""exp":86401"#));
        assert!(s.contains(r#""jti":"01HV5Z3M9G7Q8P3N5K6X7Y8Z9C""#));

        // exp == 0 + jti == "" stay off the wire (legacy compat).
        let legacy = UserClaims {
            exp: 0,
            jti: String::new(),
            ..claims
        };
        let s2 = serde_json::to_string(&legacy).unwrap();
        assert!(!s2.contains(r#""exp""#), "exp=0 must not serialize: {s2}");
        assert!(
            !s2.contains(r#""jti""#),
            "empty jti must not serialize: {s2}"
        );
    }
}
