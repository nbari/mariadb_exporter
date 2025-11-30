[![Test & Build](https://github.com/nbari/mariadb_exporter/actions/workflows/build.yml/badge.svg)](https://github.com/nbari/mariadb_exporter/actions/workflows/build.yml)
[![codecov](https://codecov.io/gh/nbari/mariadb_exporter/graph/badge.svg?token=LR19CK9679)](https://codecov.io/gh/nbari/mariadb_exporter)
[![Crates.io](https://img.shields.io/crates/v/mariadb_exporter.svg)](https://crates.io/crates/mariadb_exporter)
[![License](https://img.shields.io/crates/l/mariadb_exporter.svg)](LICENSE)

# mariadb_exporter

MariaDB metrics exporter for Prometheus written in Rust.

## Features

* **Modular collectors** – Enable only what you need; heavy/optional collectors stay off by default.
* **Compatibility** – Metric names align with Prometheus `mysqld_exporter` (prefixed `mariadb_`).
* **Lean defaults** – Essential availability, InnoDB, and replication metrics enabled by default; optional collectors opt-in.
* **Low footprint** – Designed to minimize cardinality and avoid expensive scans.

## Download or build

Install via Cargo:

```bash
cargo install mariadb_exporter
```

## Usage

### Recommended Setup (Secure)

**Best practice: Use Unix socket with dedicated user**

Create the exporter user with minimal privileges:

```sql
-- Create user for local socket connection only with connection limit
CREATE USER 'exporter'@'localhost' IDENTIFIED BY '' WITH MAX_USER_CONNECTIONS 3;

-- Grant minimal required permissions for all collectors
GRANT SELECT, PROCESS, REPLICATION CLIENT ON *.* TO 'exporter'@'localhost';

FLUSH PRIVILEGES;
```

Run the exporter:

```bash
mariadb_exporter --dsn "mysql:///mysql?socket=/var/run/mysqld/mysqld.sock&user=exporter"
```

**Why this is secure:**
- ✅ No password needed (socket authentication)
- ✅ User restricted to `localhost` only (no network access)
- ✅ Minimal privileges (read-only + monitoring)
- ✅ Connection limit prevents resource exhaustion
- ✅ No exposure to network attacks

### Alternative: TCP Connection

For remote monitoring or testing:

```bash
mariadb_exporter --dsn "mysql://exporter:password@host:3306/mysql"
```

Create user for network access:

```sql
CREATE USER 'exporter'@'%' IDENTIFIED BY 'strong_password_here' WITH MAX_USER_CONNECTIONS 3;
GRANT SELECT, PROCESS, REPLICATION CLIENT ON *.* TO 'exporter'@'%';
FLUSH PRIVILEGES;
```

### Common DSN Formats

* **Unix socket (recommended)**: `mysql:///mysql?socket=/var/run/mysqld/mysqld.sock&user=exporter`
* TCP: `mysql://user:password@host:3306/database`
* TLS required: `mysql://user:password@host/mysql?ssl-mode=REQUIRED`
* TLS verify identity: `mysql://user:password@host/mysql?ssl-mode=VERIFY_IDENTITY&ssl-ca=/path/to/ca.pem`

### Change Port

Default port is `9306`:

```bash
mariadb_exporter --dsn "..." --port 9187
```

## Available collectors

Collectors are toggled with `--collector.<name>` or `--no-collector.<name>`.

* `--collector.default` (enabled) – Core status (uptime, threads, connections, traffic), InnoDB basics, replication basics, binlog stats, config flags, version, `mariadb_up`, audit log enabled status.
* `--collector.exporter` (enabled) – Exporter self-metrics (process, scrape, cardinality).
* `--collector.tls` – TLS session + cipher info.
* `--collector.query_response_time` – Buckets from `query_response_time` plugin.
* `--collector.statements` – Statement digest summaries/top latency from `performance_schema`.
* `--collector.schema` – Table size/row estimates (largest 20 non-system tables).
* `--collector.replication` – Relay log size/pos, binlog file count.
* `--collector.locks` – Metadata/table lock waits from `performance_schema`.
* `--collector.metadata` – `metadata_lock_info` table counts.
* `--collector.userstat` – Per-user stats (requires `@@userstat=1` and `USER_STATISTICS`).

### Enabled by default

* `default`
* `exporter`

Everything else is opt-in.

## Project layout

```
mariadb_exporter
├── bin
├── cli
├── collectors
│   ├── config.rs
│   ├── default
│   ├── exporter
│   ├── locks
│   ├── metadata
│   ├── mod.rs
│   ├── query_response_time
│   ├── register_macro.rs
│   ├── registry.rs
│   ├── replication
│   ├── schema
│   ├── statements
│   ├── tls
│   ├── userstat
│   └── util.rs
└── src/lib.rs
```

Each collector lives in its own subdirectory for clarity and easy extension.

## Testing

Run tests:

```bash
cargo test
```

Run with container-backed integration (requires podman):

```bash
just test
```

Test with Unix socket connection (production-like setup):

```bash
# Test with combined MariaDB + exporter container (most realistic)
just test-socket
```

Lint:

```bash
cargo clippy --all-targets --all-features
```

### Socket Connection Testing

For detailed information on testing with Unix socket connections, see [TESTING_SOCKET.md](TESTING_SOCKET.md).

Quick start:

```bash
# Test with combined MariaDB + exporter container (most realistic)
just test-socket
```

## Developer Guidelines

### Architecture

The project follows a modular collector architecture:

```
mariadb_exporter/
├── bin/                 # Binary entry point
├── cli/                 # CLI argument parsing
├── collectors/          # All metric collectors
│   ├── mod.rs          # Collector trait and registration
│   ├── registry.rs     # Collector orchestration
│   ├── config.rs       # Collector enable/disable logic
│   └── */              # Individual collector modules
└── exporter/           # HTTP server (Axum)
```

### Adding a New Collector

1. Create a subdirectory under `src/collectors/` with a `mod.rs`
2. Define a struct implementing the `Collector` trait:
   - `register_metrics(&self, registry: &Registry)` - Register Prometheus metrics
   - `collect(&self, pool: &MySqlPool)` - Fetch data and update metrics (async)
   - `enabled_by_default(&self)` - Whether collector runs by default
3. Add ONE line to `register_collectors!` macro in `src/collectors/mod.rs`:
   ```rust
   register_collectors! {
       // ... existing collectors ...
       your_collector => YourCollector,
   }
   ```

The macro automatically generates all registration boilerplate.

### Strict Linting Rules

This project enforces strict clippy lints (see `Cargo.toml`):

- **DENY**: `unwrap_used`, `expect_used`, `panic`, `indexing_slicing`, `await_holding_lock`
- Use `?` for error propagation, never `.unwrap()` or `.expect()`
- Use `.get()` instead of `[index]` for slicing
- Use pattern matching or `.ok()` instead of `.unwrap()`

Exceptions are allowed only in test code with `#[allow(clippy::unwrap_used)]`.

### Testing

```bash
# Run unit tests
cargo test

# Run with container integration
just test

# Lint (must pass)
cargo clippy --all-targets --all-features

# Validate Grafana dashboard
just validate-dashboard
```

### Dashboard Development

When adding metrics to the Grafana dashboard:

1. Ensure metrics are exported by collectors
2. Add panels following existing structure (clean, professional, no emojis)
3. Use template variables (`$job`, `$instance`)
4. Add clear descriptions (Goal/Action format)
5. Validate before committing: `just validate-dashboard`

See [grafana/README.md](grafana/README.md) for detailed dashboard documentation.

### Commit Guidelines

- Run tests before committing: `cargo test`
- Run clippy: `cargo clippy --all-targets --all-features`
- Validate dashboard if modified: `just validate-dashboard`
- Keep commit messages clear and descriptive

## Notes

* User statistics: enable with `SET GLOBAL userstat=ON;` (or `@@userstat=1`) to expose `userstat` metrics.
* Metadata locks: load `metadata_lock_info` plugin for the `metadata` collector.
* Performance schema is needed for statements/locks collectors to return data.
* Optional collectors skip gracefully when prerequisites aren't present.
