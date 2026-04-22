//! `iotctl plugin …` — install, list, uninstall.
//!
//! The install flow is the choke point for plugin trust: it re-parses the
//! manifest via the same code the runtime uses, verifies a cosign-style
//! ECDSA-P256 signature on the .wasm against a pinned public key, and only
//! then copies the bundle into the plugin dir. Per-plugin NATS credentials
//! (an nkey seed + a manifest-derived ACL snapshot) land next to the
//! manifest so the broker-side bootstrap can later mint a user JWT without
//! re-reading the manifest.
//!
//! What's *not* here yet (see `docs/M2-PLAN.md` W3):
//!   * Rekor lookup for true cosign keyless verification — pinned-pubkey
//!     verification is the ADR-0006 "signed-by-key fallback".
//!   * cargo-audit offline SBOM scan — follow-up commit.
//!   * z2m adapter migration to wasm32-wasip2 — follow-up commit.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context as _, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use clap::Subcommand;
use p256::ecdsa::signature::Verifier as _;
use p256::ecdsa::{Signature, VerifyingKey};
use p256::pkcs8::DecodePublicKey as _;

use iot_plugin_host::capabilities::CapabilityMap;
use iot_plugin_host::manifest::Manifest;

/// Default install root. Kept in sync with `iot_plugin_host::Config`'s own
/// default so `iotctl plugin install` writes to the same place the host
/// reads from without per-operator configuration.
pub const DEFAULT_PLUGIN_DIR: &str = "/var/lib/iotathome/plugins";

#[derive(Debug, Subcommand)]
pub enum PluginCmd {
    /// Validate, verify, and copy a plugin bundle into the plugin dir.
    ///
    /// The source directory must contain at least `manifest.yaml` and the
    /// manifest's `entrypoint` (conventionally `plugin.wasm`). A detached
    /// cosign signature at `<entrypoint>.sig` is required unless
    /// `--allow-unsigned` is passed. Optional: `sbom.cdx.json`,
    /// `<entrypoint>.cert`.
    Install {
        /// Source bundle directory.
        path: PathBuf,
        /// Install root. Defaults to [`DEFAULT_PLUGIN_DIR`].
        #[arg(
            long,
            env = "IOT_PLUGIN_DIR",
            default_value = DEFAULT_PLUGIN_DIR,
        )]
        plugin_dir: PathBuf,
        /// PEM-encoded ECDSA-P256 public key used to verify the plugin's
        /// `.sig` file. Cosign emits exactly this with
        /// `cosign generate-key-pair` (the `.pub` file).
        #[arg(long, env = "IOT_PLUGIN_TRUST_PUB")]
        trust_pub: Option<PathBuf>,
        /// Dev escape hatch: accept an unsigned plugin. Refused by the
        /// runtime in prod builds (see ADR-0006 dev/prod gate).
        #[arg(long)]
        allow_unsigned: bool,
        /// Replace an existing install of the same id.
        #[arg(long)]
        force: bool,
    },
    /// List installed plugins and their declared capabilities.
    List {
        #[arg(
            long,
            env = "IOT_PLUGIN_DIR",
            default_value = DEFAULT_PLUGIN_DIR,
        )]
        plugin_dir: PathBuf,
    },
    /// Remove an installed plugin by id.
    Uninstall {
        /// Plugin id (matches `manifest.id`).
        id: String,
        #[arg(
            long,
            env = "IOT_PLUGIN_DIR",
            default_value = DEFAULT_PLUGIN_DIR,
        )]
        plugin_dir: PathBuf,
    },
}

pub fn run(cmd: &PluginCmd) -> Result<()> {
    match cmd {
        PluginCmd::Install {
            path,
            plugin_dir,
            trust_pub,
            allow_unsigned,
            force,
        } => install(
            path,
            plugin_dir,
            trust_pub.as_deref(),
            *allow_unsigned,
            *force,
        ),
        PluginCmd::List { plugin_dir } => list(plugin_dir),
        PluginCmd::Uninstall { id, plugin_dir } => uninstall(id, plugin_dir),
    }
}

// ---------------------------------------------------------------- install

