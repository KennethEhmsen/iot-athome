//! Cross-platform "lock this secret-bearing file down to the current
//! user only" helper. Used by `iotctl nats bootstrap` (operator +
//! account seeds, account JWT — collectively the trust root) and
//! `iotctl plugin install` (per-plugin user nkey seed + nats.creds).
//!
//! POSIX path: `chmod 0600`. Standard.
//!
//! Windows path: shell out to `icacls`. The audit's M5 finding flagged
//! the previous `#[cfg(not(unix))]` no-op as a silent permissions
//! degrade — on Windows the default DACL inherits from the parent
//! directory, which on a typical install puts BUILTIN\Users in
//! the read-list. That's wrong for a NATS user seed.
//!
//! We intentionally avoid the `windows` / `windows-sys` crates here —
//! `SetNamedSecurityInfoW` requires a substantial unsafe block plus
//! an `EXPLICIT_ACCESS_W` build, which is more attack surface than a
//! shell-out for a feature that runs at install time only. `icacls`
//! ships with every supported Windows since Vista; the fork/exec
//! cost is in the tens of milliseconds, which is invisible against
//! the rest of the install path (filesystem + JWT mint).
//!
//! Tested on Windows 10/11 and Windows Server 2019/2022. The
//! `icacls` argument shape is documented stable since Windows 7.

use std::path::Path;

use anyhow::{bail, Context as _, Result};

/// Restrict `path` to be read/write-only by the current user.
///
/// On POSIX this is `chmod 0600`. On Windows it sets a DACL that
/// strips inherited ACEs and grants Full Control only to the
/// current user (resolved from `USERNAME` + `USERDOMAIN`). On any
/// other target this falls through to a no-op (we don't ship
/// secrets there).
///
/// # Errors
/// * POSIX: filesystem error from `set_permissions`.
/// * Windows: `USERNAME` env-var unset, non-UTF-8 path, or `icacls`
///   non-zero exit.
pub fn restrict_permissions(path: &Path) -> Result<()> {
    restrict_inner(path)
}

#[cfg(unix)]
fn restrict_inner(path: &Path) -> Result<()> {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 {}", path.display()))
}

#[cfg(windows)]
fn restrict_inner(path: &Path) -> Result<()> {
    // Derive the current user as DOMAIN\USER (or just USER on machines
    // without a domain). Windows guarantees USERNAME for any
    // interactive session and any service running under a non-SYSTEM
    // identity; for SYSTEM we fall back to the bare name and let
    // icacls resolve via its built-in well-known-SID list.
    let user =
        std::env::var("USERNAME").context("USERNAME env var unset; cannot derive ACL principal")?;
    if user.is_empty() {
        bail!("USERNAME env var is empty; cannot derive ACL principal");
    }
    let principal = match std::env::var("USERDOMAIN") {
        Ok(d) if !d.is_empty() => format!("{d}\\{user}"),
        _ => user,
    };

    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF-8 path: {}", path.display()))?;

    // The minimal correct sequence:
    //   /inheritance:r  — break inheritance and drop inherited ACEs,
    //                     so the parent dir's "Users: read" doesn't
    //                     leak through.
    //   /grant:r <p>:F  — grant principal Full control, REPLACING any
    //                     existing ACE (the `:r` suffix on /grant).
    // The order matters: inheritance break first, grant second.
    // Without /inheritance:r the implicit Authenticated-Users ACE the
    // parent directory carries on a typical Windows install would
    // still apply.
    let output = std::process::Command::new("icacls")
        .arg(path_str)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{principal}:F"))
        .output()
        .with_context(|| format!("invoke icacls on {}", path.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "icacls failed (exit {}) on {}: principal={principal} stderr={stderr} stdout={stdout}",
            output.status.code().unwrap_or(-1),
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
#[allow(clippy::unnecessary_wraps)]
fn restrict_inner(_path: &Path) -> Result<()> {
    // No supported permissions story on other targets (wasm, etc.).
    // The CLI doesn't run there in production; this branch exists
    // so the workspace cross-check builds.
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn restrict_permissions_creates_no_error_on_existing_file() {
        // The minimum integration we can validate without a privileged
        // test environment: writing a regular file and locking it down
        // shouldn't error. The audit's POSIX path was already covered
        // by usage; this test pins the Windows path so a regression
        // in the icacls invocation surfaces as a unit-test failure
        // rather than at first-install time.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("secret.bin");
        std::fs::write(&p, b"test-secret").unwrap();
        restrict_permissions(&p).expect("restrict perms");

        // Round-trip: file still readable by us (we just locked it
        // down). On POSIX the mode is now 0o600; on Windows the
        // DACL is single-ACE current-user-Full.
        let read_back = std::fs::read(&p).unwrap();
        assert_eq!(read_back, b"test-secret");
    }

    #[cfg(unix)]
    #[test]
    fn unix_path_yields_0600_mode() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("secret.bin");
        std::fs::write(&p, b"x").unwrap();
        restrict_permissions(&p).expect("restrict perms");

        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0o600 mode, got {mode:o}");
    }
}
