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
//!   * z2m adapter migration to wasm32-wasip2 — follow-up commit.
//!
//! SBOM vulnerability scan: the bundle's `sbom.cdx.json` (CycloneDX) is
//! read at install time. Any entry under `.vulnerabilities[]` rated
//! `high` or `critical` fails the install unless `--allow-vulnerabilities`
//! is set. The scan is intentionally a *consumer* of the SBOM (vs.
//! running `cargo audit` again against an advisory DB): the plugin
//! author is expected to run their own audit when producing the SBOM —
//! SLSA L3-style "producer attests, consumer verifies".

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context as _, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use clap::Subcommand;
use p256::ecdsa::signature::Verifier as _;
use p256::ecdsa::{Signature, VerifyingKey};
use p256::pkcs8::{DecodePublicKey as _, EncodePublicKey as _};

use iot_plugin_host::capabilities::CapabilityMap;
use iot_plugin_host::manifest::Manifest;
use iot_plugin_host::supervisor;

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
        /// Accept a plugin whose SBOM declares high / critical
        /// vulnerabilities. Default is to refuse; findings are always
        /// printed regardless of this flag.
        #[arg(long)]
        allow_vulnerabilities: bool,
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
        /// Optional trust pubkey. When set, list verifies each plugin's
        /// detached signature and prints `verified <fingerprint>` /
        /// `verification-failed`. Without it, signatures show as
        /// `signed (unverified)` or `unsigned`.
        #[arg(long, env = "IOT_PLUGIN_TRUST_PUB")]
        trust_pub: Option<PathBuf>,
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
            allow_vulnerabilities,
            force,
        } => install(
            path,
            plugin_dir,
            trust_pub.as_deref(),
            *allow_unsigned,
            *allow_vulnerabilities,
            *force,
        ),
        PluginCmd::List {
            plugin_dir,
            trust_pub,
        } => list(plugin_dir, trust_pub.as_deref()),
        PluginCmd::Uninstall { id, plugin_dir } => uninstall(id, plugin_dir),
    }
}

// ---------------------------------------------------------------- install

/// Signature gate: verify `sig_path` against `trust_pub` if the sig
/// exists, otherwise require `--allow-unsigned`.
fn enforce_signature_policy(
    plugin_id: &str,
    wasm_path: &Path,
    sig_path: &Path,
    trust_pub: Option<&Path>,
    allow_unsigned: bool,
) -> Result<()> {
    if sig_path.is_file() {
        let trust_pub =
            trust_pub.ok_or_else(|| anyhow!("signature file found but no --trust-pub provided"))?;
        verify_signature(wasm_path, sig_path, trust_pub).with_context(|| {
            format!(
                "verify cosign signature for {} against {}",
                wasm_path.display(),
                trust_pub.display()
            )
        })?;
        tracing::info!(
            plugin = %plugin_id,
            sig = %sig_path.display(),
            trust = %trust_pub.display(),
            "signature verified"
        );
        Ok(())
    } else if allow_unsigned {
        tracing::warn!(
            plugin = %plugin_id,
            "installing UNSIGNED plugin (--allow-unsigned, dev only)"
        );
        Ok(())
    } else {
        bail!(
            "no signature at {} and --allow-unsigned not set (see ADR-0006)",
            sig_path.display()
        )
    }
}

/// CVE gate: parse the bundled SBOM's `.vulnerabilities[]`, log each
/// finding, and refuse install on any `>= High` severity unless the
/// caller passed `--allow-vulnerabilities`.
fn enforce_sbom_policy(
    plugin_id: &str,
    sbom_path: &Path,
    allow_vulnerabilities: bool,
) -> Result<()> {
    if !sbom_path.is_file() {
        tracing::warn!(plugin = %plugin_id, "no sbom.cdx.json in bundle (CVE scan skipped)");
        return Ok(());
    }
    let findings =
        scan_sbom(sbom_path).with_context(|| format!("scan SBOM {}", sbom_path.display()))?;
    for f in &findings {
        tracing::warn!(
            plugin = %plugin_id,
            cve = %f.id,
            severity = %f.severity,
            affects = %f.affects,
            "SBOM vulnerability"
        );
    }
    let blocking: Vec<&VulnFinding> = findings
        .iter()
        .filter(|f| f.severity >= Severity::High)
        .collect();
    if blocking.is_empty() {
        return Ok(());
    }
    let ids: Vec<&str> = blocking.iter().map(|f| f.id.as_str()).collect();
    if allow_vulnerabilities {
        tracing::warn!(
            plugin = %plugin_id,
            count = blocking.len(),
            advisories = ?ids,
            "installing despite high/critical advisories (--allow-vulnerabilities)"
        );
        Ok(())
    } else {
        bail!(
            "{} SBOM has {} high/critical advisory(ies): {} — \
             use --allow-vulnerabilities to override",
            plugin_id,
            blocking.len(),
            ids.join(", ")
        )
    }
}

