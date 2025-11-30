# Collector Integration Tests

This directory contains integration tests for all MariaDB collectors. These tests require a live MariaDB connection.

## Prerequisites

Tests expect MariaDB to be available at the DSN specified by the `MARIADB_EXPORTER_DSN` environment variable, defaulting to `mysql://root:root@127.0.0.1:3306/mysql`.

## Running Tests

### Run all collector tests

```bash
cargo test --test collectors_tests -- --nocapture
```

### Run tests for specific collectors

**Default collectors (version, global_status, global_variables, innodb_metrics)**

```bash
cargo test --test collectors_tests default
```

**Statements collector**

```bash
cargo test --test collectors_tests statements
```

**Replication collector**

```bash
cargo test --test collectors_tests replication
```

**TLS collector**

```bash
cargo test --test collectors_tests tls
```

**Userstat collector**

```bash
cargo test --test collectors_tests userstat
```

**Locks collector**

```bash
cargo test --test collectors_tests locks
```

**Metadata collector**

```bash
cargo test --test collectors_tests metadata
```

**Schema collector**

```bash
cargo test --test collectors_tests schema
```

### Run tests for a specific sub-collector

```bash
cargo test --test collectors_tests default::version
cargo test --test collectors_tests default::global_status
cargo test --test collectors_tests default::innodb_metrics
cargo test --test collectors_tests statements::perf_schema
```

### Run a specific test

```bash
cargo test --test collectors_tests test_version_collector_queries_database
cargo test --test collectors_tests test_global_status_collector_collects_metrics
```

## Test Structure

Each collector has its own directory with test modules:

```
collectors/
├── common.rs              # Re-exports common test utilities
├── default/               # Default collector tests
│   ├── mod.rs
│   ├── version.rs
│   ├── global_status.rs
│   ├── global_variables.rs
│   └── innodb_metrics.rs
├── statements/            # Statements collector tests
│   ├── mod.rs
│   └── perf_schema.rs
├── replication/           # Replication collector tests
│   ├── mod.rs
│   └── replica_status.rs
└── ...                    # Other collectors
```

## Test Categories

Each collector test module includes:

1. **Registration Test** - Ensures metrics register without errors
2. **Collection Test** - Verifies metrics are collected from database
3. **Feature Availability Test** - Gracefully handles missing features/plugins
4. **Edge Case Test** - Tests NULL values, empty results, etc.
5. **Realistic Workload Test** - Tests with actual data

## Using justfile

```bash
# Start MariaDB and run all tests
just test

# Stop test containers
just stop-containers
```
