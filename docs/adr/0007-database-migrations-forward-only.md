# ADR-0007: Database Migrations — Forward-Only, sqlx

- **Status:** Accepted
- **Date:** 2026-04-20

## Context

Every Rust service that persists state (registry, audit, automation state, plugin state) uses SQLite for small deployments and Postgres for larger ones. Schema changes must be safe against users' production data. The team is small; nobody will reliably author correct `down.sql` migrations in real projects.

## Decision

### Tooling

- **`sqlx` with `sqlx migrate`** for all Rust services.
- Migrations live in `crates/<service>/migrations/`.
- File naming: `YYYYMMDDHHMMSS_short_description.sql` (timestamp-prefixed, Unix-style).
- `sqlx::query!` macros for compile-time-verified queries against an ambient dev database.

### Forward-only

- **Only `up.sql` is authored.** No `down.sql`.
- Rollback strategy is **snapshot + forward-fix**, not schema-reversal.
- If a migration must be undone, a *new* migration is written that expresses the inverse.

### Safety rules (enforced by PR review checklist)

| Change | Rule |
|---|---|
| Add column | Must be NULL-able OR have DEFAULT. |
| Drop column | Two-phase: (1) stop writing to it + deploy, (2) drop in a later migration. |
| Rename column | Two-phase: add new → backfill → stop writing old → deploy → drop old. |
| Change type | Same as rename. |
| Add NOT NULL constraint | Prerequisite: all rows must have a value; backfill first. |
| Add index on large table | Use CONCURRENTLY in Postgres; document maintenance window. |
| Change primary key | Treat as table rewrite; plan like a release. |

### Operational

- **Migrations run at service startup** by default. A `--migrate-only` flag runs migrations and exits (for blue-green deploys).
- Every service logs the migration chain it applied at startup, with checksums.
- `sqlx` records applied migrations in `_sqlx_migrations`; drift (checksum mismatch) halts startup.

### SQLite vs Postgres portability

- Schemas are authored in ANSI SQL where practical.
- SQLite-specific or Postgres-specific deviations go in **dialect-split files**: `YYYYMMDD_description.sqlite.sql` and `.postgres.sql`. The migration runner picks based on the active driver.
- Integration tests run both dialects.

## Consequences

- No accidental data loss from a bad `down.sql` (that failure mode is eliminated).
- Operators must take snapshots before production migrations. Documented in the runbook.
- Rewriting tables (type changes, PK changes) is more ceremony than with ORM autogen. That friction is intentional — it forces thinking about live traffic.
- Dev loop: developers reset their local SQLite freely (`just db-reset`); production schemas accrete.