fn install(
    src: &Path,
    plugin_dir: &Path,
    trust_pub: Option<&Path>,
    allow_unsigned: bool,
    force: bool,
) -> Result<()> {
    // 1. Parse + schema-check the manifest via the runtime's own parser.
    let manifest_path = src.join("manifest.yaml");
    let manifest = Manifest::load(&manifest_path)
        .with_context(|| format!("load manifest {}", manifest_path.display()))?;

    let wasm_path = src.join(&manifest.entrypoint);
    if !wasm_path.is_file() {
        bail!(
            "entrypoint {} not found (declared in manifest as `{}`)",
            wasm_path.display(),
            manifest.entrypoint
        );
    }

    // 2. Signature check — gating both allow-unsigned and trust-pub.
    let sig_path = wasm_path.with_extension(extension_with_suffix(&wasm_path, "sig"));
    if sig_path.is_file() {
        let trust_pub =
            trust_pub.ok_or_else(|| anyhow!("signature file found but no --trust-pub provided"))?;
        verify_signature(&wasm_path, &sig_path, trust_pub).with_context(|| {
            format!(
                "verify cosign signature for {} against {}",
                wasm_path.display(),
                trust_pub.display()
            )
        })?;
        tracing::info!(
            plugin = %manifest.id,
            sig = %sig_path.display(),
            trust = %trust_pub.display(),
            "signature verified"
        );
    } else if allow_unsigned {
        tracing::warn!(
            plugin = %manifest.id,
            "installing UNSIGNED plugin (--allow-unsigned, dev only)"
        );
    } else {
        bail!(
            "no signature at {} and --allow-unsigned not set (see ADR-0006)",
            sig_path.display()
        );
    }

    // 3. SBOM presence — advisory for now; cargo-audit scan comes next.
    let sbom_path = src.join("sbom.cdx.json");
    if !sbom_path.is_file() {
        tracing::warn!(plugin = %manifest.id, "no sbom.cdx.json in bundle (CVE scan skipped)");
    }

    // 4. Prepare destination. Collision ⇒ require --force.
    let dest = plugin_dir.join(&manifest.id);
    if dest.exists() {
        if !force {
            bail!(
                "{} already exists — use --force to replace, or `iotctl plugin uninstall {}` first",
                dest.display(),
                manifest.id
            );
        }
        fs::remove_dir_all(&dest).with_context(|| format!("remove stale {}", dest.display()))?;
    }
    fs::create_dir_all(&dest).with_context(|| format!("mkdir -p {}", dest.display()))?;

    // 5. Copy in the bundle: manifest + wasm + optional {sig, cert, sbom}.
    copy_required(&manifest_path, &dest.join("manifest.yaml"))?;
    copy_required(&wasm_path, &dest.join(&manifest.entrypoint))?;
    copy_if_present(
        &sig_path,
        &dest.join(sig_path.file_name().unwrap_or_default()),
    )?;
    let cert_path = wasm_path.with_extension(extension_with_suffix(&wasm_path, "cert"));
    copy_if_present(
        &cert_path,
        &dest.join(cert_path.file_name().unwrap_or_default()),
    )?;
    copy_if_present(&sbom_path, &dest.join("sbom.cdx.json"))?;

    // 6. Mint per-plugin NATS credentials + ACL snapshot.
    generate_nats_identity(&dest, &manifest.id, &manifest.capabilities)
        .context("generate NATS identity")?;

    println!(
        "installed {} {} → {}",
        manifest.id,
        manifest.version,
        dest.display()
    );
    Ok(())
}

/// `foo.wasm` → `wasm.sig`; `foo.wasm.bin` → `wasm.bin.sig`. `Path::with_extension`
/// replaces the *last* component, so we have to rebuild the full suffix manually
/// when we want `<name>.wasm.sig` (two-segment extension).
fn extension_with_suffix(p: &Path, suffix: &str) -> String {
    let ext = p.extension().and_then(|s| s.to_str()).unwrap_or_default();
    if ext.is_empty() {
        suffix.to_string()
    } else {
        format!("{ext}.{suffix}")
    }
}

fn copy_required(from: &Path, to: &Path) -> Result<()> {
    fs::copy(from, to).with_context(|| format!("copy {} → {}", from.display(), to.display()))?;
    Ok(())
}

fn copy_if_present(from: &Path, to: &Path) -> Result<()> {
    if from.is_file() {
        copy_required(from, to)?;
    }
    Ok(())
}

// ------------------------------------------------------ signature verify