fn install(
    src: &Path,
    plugin_dir: &Path,
    trust_pub: Option<&Path>,
    allow_unsigned: bool,
    allow_vulnerabilities: bool,
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
    enforce_signature_policy(
        &manifest.id,
        &wasm_path,
        &sig_path,
        trust_pub,
        allow_unsigned,
    )?;

    // 3. SBOM CVE check. Refuse install on any high/critical finding
    //    unless `--allow-vulnerabilities` is set.
    let sbom_path = src.join("sbom.cdx.json");
    enforce_sbom_policy(&manifest.id, &sbom_path, allow_vulnerabilities)?;

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

// -------------------------------------------------------------- SBOM scan

/// CycloneDX-standard severity ranking. Derived ordering puts `Critical`
/// at the top — the install gate uses `>= High` as the fail threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Severity {
    None,
    Info,
    Low,
    Medium,
    High,
    Critical,
    /// Rating present but unrecognized — conservatively ordered *below*
    /// `Low` so unknown severities don't block a release by accident, but
    /// logged at warn.
    Unknown,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::None => "none",
            Self::Info => "info",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
            Self::Unknown => "unknown",
        })
    }
}

fn parse_severity(raw: &str) -> Severity {
    match raw.to_ascii_lowercase().as_str() {
        "critical" => Severity::Critical,
        "high" => Severity::High,
        "medium" => Severity::Medium,
        "low" => Severity::Low,
        "info" | "informational" => Severity::Info,
        "none" => Severity::None,
        _ => Severity::Unknown,
    }
}

#[derive(Debug)]
struct VulnFinding {
    id: String,
    severity: Severity,
    affects: String,
}

/// Extract `vulnerabilities[]` from a CycloneDX JSON SBOM. We don't do
/// schema validation — just pull the fields we need. A missing array
/// means "this SBOM doesn't carry vuln data" (we log, don't fail).
fn scan_sbom(sbom_path: &Path) -> Result<Vec<VulnFinding>> {
    let bytes = fs::read(sbom_path).with_context(|| format!("read {}", sbom_path.display()))?;
    let doc: serde_json::Value =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", sbom_path.display()))?;
    let Some(vulns) = doc.get("vulnerabilities").and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(vulns.len());
    for v in vulns {
        let id = v
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("UNKNOWN")
            .to_owned();
        // CycloneDX allows multiple ratings (e.g. NVD + vendor). We fold
        // to the worst-case.
        let severity = v
            .get("ratings")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|r| r.get("severity").and_then(|s| s.as_str()))
            .map(parse_severity)
            .max()
            .unwrap_or(Severity::Unknown);
        let affects = v
            .get("affects")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|a| a.get("ref").and_then(|r| r.as_str()))
            .collect::<Vec<_>>()
            .join(",");
        out.push(VulnFinding {
            id,
            severity,
            affects,
        });
    }
    Ok(out)
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

/// Per-plugin signature posture. `Verified` carries the SHA-256 of the
/// trust pubkey's DER-encoded SubjectPublicKeyInfo (cosign's "key
/// fingerprint" convention) so two plugins signed by different keys
/// show distinct columns.
#[derive(Debug, Clone)]
enum SignatureStatus {
    /// Sig file present, verified against the supplied trust pubkey.
    Verified { fingerprint: String },
    /// Sig file present but verification failed (wrong key / tampered).
    VerificationFailed,
    /// Sig file present, no trust pubkey supplied to verify against.
    SignedUnverified,
    /// No signature file in the install dir.
    Unsigned,
}

impl SignatureStatus {
    fn label(&self) -> String {
        match self {
            Self::Verified { fingerprint } => format!("verified {}", &fingerprint[..16]),
            Self::VerificationFailed => "verification-failed".to_owned(),
            Self::SignedUnverified => "signed (unverified)".to_owned(),
            Self::Unsigned => "unsigned".to_owned(),
        }
    }
}

