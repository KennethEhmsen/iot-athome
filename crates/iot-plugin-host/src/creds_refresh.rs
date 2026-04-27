//! Per-plugin NATS credentials refresh task (Bucket 1 audit H1).
//!
//! Pre-fix, `iot_bus::jwt::issue_user_jwt` emitted user JWTs without an
//! `exp` claim — a leaked `nats.creds` was usable for the lifetime of
//! the issuing account keypair (months / years). The minter now always
//! sets `exp = iat + validity_seconds` (default 24 h). This module is
//! the host-side counterpart: a periodic poller that scans every
//! plugin install dir's `nats.creds.expiry` sidecar and re-mints when
//! a token is approaching its expiry window.
//!
//! Design notes:
//!
//! * **Refresh keeps the same nkey + ACL.** Only the JWT body rotates;
//!   the per-plugin user nkey + the broker-side ACL stay stable. That
//!   keeps the operator's mental model (one identity per plugin) and
//!   means the broker's account-level limits don't churn.
//! * **No signal mechanism, by design.** The plugin runtime doesn't
//!   yet have a "reload creds" command (a separate ABI question that
//!   would need a new export). Instead we rewrite `nats.creds` in
//!   place; on the plugin's next reconnect — either organic, after
//!   the broker drops the old session for an expired JWT, or via the
//!   supervisor's restart loop on a crash — `async-nats` reads the
//!   fresh file. The 1 h-before-expiry default leaves room for the
//!   broker to evict the old session and the runtime to reconnect
//!   well before plugins notice user-visible disruption.
//! * **Single-host model only.** Cluster-wide synchronised expiry
//!   windows are out of scope (there's no cluster). When we add one,
//!   the refresh task becomes a leader-elected job; until then,
//!   running it on every host is a no-op for hosts with no plugins.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context as _, Result};
use iot_bus::creds;

/// Default re-mint poll interval — 60 s.
///
/// Tight enough to react inside the 1 h refresh window even if the
/// host clock skews by minutes, loose enough to be free in CPU + I/O
/// terms (one `read_dir` + one 8-byte file read per plugin per poll).
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(60);
/// Default refresh threshold — 1 h before expiry.
///
/// With the default 24 h validity, that gives the supervisor 23 h to
/// react to a failed re-mint (e.g. account seed got moved) before
/// plugins lose connectivity.
pub const DEFAULT_REFRESH_THRESHOLD_SECONDS: u64 = 3_600;

/// What the refresh task did for one plugin dir on one tick. Useful
/// for tests + log aggregation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// Sidecar absent or unparseable. Refusing to mint blindly: the
    /// operator regenerates via `iotctl plugin install --force`.
    NoExpiryRecorded,
    /// JWT is fresh enough — no action needed.
    Fresh { remaining_seconds: u64 },
    /// JWT is inside the refresh window; new creds were minted +
    /// written. `new_expiry` is the unix-seconds expiry of the
    /// freshly-minted JWT (== old_expiry + delta).
    Refreshed { old_expiry: u64, new_expiry: u64 },
}

/// Configuration for the refresh poller. Constructed from the host's
/// `Config` at startup; passed by reference into [`refresh_install_dir`]
/// + the [`run_refresh_loop`] supervisor.
#[derive(Debug, Clone)]
pub struct RefreshConfig {
    /// Path to the operator-bootstrap account seed file
    /// (`iot-account.nk`). Loaded once per refresh tick — re-reading
    /// is cheap and lets operators rotate the trust root via
    /// `iotctl nats bootstrap --force` without restarting the host.
    pub account_seed_path: PathBuf,
    /// Validity window of the freshly-minted JWT (seconds). Mirrors
    /// the install-time `--validity-seconds` flag's default.
    pub validity_seconds: u64,
    /// Refresh when `expiry - now <= refresh_threshold_seconds`.
    /// Default 1 h (`DEFAULT_REFRESH_THRESHOLD_SECONDS`).
    pub refresh_threshold_seconds: u64,
    /// Sleep interval between scans (default 60 s). Set lower in tests.
    pub poll_interval: Duration,
}