/// Verify a cosign-style detached signature: ECDSA-P256 / SHA-256 / DER-
/// encoded, base64-wrapped, over the raw blob. That's what `cosign
/// sign-blob --output-signature plugin.wasm.sig plugin.wasm` produces by
/// default, and it's the smallest format we can support without pulling
/// the whole sigstore stack.
pub fn verify_signature(blob_path: &Path, sig_path: &Path, pub_pem: &Path) -> Result<()> {
    let pem = fs::read_to_string(pub_pem)
        .with_context(|| format!("read trust pubkey {}", pub_pem.display()))?;
    let vk = VerifyingKey::from_public_key_pem(&pem)
        .map_err(|e| anyhow!("parse trust pubkey ({}): {e}", pub_pem.display()))?;

    let sig_text = fs::read_to_string(sig_path)
        .with_context(|| format!("read signature {}", sig_path.display()))?;
    let sig_bytes = B64
        .decode(sig_text.trim().as_bytes())
        .with_context(|| format!("base64-decode signature {}", sig_path.display()))?;
    let sig = Signature::from_der(&sig_bytes)
        .map_err(|e| anyhow!("parse DER signature ({}): {e}", sig_path.display()))?;

    let blob = fs::read(blob_path).with_context(|| format!("read blob {}", blob_path.display()))?;
    vk.verify(&blob, &sig)
        .map_err(|e| anyhow!("signature verification failed: {e}"))
}

// -------------------------------------------------------- NATS identity

/// Per-plugin NATS identity — an ed25519 nkey seed + a manifest-derived
/// ACL snapshot. The seed is the long-lived secret; the ACL is the
/// operator's cheat-sheet for minting a user JWT that the broker will
/// accept. Account JWT minting itself lives in a separate commit.
fn generate_nats_identity(dest: &Path, plugin_id: &str, caps: &CapabilityMap) -> Result<()> {
    let kp = nkeys::KeyPair::new_user();
    let seed = kp
        .seed()
        .map_err(|e| anyhow!("nkey seed encode failed: {e}"))?;
    let public_key = kp.public_key();

    let seed_path = dest.join("nats.nkey");
    fs::write(&seed_path, &seed).with_context(|| format!("write {}", seed_path.display()))?;
    restrict_permissions(&seed_path)?;

    let acl = serde_json::json!({
        "plugin_id": plugin_id,
        "user_nkey": public_key,
        "allow_pub": caps.bus.publish,
        "allow_sub": caps.bus.subscribe,
    });
    let acl_path = dest.join("acl.json");
    fs::write(&acl_path, serde_json::to_string_pretty(&acl)?)
        .with_context(|| format!("write {}", acl_path.display()))?;

    tracing::info!(
        plugin = %plugin_id,
        nkey = %public_key,
        "generated NATS identity"
    );
    Ok(())
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
    // Windows ACLs require a different ritual; leaving the seed at
    // default perms is acceptable on dev boxes. Signature kept Result<()>
    // so Unix + Windows call-sites don't diverge.
    Ok(())
}

// ------------------------------------------------------------- list / uninstall

