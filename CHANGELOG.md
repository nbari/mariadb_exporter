# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0] - 2025-12-15

### Fixed
- **Scraper**: Fixed a bug where scrape metrics were recorded twice (once explicitly and once via `Drop`), causing inaccurate scrape counts and duration metrics.
- **Linting**: Resolved various `clippy` warnings including long numeric literals, documentation formatting, and potential panics in test code.

### Changed
- **Refactor**: Centralized MariaDB version parsing logic into `src/collectors/util.rs` to eliminate code duplication between the exporter startup and the `version` collector.
- **Refactor**: Updated `VersionCollector` to use the new shared `normalize_mariadb_version` utility.
- **Performance**: Optimized regex compilation for version parsing using `OnceCell`.

### Added
- **Tests**: Added regression test `test_double_recording_bug` to ensure scrape metrics are recorded exactly once.
- **Tests**: Added comprehensive unit tests for `parse_mariadb_version` and `normalize_mariadb_version` covering various version string formats.
