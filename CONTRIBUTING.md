# Development Guide

## Quick Start

```bash
# Start MariaDB (podman)
just mariadb

# Verify / seed the local test database
./scripts/setup-local-test-db.sh

# Run all tests (clippy + fmt + tests)
just test
```

> **Prefer a zero-setup environment?** The repo ships a compose-based
> [Dev Container](.devcontainer/README.md) (Rust + MariaDB, plus an optional
> Prometheus + Grafana profile). With [DevPod](https://devpod.sh): `scripts/dev-up`,
> then `just test` inside — works on Linux, macOS, and fedora-atomic with no host
> database. See [`.devcontainer/README.md`](.devcontainer/README.md).

---

## Local Setup

### Prerequisites

- MariaDB 10.6+ / 11.x (via podman/docker or locally)
- Rust toolchain (latest stable)
- `just` command runner (optional)
- MariaDB client (`mariadb` + `mariadb-admin`) for the helper scripts —
  `scripts/install-mariadb-client.sh` installs it on Debian/Ubuntu

### MariaDB

Start MariaDB in a container:

```bash
just mariadb              # mariadb:11.4 on 127.0.0.1:3306, root password "root"
```

Verify / seed the test database:

```bash
./scripts/setup-local-test-db.sh
```

This script waits for MariaDB, then creates a small `testdb` with sample tables so
the collectors (notably `schema`) have data to report on. It honors `MARIADB_HOST`,
`MARIADB_PORT`, `MARIADB_USER`, and `MARIADB_PASS`.

For a least-privilege exporter user (mirrors the README's production guidance) use
[`scripts/setup-exporter-user.sql`](scripts/setup-exporter-user.sql).

---

## Testing

### Run Tests

```bash
# 'just test' runs clippy + fmt, ensures MariaDB is reachable, then runs the suite.
just test

# Or set the DSN manually for a specific test:
MARIADB_EXPORTER_DSN="mysql://root:root@127.0.0.1:3306/mysql" \
  cargo test --test collectors_tests schema
```

`just test` detects a reachable MariaDB at `MARIADB_HOST:MARIADB_PORT` (default
`127.0.0.1:3306`) and only starts a container when one is not already running. It
honors a pre-set `MARIADB_EXPORTER_DSN` (this is what lets the devcontainer point at
the `mariadb` service), falling back to the local default when unset.

See [tests/TESTING.md](tests/TESTING.md) for detailed patterns and examples.

### Required Tests for New Collectors

Every collector **must** include these test categories:

1. **Registration Test** — metrics register without errors
2. **Collection Test** — metrics populate against a real MariaDB
3. **Feature Availability Test** — handle a missing plugin/table/privilege gracefully
4. **Edge Case Test** — NULL values, empty result sets, zero values
5. **Type Compatibility Test** — verify SQL → Rust conversions

Comprehensive collector tests live in `tests/collectors/`, mirroring
`src/collectors/`.

---

## Safe Coding Patterns

The crate's Clippy policy denies panics and unchecked access (`unwrap_used`,
`expect_used`, `panic`, `indexing_slicing`, `await_holding_lock`). Prefer `?`,
`.get()`/pattern matching, and explicit error types over `Box<dyn Error>`.

### Key Rules

1. **No panics in production code** — prefer `?` and graceful fallbacks.
2. **Guard every division against zero** — both in SQL and Rust.
3. **Fail closed on missing features** — a missing plugin, table, or privilege must
   degrade gracefully (skip/emit nothing), never crash the scrape.
4. **Keep `mariadb_up` honest** — `/metrics` must always return 200; during an
   outage `mariadb_up` becomes `0` and DB-dependent metrics are omitted.

### Single-Pool Connection Model

Unlike PostgreSQL (where each database has its own catalog and needs a separate
connection), MariaDB exposes **all schemas from one connection** via
`information_schema` / `performance_schema`. So this exporter uses a **single shared
`MySqlPool`**, created once at startup and passed to every collector's
`collect(&pool)`.

- There is **no per-database/per-schema connection fan-out** and therefore no
  per-database connection accumulation in normal operation.
- Collectors that report per-schema/per-table data (e.g. `schema`) do so with
  ordinary queries against `information_schema` on the shared pool.
- For the rare case where a collector must run a query **in the context of another
  database**, use the ephemeral helper `util::open_db_connection(datname)` — it opens
  a bare connection that is **closed on drop** and is never cached.
- **Do not introduce a per-database/per-schema connection or pool cache.** Caching
  pins one persistent connection per database and can exhaust `max_connections` on
  large or connection-constrained servers. This invariant is locked by
  `tests/collectors/connection.rs` (fresh server thread per call + closed on drop);
  keep it green.

---

## Git Hooks

### Installing Pre-Commit Hook

```bash
cp scripts/pre-commit-hook.sh .git/hooks/pre-commit
chmod +x .git/hooks/pre-commit
```

The hook:

- Checks that MariaDB is reachable when collector code changes (so tests can run).
- Enforces formatting (`cargo fmt --all -- --check`) on staged Rust changes.

Clippy and the full suite run via `just test`.

---

## Pre-Commit Checklist

Before committing code:

- [ ] MariaDB is running (`just mariadb`)
- [ ] `./scripts/setup-local-test-db.sh` passes
- [ ] `cargo fmt --all -- --check` is clean
- [ ] `cargo clippy --all-targets --all-features` is clean
- [ ] `just test` passes
- [ ] New collectors have all required test categories
- [ ] Edge cases tested (NULL, zero, empty results)
- [ ] `just validate-dashboard` passes if dashboards changed

---

## Debugging

### MariaDB Not Running

```bash
just mariadb
mariadb-admin ping -h 127.0.0.1 -P 3306 -uroot -proot --silent
```

### Inspecting the Exporter

```bash
# Run with a broad collector set and hot reload on change
just watch

# Live resource snapshot of a running exporter
./scripts/monitor-exporter.sh -d 60
```

### Soak / Stability

A self-contained local soak harness drives sustained load with
`scripts/mariadb_loadtest.py` and samples the exporter's own `/metrics` (RSS, open
FDs, scrape counters) to catch leaks:

```bash
scripts/benchmark/run-soak.sh --hours 1
scripts/benchmark/check-soak.sh            # analyze the latest run
```

See [scripts/benchmark/README.md](scripts/benchmark/README.md).

---

## Dashboards

`grafana/dashboard.json` is validated against the live exporter output:

```bash
just validate-dashboard
```

The devcontainer can run Prometheus + Grafana on demand (`just metrics-dev`) with the
dashboard hot-reloaded from disk — see [`.devcontainer/README.md`](.devcontainer/README.md).

---

## CI/CD

GitHub Actions builds, lints, and runs the test suite against supported MariaDB
versions. If CI fails but local tests pass, ensure your local MariaDB is reachable
and seeded (`./scripts/setup-local-test-db.sh`).

---

## Quick Reference

```bash
# Daily workflow
just mariadb                           # Start MariaDB
./scripts/setup-local-test-db.sh       # Verify / seed
just test                              # clippy + fmt + tests
git commit                             # Pre-commit hook runs

# Install pre-commit hook
cp scripts/pre-commit-hook.sh .git/hooks/pre-commit
```

---

## Zero Tolerance for Panics

All code must handle missing plugins/tables/privileges, NULL values, type
mismatches, division by zero, and empty result sets — gracefully, never with a
panic.

**Remember**: every production panic is a test we didn't write.
