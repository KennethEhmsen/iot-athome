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

use iot_bus::jwt::{format_creds_file, issue_account_jwt, issue_user_jwt, AccountLimits, UserAcl};

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
        #[arg(long)]
        out: Option<PathBuf>,
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
        } => cmd_mint_user(account_seed, user_seed, acl, name, out.as_deref()),
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
    crate::secrets::restrict_permissions(&operator_seed_path)?;
    fs::write(&operator_pub_path, operator.public_key())
        .with_context(|| format!("write {}", operator_pub_path.display()))?;

    let account = nkeys::KeyPair::new_account();
    let account_seed = account
        .seed()
        .map_err(|e| anyhow!("encode account seed: {e}"))?;
    fs::write(&account_seed_path, &account_seed)
        .with_context(|| format!("write {}", account_seed_path.display()))?;
    crate::secrets::restrict_permissions(&account_seed_path)?;
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
) -> Result<()> {
    let account_seed = fs::read_to_string(account_seed_path)
        .with_context(|| format!("read {}", account_seed_path.display()))?;
    let account = nkeys::KeyPair::from_seed(account_seed.trim())
        .map_err(|e| anyhow!("parse account seed: {e}"))?;

    let user_seed = fs::read_to_string(user_seed_path)
        .with_context(|| format!("read {}", user_seed_path.display()))?;
    let user =
        nkeys::KeyPair::from_seed(user_seed.trim()).map_err(|e| anyhow!("parse user seed: {e}"))?;

    let acl = parse_acl(acl_path)?;
    let creds = issue_and_format_creds(&account, &user, name, &acl)?;

    let default_out = user_seed_path
        .parent()
        .map(|p| p.join("nats.creds"))
        .ok_or_else(|| anyhow!("user seed has no parent dir"))?;
    let out_path = out.map_or(default_out, Path::to_path_buf);

    fs::write(&out_path, &creds).with_context(|| format!("write {}", out_path.display()))?;
    crate::secrets::restrict_permissions(&out_path)?;

    println!(
        "minted user JWT for {} → {}\n  user nkey: {}\n  account:   {}",
        name,
        out_path.display(),
        user.public_key(),
        account.public_key(),
    );
    Ok(())
}

/// Library-level helper — called both from the CLI path and from
/// `iotctl plugin install`'s post-install hook (no fork/exec).
///
/// Reads the `acl.json` an earlier pass of `generate_nats_identity`
/// wrote, then mints a user JWT + formats a creds-file blob.
///
/// # Errors
/// IO on the ACL path, JSON parse, or JWT minting failure.
pub fn issue_and_format_creds(
    account: &nkeys::KeyPair,
    user: &nkeys::KeyPair,
    name: &str,
    acl: &UserAcl,
) -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let jwt = issue_user_jwt(account, &user.public_key(), name, acl, now)
        .map_err(|e| anyhow!("mint user JWT: {e}"))?;
    let seed = user.seed().map_err(|e| anyhow!("encode user seed: {e}"))?;
    Ok(format_creds_file(&jwt, &seed))
}

/// Parse the `acl.json` snapshot `iotctl plugin install` wrote.
///
/// Exported for re-use from the install path — the fields are
/// `allow_pub` + `allow_sub`, matching the [`UserAcl`] layout.
///
/// # Errors
/// IO on the ACL path or JSON parse.
pub fn parse_acl(path: &Path) -> Result<UserAcl> {
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).with_context(|| format!("parse JSON {}", path.display()))?;
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
    fn parse_acl_reads_existing_format() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("acl.json");
        fs::write(
            &path,
            r#"{
                "plugin_id": "demo-echo",
                "user_nkey": "UAAAA",
                "allow_pub": ["device.demo-echo.>", "sys.demo-echo.>"],
                "allow_sub": ["cmd.demo-echo.>"]
            }"#,
        )
        .unwrap();
        let acl = parse_acl(&path).expect("parse");
        assert_eq!(acl.allow_pub, vec!["device.demo-echo.>", "sys.demo-echo.>"]);
        assert_eq!(acl.allow_sub, vec!["cmd.demo-echo.>"]);
    }

    #[test]
    fn mint_user_roundtrip() {
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
        )
        .expect("mint");

        let creds = fs::read_to_string(&out).unwrap();
        assert!(creds.contains("-----BEGIN NATS USER JWT-----"));
        assert!(creds.contains("-----BEGIN USER NKEY SEED-----"));

        // Extract the JWT from the creds blob and verify under the
        // account — proves the whole mint chain works.
        let jwt_start = creds.find("-----BEGIN NATS USER JWT-----\n").unwrap()
            + "-----BEGIN NATS USER JWT-----\n".len();
        let jwt_end = creds.find("\n------END NATS USER JWT------").unwrap();
        let jwt = &creds[jwt_start..jwt_end];
        let account_seed = fs::read_to_string(td.path().join("iot-account.nk")).unwrap();
        let account = nkeys::KeyPair::from_seed(account_seed.trim()).unwrap();
        let claims = iot_bus::jwt::verify_user_jwt(&account, jwt).expect("verify");
        assert_eq!(claims.name, "demo-echo");
        assert_eq!(claims.sub, user.public_key());
        assert_eq!(claims.nats.publish.allow, vec!["device.demo-echo.>"]);
    }
}
