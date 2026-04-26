//! Live-Postgres integration test for the [`iot_history`] storage layer.
//!
//! Spins `timescale/timescaledb:2.17.2-pg16` (the same image the
//! `history` profile in `deploy/compose/dev-stack.yml` pulls) in a
//! testcontainer, then exercises every public method on
//! [`iot_history::HistoryStore`] end-to-end:
//!
//! - [`HistoryStore::connect`] — pool boots against the mapped port.
//! - [`HistoryStore::ensure_schema`] — table + hypertable + index DDL,
//!   called twice to confirm idempotence.
//! - [`HistoryStore::record`] — five rows with varying payload sizes,
//!   then a binary-payload byte-for-byte round-trip check.
//! - [`HistoryStore::fetch_range`] — wide window count, narrow window
//!   filtering, `ts DESC` ordering, and `LIMIT` clamping.
//! - [`HistoryStore::prune_older_than`] — `rows_affected` matches the
//!   number of rows whose ts falls below the cutoff.
//!
//! Plus a sanity probe via a sibling `sqlx::PgPool` that the
//! `entity_state_history` table actually became a Timescale hypertable
//! (queries `timescaledb_information.hypertables`). That's the
//! invariant the `DO $$ ... pg_extension ... $$` block in
//! `ensure_schema` is supposed to guarantee on a Timescale image.
//!
//! Run locally with:
//!
//!     cargo test -p iot-history --test postgres_roundtrip -- --nocapture
//!
//! Requires a Docker / Podman / Testcontainers-compatible runtime.
//! Skips with a no-op when no socket is reachable, matching the
//! convention from `iot-bus/tests/roundtrip.rs`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use chrono::{Duration as ChronoDuration, Utc};
use iot_history::HistoryStore;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row as _;
use std::time::Duration;
use testcontainers::{
    core::{ContainerPort, WaitFor},
    runners::AsyncRunner,
    GenericImage, ImageExt,
};

const PG_USER: &str = "iot";
const PG_PASS: &str = "iot";
const PG_DB: &str = "iot_history";

/// True iff a container runtime appears reachable. Same intent as
/// the gate in `iot-bus/tests/roundtrip.rs`, extended to recognise
/// the Windows Docker Desktop named pipe so the tests actually run
/// during local development on Windows hosts (CI is Linux + uds).
fn container_runtime_available() -> bool {
    if std::env::var_os("DOCKER_HOST").is_some() {
        return true;
    }
    if std::path::Path::new("/var/run/docker.sock").exists() {
        return true;
    }
    // Docker Desktop on Windows exposes the engine over a named pipe.
    // `Path::exists` returns true for it on Win32.
    #[cfg(windows)]
    if std::path::Path::new(r"\\.\pipe\docker_engine").exists() {
        return true;
    }
    false
}

/// Connect with retry — the postgres image emits "ready to accept
/// connections" twice (init phase + final boot). Testcontainers'
/// `WaitFor` matches the first occurrence; TCP isn't listening yet,
/// so connect will fail until the second startup. 30 s × 250 ms is
/// generous for CI under load.
async fn connect_with_retry(url: &str) -> HistoryStore {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        match HistoryStore::connect(url).await {
            Ok(store) => return store,
            Err(e) if std::time::Instant::now() < deadline => {
                eprintln!("connect retry: {e}");
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(e) => panic!("could not connect to test postgres within 30s: {e}"),
        }
    }
}

/// `get_host()` returns `localhost` on local Docker / Docker Desktop.
/// On Windows hosts where `hosts` resolves `localhost` to `::1` first
/// and the container only publishes on the IPv4 mapped port, sqlx's
/// resolver wedges on the IPv6 attempt long enough to trip the pool's
/// `acquire_timeout`. Force the IPv4 literal — the port we use is
/// `get_host_port_ipv4(...)` so the address-family choice is already
/// implied by the test.
async fn host_for_ipv4(container: &testcontainers::ContainerAsync<GenericImage>) -> String {
    match container.get_host().await {
        Ok(h) => {
            let s = h.to_string();
            if s == "localhost" {
                "127.0.0.1".into()
            } else {
                s
            }
        }
        Err(_) => "127.0.0.1".into(),
    }
}

