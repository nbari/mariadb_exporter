# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.6.0] - 2026-02-23

### Fixed
- **InnoDB**: Correctly sum all "OS waits" in `mariadb_innodb_semaphore_waits_total` instead of only reporting the last occurrence.
- **Replication**: Report `-1` for `mariadb_slave_status_seconds_behind_master` when the value is `NULL` (replication stopped/broken) to avoid false sync signals.
- **CLI**: Fixed `test_handle_action_signature` to properly test invalid DSN formats without hanging.
- **Correctness**: Added `.reset()` calls to multiple collectors (`Tables`, `UserStat`, `Metadata`, `Statements`, `TLS`, `Version`) to prevent stale labels when entities are dropped.
- **Robustness**: Skip setting metrics if queries fail (e.g. `Performance Schema` missing) rather than reporting misleading zero values.

### Changed
- **Resilience**: The exporter now uses lazy database connections and a zero-minimum pool, allowing it to start even when MariaDB is unreachable.
- **Resilience**: The `/metrics` endpoint now always returns `HTTP 200`. During MariaDB outages, it serves a best-effort response with `mariadb_up 0` and omits DB-dependent metrics.
- **Resilience**: MariaDB version detection is now deferred if it fails at startup, retrying during the first scrape.

### Added
- **InnoDB**: New `mariadb_innodb_semaphore_wait_time_ms_total` metric parsing individual thread wait times from `SHOW ENGINE INNODB STATUS`.
- **Tests**: New end-to-end integration test `tests/connectivity_failure.rs` for database outage scenarios.
- **Tests**: Comprehensive unit tests for `CollectorRegistry` in `src/collectors/registry.rs`.
- **Tests**: Regression tests for InnoDB semaphore parsing and metrics resetting.

## [0.5.1] - 2026-02-02

### Fixed
- **Replication**: Correctly decode unsigned `Master_Server_Id` from `SHOW SLAVE STATUS` to avoid false zeros.
- **Tests**: Align `mariadb_exporter_metrics_total` smoke check with the previous scrape count to prevent off-by-one failures.
- **Version**: Clear stale `mariadb_version_info` labels after upgrade to prevent duplicate version series.

### Added
- **Replication**: New `mariadb_replica_configured` gauge to indicate replication configuration even when threads are down.
- **Tests**: Container-based replication integration test that validates `mariadb_replica_master_server_id` against a live master/replica pair.

## [0.5.0] - 2025-12-15

### Fixed
- **Scraper**: Implemented missing `Drop` trait for `ScrapeTimer` to ensure metrics are recorded on scope exit (RAII), and added safeguards to prevent double-recording.
- **Linting**: Resolved various `clippy` warnings including long numeric literals, documentation formatting, and potential panics in test code.

### Changed
- **Refactor**: Centralized MariaDB version parsing logic into `src/collectors/util.rs` to eliminate code duplication between the exporter startup and the `version` collector.
- **Refactor**: Updated `VersionCollector` to use the new shared `normalize_mariadb_version` utility.
- **Performance**: Optimized regex compilation for version parsing using `OnceCell`.

### Added
- **Tests**: Added regression test `test_double_recording_bug` to ensure scrape metrics are recorded exactly once.
- **Tests**: Added comprehensive unit tests for `parse_mariadb_version` and `normalize_mariadb_version` covering various version string formats.
