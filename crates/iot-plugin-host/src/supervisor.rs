//! Crash supervision scaffolding (M2 W4).
//!
//! This module carries the *decision* side of plugin crash recovery:
//! given a stream of crash events, when should the host restart the
//! plugin, with what delay, and when should it give up and dead-letter
//! the install? The actual async loop that *runs* plugins and catches
//! traps is W4-next — but keeping the decision logic pure and
//! exhaustively-tested means the runtime loop that uses it stays thin.
//!
//! Contract (M2-PLAN W4): exponential back-off per crash, capped at
//! 30 s; dead-letter after 5 crashes inside a 10-minute rolling window.
//!
//! Dead-lettered plugins are marked on disk via a tiny `.dead-lettered`
//! file in the plugin's install directory. `iotctl plugin list` picks
//! this up without needing to talk to the running host, and the host's
//! next startup sweep refuses to reload a dead-lettered plugin until
//! the operator clears the marker (or re-runs `iotctl plugin install
//! --force`, which removes the dir tree).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use wasmtime::Engine;

use crate::runtime::{spawn_plugin_task, PluginHandle, DEFAULT_FUEL_PER_CALL};
use crate::{load_plugin_dir, HostBindings};

/// Marker filename inside `<plugin_dir>/<id>/` recording that the host
/// gave up trying to restart this plugin.
pub const DLQ_MARKER_FILENAME: &str = ".dead-lettered";

/// Default policy numbers. Callers can construct a `CrashTracker` with
/// bespoke values (e.g. tests), but `CrashTracker::default` lines up
/// with the M2-PLAN W4 contract.
pub const DEFAULT_WINDOW: Duration = Duration::from_secs(10 * 60);
pub const DEFAULT_MAX_IN_WINDOW: usize = 5;
pub const DEFAULT_BACKOFF_BASE: Duration = Duration::from_secs(1);
pub const DEFAULT_BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Single crash event.
#[derive(Debug, Clone)]
pub struct CrashRecord {
    pub at: Instant,
    pub reason: String,
}

/// Per-plugin rolling crash history + the thresholds that govern
/// restart-vs-dead-letter decisions.
#[derive(Debug, Clone)]
pub struct CrashTracker {
    records: Vec<CrashRecord>,
    window: Duration,
    max_in_window: usize,
    backoff_base: Duration,
    backoff_cap: Duration,
}

impl Default for CrashTracker {
    fn default() -> Self {
        Self::new(
            DEFAULT_WINDOW,
            DEFAULT_MAX_IN_WINDOW,
            DEFAULT_BACKOFF_BASE,
            DEFAULT_BACKOFF_CAP,
        )
    }
}

/// What the supervisor should do *after* a crash has been recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Restart the plugin after sleeping for `after`.
    Restart { after: Duration },
    /// Give up — too many crashes inside the rolling window.
    DeadLetter { crashes: usize, window: Duration },
}

impl CrashTracker {
    #[must_use]
    pub const fn new(
        window: Duration,
        max_in_window: usize,
        backoff_base: Duration,
        backoff_cap: Duration,
    ) -> Self {
        Self {
            records: Vec::new(),
            window,
            max_in_window,
            backoff_base,
            backoff_cap,
        }
    }

    /// Record a crash at `now` and return the decision.
    ///
    /// The "count" used for both the DLQ threshold and the backoff
    /// exponent is the number of crashes that have landed inside the
    /// rolling window (`window`) ending at `now`. Crashes older than
    /// that are pruned — so a plugin that was stable for hours and
    /// then crashes once starts with a fresh 1-second backoff, not a
    /// penalty inherited from last week.
    pub fn record(&mut self, now: Instant, reason: impl Into<String>) -> Decision {
        self.prune(now);
        self.records.push(CrashRecord {
            at: now,
            reason: reason.into(),
        });
        let count = self.records.len();
        if count >= self.max_in_window {
            Decision::DeadLetter {
                crashes: count,
                window: self.window,
            }
        } else {
            Decision::Restart {
                after: backoff(count, self.backoff_base, self.backoff_cap),
            }
        }
    }

