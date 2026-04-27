//! `iotctl nats …` — NATS decentralized-auth bootstrap (M5a W1).
//!
//! Two operator-facing subcommands:
//!
//! * `bootstrap` — generate an operator keypair + account keypair, mint
//!   an operator-signed account JWT, and write:
//!     - `<out>/operator.nk`        — operator seed (0600)
//!     - `<out>/operator.pub`       — operator public key (ASCII)
//!     - `<out>/iot-account.nk`     — account seed (0600)
//!     - `<out>/iot-account.pub`    — account public key (ASCII)
//!     - `<out>/iot-account.jwt`    — operator-signed account JWT
//!     - `<out>/resolver.conf`      — `nats.conf` snippet to `include`
//!       containing the `operator:` line, `resolver: MEMORY`, and a
//!       `resolver_preload` map populated with the account.
//!
//!   The NATS server config reads `operator.pub` for the `operator:`
//!   directive and `iot-account.{pub,jwt}` for the `resolver_preload`
//!   map. `mint.sh` calls this before `just dev` starts the compose
//!   stack; `deploy/compose/nats/nats.conf` then `include`s the
//!   generated snippet.
//!
//! * `mint-user` — read an existing plugin's `nats.nkey` + `acl.json` pair
//!   plus the account seed, issue a user JWT, and write a NATS creds file.
//!   `iotctl plugin install`'s post-install hook calls the same library
//!   path directly (no fork/exec).
//!
//! Neither subcommand talks to the registry or NATS — they're pure
//! local filesystem + cryptographic work.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context as _, Result};
use clap::Subcommand;

use iot_bus::creds;
use iot_bus::jwt::{issue_account_jwt, AccountLimits};

/// Default validity for a freshly-minted user JWT — 24 h. Bucket 1
/// audit H1 closure: leaked `nats.creds` files now have a finite
/// blast-radius window. Operators override per-mint via
/// `--validity-seconds` (or the install hook's same-named flag).
pub const DEFAULT_VALIDITY_SECONDS: u64 = 86_400;

#[derive(Debug, Subcommand)]
pub enum NatsCmd {
    /// Generate operator + account keypairs and mint the account JWT.
    ///
    /// Writes everything `deploy/compose/nats/nats.conf`'s memory-resolver
    /// configuration needs to boot. Idempotent via `--force`: the default
    /// behaviour refuses to overwrite existing keys so operators don't
    /// accidentally destroy trust roots.
    Bootstrap {
        /// Output directory. Default: `tools/devcerts/generated/nats/`.
        #[arg(
            long,
            env = "IOT_NATS_BOOTSTRAP_DIR",
            default_value = "tools/devcerts/generated/nats"
        )]
        out: PathBuf,
        /// Overwrite existing keypairs + JWTs. Destructive — a new
        /// account keypair invalidates all previously-minted user creds.
        #[arg(long)]
        force: bool,
        /// Account display name. Defaults to `IOT`, matching the dev
        /// compose stack's single-account layout.
        #[arg(long, default_value = "IOT")]
        account_name: String,
    },
    /// Mint a NATS user JWT from an existing nkey + ACL pair, write
    /// the result as a `.creds` file.
    MintUser {
        /// Account seed file (produced by `bootstrap` as
        /// `iot-account.nk`).
        #[arg(long)]
        account_seed: PathBuf,
        /// User nkey seed file (`<plugin_dir>/<id>/nats.nkey`).
        #[arg(long)]
        user_seed: PathBuf,
        /// ACL JSON (`<plugin_dir>/<id>/acl.json`, produced by
        /// `iotctl plugin install`).
        #[arg(long)]
        acl: PathBuf,
        /// User / plugin display name — populated into the JWT `name`
        /// claim so NATS server logs identify the caller.
        #[arg(long)]
        name: String,
        /// Creds-file output path. Defaults to `<user_seed dir>/nats.creds`.
        /// The unix-seconds expiry sidecar is always written next to
        /// the creds file as `<creds-name>.expiry`.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Validity window of the resulting JWT, in seconds. Default
        /// 24 h (86400). The host's refresh task re-mints when within
        /// 1 h of expiry; operators with shorter rotation windows can
        /// shrink this for tighter blast radius.
        #[arg(long, default_value_t = DEFAULT_VALIDITY_SECONDS)]
        validity_seconds: u64,
    },
}