impl Default for RefreshConfig {
    fn default() -> Self {
        Self {
            account_seed_path: PathBuf::new(),
            validity_seconds: 86_400,
            refresh_threshold_seconds: DEFAULT_REFRESH_THRESHOLD_SECONDS,
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }
}

/// Refresh-decision + (if needed) re-mint for one plugin install dir.
///
/// Pure-ish — touches the filesystem but takes `now` as a parameter
/// so tests can pin the wall-clock and exercise both branches without
/// a `std::thread::sleep`.
///
/// `account` is the loaded account keypair the supervisor already
/// derived from `cfg.account_seed_path`; passing it in (vs. re-loading
/// per call) avoids one `read_to_string` per plugin per tick when
/// scanning many plugins.
///
/// # Errors
/// IO failures reading the per-plugin nkey or ACL, or writing the new
/// creds files. JWT mint errors. The caller (refresh loop) logs +
/// continues so one broken plugin doesn't poison the whole tick.
pub fn refresh_install_dir(
    account: &nkeys::KeyPair,
    plugin_install_dir: &Path,
    plugin_id: &str,
    cfg: &RefreshConfig,
    now: u64,
) -> Result<RefreshOutcome> {
    let Some(expiry) = creds::read_expiry(plugin_install_dir)
        .with_context(|| format!("read expiry sidecar in {}", plugin_install_dir.display()))?
    else {
        return Ok(RefreshOutcome::NoExpiryRecorded);
    };

    if !creds::needs_refresh(now, expiry, cfg.refresh_threshold_seconds) {
        return Ok(RefreshOutcome::Fresh {
            remaining_seconds: expiry.saturating_sub(now),
        });
    }

    // Inside the refresh window — re-mint a fresh JWT against the
    // existing user nkey + ACL. write_creds rewrites both
    // `nats.creds` and `nats.creds.expiry` atomically (one syscall
    // each). The plugin's next reconnect picks up the new bundle.
    let minted = creds::mint_creds_for_install_dir(
        account,
        plugin_install_dir,
        plugin_id,
        now,
        cfg.validity_seconds,
    )
    .with_context(|| format!("re-mint nats.creds for {plugin_id}"))?;
    creds::write_creds(plugin_install_dir, &minted)
        .with_context(|| format!("write refreshed nats.creds for {plugin_id}"))?;

    Ok(RefreshOutcome::Refreshed {
        old_expiry: expiry,
        new_expiry: minted.exp,
    })
}

/// Walk every subdirectory under `plugin_dir_root` and refresh.
///
/// One per installed plugin; runs [`refresh_install_dir`] on each.
/// Logs each outcome. Errors on individual plugins are logged +
/// swallowed so one bad install doesn't block refresh on the rest.
///
/// `account` is loaded once at the top of each scan — see the comment
/// on [`RefreshConfig::account_seed_path`].
///
/// # Errors
/// Top-level scan failures (account seed read / parse, `read_dir` on
/// the plugin root). Per-plugin errors stay inside the loop.
pub fn refresh_all(plugin_dir_root: &Path, cfg: &RefreshConfig, now: u64) -> Result<()> {
    let account_seed = std::fs::read_to_string(&cfg.account_seed_path).with_context(|| {
        format!(
            "read account seed {} (creds refresh)",
            cfg.account_seed_path.display()
        )
    })?;
    let account = nkeys::KeyPair::from_seed(account_seed.trim())
        .map_err(|e| anyhow!("parse account seed: {e}"))?;

    let entries = match std::fs::read_dir(plugin_dir_root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("read_dir {} (creds refresh)", plugin_dir_root.display()))
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "creds refresh: skipping unreadable dir entry");
                continue;
            }
        };
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let id = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("<non-utf8-id>")
            .to_owned();
        match refresh_install_dir(&account, &path, &id, cfg, now) {
            Ok(RefreshOutcome::Refreshed {
                old_expiry,
                new_expiry,
            }) => {
                tracing::info!(
                    plugin = %id,
                    old_expiry,
                    new_expiry,
                    "creds refresh: re-minted nats.creds"
                );
            }
            Ok(RefreshOutcome::Fresh { remaining_seconds }) => {
                tracing::debug!(
                    plugin = %id,
                    remaining_seconds,
                    "creds refresh: still fresh"
                );
            }
            Ok(RefreshOutcome::NoExpiryRecorded) => {
                tracing::debug!(
                    plugin = %id,
                    "creds refresh: no expiry sidecar (legacy install — no-op)"
                );
            }
            Err(e) => {
                // Per-plugin errors don't poison the whole tick — log
                // loud and let the next tick retry.
                tracing::error!(
                    plugin = %id,
                    error = %format!("{e:#}"),
                    "creds refresh: re-mint failed"
                );
            }
        }
    }
    Ok(())
}

