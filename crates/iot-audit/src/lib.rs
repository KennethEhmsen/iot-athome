//! Append-only, hash-chained audit log.
//!
//! Each entry carries a SHA-256 hash over (previous_hash || canonical_json(payload)).
//! Tampering with any historical entry invalidates every hash thereafter.
//! The log is append-only on disk; compaction is out of scope for W1.

#![forbid(unsafe_code)]

use chrono::{DateTime, Utc};
use ring::digest;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::sync::Mutex;

/// Hex-encoded SHA-256 digest.
pub type Hash = String;

/// One audit entry. The serialized JSON form is what gets appended to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Monotonically-increasing sequence number starting at 0.
    pub seq: u64,
    /// Hash of the previous entry. Empty string for seq=0.
    pub prev: Hash,
    /// Wall-clock timestamp at log time.
    pub at: DateTime<Utc>,
    /// Short, stable kind tag ("device.added", "plugin.installed", ...).
    pub kind: String,
    /// Structured payload. Keep it small; the audit log is not a TSDB.
    pub payload: serde_json::Value,
    /// This entry's own hash, computed over (prev || canonical_payload).
    pub hash: Hash,
}

/// Errors produced by the audit log.
#[derive(Debug, Error)]
pub enum AuditError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("chain is broken at seq {seq}: expected prev {expected}, got {actual}")]
    ChainBroken {
        seq: u64,
        expected: Hash,
        actual: Hash,
    },
}

/// Append-only handle to the audit log.
#[derive(Debug)]
pub struct AuditLog {
    path: PathBuf,
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    last_seq: u64,
    last_hash: Hash,
}

impl AuditLog {
    /// Open (creating if absent) the audit log at `path`. Replays it to
    /// recover the tail state.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, AuditError> {
        let path = path.as_ref().to_path_buf();
        let (last_seq, last_hash) = replay_tail(&path).await?;
        Ok(Self {
            path,
            inner: Mutex::new(Inner {
                last_seq,
                last_hash,
            }),
        })
    }

    /// Append a new entry. Returns the stored entry (with assigned seq + hash).
    pub async fn append(
        &self,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<Entry, AuditError> {
        let mut inner = self.inner.lock().await;

        let seq = if inner.last_hash.is_empty() {
            0
        } else {
            inner.last_seq + 1
        };
        let prev = inner.last_hash.clone();
        let at = Utc::now();

        let hash = hash_entry(&prev, kind, &payload)?;

        let entry = Entry {
            seq,
            prev,
            at,
            kind: kind.to_owned(),
            payload,
            hash: hash.clone(),
        };

        let line = serde_json::to_string(&entry)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
        f.flush().await?;

        inner.last_seq = seq;
        inner.last_hash = hash;

        Ok(entry)
    }

    /// Verify the entire log file from seq=0.
    ///
    /// Now does three things (M3 W1.4 strengthening from W1-era
    /// chain-linkage-only check):
    ///   1. sequence numbers are contiguous from 0
    ///   2. every entry's `prev` matches the previous entry's `hash`
    ///   3. **recompute** each entry's hash from (prev || kind ||
    ///      canonical_json(payload)) and check it matches the stored
    ///      `hash`. This is what makes the log tamper-detectable —
    ///      editing any historical payload invalidates its hash, and
    ///      resetting the stored hash cascades into a chain break.
    pub async fn verify(&self) -> Result<(), AuditError> {
        let f = File::open(&self.path).await?;
        let mut reader = BufReader::new(f);
        let mut line = String::new();
        let mut expected_seq: u64 = 0;
        let mut expected_prev: Hash = Hash::new();

        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }
            let entry: Entry = serde_json::from_str(line.trim_end())?;
            if entry.seq != expected_seq {
                return Err(AuditError::ChainBroken {
                    seq: entry.seq,
                    expected: format!("seq {expected_seq}"),
                    actual: format!("seq {}", entry.seq),
                });
            }
            if entry.prev != expected_prev {
                return Err(AuditError::ChainBroken {
                    seq: entry.seq,
                    expected: expected_prev,
                    actual: entry.prev,
                });
            }

            // Recompute the hash from (prev, kind, canonical_payload)
            // and compare with the stored `hash`. If someone tampered
            // with any of those three fields after write, the hashes
            // diverge and we fail with ChainBroken.
            let recomputed = hash_entry(&entry.prev, &entry.kind, &entry.payload)?;
            if recomputed != entry.hash {
                return Err(AuditError::ChainBroken {
                    seq: entry.seq,
                    expected: recomputed,
                    actual: entry.hash,
                });
            }

            expected_prev = entry.hash;
            expected_seq += 1;
        }

        Ok(())
    }
}

