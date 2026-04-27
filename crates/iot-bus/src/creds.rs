//! Per-plugin NATS credentials — disk layout + mint helpers.
//!
//! `iot_bus::jwt` is the pure cryptographic module. This one carries
//! the conventions for *where on disk* per-plugin credentials live and
//! the higher-level "mint creds for this install dir" flow that
//! `iotctl plugin install`, `iotctl nats mint-user`, and the host-side
//! refresh task all share.
//!
//! Disk layout under each `<plugin_dir>/<id>/`:
//!
//! ```text
//!   nats.nkey            user nkey seed (0600 on Unix)
//!   acl.json             {plugin_id, user_nkey, allow_pub, allow_sub}
//!   nats.creds           JWT + seed bundle (0600), regenerated on refresh
//!   nats.creds.expiry    unix-seconds expiry of the JWT in nats.creds
//! ```
//!
//! The `nats.creds.expiry` sidecar exists so a host-side refresh task
//! can decide "is this plugin's JWT close to expiry?" without parsing
//! the JWT body itself — the file is one ASCII integer, atomically
//! rewritten alongside `nats.creds`.

use std::fs;
use std::path::Path;

use crate::jwt::{self, format_creds_file, JwtError, UserAcl};

/// User nkey seed file inside `<plugin_dir>/<id>/`.
pub const USER_NKEY_FILE: &str = "nats.nkey";
/// ACL snapshot written by `iotctl plugin install` from the manifest's
/// `capabilities.bus` block.
pub const ACL_FILE: &str = "acl.json";
/// JWT + seed bundle the plugin runtime hands to `async-nats` via
/// `ConnectOptions::credentials_file`.
pub const CREDS_FILE: &str = "nats.creds";
/// Unix-seconds expiry of the JWT in `CREDS_FILE`. Sidecar so the
/// host's refresh task can poll without parsing the bundle.
pub const CREDS_EXPIRY_FILE: &str = "nats.creds.expiry";