/// Dispatch `iotctl nats …`.
///
/// # Errors
/// Surfaces filesystem, nkey, and JWT-minting errors with enough
/// context for an operator to fix the underlying problem.
pub fn run(cmd: &NatsCmd) -> Result<()> {
    match cmd {
        NatsCmd::Bootstrap {
            out,
            force,
            account_name,
        } => cmd_bootstrap(out, *force, account_name),
        NatsCmd::MintUser {
            account_seed,
            user_seed,
            acl,
            name,
            out,
            validity_seconds,
        } => cmd_mint_user(
            account_seed,
            user_seed,
            acl,
            name,
            out.as_deref(),
            *validity_seconds,
        ),
    }
}

fn cmd_bootstrap(out: &Path, force: bool, account_name: &str) -> Result<()> {
    fs::create_dir_all(out).with_context(|| format!("create {}", out.display()))?;

    let operator_seed_path = out.join("operator.nk");
    let operator_pub_path = out.join("operator.pub");
    let account_seed_path = out.join("iot-account.nk");
    let account_pub_path = out.join("iot-account.pub");
    let account_jwt_path = out.join("iot-account.jwt");

    if !force {
        for p in [&operator_seed_path, &account_seed_path, &account_jwt_path] {
            if p.exists() {
                bail!(
                    "{} already exists — re-run with --force to overwrite (destructive: \
                     invalidates previously-minted user creds)",
                    p.display()
                );
            }
        }
    }

    let operator = nkeys::KeyPair::new_operator();
    let operator_seed = operator
        .seed()
        .map_err(|e| anyhow!("encode operator seed: {e}"))?;
    fs::write(&operator_seed_path, &operator_seed)
        .with_context(|| format!("write {}", operator_seed_path.display()))?;
    restrict_permissions(&operator_seed_path)?;
    fs::write(&operator_pub_path, operator.public_key())
        .with_context(|| format!("write {}", operator_pub_path.display()))?;

    let account = nkeys::KeyPair::new_account();
    let account_seed = account
        .seed()
        .map_err(|e| anyhow!("encode account seed: {e}"))?;
    fs::write(&account_seed_path, &account_seed)
        .with_context(|| format!("write {}", account_seed_path.display()))?;
    restrict_permissions(&account_seed_path)?;
    fs::write(&account_pub_path, account.public_key())
        .with_context(|| format!("write {}", account_pub_path.display()))?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let account_jwt = issue_account_jwt(
        &operator,
        &account.public_key(),
        account_name,
        AccountLimits::default(),
        now,
    )
    .map_err(|e| anyhow!("mint account JWT: {e}"))?;
    fs::write(&account_jwt_path, &account_jwt)
        .with_context(|| format!("write {}", account_jwt_path.display()))?;

    // resolver.conf — a snippet the broker `include`s. Contains the
    // operator pubkey, declares MEMORY-backed resolver, and preloads
    // the account JWT so the server validates user JWTs against it.
    let resolver_conf_path = out.join("resolver.conf");
    let resolver_snippet = format!(
        "# Generated by `iotctl nats bootstrap` — do not edit.\n\
         # Re-run with --force to regenerate (DESTRUCTIVE).\n\
         operator: {operator_pub}\n\
         resolver: MEMORY\n\
         resolver_preload: {{\n  \
           {account_pub}: \"{account_jwt}\"\n\
         }}\n",
        operator_pub = operator.public_key(),
        account_pub = account.public_key(),
        account_jwt = account_jwt,
    );
    fs::write(&resolver_conf_path, &resolver_snippet)
        .with_context(|| format!("write {}", resolver_conf_path.display()))?;

    println!(
        "bootstrapped NATS trust root at {}\n  \
         operator: {}\n  \
         account:  {}\n  \
         include:  {}",
        out.display(),
        operator.public_key(),
        account.public_key(),
        resolver_conf_path.display(),
    );
    Ok(())
}

