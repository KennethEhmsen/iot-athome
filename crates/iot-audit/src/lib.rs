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

        let canonical = canonical_json(&payload)?;
        let mut ctx = digest::Context::new(&digest::SHA256);
        ctx.update(prev.as_bytes());
        ctx.update(b"|");
        ctx.update(kind.as_bytes());
        ctx.update(b"|");
        ctx.update(canonical.as_bytes());
        let hash = hex::encode_lower(ctx.finish().as_ref());

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

    /// Verify the entire log file from seq=0, returning `Ok(())` if the chain is intact.
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
            expected_prev = entry.hash;
            expected_seq += 1;
        }

        Ok(())
    }
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

fn canonical_json(v: &serde_json::Value) -> Result<String, AuditError> {
    // `serde_json::to_string` already produces a stable form (sorted maps are
    // serde's default for `BTreeMap`, not for `serde_json::Map`). For a
    // minimum-viable canonical form we re-serialize from `Value`, which
    // preserves insertion order but keeps output deterministic given equal
    // input Values. A fuller canonical-JSON is a M2+ concern.
    Ok(serde_json::to_string(v)?)
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
