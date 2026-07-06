# GitHub Copilot Instructions for mariadb_exporter

## Project Overview

This is a MariaDB metrics exporter for Prometheus, written in Rust. It collects
metrics from MariaDB and exposes them in Prometheus format. Metric names align with
the community `mysqld_exporter` (prefixed `mariadb_`). Safety and reliability are
paramount: all code must handle edge cases gracefully without panics, and `/metrics`
must always return HTTP 200 even when MariaDB is unreachable.

## Language & Tooling

- **Language**: Rust (latest stable), edition 2021
- **Formatting**: `cargo fmt --all -- --check`
- **Linting**: `cargo clippy --all-targets --all-features`
- **Testing**: `just test` (runs clippy, fmt, ensures MariaDB is up, then all tests)
- **Build Tool**: `just` command runner (see `.justfile`)
- **Local DB**: `just mariadb` (mariadb:11.4 on 127.0.0.1:3306, root password `root`)

## Lint Contract (merge blocker)

`Cargo.toml` `[lints]` denies Rust `warnings` and broad Clippy groups (`all`,
`pedantic`, `correctness`, `suspicious`, `perf`, `complexity`) plus the
safety-critical lints `unwrap_used`, `expect_used`, `panic`, `indexing_slicing`, and
`await_holding_lock`. Treat these as hard requirements:

- `unwrap()` / `expect()` are not acceptable in normal code — use `?` and fallbacks.
- Never introduce `panic!()` on a production path.
- Avoid direct indexing / unchecked slicing that can panic; prefer `.get()` +
  pattern matching.
- Do **not** silence a lint with `#[allow(clippy::...)]` as a default fix — fix the
  code. (Narrowly-scoped allowances in tests are acceptable when justified.)

## Critical Safety Rules

### 1. No panics — degrade gracefully

```rust
// ❌ WRONG - panics on error/NULL
let v: i64 = row.get("value");
let x = list[0];

// ✅ CORRECT - handle fallibly
let v: i64 = row.try_get("value").unwrap_or(0);
let x = list.first().copied().unwrap_or_default();
```

### 2. Always guard division by zero

```rust
// ✅ CORRECT
let ratio = if total > 0 { hits as f64 / total as f64 } else { 0.0 };
```

### 3. Fail closed on missing features

MariaDB features vary by version/config (plugins like `query_response_time`,
`userstat`, tables in `performance_schema`, or privileges). A missing feature must
degrade gracefully — skip the metric, log at `debug!`/`warn!`, and continue. Never
let it fail the whole scrape.

```rust
let rows = sqlx::query("SELECT ... FROM information_schema.some_table")
    .fetch_all(pool)
    .await;
match rows {
    Ok(rows) => { /* populate */ }
    Err(e) => { warn!("feature unavailable, skipping: {e}"); }
}
```

### 4. Keep `mariadb_up` honest

`/metrics` must always serve HTTP 200. When MariaDB is unreachable, `mariadb_up`
becomes `0` and DB-dependent metrics are omitted (no stale data), without crashing
the exporter.

## Single-Pool Connection Model

MariaDB exposes **all schemas from one connection** via `information_schema` /
`performance_schema`. This exporter therefore uses a **single shared `MySqlPool`**,
created at startup and passed to every collector's `collect(&pool)`.

- There is **no per-database/per-schema connection fan-out** (unlike PostgreSQL) and
  no per-database connection accumulation in normal operation.
- Per-schema/per-table collectors (e.g. `schema`) query `information_schema` on the
  shared pool.
- For the rare case where a collector must run a query **in the context of another
  database**, use the ephemeral helper `util::open_db_connection(datname)` — it opens
  a bare connection closed on drop, never cached.
- **Do not add a per-database/per-schema connection or pool cache** — it pins one
  persistent connection per database and can exhaust `max_connections`. The invariant
  is locked by `tests/collectors/connection.rs`.

## Code Style

### Comments
- Only comment code that needs clarification; code should be self-documenting.
- Use doc comments (`///`) for public APIs.

### Error Handling
- Use `anyhow::Result` for application errors; prefer explicit error types over
  `Box<dyn Error>`.
- Log non-critical issues with `warn!()`; verbose operational detail with `debug!()`.
- Never panic in production code.

### Async & Tracing
```rust
use tracing::{debug, warn, instrument};

#[instrument(skip(self, pool))]
async fn collect(&self, pool: &MySqlPool) -> Result<()> {
    debug!("collecting");
    Ok(())
}
```

### Metrics Registration
Collectors implement the `Collector` trait — register metrics in `register()` and
populate them in `collect()`. Keep one collector per folder with its own `mod.rs`
and a registration entry in `collectors/mod.rs`.

## Testing Requirements

Every collector **must** include:

1. **Registration Test** — metrics register without errors
2. **Collection Test** — metrics populate against a real MariaDB
3. **Feature Availability Test** — handle a missing plugin/table/privilege gracefully
4. **Edge Case Test** — NULL values, empty result sets, zero values
5. **Type Compatibility Test** — verify SQL → Rust conversions

Comprehensive collector tests live in `tests/collectors/`, mirroring
`src/collectors/`. Tests use `MARIADB_EXPORTER_DSN` (default
`mysql://root@127.0.0.1:3306/mysql`). See `tests/TESTING.md`.

## Pre-Commit Workflow

```bash
# 1. Start MariaDB
just mariadb

# 2. Seed / verify the test database
./scripts/setup-local-test-db.sh

# 3. Run all checks (clippy, fmt, tests)
just test

# 4. Commit (optional pre-commit hook checks DB reachability + fmt)
git commit
```

Install the pre-commit hook:

```bash
cp scripts/pre-commit-hook.sh .git/hooks/pre-commit
chmod +x .git/hooks/pre-commit
```

## Git Commit Signing

The release flow creates **signed tags** (`git tag -s`). Respect the repository's and
the user's signing configuration:

- Do **not** bypass signing with `git commit --no-gpg-sign`.
- If SSH signing is configured (`gpg.format ssh`, `user.signingkey`), let Git sign
  automatically.

## Dev Container & Dashboards

- A compose-based dev container (`.devcontainer/`) provides Rust + MariaDB and an
  optional Prometheus + Grafana profile. Enter with `scripts/dev-up`, then
  `just test` inside.
- `grafana/dashboard.json` is validated against live exporter output via
  `just validate-dashboard`. Bring up Prometheus + Grafana on demand with
  `just metrics-dev`.
- A self-contained soak harness (`scripts/benchmark/`) drives load with
  `scripts/mariadb_loadtest.py` and samples the exporter's `/metrics` for leak checks.

---

**Remember**: every production panic is a test we didn't write.
