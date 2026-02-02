# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