fn list(plugin_dir: &Path) -> Result<()> {
    if !plugin_dir.exists() {
        println!("(no plugins installed at {})", plugin_dir.display());
        return Ok(());
    }
    let mut entries = Vec::new();
    for entry in
        fs::read_dir(plugin_dir).with_context(|| format!("read_dir {}", plugin_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let manifest_path = entry.path().join("manifest.yaml");
        if !manifest_path.is_file() {
            continue;
        }
        match Manifest::load(&manifest_path) {
            Ok(m) => entries.push(m),
            Err(e) => tracing::warn!(
                path = %manifest_path.display(),
                error = %e,
                "skipping unreadable manifest"
            ),
        }
    }
    if entries.is_empty() {
        println!("(no plugins installed at {})", plugin_dir.display());
        return Ok(());
    }
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    for m in &entries {
        let pubs = m.capabilities.bus.publish.len();
        let subs = m.capabilities.bus.subscribe.len();
        println!(
            "{:<28} {:<10} {} (publish:{pubs} subscribe:{subs})",
            m.id, m.version, m.runtime,
        );
    }
    Ok(())
}

fn uninstall(id: &str, plugin_dir: &Path) -> Result<()> {
    let dest = plugin_dir.join(id);
    if !dest.exists() {
        bail!("{} is not installed under {}", id, plugin_dir.display());
    }
    if !dest.join("manifest.yaml").is_file() {
        bail!(
            "{} exists but has no manifest.yaml — refusing to rm -rf a directory we didn't install",
            dest.display()
        );
    }
    fs::remove_dir_all(&dest).with_context(|| format!("remove {}", dest.display()))?;
    println!("uninstalled {id} (removed {})", dest.display());
    Ok(())
}

// --------------------------------------------------------------- tests

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Signer as _;
    use p256::ecdsa::SigningKey;
    use p256::pkcs8::{EncodePublicKey as _, LineEnding};
    use rand_core::OsRng;

    const DEMO_MANIFEST: &str = r#"
schema_version: 1
id: test-plugin
name: Test Plugin
version: 0.1.0
runtime: wasm-component
entrypoint: plugin.wasm
capabilities:
  bus:
    publish:
      - "device.test-plugin.>"
    subscribe:
      - "cmd.test-plugin.>"
"#;

    /// Non-empty bytes that look vaguely like a WASM component — good enough
    /// for the install path, which doesn't parse the wasm itself.
    const FAKE_WASM: &[u8] = b"\0asm\x01\x00\x00\x00test-plugin-bytes";

    fn write_staging(dir: &Path, manifest: &str, wasm: &[u8]) {
        fs::write(dir.join("manifest.yaml"), manifest).unwrap();
        fs::write(dir.join("plugin.wasm"), wasm).unwrap();
    }

    fn sign_and_export(dir: &Path, wasm: &[u8]) {
        // Generate a fresh ECDSA-P256 keypair, sign the wasm, and drop
        // `plugin.wasm.sig` + `cosign.pub` alongside — the shape real
        // `cosign sign-blob` produces.
        let sk = SigningKey::random(&mut OsRng);
        let sig: Signature = sk.sign(wasm);
        let sig_der = sig.to_der();
        fs::write(dir.join("plugin.wasm.sig"), B64.encode(sig_der.as_bytes())).unwrap();

        let vk = sk.verifying_key();
        let pem = vk.to_public_key_pem(LineEnding::LF).expect("pubkey → PEM");
        fs::write(dir.join("cosign.pub"), pem).unwrap();
    }

    #[test]
    fn install_rejects_unsigned_without_flag() {
        let staging = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        write_staging(staging.path(), DEMO_MANIFEST, FAKE_WASM);

        let err = install(staging.path(), installed.path(), None, false, false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no signature"),
            "expected unsigned-rejection, got: {msg}"
        );
    }

    #[test]
    fn install_accepts_unsigned_with_flag() {
        let staging = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        write_staging(staging.path(), DEMO_MANIFEST, FAKE_WASM);

        install(staging.path(), installed.path(), None, true, false)
            .expect("install allow-unsigned");

        let dest = installed.path().join("test-plugin");
        assert!(dest.join("manifest.yaml").is_file());
        assert!(dest.join("plugin.wasm").is_file());
        assert!(dest.join("nats.nkey").is_file(), "nkey seed");
        assert!(dest.join("acl.json").is_file(), "acl snapshot");
    }

    #[test]
    fn install_verifies_signed_plugin() {
        let staging = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        write_staging(staging.path(), DEMO_MANIFEST, FAKE_WASM);
        sign_and_export(staging.path(), FAKE_WASM);

        let trust_pub = staging.path().join("cosign.pub");
        install(
            staging.path(),
            installed.path(),
            Some(&trust_pub),
            false,
            false,
        )
        .expect("install signed");

        // Signature file carries through to the install dir.
        assert!(installed
            .path()
            .join("test-plugin")
            .join("plugin.wasm.sig")
            .is_file());
    }

    #[test]
    fn install_rejects_tampered_wasm() {
        let staging = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        write_staging(staging.path(), DEMO_MANIFEST, FAKE_WASM);
        sign_and_export(staging.path(), FAKE_WASM);
        // Attacker swaps the wasm *after* signing.
        fs::write(staging.path().join("plugin.wasm"), b"\0asmTAMPERED").unwrap();

        let trust_pub = staging.path().join("cosign.pub");
        let err = install(
            staging.path(),
            installed.path(),
            Some(&trust_pub),
            false,
            false,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("signature") || msg.contains("verification"),
            "expected sig-verification error, got: {msg}"
        );
    }

    #[test]
    fn install_refuses_collision_without_force() {
        let staging = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        write_staging(staging.path(), DEMO_MANIFEST, FAKE_WASM);

        install(staging.path(), installed.path(), None, true, false).unwrap();
        let err = install(staging.path(), installed.path(), None, true, false).unwrap_err();
        assert!(format!("{err:#}").contains("already exists"));

        // With --force it succeeds.
        install(staging.path(), installed.path(), None, true, true).unwrap();
    }

    #[test]
    fn list_and_uninstall_roundtrip() {
        let staging = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        write_staging(staging.path(), DEMO_MANIFEST, FAKE_WASM);

        install(staging.path(), installed.path(), None, true, false).unwrap();
        list(installed.path()).expect("list");
        uninstall("test-plugin", installed.path()).expect("uninstall");
        assert!(!installed.path().join("test-plugin").exists());
    }
}