/// Sibling pool used for raw-SQL probes (timestamp manipulation +
/// hypertable introspection). The public API doesn't expose the inner
/// pool by design.
async fn sibling_pool(url: &str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(2))
        .connect(url)
        .await
        .expect("sibling pool")
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn timescale_full_surface_roundtrip() {
    if !container_runtime_available() {
        eprintln!("no container runtime — skipping");
        return;
    }

    let container = GenericImage::new("timescale/timescaledb", "2.17.2-pg16")
        .with_exposed_port(ContainerPort::Tcp(5432))
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_USER", PG_USER)
        .with_env_var("POSTGRES_PASSWORD", PG_PASS)
        .with_env_var("POSTGRES_DB", PG_DB)
        .start()
        .await
        .expect("start timescaledb");

    let host = host_for_ipv4(&container).await;
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("mapped port");
    let url = format!("postgres://{PG_USER}:{PG_PASS}@{host}:{port}/{PG_DB}");

    let store = connect_with_retry(&url).await;

    // 1. ensure_schema is idempotent — calling twice must not error
    //    and must not duplicate the table or the hypertable conversion.
    store.ensure_schema().await.expect("ensure_schema #1");
    store.ensure_schema().await.expect("ensure_schema #2");

    // 2. Confirm the hypertable conversion actually fired. The DO-block
    //    is the most failure-prone bit of the schema bootstrap (it
    //    silently no-ops when timescaledb isn't installed); on an
    //    actual Timescale image, the row must exist.
    let probe = sibling_pool(&url).await;
    let hyper_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM timescaledb_information.hypertables \
         WHERE hypertable_name = 'entity_state_history'",
    )
    .fetch_one(&probe)
    .await
    .expect("hypertable probe");
    assert_eq!(
        hyper_count, 1,
        "entity_state_history must be a Timescale hypertable"
    );

    // 3. Five rows with varying payload sizes — empty, small, medium,
    //    large, and a 4 KiB binary blob with non-ASCII bytes to flush
    //    out any TEXT-vs-BYTEA encoding regressions.
    let device = "z2m-roundtrip";
    let other_device = "z2m-noise";
    let blob: Vec<u8> = (0u32..4096)
        .map(|i| u8::try_from(i % 256).unwrap())
        .collect();
    let payloads: [&[u8]; 5] = [b"", b"x", b"hello world", b"medium-ish payload", &blob];
    for (i, p) in payloads.iter().enumerate() {
        store
            .record(device, &format!("device.test.{i}.state"), p)
            .await
            .expect("record");
    }
    // A row for a different device, to confirm fetch_range filters
    // by device_id (not just by time).
    store
        .record(other_device, "device.test.noise.state", b"noise")
        .await
        .expect("record other");

    // 4. Wide-window fetch — all five rows for `device`, none for
    //    `other_device`.
    let now = Utc::now();
    let wide_from = now - ChronoDuration::hours(1);
    let wide_to = now + ChronoDuration::hours(1);
    let rows = store
        .fetch_range(device, wide_from, wide_to, 100)
        .await
        .expect("fetch wide");
    assert_eq!(rows.len(), 5, "wide window should see all 5 inserts");
    assert!(
        rows.iter().all(|r| r.device_id == device),
        "wide window should not bleed across device_id"
    );

    // 5. ts DESC ordering — `record()` writes happen-before so each
    //    successive insert has ts >= the previous. Verify the result
    //    set is monotonically non-increasing.
    for win in rows.windows(2) {
        assert!(win[0].ts >= win[1].ts, "rows must be ts DESC");
    }

    // 6. Payload byte-for-byte round-trip. The 4 KiB blob is the
    //    most discriminating sample — TEXT/encoding regressions
    //    typically truncate or mangle non-UTF-8 byte sequences.
    let big = rows
        .iter()
        .find(|r| r.payload.len() == blob.len())
        .expect("blob row present");
    assert_eq!(big.payload, blob, "BYTEA round-trip must be exact");

    // 7. LIMIT clamping — pass limit=2, get exactly 2 rows.
    let limited = store
        .fetch_range(device, wide_from, wide_to, 2)
        .await
        .expect("fetch limit");
    assert_eq!(limited.len(), 2, "LIMIT 2 must return 2 rows");

    // 8. Time-bound filtering. We rewrite ts directly via the sibling
    //    pool so we can pin rows to known instants — record() always
    //    uses the server-side `now()` default and that doesn't give
    //    us enough range to test bound filtering inside one test run.
    //
    //    Spread the device's rows across t-30m, t-20m, t-10m, t-5m,
    //    t-1m. Then a fetch from t-15m to t-2m should land exactly
    //    two rows (t-10m and t-5m).
    let anchor = now;
    let offsets = [30, 20, 10, 5, 1];
    for (i, mins) in offsets.iter().enumerate() {
        let target = anchor - ChronoDuration::minutes(i64::from(*mins));
        sqlx::query("UPDATE entity_state_history SET ts = $1 WHERE subject = $2")
            .bind(target)
            .bind(format!("device.test.{i}.state"))
            .execute(&probe)
            .await
            .expect("rewrite ts");
    }

    let narrow_from = anchor - ChronoDuration::minutes(15);
    let narrow_to = anchor - ChronoDuration::minutes(2);
    let narrow = store
        .fetch_range(device, narrow_from, narrow_to, 100)
        .await
        .expect("fetch narrow");
    assert_eq!(
        narrow.len(),
        2,
        "narrow window [t-15m, t-2m] must contain exactly the t-10m and t-5m rows, got: {:?}",
        narrow.iter().map(|r| r.ts).collect::<Vec<_>>()
    );

    // 9. prune_older_than. Cutoff = t-12m, drops the t-30m and t-20m
    //    rows (2 rows) but leaves t-10m, t-5m, t-1m.
    let cutoff = anchor - ChronoDuration::minutes(12);
    let pruned = store.prune_older_than(cutoff).await.expect("prune");
    assert_eq!(pruned, 2, "prune cutoff t-12m must drop exactly 2 rows");

    let after_prune = store
        .fetch_range(device, wide_from, wide_to, 100)
        .await
        .expect("fetch after prune");
    assert_eq!(
        after_prune.len(),
        3,
        "after pruning 2 of 5, 3 device rows must remain"
    );

    // 10. Index sanity — confirm the `(device_id, ts DESC)` btree
    //     exists. Doesn't check the planner uses it (would require
    //     a populated hypertable + EXPLAIN parsing), but does verify
    //     the DDL didn't get silently skipped.
    let idx_count: i64 = sqlx::query(
        "SELECT COUNT(*) FROM pg_indexes \
         WHERE schemaname = 'public' \
           AND tablename  = 'entity_state_history' \
           AND indexname  = 'entity_state_history_device_ts'",
    )
    .fetch_one(&probe)
    .await
    .expect("index probe")
    .get::<i64, _>(0);
    assert_eq!(idx_count, 1, "secondary (device_id, ts DESC) index missing");
}