/// Compute the canonical SHA-256 of an entry.
///
/// Canonicalised via JCS (RFC 8785) so the byte-level JSON
/// representation is unambiguous — key order is sorted, numbers are
/// normalised. Replaces the M1/M2 ad-hoc `serde_json::to_string` form
/// per the M2 retro's architectural-debt item 6.
fn hash_entry(prev: &str, kind: &str, payload: &serde_json::Value) -> Result<Hash, AuditError> {
    let canonical = serde_jcs::to_string(payload).map_err(AuditError::Json)?;
    let mut ctx = digest::Context::new(&digest::SHA256);
    ctx.update(prev.as_bytes());
    ctx.update(b"|");
    ctx.update(kind.as_bytes());
    ctx.update(b"|");
    ctx.update(canonical.as_bytes());
    Ok(hex::encode_lower(ctx.finish().as_ref()))
}

async fn replay_tail(path: &Path) -> Result<(u64, Hash), AuditError> {
    if !path.exists() {
        return Ok((0, Hash::new()));
    }
    let f = File::open(path).await?;
    let mut reader = BufReader::new(f);
    let mut line = String::new();
    let mut last_seq: u64 = 0;
    let mut last_hash = Hash::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let entry: Entry = serde_json::from_str(line.trim_end())?;
        last_seq = entry.seq;
        last_hash = entry.hash;
    }

    Ok((last_seq, last_hash))
}

// ring does not re-export hex; tiny inline helper to avoid an extra dep.
mod hex {
    pub fn encode_lower(bytes: &[u8]) -> String {
        const CHARS: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            out.push(CHARS[(b >> 4) as usize] as char);
            out.push(CHARS[(b & 0xF) as usize] as char);
        }
        out
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn append_and_verify() {
        let tmp = tempfile_path();
        let log = AuditLog::open(&tmp).await.expect("open");
        log.append("test.first", json!({"x": 1}))
            .await
            .expect("append");
        log.append("test.second", json!({"x": 2}))
            .await
            .expect("append");
        log.verify().await.expect("verify");
        std::fs::remove_file(&tmp).ok();
    }

    #[tokio::test]
    async fn payload_tampering_is_detected() {
        let tmp = tempfile_path();
        let log = AuditLog::open(&tmp).await.expect("open");
        log.append("test.event", json!({"x": 1})).await.unwrap();
        log.append("test.event", json!({"x": 2})).await.unwrap();

        // Rewrite the file flipping the payload of seq=0 without
        // touching the hash. Pre-M3 this was undetectable; with the
        // JCS canonical form + verify() that recomputes hashes, the
        // chain now breaks.
        let raw = tokio::fs::read_to_string(&tmp).await.unwrap();
        let tampered = raw.replacen(r#""x":1"#, r#""x":999"#, 1);
        assert_ne!(raw, tampered, "tamper substitution must land");
        tokio::fs::write(&tmp, tampered).await.unwrap();

        let fresh = AuditLog::open(&tmp).await.expect("reopen");
        let err = fresh.verify().await.expect_err("verify should fail");
        assert!(
            matches!(err, AuditError::ChainBroken { .. }),
            "expected ChainBroken, got {err:?}"
        );
        std::fs::remove_file(&tmp).ok();
    }

    #[tokio::test]
    async fn jcs_key_order_is_stable_across_appends() {
        // Two structurally-identical payloads with different key
        // insertion orders must hash identically under JCS. Proves
        // the canonicalisation actually normalises.
        let a = json!({"y": 2, "x": 1});
        let b = json!({"x": 1, "y": 2});
        let h1 = hash_entry("", "k", &a).unwrap();
        let h2 = hash_entry("", "k", &b).unwrap();
        assert_eq!(h1, h2);
    }

    fn tempfile_path() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("iot-audit-test-{}.log", ulid_like()));
        p
    }

    fn ulid_like() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{nanos:x}")
    }
}
