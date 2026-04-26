//! Plain-Postgres compatibility test for [`iot_history::HistoryStore`].
//!
//! Sister to `postgres_roundtrip.rs`, but spins vanilla `postgres:16`
//! instead of the Timescale image — a regression guard for the
//! "graceful no-op" claim in [`iot_history::HistoryStore::ensure_schema`]:
//!
//! > Wrapped in a do-block so non-Timescale Postgres simply ignores
//! > the missing function and keeps the plain table.
//!
//! What this test asserts:
//!
//! 1. [`HistoryStore::ensure_schema`] succeeds against vanilla Postgres
//!    (the `DO $$ ... pg_extension ... $$` guard takes the empty path
//!    and the rest of the migration is plain DDL).
//! 2. The table exists, **but** the `timescaledb` extension does NOT
//!    — proving the no-op branch was actually taken (rather than a
//!    silent error swallowed by the `let _ = ...` discard pattern).
//! 3. [`HistoryStore::record`] + [`HistoryStore::fetch_range`] work
//!    end-to-end — the storage layer is fully functional in the
//!    degenerate "no Timescale extension" deployment people might
//!    actually hit when using a managed Postgres without Timescale
//!    add-ons.
//!
//! Together with `postgres_roundtrip.rs` this nails down the contract
//! both ways: the schema bootstrap upgrades to a hypertable when
//! possible, and falls back to a plain table when not — no silent
//! failures in either direction.
//!
//! Run locally with:
//!
//!     cargo test -p iot-history --test plain_postgres_compat -- --nocapture
//!
//! Requires a Docker / Podman / Testcontainers-compatible runtime.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use chrono::{Duration as ChronoDuration, Utc};
use iot_history::HistoryStore;
use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;
use testcontainers::{
    core::{ContainerPort, WaitFor},
    runners::AsyncRunner,
    GenericImage, ImageExt,
};

const PG_USER: &str = "iot";
const PG_PASS: &str = "iot";
const PG_DB: &str = "iot_history";

fn container_runtime_available() -> bool {
    if std::env::var_os("DOCKER_HOST").is_some() {
        return true;
    }
    if std::path::Path::new("/var/run/docker.sock").exists() {
        return true;
    }
    #[cfg(windows)]
    if std::path::Path::new(r"\\.\pipe\docker_engine").exists() {
        return true;
    }
    false
}

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

/// See the corresponding helper in `postgres_roundtrip.rs` — same
/// reason (IPv6-first resolution of `localhost` on Windows hosts).
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

async fn sibling_pool(url: &str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(2))
        .connect(url)
        .await
        .expect("sibling pool")
}

#[tokio::test]
async fn plain_postgres_no_op_hypertable_path() {
    if !container_runtime_available() {
        eprintln!("no container runtime — skipping");
        return;
    }

    let container = GenericImage::new("postgres", "16")
        .with_exposed_port(ContainerPort::Tcp(5432))
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_USER", PG_USER)
        .with_env_var("POSTGRES_PASSWORD", PG_PASS)
        .with_env_var("POSTGRES_DB", PG_DB)
        .start()
        .await
        .expect("start vanilla postgres");

    let host = host_for_ipv4(&container).await;
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("mapped port");
    let url = format!("postgres://{PG_USER}:{PG_PASS}@{host}:{port}/{PG_DB}");

    let store = connect_with_retry(&url).await;

    // 1. Schema bootstrap — must succeed even without timescaledb.
    //    The DO-block's IF EXISTS check on pg_extension is the load-
    //    bearing guard here; if that branch silently errored we'd
    //    discard the error (it's behind `let _ = ...`) but the
    //    subsequent CREATE INDEX would still run.
    store
        .ensure_schema()
        .await
        .expect("ensure_schema must work without timescaledb");

    let probe = sibling_pool(&url).await;

    // 2. The timescaledb extension MUST NOT be installed — this is
    //    the whole point of the test. If it somehow were, the
    //    Timescale roundtrip test in the sister file is what actually
    //    exercises the hypertable path.
    let ext_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pg_extension WHERE extname = 'timescaledb'")
            .fetch_one(&probe)
            .await
            .expect("pg_extension probe");
    assert_eq!(
        ext_count, 0,
        "vanilla postgres:16 must not have timescaledb installed"
    );

    // 3. The table must exist as a regular (non-hypertable) table.
    //    Querying timescaledb_information.hypertables would error on
    //    vanilla pg (the schema doesn't exist), so we just confirm
    //    the table is in the public catalog.
    let tab_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_name = 'entity_state_history'",
    )
    .fetch_one(&probe)
    .await
    .expect("information_schema probe");
    assert_eq!(
        tab_count, 1,
        "entity_state_history must exist as a regular table on vanilla pg"
    );

    // 4. The secondary index must still be there — that DDL runs
    //    after the no-op DO-block and is the most likely casualty if
    //    a future refactor moves the index inside the timescale-only
    //    branch by mistake.
    let idx_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_indexes \
         WHERE schemaname = 'public' \
           AND tablename  = 'entity_state_history' \
           AND indexname  = 'entity_state_history_device_ts'",
    )
    .fetch_one(&probe)
    .await
    .expect("index probe");
    assert_eq!(
        idx_count, 1,
        "secondary (device_id, ts DESC) index must exist on vanilla pg too"
    );

    // 5. End-to-end record + fetch — the storage layer must be fully
    //    functional in the degenerate "no Timescale" deployment.
    let device = "vanilla-roundtrip";
    let payloads: [&[u8]; 3] = [b"a", b"bb", b"\x00\x01\x02\xff"];
    for (i, p) in payloads.iter().enumerate() {
        store
            .record(device, &format!("device.plain.{i}.state"), p)
            .await
            .expect("record");
    }

    let now = Utc::now();
    let rows = store
        .fetch_range(
            device,
            now - ChronoDuration::hours(1),
            now + ChronoDuration::hours(1),
            10,
        )
        .await
        .expect("fetch");
    assert_eq!(rows.len(), 3, "all 3 vanilla-pg inserts must round-trip");
    assert!(
        rows.iter().any(|r| r.payload == b"\x00\x01\x02\xff"),
        "non-UTF-8 BYTEA payload must round-trip on plain pg as well"
    );
}
