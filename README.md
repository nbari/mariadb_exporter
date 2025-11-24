[![Test & Build](https://github.com/nbari/mariadb_exporter/actions/workflows/build.yml/badge.svg)](https://github.com/nbari/mariadb_exporter/actions/workflows/build.yml)
[![codecov](https://codecov.io/gh/nbari/mariadb_exporter/graph/badge.svg?token=LR19CK9679)](https://codecov.io/gh/nbari/mariadb_exporter)
[![Crates.io](https://img.shields.io/crates/v/mariadb_exporter.svg)](https://crates.io/crates/mariadb_exporter)
[![License](https://img.shields.io/crates/l/mariadb_exporter.svg)](LICENSE)

# mariadb_exporter

MariaDB metrics exporter for Prometheus written in Rust.

## Goals

`mariadb_exporter` focuses on DBRE-friendly, selective metrics:

* **Modular collectors** – Enable only what you need; heavy/optional plugins stay off by default.
* **Compatibility** – Metric names align with Prometheus `mysqld_exporter` (prefixed `mariadb_`).
* **Lean defaults** – Useful availability/innodb/replication basics on by default; optional collectors gated.
* **Low footprint** – Avoid unnecessary cardinality and expensive scans.

## Download or build

Install via Cargo:

```bash
cargo install mariadb_exporter
```

## Usage

Run the exporter:

```bash
mariadb_exporter --dsn "mysql://user:password@localhost:3306/mysql"
```

Change port (default `9306`):

```bash
mariadb_exporter --dsn "mysql://user:password@localhost:3306/mysql" --port 9187
```

Common DSN formats:

* TCP: `mysql://user:password@host:3306/database`
* Unix socket: `mysql:///mysql?socket=/var/run/mysqld/mysqld.sock&user=exporter`
* TLS required: `mysql://user@host/mysql?ssl-mode=REQUIRED`
* TLS verify identity: `mysql://user@host/mysql?ssl-mode=VERIFY_IDENTITY&ssl-ca=/path/to/ca.pem`

## Available collectors

Collectors are toggled with `--collector.<name>` or `--no-collector.<name>`.

* `--collector.default` (enabled) – Core status (uptime, threads, connections, traffic), InnoDB basics, replication basics, binlog stats, config flags, version, `mariadb_up`.
* `--collector.exporter` (enabled) – Exporter self-metrics (process, scrape, cardinality).
* `--collector.tls` – TLS session + cipher info.
* `--collector.query_response_time` – Buckets from `query_response_time` plugin.
* `--collector.audit` – Audit plugin status.
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
│   ├── audit
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

Run with container-backed integration (requires podman/docker):

```bash
just test
```

Lint:

```bash
cargo clippy --all-targets --all-features
```

## Notes

* User statistics: enable with `SET GLOBAL userstat=ON;` (or `@@userstat=1`) to expose `userstat` metrics.
* Metadata locks: load `metadata_lock_info` plugin for the `metadata` collector.
* Performance schema is needed for statements/locks collectors to return data.
* Optional collectors skip gracefully when prerequisites aren’t present.***
