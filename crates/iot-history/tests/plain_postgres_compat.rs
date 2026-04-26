//! Integration test: plain-Postgres compat (no timescaledb extension).
//!
//! Spins `postgres:16-alpine` (no Timescale extension) and verifies
//! that [`HistoryStore::ensure_schema`]'s guarded
//! `create_hypertable(...)` block degrades gracefully. The test
//! exists because the production guard (a `DO $$ ... IF EXISTS
//! (SELECT 1 FROM pg_extension WHERE extname = 'timescaledb') THEN
//! ... END IF $$` block) is the kind of code that's easy to break in
//! a regression that nobody notices until someone tries to run on
//! plain Postgres in a degenerate setup or test environment.
//!
//! On plain Postgres:
//!
//! * `connect()` + `ensure_schema()` must succeed.
//! * The `entity_state_history` table exists as a regular table
//!   (no hypertable conversion).
//! * `record()` + `fetch_range()` + `prune_older_than()` work
//!   identically to the Timescale path — the storage contract
//!   doesn't depend on the extension being present.
//!
//! Run locally with `cargo test -p iot-history --test
//! plain_postgres_compat -- --nocapture`. Self-skips when no
//! container runtime is available.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use chrono::{Duration, Utc};
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

fn has_container_runtime() -> bool {
    std::env::var_os("DOCKER_HOST").is_some()
        || std::path::Path::new("/var/run/docker.sock").exists()
}

#[tokio::test]
async fn ensure_schema_no_ops_hypertable_on_plain_postgres(
) -> Result<(), Box<dyn std::error::Error>> {
    if !has_container_runtime() {
        eprintln!("no container runtime — skipping");
        return Ok(());
    }

    let container = GenericImage::new("postgres", "16-alpine")
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

    // ---- 1. ensure_schema must succeed despite no extension ----
    let store = HistoryStore::connect(&url).await?;
    store.ensure_schema().await?;

    // ---- 2. timescaledb extension is NOT installed ----
    //
    // Sanity: a regression that quietly enables the extension via a
    // forgotten `CREATE EXTENSION` would break this expectation.
    let pool = pg_pool_for_assertions(&url).await?;
    let (ext_count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM pg_extension WHERE extname = 'timescaledb'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        ext_count, 0,
        "plain postgres should NOT have the timescaledb extension"
    );

    // ---- 3. Table exists as a regular relation ----
    let (relkind,): (String,) =
        sqlx::query_as("SELECT relkind::text FROM pg_class WHERE relname = 'entity_state_history'")
            .fetch_one(&pool)
            .await?;
    // 'r' = ordinary table. The hypertable virtualisation would have
    // shown up as a different relkind / extra catalog entries.
    assert_eq!(relkind, "r", "expected ordinary table on plain Postgres");

    // ---- 4. The (device_id, ts DESC) index is still there ----
    //
    // Index creation is independent of the extension and must work
    // identically — that's the secondary index queries depend on.
    let (idx_count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM pg_indexes \
         WHERE tablename = 'entity_state_history' \
           AND indexname = 'entity_state_history_device_ts'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        idx_count, 1,
        "secondary index should exist on plain Postgres"
    );

    // ---- 5. record / fetch_range / prune all work ----
    //
    // The storage contract is "row-store + secondary index" — Timescale
    // is a transparent perf accelerator. Verify the public API works
    // identically here.
    let now = Utc::now();
    store
        .record("device-X", "device.test.foo.temp.state", b"first")
        .await?;
    store
        .record("device-X", "device.test.foo.temp.state", b"second")
        .await?;
    store
        .record("device-X", "device.test.foo.humid.state", b"third")
        .await?;

    let from = now - Duration::minutes(1);
    let to = now + Duration::minutes(1);
    let rows = store.fetch_range("device-X", from, to, 100).await?;
    assert_eq!(rows.len(), 3, "all three rows should be visible");
    // Most-recent-first ordering.
    assert_eq!(rows[0].payload, b"third");

    let pruned = store
        .prune_older_than(Utc::now() + Duration::minutes(1))
        .await?;
    assert_eq!(pruned, 3, "all rows older than now+1min should be pruned");
    let after = store.fetch_range("device-X", from, to, 100).await?;
    assert!(after.is_empty());

    Ok(())
}

async fn pg_pool_for_assertions(url: &str) -> Result<sqlx::PgPool, sqlx::Error> {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(StdDuration::from_secs(2))
        .connect(url)
        .await
}