/// Errors from the creds-mint flow.
#[derive(Debug, thiserror::Error)]
pub enum CredsError {
    #[error("io {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse {path}: {message}")]
    Parse {
        path: std::path::PathBuf,
        message: String,
    },
    #[error("nkeys: {0}")]
    Nkeys(String),
    #[error("jwt: {0}")]
    Jwt(#[from] JwtError),
}

impl CredsError {
    fn io(path: impl Into<std::path::PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
    fn parse(path: impl Into<std::path::PathBuf>, message: impl Into<String>) -> Self {
        Self::Parse {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl From<nkeys::error::Error> for CredsError {
    fn from(e: nkeys::error::Error) -> Self {
        Self::Nkeys(e.to_string())
    }
}

/// Output of [`mint_user_creds`] — JWT body, the ready-to-write
/// creds-file blob, the unix-seconds expiry, and the random `jti` so
/// callers can log it for revocation-list audit trails.
#[derive(Debug, Clone)]
pub struct MintedCreds {
    /// The signed JWT (3 dot-separated b64url segments).
    pub jwt: String,
    /// Full `nats.creds` file contents (PEM-ish JWT + seed blocks).
    pub creds_blob: String,
    /// `iat` from the claims — the second the mint happened.
    pub iat: u64,
    /// `exp` from the claims — `iat + validity_seconds`.
    pub exp: u64,
    /// Random `jti` claim. Surfaced for revocation-list keying.
    pub jti: String,
}

/// Mint a user JWT + assemble the `nats.creds` blob.
///
/// `validity_seconds` is the lifetime of the resulting token; `iat`
/// is unix-seconds taken from the caller (so tests can pin clocks).
/// The `jti` claim is generated as a fresh ULID per call — sortable,
/// random enough to key a future revocation list, and short enough
/// to keep the JWT compact.
///
/// # Errors
/// Propagates `JwtError` from the minter and `nkeys` errors from
/// seed-encoding the user.
pub fn mint_user_creds(
    account: &nkeys::KeyPair,
    user: &nkeys::KeyPair,
    name: &str,
    acl: &UserAcl,
    iat: u64,
    validity_seconds: u64,
) -> Result<MintedCreds, CredsError> {
    let exp = iat.saturating_add(validity_seconds);
    let jti = ulid::Ulid::new().to_string();
    let jwt = jwt::issue_user_jwt(account, &user.public_key(), name, acl, iat, exp, &jti)?;
    let seed = user.seed()?;
    let creds_blob = format_creds_file(&jwt, &seed);
    Ok(MintedCreds {
        jwt,
        creds_blob,
        iat,
        exp,
        jti,
    })
}

/// Read + parse the ACL snapshot at `path`.
///
/// Returns the `{allow_pub, allow_sub}` pair the JWT minter consumes;
/// ignores other fields (`plugin_id`, `user_nkey`) that `iotctl plugin
/// install` writes for human / debug consumption.
///
/// # Errors
/// IO failures on `path`, or JSON parse errors.
pub fn parse_acl_file(path: &Path) -> Result<UserAcl, CredsError> {
    let raw = fs::read_to_string(path).map_err(|e| CredsError::io(path, e))?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| CredsError::parse(path, e.to_string()))?;
    let collect_subjects = |key: &str| {
        v.get(key)
            .and_then(|a| a.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|s| s.as_str().map(ToOwned::to_owned))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    Ok(UserAcl {
        allow_pub: collect_subjects("allow_pub"),
        allow_sub: collect_subjects("allow_sub"),
    })
}

/// Read the unix-seconds expiry sidecar from `<plugin_dir>/<id>/`.
///
/// `Ok(None)` covers two cases the refresh task treats identically:
/// the file isn't there yet (legacy install before this commit), and
/// the file is empty / non-numeric (treated as "no known expiry —
/// don't refresh blindly, let the operator regenerate"). Caller
/// decides whether either is an error in their context.
///
/// # Errors
/// Filesystem errors other than `NotFound` propagate.
pub fn read_expiry(plugin_install_dir: &Path) -> std::io::Result<Option<u64>> {
    let path = plugin_install_dir.join(CREDS_EXPIRY_FILE);
    match fs::read_to_string(&path) {
        Ok(s) => Ok(s.trim().parse::<u64>().ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Write the expiry sidecar atomically-ish (`fs::write` is one syscall
/// on POSIX, kernel-level atomic for files under PIPE_BUF). Sets 0600
/// on Unix; no-op on Windows (audit M5 covers that gap).
///
/// # Errors
/// Filesystem errors writing the sidecar or chmod'ing it.
pub fn write_expiry(plugin_install_dir: &Path, expiry_unix_seconds: u64) -> std::io::Result<()> {
    let path = plugin_install_dir.join(CREDS_EXPIRY_FILE);
    fs::write(&path, expiry_unix_seconds.to_string())?;
    restrict_permissions(&path)?;
    Ok(())
}

/// Write the creds blob + the matching expiry sidecar. Both files
/// land 0600 on Unix.
///
/// # Errors
/// Filesystem errors on either file or its chmod.
pub fn write_creds(plugin_install_dir: &Path, minted: &MintedCreds) -> std::io::Result<()> {
    let creds_path = plugin_install_dir.join(CREDS_FILE);
    fs::write(&creds_path, &minted.creds_blob)?;
    restrict_permissions(&creds_path)?;
    write_expiry(plugin_install_dir, minted.exp)?;
    Ok(())
}

/// End-to-end: read the per-plugin user nkey + ACL from the install
/// dir, mint a fresh user JWT against `account`, return the
/// [`MintedCreds`]. Caller writes the result with [`write_creds`].
///
/// Splitting "mint" from "write" lets the host's refresh task log
/// the new expiry before clobbering the on-disk file, and lets tests
/// poke at the minted blob without touching the filesystem.
///
/// # Errors
/// IO + parse errors on the input files; nkey / JWT errors from the
/// crypto path.
pub fn mint_creds_for_install_dir(
    account: &nkeys::KeyPair,
    plugin_install_dir: &Path,
    name: &str,
    iat: u64,
    validity_seconds: u64,
) -> Result<MintedCreds, CredsError> {
    let user_seed_path = plugin_install_dir.join(USER_NKEY_FILE);
    let user_seed =
        fs::read_to_string(&user_seed_path).map_err(|e| CredsError::io(&user_seed_path, e))?;
    let user = nkeys::KeyPair::from_seed(user_seed.trim())
        .map_err(|e| CredsError::Nkeys(format!("parse user seed: {e}")))?;

    let acl_path = plugin_install_dir.join(ACL_FILE);
    let acl = parse_acl_file(&acl_path)?;

    mint_user_creds(account, &user, name, &acl, iat, validity_seconds)
}

/// Pure refresh-decision helper. Returns `true` when the JWT in the
/// `nats.creds` file is within `threshold_seconds` of expiry (i.e.
/// `expiry - now <= threshold_seconds`), or already past it.
///
/// `expiry == 0` means "no expiry recorded" — refusing to refresh in
/// that case is the safer default; the operator regenerates via
/// `iotctl plugin install --force` instead of letting the host
/// silently rotate creds against a sidecar it can't trust.
#[must_use]
pub fn needs_refresh(now: u64, expiry: u64, threshold_seconds: u64) -> bool {
    if expiry == 0 {
        return false;
    }
    let remaining = expiry.saturating_sub(now);
    remaining <= threshold_seconds
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
fn restrict_permissions(_path: &Path) -> std::io::Result<()> {
    // Windows ACLs are a different beast — see audit M5 (queued
    // separately). Kept Result<()> so Unix + Windows call sites don't
    // diverge.
    Ok(())
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
    fn mint_user_creds_populates_exp_and_jti() {
        let account = nkeys::KeyPair::new_account();
        let user = nkeys::KeyPair::new_user();
        let minted = mint_user_creds(
            &account,
            &user,
            "demo-echo",
            &fixed_acl(),
            1_700_000_000,
            3600,
        )
        .expect("mint");
        assert_eq!(minted.iat, 1_700_000_000);
        assert_eq!(minted.exp, 1_700_003_600);
        // ULID = 26 chars Crockford base32.
        assert_eq!(minted.jti.len(), 26);

        // The blob carries both PEM-ish blocks…
        assert!(minted.creds_blob.contains("-----BEGIN NATS USER JWT-----"));
        assert!(minted.creds_blob.contains("-----BEGIN USER NKEY SEED-----"));

        // …and the JWT verifies under the issuing account at IAT and
        // pre-exp, but not at-or-past exp.
        let claims = jwt::verify_user_jwt(&account, &minted.jwt, minted.iat + 1).expect("verify");
        assert_eq!(claims.exp, minted.exp);
        assert_eq!(claims.jti, minted.jti);
        assert!(matches!(
            jwt::verify_user_jwt(&account, &minted.jwt, minted.exp),
            Err(JwtError::Expired { .. })
        ));
    }

    #[test]
    fn mint_user_creds_jti_is_unique_per_call() {
        let account = nkeys::KeyPair::new_account();
        let user = nkeys::KeyPair::new_user();
        let a = mint_user_creds(&account, &user, "n", &fixed_acl(), 1, 60).unwrap();
        let b = mint_user_creds(&account, &user, "n", &fixed_acl(), 1, 60).unwrap();
        assert_ne!(a.jti, b.jti, "ULID collision across two consecutive mints");
    }

    #[test]
    fn parse_acl_file_reads_expected_format() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("acl.json");
        fs::write(
            &path,
            r#"{
                "plugin_id": "demo-echo",
                "user_nkey": "UAAAA",
                "allow_pub": ["device.demo-echo.>"],
                "allow_sub": ["cmd.demo-echo.>"]
            }"#,
        )
        .unwrap();
        let acl = parse_acl_file(&path).expect("parse");
        assert_eq!(acl.allow_pub, vec!["device.demo-echo.>"]);
        assert_eq!(acl.allow_sub, vec!["cmd.demo-echo.>"]);
    }

    #[test]
    fn expiry_sidecar_roundtrips() {
        let td = tempfile::tempdir().unwrap();
        // Missing sidecar → Ok(None).
        assert_eq!(read_expiry(td.path()).unwrap(), None);

        write_expiry(td.path(), 1_700_086_400).unwrap();
        assert_eq!(read_expiry(td.path()).unwrap(), Some(1_700_086_400));

        // Garbage in the file is read as None (don't crash the
        // refresh task on an operator's vim slip).
        fs::write(td.path().join(CREDS_EXPIRY_FILE), b"not-a-number").unwrap();
        assert_eq!(read_expiry(td.path()).unwrap(), None);
    }

    #[test]
    fn mint_creds_for_install_dir_e2e() {
        // Simulate the `iotctl plugin install` install-dir state: a
        // user nkey + an acl.json. Then mint, write, read back —
        // proves the refresh task's main code path is wired end to
        // end.
        let td = tempfile::tempdir().unwrap();
        let account = nkeys::KeyPair::new_account();
        let user = nkeys::KeyPair::new_user();
        fs::write(td.path().join(USER_NKEY_FILE), user.seed().unwrap()).unwrap();
        fs::write(
            td.path().join(ACL_FILE),
            r#"{ "plugin_id": "demo-echo",
                 "user_nkey": "UAAAA",
                 "allow_pub": ["device.demo-echo.>"],
                 "allow_sub": ["cmd.demo-echo.>"] }"#,
        )
        .unwrap();

        let minted =
            mint_creds_for_install_dir(&account, td.path(), "demo-echo", 1_700_000_000, 86400)
                .expect("mint");
        assert_eq!(minted.exp, 1_700_086_400);

        write_creds(td.path(), &minted).unwrap();
        assert!(td.path().join(CREDS_FILE).is_file());
        assert_eq!(read_expiry(td.path()).unwrap(), Some(1_700_086_400));

        let claims = jwt::verify_user_jwt(&account, &minted.jwt, minted.iat + 1).unwrap();
        assert_eq!(claims.sub, user.public_key());
    }

    #[test]
    fn needs_refresh_decision_table() {
        // 1h threshold, exp = 1000.
        // Far from expiry — no refresh.
        assert!(!needs_refresh(0, 1000, 600));
        // Inside the threshold — refresh.
        assert!(needs_refresh(500, 1000, 600));
        // Exactly at the threshold edge — refresh (>= boundary).
        assert!(needs_refresh(400, 1000, 600));
        // Already expired — refresh (the supervisor will re-mint and
        // retry; better than letting the plugin stay broken).
        assert!(needs_refresh(2000, 1000, 600));
        // No expiry recorded — never refresh blindly.
        assert!(!needs_refresh(0, 0, 600));
    }
}
