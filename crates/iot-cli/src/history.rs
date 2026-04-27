//! `iotctl history …` — operator-side history-store maintenance
//! (M6 W2).
//!
//! Single subcommand today: `iotctl history prune`. Deletes rows
//! from the optional TimescaleDB-backed `entity_state_history`
//! table that the M5a host wires up when `IOT_TIMESCALE_URL` is
//! set. The CLI is the operator surface ETSI EN 303 645 §5.11
//! ("Make it easy for users to delete user data") asks for —
//! without it, the only history-deletion path is direct SQL
//! against the database, which is operator-error-prone.
//!
//! Filter shapes:
//!
//! ```text
//! iotctl history prune --device-id <ulid>                       # all rows for a device
//! iotctl history prune --device-id <ulid> --before <rfc3339>    # device + time
//! iotctl history prune --before <rfc3339>                       # all devices, time-only
//! ```
//!
//! At least one filter is required — refusing a no-args prune
//! prevents accidental whole-table wipes.
//!
//! No-network: connects directly to the configured Postgres URL
//! via `iot_history::HistoryStore::connect`. Bypasses the gateway
//! by design — operator deletion shouldn't depend on the gateway
//! being up.

use anyhow::{anyhow, bail, Context as _, Result};
use chrono::{DateTime, Utc};
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum HistoryCmd {
    /// Delete rows from the `entity_state_history` table.
    ///
    /// Reads the database URL from `IOT_TIMESCALE_URL` (same env
    /// var the host uses). Operator must pass at least one of
    /// `--device-id` or `--before`.
    Prune {
        /// Delete only rows whose `device_id` matches. Pass the
        /// device ULID exactly as it appears in the registry.
        #[arg(long)]
        device_id: Option<String>,
        /// Delete only rows captured strictly before this
        /// timestamp. RFC 3339 format, e.g. `2026-01-01T00:00:00Z`.
        #[arg(long)]
        before: Option<String>,
        /// Skip the confirmation prompt. Default: print the row
        /// count that *would* be deleted, ask for `y` confirmation,
        /// then run.
        #[arg(long)]
        yes: bool,
    },
}

pub async fn run(cmd: &HistoryCmd) -> Result<()> {
    match cmd {
        HistoryCmd::Prune {
            device_id,
            before,
            yes,
        } => cmd_prune(device_id.as_deref(), before.as_deref(), *yes).await,
    }
}

async fn cmd_prune(device_id: Option<&str>, before: Option<&str>, yes: bool) -> Result<()> {
    use std::io::Write as _;

    if device_id.is_none() && before.is_none() {
        bail!(
            "refusing whole-table prune: pass --device-id <ulid> and/or --before <rfc3339>. \
             For a deliberate full purge, drop the table at the SQL level — the CLI doesn't \
             expose that path."
        );
    }

    let cutoff = before
        .map(|s| {
            DateTime::parse_from_rfc3339(s)
                .map(|d| d.with_timezone(&Utc))
                .map_err(|e| anyhow!("invalid --before timestamp: {e}"))
        })
        .transpose()?;

    let url = std::env::var("IOT_TIMESCALE_URL")
        .context("IOT_TIMESCALE_URL not set; this CLI talks to the Timescale backend directly")?;
    let store = iot_history::HistoryStore::connect(&url)
        .await
        .map_err(|e| anyhow!("connect history backend: {e}"))?;

    // Confirmation step — print what we're about to do, ask for `y`,
    // unless --yes. The CLI is a destructive surface; a single
    // typo on a device-id should never silently wipe production
    // data without an explicit acknowledgement.
    let what = describe_filter(device_id, cutoff);
    if !yes {
        println!("about to {what}");
        print!("proceed? [y/N] ");
        std::io::stdout().flush().ok();
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf).context("read stdin")?;
        if !matches!(buf.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            println!("aborted (no rows deleted)");
            return Ok(());
        }
    }

    let deleted = match (device_id, cutoff) {
        (Some(id), c) => store
            .prune_for_device(id, c)
            .await
            .map_err(|e| anyhow!("prune_for_device: {e}"))?,
        (None, Some(c)) => store
            .prune_older_than(c)
            .await
            .map_err(|e| anyhow!("prune_older_than: {e}"))?,
        (None, None) => unreachable!("guarded above"),
    };

    println!("deleted {deleted} rows ({what})");
    Ok(())
}

fn describe_filter(device_id: Option<&str>, cutoff: Option<DateTime<Utc>>) -> String {
    match (device_id, cutoff) {
        (Some(id), Some(c)) => format!(
            "delete rows for device {id} captured before {}",
            c.to_rfc3339()
        ),
        (Some(id), None) => format!("delete ALL rows for device {id}"),
        (None, Some(c)) => format!("delete all rows captured before {}", c.to_rfc3339()),
        (None, None) => "delete nothing (no filters)".into(),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn describe_filter_shapes() {
        let ts = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(describe_filter(Some("01HXX"), Some(ts)).contains("device 01HXX"));
        assert!(describe_filter(Some("01HXX"), None).contains("ALL rows for device"));
        assert!(describe_filter(None, Some(ts)).contains("all rows captured before"));
    }
}
