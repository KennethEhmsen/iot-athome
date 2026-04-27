//! Owner-only file permissions for the seed + credential material
//! `iotctl` writes during `nats bootstrap` and `plugin install`.
//!
//! On Unix we keep the long-standing `chmod 0600`. On Windows we
//! shell out to `icacls` to (a) strip the inherited DACL the new
//! file picks up from its parent dir, and (b) install a single
//! explicit ACE granting the current user full control. That's the
//! closest analogue of the Unix mode bits — no inheritance, exactly
//! one ACE, owner-only.
//!
//! Why icacls (and not the `windows-acl` crate): the trailofbits
//! `windows-acl` is the obvious dep, but its last release is over
//! four years old and it still pulls legacy `winapi 0.3.x` (vs.
//! `windows-sys`). The seed-file write surface is small (≤6 sites
//! across the install + bootstrap flow), so the per-write
//! process-spawn cost is acceptable. icacls ships with every
//! Windows since Vista, so there's no install-time dep either.
//!
//! Bucket 1 audit finding M5 — see ADR-0011's Superseded → Windows
//! hardening note.

use std::path::Path;

use anyhow::{Context as _, Result};

/// Restrict a freshly-written file to its owner only.
///
/// On Unix this is `chmod 0600`. On Windows it strips inherited
/// ACEs and re-installs a single explicit ACE granting the current
/// user full control.
///
/// # Errors
/// Surfaces `chmod` / `icacls` failures with the file path and
/// (on Windows) icacls' stderr so an operator can diagnose ACL
/// trouble — typically a stale temp dir held by another user.
pub fn restrict_permissions(path: &Path) -> Result<()> {
    inner(path)
}

#[cfg(unix)]
fn inner(path: &Path) -> Result<()> {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 {}", path.display()))
}

#[cfg(windows)]
fn inner(path: &Path) -> Result<()> {
    use anyhow::{anyhow, bail};
    use std::process::Command;

    // USERNAME is the Windows-native variable, set by the OS for
    // every interactive + service session. We deliberately don't
    // fall back to USER (the Unix variable some shells set) — that
    // would mis-grant if `iotctl` runs under Git Bash with USER
    // overridden to a non-existent principal.
    let username =
        std::env::var("USERNAME").map_err(|e| anyhow!("read USERNAME from environment: {e}"))?;

    // /inheritance:r — drop inherited ACEs (parent dir's "Users",
    //                  "Authenticated Users" etc. that a new file
    //                  would otherwise pick up).
    // /grant:r       — replace any existing explicit grant for
    //                  `<user>` with full control. After both, the
    //                  DACL contains exactly one ACE and the file
    //                  is owner-only — the closest Windows analogue
    //                  of `chmod 0600`.
    let output = Command::new("icacls")
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{username}:F"))
        .output()
        .with_context(|| format!("spawn icacls for {}", path.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "icacls failed for {} (exit {}): {} {}",
            path.display(),
            output.status,
            stderr.trim(),
            stdout.trim()
        );
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[cfg(unix)]
    #[test]
    fn unix_chmods_to_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let td = TempDir::new().unwrap();
        let p = td.path().join("seed");
        fs::write(&p, b"secret").unwrap();
        restrict_permissions(&p).expect("restrict");
        let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
    }

    /// Read `icacls <path>` back and assert the resulting DACL is
    /// (a) owner-only — current user appears with `(F)` — and
    /// (b) carries no inherited ACEs (the `(I)` flag is icacls'
    /// tell-tale for inheritance) and no broad-access principals.
    #[cfg(windows)]
    #[test]
    fn windows_grants_owner_only() {
        use std::process::Command;

        let td = TempDir::new().unwrap();
        let p = td.path().join("seed");
        fs::write(&p, b"secret").unwrap();
        restrict_permissions(&p).expect("restrict");

        let out = Command::new("icacls")
            .arg(&p)
            .output()
            .expect("spawn icacls");
        assert!(out.status.success(), "icacls read failed: {out:?}");
        let acl_text = String::from_utf8_lossy(&out.stdout);

        let username = std::env::var("USERNAME").expect("USERNAME");
        assert!(
            acl_text.contains(&format!("{username}:(F)")),
            "expected explicit owner ACE for {username} in:\n{acl_text}"
        );
        // After /inheritance:r, no ACE may carry the (I) inherited
        // flag. icacls only ever prints `(I)` next to inherited
        // entries, so its absence is the assertion we need.
        assert!(
            !acl_text.contains("(I)"),
            "unexpected inherited ACE in:\n{acl_text}"
        );
        // The broad-access principals that the parent's DACL would
        // normally hand a freshly-created file must be gone.
        for forbidden in ["Everyone", "Authenticated Users", "BUILTIN\\Users"] {
            assert!(
                !acl_text.contains(forbidden),
                "forbidden principal {forbidden} present in:\n{acl_text}"
            );
        }
    }

    /// The bootstrap + install paths call `restrict_permissions`
    /// repeatedly on different files; idempotent re-application on
    /// the same file (e.g. `--force` overwrites) must succeed.
    #[test]
    fn idempotent_on_same_file() {
        let td = TempDir::new().unwrap();
        let p = td.path().join("seed");
        fs::write(&p, b"secret").unwrap();
        restrict_permissions(&p).expect("first");
        restrict_permissions(&p).expect("second");
    }
}
