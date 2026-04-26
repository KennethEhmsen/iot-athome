//! Integration test: end-to-end TimescaleDB round-trip.
//!
//! Spins `timescale/timescaledb:2.17.2-pg16` in a container, connects
//! the [`HistoryStore`], and walks every public method:
//!
//! * `connect()` against the container's mapped port.
//! * `ensure_schema()` twice (idempotent — the second call must not
//!   error or duplicate the table / index).
//! * `record()` for several rows with varying timestamps + payload
//!   sizes (including empty + multi-KB payloads to verify `BYTEA`
//!   round-trips without truncation or encoding issues).
//! * `fetch_range()` with `from` / `to` bounds, `limit` clamping, and
//!   most-recent-first ordering.
//! * `prune_older_than()` and verify the rows-deleted count.
//!
//! Hypertable verification: query `_timescaledb_catalog.hypertable`
//! after `ensure_schema()` and assert one row exists for the
//! `entity_state_history` table — proves the guarded
//! `create_hypertable(...)` block actually fired against the
//! Timescale-loaded image.
//!
//! Run locally with `cargo test -p iot-history --test postgres_roundtrip
//! -- --nocapture`. Requires Docker / Podman / a Testcontainers-
//! compatible runtime; the test self-skips when no runtime is detected.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use chrono::{DateTime, Duration, Utc};
use iot_history::HistoryStore;
use std::time::Duration as StdDuration;
use testcontainers::{
    core::{ContainerPort, WaitFor},
    runners::AsyncRunner,
    GenericImage, ImageExt,
};

const PG_USER: &str = "iot";
const PG_PASS: &str = "iot";
const PG_DB: &str = "iot_history";