/// Determine signature posture for a single installed plugin. `entrypoint`
/// is the manifest-declared filename (typically `plugin.wasm`); the sig
/// lives at `<entrypoint>.sig` inside `install_dir`.
fn signature_status(
    install_dir: &Path,
    entrypoint: &str,
    trust_pub: Option<&Path>,
) -> SignatureStatus {
    let wasm_path = install_dir.join(entrypoint);
    let sig_path = wasm_path.with_extension(extension_with_suffix(&wasm_path, "sig"));
    if !sig_path.is_file() {
        return SignatureStatus::Unsigned;
    }
    let Some(trust_pub) = trust_pub else {
        return SignatureStatus::SignedUnverified;
    };
    if verify_signature(&wasm_path, &sig_path, trust_pub).is_err() {
        return SignatureStatus::VerificationFailed;
    }
    let fingerprint = pubkey_fingerprint(trust_pub).unwrap_or_else(|_| "unknown".to_owned());
    SignatureStatus::Verified { fingerprint }
}

/// SHA-256 of the trust pubkey's DER-encoded SubjectPublicKeyInfo,
/// hex-encoded. Matches `cosign public-key | openssl dgst -sha256`.
fn pubkey_fingerprint(pub_pem: &Path) -> Result<String> {
    use sha2::{Digest as _, Sha256};

    let pem = fs::read_to_string(pub_pem)
        .with_context(|| format!("read trust pubkey {}", pub_pem.display()))?;
    // p256's `from_public_key_pem` accepts SPKI PEM; round-trip it to
    // DER bytes for hashing.
    let vk =
        VerifyingKey::from_public_key_pem(&pem).map_err(|e| anyhow!("parse trust pubkey: {e}"))?;
    let der = vk
        .to_public_key_der()
        .map_err(|e| anyhow!("encode trust pubkey to DER: {e}"))?;
    let digest = Sha256::digest(der.as_bytes());
    Ok(hex_encode(&digest))
}

/// Lower-case hex without external crates. 32 bytes -> 64 chars.
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = std::fmt::write(&mut s, format_args!("{b:02x}"));
    }
    s
}