    fn prune(&mut self, now: Instant) {
        let cutoff = now.checked_sub(self.window).unwrap_or(now);
        self.records.retain(|r| r.at >= cutoff);
    }

    /// Read-only view of the pruned crash history. Used by
    /// `iotctl plugin list --verbose` in a later slice.
    #[must_use]
    pub fn history(&self) -> &[CrashRecord] {
        &self.records
    }
}

/// Exponential backoff with a cap. `count = 1` → `base`; `count = 2` →
/// `2 * base`; `count = 3` → `4 * base`; … capped at `cap`.
///
/// Using `u32` multipliers throughout: the cap kicks in well before
/// anything approaches `u32::MAX`, but `saturating_mul` keeps us safe
/// if the caller picks pathological values.
#[must_use]
pub fn backoff(count: usize, base: Duration, cap: Duration) -> Duration {
    // count starts at 1 after the first crash; shift left by (count-1).
    // Clamp to 30 so `1u32 << shift` never overflows (2^30 = ~1 Gs backoff,
    // far past any reasonable cap).
    let shift = u32::try_from(count.saturating_sub(1))
        .unwrap_or(u32::MAX)
        .min(30);
    let multiplier = 1u32.checked_shl(shift).unwrap_or(u32::MAX);
    let candidate = base.saturating_mul(multiplier);
    candidate.min(cap)
}

// -------------------------------------------------------- DLQ marker file

/// Write a dead-letter marker in `install_dir`. Idempotent — if the
/// marker already exists the reason is overwritten, useful when the
/// host wants to update the latest-crash reason without re-DLQ-ing.
///
/// # Errors
/// Propagates any `std::fs::write` error (typically permission-denied
/// on the install dir).
pub fn write_dead_lettered(install_dir: &Path, reason: &str) -> std::io::Result<()> {
    std::fs::write(install_dir.join(DLQ_MARKER_FILENAME), reason.as_bytes())
}

/// True iff the plugin install dir carries a `.dead-lettered` marker.
#[must_use]
pub fn is_dead_lettered(install_dir: &Path) -> bool {
    install_dir.join(DLQ_MARKER_FILENAME).is_file()
}