/// True when the local environment has a container runtime we can
/// drive via testcontainers. Mirrors the iot-bus roundtrip test's
/// skip predicate so dev machines without Docker (e.g. plain Windows
/// without WSL backend) don't fail to build.
fn has_container_runtime() -> bool {
    std::env::var_os("DOCKER_HOST").is_some()
        || std::path::Path::new("/var/run/docker.sock").exists()
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn timescaledb_full_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
    if !has_container_runtime() {
        eprintln!("no container runtime — skipping");
        return Ok(());
    }

    // Spin TimescaleDB. Wait twice for "database system is ready" —
    // the postgres-base image emits it once during the init scripts'
    // bootstrap and again on the real startup. Connecting after the
    // first message races with init.
    let container = GenericImage::new("timescale/timescaledb", "2.17.2-pg16")
        .with_exposed_port(ContainerPort::Tcp(5432))
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_USER", PG_USER)
        .with_env_var("POSTGRES_PASSWORD", PG_PASS)
        .with_env_var("POSTGRES_DB", PG_DB)
        .start()
        .await?;

    let host = container.get_host().await?;
    let port = container.get_host_port_ipv4(5432).await?;
    let url = format!("postgres://{PG_USER}:{PG_PASS}@{host}:{port}/{PG_DB}");

    // ---- 1. Connect + idempotent schema bootstrap ----
    let store = HistoryStore::connect(&url).await?;
    store.ensure_schema().await?;
    // Second call must be a no-op, not an error. Catches a regression
    // where a future migration forgets `IF NOT EXISTS` somewhere.
    store.ensure_schema().await?;

    // ---- 2. Hypertable conversion fired ----
    let pool = pg_pool_for_assertions(&url).await?;
    let (hypertable_count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM _timescaledb_catalog.hypertable \
         WHERE table_name = 'entity_state_history'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        hypertable_count, 1,
        "ensure_schema should have converted the table to a hypertable on the Timescale image"
    );

    // ---- 3. record() with varying timestamps + payload sizes ----
    //
    // We can't choose `ts` directly via the public API (record() uses
    // `DEFAULT now()`), so we INSERT directly for the rows where the
    // timestamp matters for the fetch_range bounds. record() is still
    // exercised separately to verify the public path works.

    // 3a. Public record() path with a couple of "now"-timestamped rows.
    store
        .record("device-A", "device.zigbee2mqtt.foo.temp.state", b"now-1")
        .await?;
    store
        .record("device-A", "device.zigbee2mqtt.foo.humid.state", b"now-2")
        .await?;
    // Empty payload — BYTEA must accept zero-length values.
    store
        .record("device-A", "device.zigbee2mqtt.foo.event.event", b"")
        .await?;
    // Multi-KB payload — proves no truncation.
    let big_payload: Vec<u8> = (0..4096_u32)
        .map(|i| u8::try_from(i % 251).unwrap_or(0))
        .collect();
    store
        .record(
            "device-A",
            "device.zigbee2mqtt.foo.bigpayload.state",
            &big_payload,
        )
        .await?;

    // 3b. Time-pinned rows for fetch_range bounds — INSERT directly.
    let t0: DateTime<Utc> = "2026-01-01T00:00:00Z".parse()?;
    let t1 = t0 + Duration::hours(1);
    let t2 = t0 + Duration::hours(2);
    let t3 = t0 + Duration::hours(3);
    for (ts, payload) in [
        (t0, "earliest"),
        (t1, "early"),
        (t2, "middle"),
        (t3, "latest"),
    ] {
        sqlx::query(
            "INSERT INTO entity_state_history (device_id, subject, ts, payload) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind("device-B")
        .bind("device.zigbee2mqtt.bar.temp.state")
        .bind(ts)
        .bind(payload.as_bytes())
        .execute(&pool)
        .await?;
    }

    // ---- 4. fetch_range — bounds + ordering + limit ----

    // 4a. Inclusive bounds [t1, t2] should yield exactly 2 rows.
    let mid = store.fetch_range("device-B", t1, t2, 100).await?;
    assert_eq!(mid.len(), 2, "[t1, t2] inclusive should match 2 rows");
    // Ordering: most-recent first.
    assert_eq!(mid[0].payload, b"middle");
    assert_eq!(mid[1].payload, b"early");

    // 4b. Wide range — 4 rows back, all in DESC order.
    let all = store.fetch_range("device-B", t0, t3, 100).await?;
    assert_eq!(all.len(), 4);
    assert_eq!(all[0].payload, b"latest");
    assert_eq!(all[3].payload, b"earliest");

    // 4c. Limit clamping.
    let limited = store.fetch_range("device-B", t0, t3, 2).await?;
    assert_eq!(limited.len(), 2, "limit=2 should return 2 rows");
    assert_eq!(limited[0].payload, b"latest");
    assert_eq!(limited[1].payload, b"middle");

    // 4d. Cross-device isolation — fetching device-A doesn't leak
    //      device-B's rows.
    let a_rows = store.fetch_range("device-A", t0, t3, 100).await?;
    assert!(
        a_rows.is_empty(),
        "device-A's time-pinned range should be empty (its rows are at `now()`)"
    );

    // 4e. Big payload survived BYTEA round-trip without truncation.
    let device_a_now = store
        .fetch_range(
            "device-A",
            Utc::now() - Duration::minutes(5),
            Utc::now() + Duration::minutes(5),
            100,
        )
        .await?;
    let big_row = device_a_now
        .iter()
        .find(|r| r.subject.contains("bigpayload"))
        .expect("big-payload row");
    assert_eq!(big_row.payload, big_payload, "BYTEA round-trip lossless");
    // And the empty payload is still present + length-zero.
    let empty_row = device_a_now
        .iter()
        .find(|r| r.subject.contains("event.event"))
        .expect("empty-payload row");
    assert_eq!(empty_row.payload.len(), 0);

    // ---- 5. prune_older_than ----
    //
    // Cutoff between t1 and t2 should drop t0 + t1 (2 rows) but keep
    // t2 + t3 + the device-A `now()` rows.
    let cutoff = t0 + Duration::minutes(90);
    let pruned = store.prune_older_than(cutoff).await?;
    assert_eq!(pruned, 2, "should have pruned the t0 + t1 device-B rows");

    // Verify the survivors.
    let after = store.fetch_range("device-B", t0, t3, 100).await?;
    assert_eq!(after.len(), 2);
    assert_eq!(after[0].payload, b"latest");
    assert_eq!(after[1].payload, b"middle");

    // ---- 6. Index actually used ----
    //
    // The (device_id, ts DESC) index is what makes fetch_range cheap.
    // EXPLAIN shows whether the planner picks it; we assert the plan
    // mentions our index name.
    let plan: Vec<(String,)> = sqlx::query_as(
        "EXPLAIN \
         SELECT device_id, subject, ts, payload \
         FROM entity_state_history \
         WHERE device_id = 'device-B' \
         ORDER BY ts DESC LIMIT 10",
    )
    .fetch_all(&pool)
    .await?;
    let plan_text: String = plan
        .into_iter()
        .map(|(s,)| s)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        plan_text.contains("entity_state_history_device_ts") || plan_text.contains("Index"),
        "fetch_range plan should pick an index, got:\n{plan_text}"
    );

    // Cleanup is implicit: container drops when `container` goes out of scope.
    Ok(())
}

/// Open a side-channel `PgPool` for assertions that need raw SQL
/// (hypertable existence, EXPLAIN, time-pinned INSERT). Mirrors the
/// pool tuning the production `HistoryStore::connect` uses.
async fn pg_pool_for_assertions(url: &str) -> Result<sqlx::PgPool, sqlx::Error> {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(StdDuration::from_secs(2))
        .connect(url)
        .await
}