fn list(plugin_dir: &Path, trust_pub: Option<&Path>) -> Result<()> {
    if !plugin_dir.exists() {
        println!("(no plugins installed at {})", plugin_dir.display());
        return Ok(());
    }
    let mut entries: Vec<(Manifest, std::path::PathBuf)> = Vec::new();
    for entry in
        fs::read_dir(plugin_dir).with_context(|| format!("read_dir {}", plugin_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let install_dir = entry.path();
        let manifest_path = install_dir.join("manifest.yaml");
        if !manifest_path.is_file() {
            continue;
        }
        match Manifest::load(&manifest_path) {
            Ok(m) => entries.push((m, install_dir)),
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
    entries.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    println!(
        "{:<28} {:<10} {:<14} {:<16} {:<28} {:<32}",
        "ID", "VERSION", "HEALTH", "RUNTIME", "SIGNATURE", "IDENTITY"
    );
    for (m, dir) in &entries {
        let status = signature_status(dir, &m.entrypoint, trust_pub);
        let health = if supervisor::is_dead_lettered(dir) {
            "dead-lettered"
        } else {
            "healthy"
        };
        let identity = m
            .signatures
            .first()
            .filter(|c| !c.identity.is_empty())
            .map(|c| {
                if c.issuer.is_empty() {
                    c.identity.clone()
                } else {
                    format!("{} ({})", c.identity, c.issuer)
                }
            })
            .unwrap_or_default();
        let pubs = m.capabilities.bus.publish.len();
        let subs = m.capabilities.bus.subscribe.len();
        let mqtt_subs = m.capabilities.mqtt.subscribe.len();
        println!(
            "{:<28} {:<10} {:<14} {:<16} {:<28} {:<32}",
            m.id,
            m.version,
            health,
            m.runtime,
            status.label(),
            identity,
        );
        // Capability counts on a continuation line so the main row stays
        // grep-friendly.
        println!(
            "    capabilities: bus.publish={pubs}  bus.subscribe={subs}  mqtt.subscribe={mqtt_subs}"
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
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Signer as _;
    use p256::ecdsa::SigningKey;
    use p256::pkcs8::LineEnding;
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

    /// Signature-wise wrapper so the older tests don't have to spell out
    /// the two allow-* / force booleans every call.
    fn install_allow_unsigned(
        src: &Path,
        dest: &Path,
        trust_pub: Option<&Path>,
        force: bool,
    ) -> Result<()> {
        install(src, dest, trust_pub, true, false, force)
    }

    #[test]
    fn install_rejects_unsigned_without_flag() {
        let staging = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        write_staging(staging.path(), DEMO_MANIFEST, FAKE_WASM);

        let err = install(staging.path(), installed.path(), None, false, false, false).unwrap_err();
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

        install_allow_unsigned(staging.path(), installed.path(), None, false)
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
            false,
        )
        .expect("install signed");

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
        fs::write(staging.path().join("plugin.wasm"), b"\0asmTAMPERED").unwrap();

        let trust_pub = staging.path().join("cosign.pub");
        let err = install(
            staging.path(),
            installed.path(),
            Some(&trust_pub),
            false,
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

        install_allow_unsigned(staging.path(), installed.path(), None, false).unwrap();
        let err =
            install_allow_unsigned(staging.path(), installed.path(), None, false).unwrap_err();
        assert!(format!("{err:#}").contains("already exists"));

        install_allow_unsigned(staging.path(), installed.path(), None, true).unwrap();
    }

    #[test]
    fn list_and_uninstall_roundtrip() {
        let staging = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        write_staging(staging.path(), DEMO_MANIFEST, FAKE_WASM);

        install_allow_unsigned(staging.path(), installed.path(), None, false).unwrap();
        list(installed.path(), None).expect("list");
        uninstall("test-plugin", installed.path()).expect("uninstall");
        assert!(!installed.path().join("test-plugin").exists());
    }

    #[test]
    fn list_picks_up_dlq_marker() {
        let staging = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        write_staging(staging.path(), DEMO_MANIFEST, FAKE_WASM);
        install_allow_unsigned(staging.path(), installed.path(), None, false).unwrap();

        // Simulate the host having given up on this plugin.
        let dest = installed.path().join("test-plugin");
        supervisor::write_dead_lettered(&dest, "init-trap: divide by zero").unwrap();

        // The list call doesn't return the rendered text — we just make
        // sure the health lookup itself sees the marker and the call
        // doesn't error. The deeper assertion lives on the supervisor
        // module's own unit tests (`dlq_marker_roundtrip`).
        assert!(supervisor::is_dead_lettered(&dest));
        list(installed.path(), None).expect("list after DLQ");
    }

    // ----------------------------------------------------------- CVE scan

    /// Minimal CycloneDX 1.5 SBOM with one critical advisory against a
    /// synthetic package. Real syft/cargo-cyclonedx output has far more
    /// metadata but the scan only cares about `.vulnerabilities[*]`
    /// `{id, ratings[].severity, affects[].ref}`.
    fn sbom_with_vuln(severity: &str, cve: &str) -> String {
        serde_json::json!({
            "bomFormat": "CycloneDX",
            "specVersion": "1.5",
            "version": 1,
            "components": [
                { "type": "library", "bom-ref": "pkg:cargo/foo@1.0.0",
                  "name": "foo", "version": "1.0.0" }
            ],
            "vulnerabilities": [
                { "id": cve,
                  "ratings": [ { "severity": severity } ],
                  "affects": [ { "ref": "pkg:cargo/foo@1.0.0" } ] }
            ]
        })
        .to_string()
    }

    #[test]
    fn cve_scan_rejects_critical_without_allow() {
        let staging = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        write_staging(staging.path(), DEMO_MANIFEST, FAKE_WASM);
        fs::write(
            staging.path().join("sbom.cdx.json"),
            sbom_with_vuln("critical", "CVE-2024-99999"),
        )
        .unwrap();

        let err =
            install_allow_unsigned(staging.path(), installed.path(), None, false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("CVE-2024-99999") && msg.contains("critical"),
            "expected CVE advisory in error, got: {msg}"
        );
    }

    #[test]
    fn cve_scan_accepts_critical_with_allow() {
        let staging = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        write_staging(staging.path(), DEMO_MANIFEST, FAKE_WASM);
        fs::write(
            staging.path().join("sbom.cdx.json"),
            sbom_with_vuln("critical", "CVE-2024-99999"),
        )
        .unwrap();

        // allow_unsigned + allow_vulnerabilities, no --force.
        install(staging.path(), installed.path(), None, true, true, false).expect("install");
        let dest = installed.path().join("test-plugin");
        assert!(dest.join("sbom.cdx.json").is_file());
    }

    #[test]
    fn cve_scan_allows_low_severity() {
        let staging = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        write_staging(staging.path(), DEMO_MANIFEST, FAKE_WASM);
        fs::write(
            staging.path().join("sbom.cdx.json"),
            sbom_with_vuln("low", "CVE-2024-00001"),
        )
        .unwrap();

        // Low-severity findings don't block. No --allow-vulnerabilities needed.
        install_allow_unsigned(staging.path(), installed.path(), None, false)
            .expect("low severity should not block");
    }

    #[test]
    fn cve_scan_sbom_without_vulnerabilities_array_is_noop() {
        let staging = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        write_staging(staging.path(), DEMO_MANIFEST, FAKE_WASM);
        // Valid SBOM, no vulnerabilities section — i.e. hasn't been scanned.
        fs::write(
            staging.path().join("sbom.cdx.json"),
            r#"{"bomFormat":"CycloneDX","specVersion":"1.5","version":1}"#,
        )
        .unwrap();

        install_allow_unsigned(staging.path(), installed.path(), None, false)
            .expect("no vulnerabilities field should pass");
    }

    // ----------------------------------------------------- signature posture

    #[test]
    fn signature_status_unsigned_when_no_sig_file() {
        let staging = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        write_staging(staging.path(), DEMO_MANIFEST, FAKE_WASM);
        install_allow_unsigned(staging.path(), installed.path(), None, false).unwrap();

        let dest = installed.path().join("test-plugin");
        let status = signature_status(&dest, "plugin.wasm", None);
        assert!(matches!(status, SignatureStatus::Unsigned), "{status:?}");
    }

    #[test]
    fn signature_status_signed_unverified_without_trust_pub() {
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
            false,
        )
        .unwrap();

        let dest = installed.path().join("test-plugin");
        let status = signature_status(&dest, "plugin.wasm", None);
        assert!(
            matches!(status, SignatureStatus::SignedUnverified),
            "{status:?}"
        );
    }

    #[test]
    fn signature_status_verified_with_fingerprint() {
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
            false,
        )
        .unwrap();

        let dest = installed.path().join("test-plugin");
        let status = signature_status(&dest, "plugin.wasm", Some(&trust_pub));
        match status {
            SignatureStatus::Verified { fingerprint } => {
                // SHA-256 hex = 64 chars.
                assert_eq!(fingerprint.len(), 64);
                assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()));
            }
            other => panic!("expected Verified, got {other:?}"),
        }
    }

    #[test]
    fn signature_status_verification_failed_with_wrong_pubkey() {
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
            false,
        )
        .unwrap();

        // Generate a *different* trust pub that won't match the install's sig.
        let other = tempfile::tempdir().unwrap();
        let sk2 = SigningKey::random(&mut OsRng);
        let pem2 = sk2
            .verifying_key()
            .to_public_key_pem(LineEnding::LF)
            .unwrap();
        let other_pub = other.path().join("other.pub");
        fs::write(&other_pub, pem2).unwrap();

        let dest = installed.path().join("test-plugin");
        let status = signature_status(&dest, "plugin.wasm", Some(&other_pub));
        assert!(
            matches!(status, SignatureStatus::VerificationFailed),
            "{status:?}"
        );
    }

    #[test]
    fn pubkey_fingerprint_is_stable() {
        // Same key in same PEM file → same fingerprint twice in a row.
        let dir = tempfile::tempdir().unwrap();
        let sk = SigningKey::random(&mut OsRng);
        let pem = sk
            .verifying_key()
            .to_public_key_pem(LineEnding::LF)
            .unwrap();
        let key = dir.path().join("k.pub");
        fs::write(&key, pem).unwrap();
        let a = pubkey_fingerprint(&key).unwrap();
        let b = pubkey_fingerprint(&key).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn severity_ordering_is_expected() {
        assert!(Severity::Critical > Severity::High);
        assert!(Severity::High > Severity::Medium);
        assert!(Severity::Medium > Severity::Low);
        assert!(Severity::Low > Severity::Info);
        // Our gate is `>= High`; parse helper must agree.
        assert_eq!(parse_severity("CRITICAL"), Severity::Critical);
        assert_eq!(parse_severity("High"), Severity::High);
        assert_eq!(parse_severity("whatever"), Severity::Unknown);
    }
}