/// Remove any dead-letter marker. No-op if the marker isn't present.
///
/// # Errors
/// Propagates `std::fs::remove_file` errors other than NotFound.
pub fn clear_dead_lettered(install_dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(install_dir.join(DLQ_MARKER_FILENAME)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// ------------------------------------------------------ supervise loop

/// Supervise one plugin install directory. Loads the plugin, spawns its
/// runtime task, and on crash consults [`CrashTracker`] to decide
/// restart-vs-dead-letter.
///
/// Returns:
///   * `Ok(())` — plugin exited cleanly (explicit Shutdown command, or
///     its mpsc channel was dropped) OR the supervisor DLQ'd it.
///   * `Err(_)` — something the supervisor itself couldn't handle
///     (e.g. the DLQ marker couldn't be written).
///
/// On startup, if the install directory is already dead-lettered
/// (`.dead-lettered` marker exists), the supervisor refuses to load
/// and returns `Ok(())` immediately. The operator clears the marker
/// (or re-runs `iotctl plugin install --force`) to un-stick the plugin.
///
/// # Errors
/// Only returns `Err` when the supervisor itself fails (e.g. can't
/// write the DLQ marker to disk). Plugin-level failures are handled
/// internally and converted to `Ok(())` on DLQ.
pub async fn supervise(engine: Engine, install_dir: PathBuf, bindings: HostBindings) -> Result<()> {
    if is_dead_lettered(&install_dir) {
        tracing::warn!(
            dir = %install_dir.display(),
            "refusing to supervise dead-lettered plugin (clear .dead-lettered to retry)"
        );
        return Ok(());
    }

    let mut tracker = CrashTracker::default();

    loop {
        // Rebuild Store + Plugin fresh each iteration — Wasmtime doesn't
        // let you reuse a crashed Store, and the manifest might have been
        // replaced out from under us between restarts.
        let (store, plugin, manifest) =
            match load_plugin_dir(&engine, &install_dir, bindings.clone()).await {
                Ok(x) => x,
                Err(e) => {
                    let reason = format!("load_plugin_dir failed: {e:#}");
                    tracing::error!(dir = %install_dir.display(), reason = %reason);
                    if on_crash(&mut tracker, &reason, &install_dir).await? {
                        return Ok(()); // DLQ'd
                    }
                    continue;
                }
            };

        tracing::info!(plugin = %manifest.id, version = %manifest.version, "plugin starting");
        // Per-call fuel budget: manifest's resources.fuel_max if set, else
        // the host default. Refilled by the runtime task before each guest
        // invocation (M5a W3 — debt #6 closure).
        let fuel_per_call = if manifest.resources.fuel_max > 0 {
            manifest.resources.fuel_max
        } else {
            DEFAULT_FUEL_PER_CALL
        };
        let PluginHandle { id, tx, join } =
            spawn_plugin_task(manifest.id.clone(), store, plugin, fuel_per_call);

        // We drop `tx` *after* `join.await` so the channel stays open for
        // the full task lifetime — otherwise the task would see a closed
        // channel and exit cleanly as soon as it finished init.
        let outcome = join.await;
        drop(tx);

        // Flush any MQTT router registrations this incarnation left
        // behind. On restart we'll register fresh; without this, stale
        // entries hold a dead tx and get pruned lazily on next dispatch.
        // Also drop the broker-side refcount for each filter the plugin
        // held — the last subscriber leaving triggers an UNSUBSCRIBE so
        // the broker stops delivering messages no one will handle (M5a
        // W3 — debt #7 closure).
        if let Some(broker) = bindings.mqtt.as_ref() {
            for filter in broker.router().unregister(&id) {
                if let Err(e) = broker.unsubscribe_filter(&filter).await {
                    tracing::warn!(
                        plugin = %id,
                        filter = %filter,
                        error = %format!("{e:#}"),
                        "broker unsubscribe failed during plugin exit cleanup"
                    );
                }
            }
        }

        match outcome {
            Ok(Ok(())) => {
                tracing::info!(plugin = %id, "clean shutdown");
                return Ok(());
            }
            Ok(Err(reason)) => {
                let reason_s = reason.to_string();
                tracing::warn!(plugin = %id, reason = %reason_s, "plugin crashed");
                if on_crash(&mut tracker, &reason_s, &install_dir).await? {
                    return Ok(()); // DLQ'd
                }
            }
            Err(join_err) => {
                // Task panicked — treat as a crash.
                let reason_s = format!("task panicked: {join_err}");
                tracing::error!(plugin = %id, reason = %reason_s);
                if on_crash(&mut tracker, &reason_s, &install_dir).await? {
                    return Ok(()); // DLQ'd
                }
            }
        }
    }
}

/// Feed one crash into the tracker and react. Returns `true` if the
/// plugin was just dead-lettered (caller should stop supervising).
async fn on_crash(tracker: &mut CrashTracker, reason: &str, install_dir: &Path) -> Result<bool> {
    match tracker.record(Instant::now(), reason) {
        Decision::Restart { after } => {
            // `Duration::as_millis()` returns u128 but the tracing macro
            // records numeric fields as i64/u64; saturating down to u64
            // is lossless in practice (u64::MAX ms ≈ 584 million years).
            let backoff_ms = u64::try_from(after.as_millis()).unwrap_or(u64::MAX);
            tracing::info!(
                dir = %install_dir.display(),
                backoff_ms,
                "scheduling restart"
            );
            tokio::time::sleep(after).await;
            Ok(false)
        }
        Decision::DeadLetter { crashes, window } => {
            tracing::error!(
                dir = %install_dir.display(),
                crashes = crashes,
                window_secs = window.as_secs(),
                "dead-lettering plugin"
            );
            write_dead_lettered(install_dir, reason)
                .with_context(|| format!("write .dead-lettered at {}", install_dir.display()))?;
            Ok(true)
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tracker(max: usize) -> CrashTracker {
        CrashTracker::new(
            Duration::from_secs(600),
            max,
            Duration::from_secs(1),
            Duration::from_secs(30),
        )
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        let base = Duration::from_secs(1);
        let cap = Duration::from_secs(30);
        assert_eq!(backoff(1, base, cap), Duration::from_secs(1));
        assert_eq!(backoff(2, base, cap), Duration::from_secs(2));
        assert_eq!(backoff(3, base, cap), Duration::from_secs(4));
        assert_eq!(backoff(4, base, cap), Duration::from_secs(8));
        assert_eq!(backoff(5, base, cap), Duration::from_secs(16));
        // 6 → 32s but capped at 30s.
        assert_eq!(backoff(6, base, cap), Duration::from_secs(30));
        // Huge count must not overflow — cap kicks in.
        assert_eq!(backoff(100, base, cap), Duration::from_secs(30));
    }

    #[test]
    fn first_crash_restarts_with_base_backoff() {
        let now = Instant::now();
        let mut t = tracker(5);
        let d = t.record(now, "oom");
        assert_eq!(
            d,
            Decision::Restart {
                after: Duration::from_secs(1)
            }
        );
    }

    #[test]
    fn crashes_growing_inside_window_exponentiate_then_dlq() {
        let t0 = Instant::now();
        let mut t = tracker(5);
        // 4 crashes across a 2-minute stretch — all inside the window.
        let d1 = t.record(t0, "one");
        let d2 = t.record(t0 + Duration::from_secs(5), "two");
        let d3 = t.record(t0 + Duration::from_secs(15), "three");
        let d4 = t.record(t0 + Duration::from_secs(45), "four");
        assert_eq!(
            d1,
            Decision::Restart {
                after: Duration::from_secs(1)
            }
        );
        assert_eq!(
            d2,
            Decision::Restart {
                after: Duration::from_secs(2)
            }
        );
        assert_eq!(
            d3,
            Decision::Restart {
                after: Duration::from_secs(4)
            }
        );
        assert_eq!(
            d4,
            Decision::Restart {
                after: Duration::from_secs(8)
            }
        );

        // 5th crash hits the dead-letter threshold.
        let d5 = t.record(t0 + Duration::from_secs(90), "five");
        assert!(matches!(d5, Decision::DeadLetter { crashes: 5, .. }));
    }

    #[test]
    fn crashes_fully_outside_window_are_forgiven() {
        let t0 = Instant::now();
        let mut t = tracker(5);
        // 4 early crashes → no DLQ yet.
        for i in 1..=4 {
            t.record(t0 + Duration::from_secs(i), format!("early {i}"));
        }
        // 10 min + some slack later, a fresh crash. The early ones are
        // pruned; the fresh crash is the only record.
        let later = t0 + Duration::from_secs(11 * 60);
        let d = t.record(later, "fresh");
        assert_eq!(
            d,
            Decision::Restart {
                after: Duration::from_secs(1)
            }
        );
        assert_eq!(t.history().len(), 1);
    }

    #[test]
    fn dlq_marker_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_dead_lettered(dir.path()));
        write_dead_lettered(dir.path(), "init-trap: divide by zero").unwrap();
        assert!(is_dead_lettered(dir.path()));
        let body = std::fs::read_to_string(dir.path().join(DLQ_MARKER_FILENAME)).unwrap();
        assert!(body.contains("divide by zero"));
        clear_dead_lettered(dir.path()).unwrap();
        assert!(!is_dead_lettered(dir.path()));
    }

    #[test]
    fn clear_is_idempotent_on_missing_marker() {
        let dir = tempfile::tempdir().unwrap();
        // No marker yet — clearing shouldn't error.
        clear_dead_lettered(dir.path()).unwrap();
    }
}