fn cmd_mint_user(
    account_seed_path: &Path,
    user_seed_path: &Path,
    acl_path: &Path,
    name: &str,
    out: Option<&Path>,
    validity_seconds: u64,
) -> Result<()> {
    let account_seed = fs::read_to_string(account_seed_path)
        .with_context(|| format!("read {}", account_seed_path.display()))?;
    let account = nkeys::KeyPair::from_seed(account_seed.trim())
        .map_err(|e| anyhow!("parse account seed: {e}"))?;

    let user_seed = fs::read_to_string(user_seed_path)
        .with_context(|| format!("read {}", user_seed_path.display()))?;
    let user =
        nkeys::KeyPair::from_seed(user_seed.trim()).map_err(|e| anyhow!("parse user seed: {e}"))?;

    let acl = creds::parse_acl_file(acl_path)?;
    let now = unix_now();
    let minted = creds::mint_user_creds(&account, &user, name, &acl, now, validity_seconds)
        .map_err(|e| anyhow!("mint user creds: {e}"))?;

    let default_out = user_seed_path
        .parent()
        .map(|p| p.join(creds::CREDS_FILE))
        .ok_or_else(|| anyhow!("user seed has no parent dir"))?;
    let out_path = out.map_or(default_out, Path::to_path_buf);

    fs::write(&out_path, &minted.creds_blob)
        .with_context(|| format!("write {}", out_path.display()))?;
    restrict_permissions(&out_path)?;

    // Sidecar: the unix-seconds expiry, written next to the creds
    // file under `<basename>.expiry`. Lets the host's refresh task
    // poll without parsing the JWT body.
    let expiry_path = expiry_sidecar_path(&out_path);
    fs::write(&expiry_path, minted.exp.to_string())
        .with_context(|| format!("write {}", expiry_path.display()))?;
    restrict_permissions(&expiry_path)?;

    println!(
        "minted user JWT for {name} → {}\n  user nkey: {}\n  account:   {}\n  iat:       {}\n  exp:       {} ({}h validity)\n  jti:       {}",
        out_path.display(),
        user.public_key(),
        account.public_key(),
        minted.iat,
        minted.exp,
        validity_seconds / 3600,
        minted.jti,
    );
    Ok(())
}