/// Long-running supervisor for the refresh loop.
///
/// Sleeps `cfg.poll_interval`, then runs [`refresh_all`]. Designed to
/// be `tokio::spawn`-ed at host startup when an account seed is
/// configured. Returns only on `Err` (the outer host loop logs + bails).
///
/// # Errors
/// Top-level errors (filesystem unreadable, account seed parse) bubble
/// up. Per-plugin errors are logged + swallowed inside [`refresh_all`].
pub async fn run_refresh_loop(plugin_dir_root: PathBuf, cfg: RefreshConfig) -> Result<()> {
    tracing::info!(
        plugin_dir = %plugin_dir_root.display(),
        validity_seconds = cfg.validity_seconds,
        threshold_seconds = cfg.refresh_threshold_seconds,
        poll_interval_secs = cfg.poll_interval.as_secs(),
        "creds-refresh loop starting"
    );
    loop {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        if let Err(e) = refresh_all(&plugin_dir_root, &cfg, now) {
            // Top-level scan failure (e.g. account seed went missing).
            // Log + retry on next tick — restarting the host won't
            // help if the seed is gone.
            tracing::error!(
                error = %format!("{e:#}"),
                "creds refresh: scan failed (will retry)"
            );
        }
        tokio::time::sleep(cfg.poll_interval).await;
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use std::fs;

    /// Lay down the M5a-shaped install-dir state: a user nkey seed +
    /// an `acl.json` + a current `nats.creds` minted with `expiry`.
    fn seed_install_dir(
        dir: &Path,
        account: &nkeys::KeyPair,
        iat: u64,
        validity: u64,
    ) -> nkeys::KeyPair {
        let user = nkeys::KeyPair::new_user();
        fs::write(
            dir.join(creds::USER_NKEY_FILE),
            user.seed().expect("user seed"),
        )
        .unwrap();
        fs::write(
            dir.join(creds::ACL_FILE),
            r#"{ "plugin_id": "demo-echo",
                 "user_nkey": "UAAAA",
                 "allow_pub": ["device.demo-echo.>"],
                 "allow_sub": ["cmd.demo-echo.>"] }"#,
        )
        .unwrap();
        let minted = creds::mint_user_creds(
            account,
            &user,
            "demo-echo",
            &iot_bus::jwt::UserAcl {
                allow_pub: vec!["device.demo-echo.>".into()],
                allow_sub: vec!["cmd.demo-echo.>".into()],
            },
            iat,
            validity,
        )
        .expect("seed mint");
        creds::write_creds(dir, &minted).expect("seed write");
        user
    }

    #[test]
    fn missing_expiry_sidecar_yields_no_action() {
        let td = tempfile::tempdir().unwrap();
        let account = nkeys::KeyPair::new_account();
        let cfg = RefreshConfig::default();
        let outcome = refresh_install_dir(&account, td.path(), "demo-echo", &cfg, 0).unwrap();
        assert_eq!(outcome, RefreshOutcome::NoExpiryRecorded);
    }

    #[test]
    fn fresh_jwt_is_left_alone() {
        let td = tempfile::tempdir().unwrap();
        let account = nkeys::KeyPair::new_account();
        // Seed: minted at iat=1000 with 24h validity → exp = 1000 + 86400.
        seed_install_dir(td.path(), &account, 1_000, 86_400);

        // Threshold 1h. Right after mint, plenty of time left.
        let cfg = RefreshConfig {
            account_seed_path: PathBuf::from("unused-in-this-call"),
            validity_seconds: 86_400,
            refresh_threshold_seconds: 3_600,
            poll_interval: Duration::from_secs(60),
        };
        let outcome = refresh_install_dir(&account, td.path(), "demo-echo", &cfg, 1_001).unwrap();
        match outcome {
            RefreshOutcome::Fresh { remaining_seconds } => {
                assert_eq!(remaining_seconds, 86_400 - 1);
            }
            other => panic!("expected Fresh, got {other:?}"),
        }
    }

    #[test]
    fn expiry_approaching_triggers_remint() {
        // Core test for the audit memo's "expiry approaching" branch.
        let td = tempfile::tempdir().unwrap();
        let account = nkeys::KeyPair::new_account();
        let user = seed_install_dir(td.path(), &account, 1_000, 86_400);
        // Original creds carry exp = 87_400.
        assert_eq!(creds::read_expiry(td.path()).unwrap(), Some(87_400));
        let original_jwt = fs::read_to_string(td.path().join(creds::CREDS_FILE)).unwrap();

        // Now jump forward to 30 min before the original expiry — well
        // inside a 1h refresh window. Refresh should re-mint with a
        // fresh 24h validity.
        let now = 87_400 - 30 * 60; // 1800s before exp
        let cfg = RefreshConfig {
            account_seed_path: PathBuf::from("unused-in-this-call"),
            validity_seconds: 86_400,
            refresh_threshold_seconds: 3_600,
            poll_interval: Duration::from_secs(60),
        };
        let outcome = refresh_install_dir(&account, td.path(), "demo-echo", &cfg, now).unwrap();
        let new_expiry = match outcome {
            RefreshOutcome::Refreshed {
                old_expiry,
                new_expiry,
            } => {
                assert_eq!(old_expiry, 87_400);
                assert_eq!(new_expiry, now + 86_400);
                new_expiry
            }
            other => panic!("expected Refreshed, got {other:?}"),
        };

        // Sidecar got rewritten; on-disk creds blob changed; the new
        // JWT verifies under the same account at `now+1` AND its
        // subject is still our original user nkey (refresh keeps the
        // identity stable).
        assert_eq!(creds::read_expiry(td.path()).unwrap(), Some(new_expiry));
        let new_creds_blob = fs::read_to_string(td.path().join(creds::CREDS_FILE)).unwrap();
        assert_ne!(
            new_creds_blob, original_jwt,
            "creds blob must change on refresh"
        );
        let jwt_start = new_creds_blob
            .find("-----BEGIN NATS USER JWT-----\n")
            .unwrap()
            + "-----BEGIN NATS USER JWT-----\n".len();
        let jwt_end = new_creds_blob
            .find("\n------END NATS USER JWT------")
            .unwrap();
        let jwt = &new_creds_blob[jwt_start..jwt_end];
        let claims = iot_bus::jwt::verify_user_jwt(&account, jwt, now + 1).expect("verify");
        assert_eq!(claims.exp, new_expiry);
        assert_eq!(claims.sub, user.public_key(), "refresh preserves identity");
    }

    #[test]
    fn already_expired_jwt_still_remints() {
        // Defence against operator clocks skewing past expiry: the
        // refresh task re-mints anyway so the next plugin reconnect
        // works. The plugin will have been kicked off the broker, but
        // the supervisor's restart loop reconnects with the fresh
        // creds.
        let td = tempfile::tempdir().unwrap();
        let account = nkeys::KeyPair::new_account();
        // 60s validity → exp 1060. Now is 5 minutes past that.
        seed_install_dir(td.path(), &account, 1_000, 60);
        let now = 1_060 + 300;
        let cfg = RefreshConfig {
            account_seed_path: PathBuf::from("unused-in-this-call"),
            validity_seconds: 86_400,
            refresh_threshold_seconds: 3_600,
            poll_interval: Duration::from_secs(60),
        };
        let outcome = refresh_install_dir(&account, td.path(), "demo-echo", &cfg, now).unwrap();
        assert!(matches!(outcome, RefreshOutcome::Refreshed { .. }));
    }

    #[test]
    fn refresh_all_loads_account_seed_and_walks_subdirs() {
        // Scaffold a plugin dir root with two installs. Seed one
        // close to expiry, leave the other fresh. After refresh_all,
        // only the close-to-expiry one's creds blob should change.
        let root = tempfile::tempdir().unwrap();
        let stale_dir = root.path().join("stale-plugin");
        let fresh_dir = root.path().join("fresh-plugin");
        fs::create_dir_all(&stale_dir).unwrap();
        fs::create_dir_all(&fresh_dir).unwrap();

        let account = nkeys::KeyPair::new_account();
        let account_seed_path = root.path().join("iot-account.nk");
        fs::write(&account_seed_path, account.seed().unwrap()).unwrap();

        seed_install_dir(&stale_dir, &account, 1_000, 60); // exp 1060
        seed_install_dir(&fresh_dir, &account, 1_000, 86_400); // exp 87_400

        let stale_before = fs::read_to_string(stale_dir.join(creds::CREDS_FILE)).unwrap();
        let fresh_before = fs::read_to_string(fresh_dir.join(creds::CREDS_FILE)).unwrap();

        let cfg = RefreshConfig {
            account_seed_path,
            validity_seconds: 86_400,
            refresh_threshold_seconds: 3_600,
            poll_interval: Duration::from_secs(60),
        };
        // Now = 1500. Stale plugin's exp 1060 is in the past → refresh.
        // Fresh plugin's exp 87_400 has 85_900s left → no refresh.
        refresh_all(root.path(), &cfg, 1_500).unwrap();

        let stale_after = fs::read_to_string(stale_dir.join(creds::CREDS_FILE)).unwrap();
        let fresh_after = fs::read_to_string(fresh_dir.join(creds::CREDS_FILE)).unwrap();
        assert_ne!(stale_before, stale_after, "stale-plugin should re-mint");
        assert_eq!(
            fresh_before, fresh_after,
            "fresh-plugin should be left alone"
        );
    }
}