/// `<creds_path>.expiry`. Mirrors the install-dir convention but
/// derived from whatever output path the operator passed to
/// `mint-user --out`.
fn expiry_sidecar_path(creds_path: &Path) -> PathBuf {
    let mut s = creds_path.as_os_str().to_owned();
    s.push(".expiry");
    PathBuf::from(s)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 {}", path.display()))
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
fn restrict_permissions(_path: &Path) -> Result<()> {
    // Windows ACLs are a different beast — dev boxes only. Kept
    // Result<()> so Unix + Windows call-sites don't diverge.
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn bootstrap_writes_expected_files() {
        let td = TempDir::new().unwrap();
        cmd_bootstrap(td.path(), false, "IOT").expect("bootstrap");
        for name in [
            "operator.nk",
            "operator.pub",
            "iot-account.nk",
            "iot-account.pub",
            "iot-account.jwt",
            "resolver.conf",
        ] {
            assert!(td.path().join(name).is_file(), "missing {name}");
        }

        // Account JWT verifies under the written operator key.
        let operator_seed = fs::read_to_string(td.path().join("operator.nk")).unwrap();
        let operator = nkeys::KeyPair::from_seed(operator_seed.trim()).unwrap();
        let jwt = fs::read_to_string(td.path().join("iot-account.jwt")).unwrap();
        let claims = iot_bus::jwt::verify_account_jwt(&operator, jwt.trim()).expect("verify");
        assert_eq!(claims.name, "IOT");
        assert_eq!(claims.iss, operator.public_key());

        // resolver.conf carries the operator + account + JWT inline.
        let resolver = fs::read_to_string(td.path().join("resolver.conf")).unwrap();
        assert!(
            resolver.contains(&format!("operator: {}", operator.public_key())),
            "missing operator line in resolver.conf"
        );
        assert!(
            resolver.contains("resolver: MEMORY"),
            "missing resolver: MEMORY line"
        );
        let account_pub = fs::read_to_string(td.path().join("iot-account.pub")).unwrap();
        assert!(
            resolver.contains(account_pub.trim()),
            "missing account pubkey"
        );
        assert!(resolver.contains(jwt.trim()), "missing account JWT inline");
    }

    #[test]
    fn bootstrap_refuses_overwrite_without_force() {
        let td = TempDir::new().unwrap();
        cmd_bootstrap(td.path(), false, "IOT").expect("first");
        let err = cmd_bootstrap(td.path(), false, "IOT").unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn mint_user_roundtrip_writes_creds_and_expiry() {
        let td = TempDir::new().unwrap();
        cmd_bootstrap(td.path(), false, "IOT").expect("bootstrap");

        // A plugin's existing install-time outputs.
        let user = nkeys::KeyPair::new_user();
        let user_seed_path = td.path().join("demo-echo.nk");
        fs::write(&user_seed_path, user.seed().unwrap()).unwrap();
        let acl_path = td.path().join("acl.json");
        fs::write(
            &acl_path,
            r#"{
                "plugin_id": "demo-echo",
                "user_nkey": "UAAAA",
                "allow_pub": ["device.demo-echo.>"],
                "allow_sub": ["cmd.demo-echo.>"]
            }"#,
        )
        .unwrap();

        let out = td.path().join("demo-echo.creds");
        cmd_mint_user(
            &td.path().join("iot-account.nk"),
            &user_seed_path,
            &acl_path,
            "demo-echo",
            Some(&out),
            3600, // 1h validity for this test
        )
        .expect("mint");

        let creds = fs::read_to_string(&out).unwrap();
        assert!(creds.contains("-----BEGIN NATS USER JWT-----"));
        assert!(creds.contains("-----BEGIN USER NKEY SEED-----"));

        // Sidecar got written alongside, with a sane integer.
        let expiry_text = fs::read_to_string(td.path().join("demo-echo.creds.expiry")).unwrap();
        let expiry: u64 = expiry_text.trim().parse().expect("expiry is integer");
        // Some time after epoch + the 1h validity window we passed.
        assert!(expiry > 3600, "expiry {expiry} must be after epoch");

        // Extract the JWT from the creds blob and verify under the
        // account — proves the whole mint chain works.
        let jwt_start = creds.find("-----BEGIN NATS USER JWT-----\n").unwrap()
            + "-----BEGIN NATS USER JWT-----\n".len();
        let jwt_end = creds.find("\n------END NATS USER JWT------").unwrap();
        let jwt = &creds[jwt_start..jwt_end];
        let account_seed = fs::read_to_string(td.path().join("iot-account.nk")).unwrap();
        let account = nkeys::KeyPair::from_seed(account_seed.trim()).unwrap();
        // `now == 0` skips the freshness check — we only care that
        // signature + claim shape are correct here.
        let claims = iot_bus::jwt::verify_user_jwt(&account, jwt, 0).expect("verify");
        assert_eq!(claims.name, "demo-echo");
        assert_eq!(claims.sub, user.public_key());
        assert_eq!(claims.nats.publish.allow, vec!["device.demo-echo.>"]);
        assert_eq!(claims.exp, expiry, "JWT exp matches sidecar expiry");
        assert_eq!(claims.jti.len(), 26, "ULID jti");
    }
}
