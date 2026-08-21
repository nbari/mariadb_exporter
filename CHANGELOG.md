# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.8.0] - 2026-08-21

### Changed
- **Graceful Collector Skips Now Clear Their Metrics** *(breaking for external `Collector` implementers; wire-compatible for scrapers)*: A collector that gracefully published nothing — because its source is known to be unavailable — used to leave its previous values in the registry, where they were served as current on every later scrape. Every skip path now settles: **when a collector publishes nothing because its source is unavailable, every series it owned disappears.**
  - **Trait contract**: `Collector` gained `#[must_use] enum Collected { Fresh, Skipped }`. The implementation hook is now `collect_once(&self, pool) -> BoxFuture<Result<Collected>>` plus a **required** `reset_metrics(&self)`, and `collect(&self, pool) -> BoxFuture<Result<()>>` became a provided safe wrapper that calls `reset_metrics()` exactly when `collect_once` returned `Skipped`. Callers are unaffected — `collect()` keeps its name and signature — but out-of-tree implementations of `Collector` must be updated.
  - **Outcome semantics**: `Fresh` = a current snapshot was published (a successful *empty* result is Fresh when zero rows is the current truth). `Skipped` = nothing was published because the source is unavailable; the previous snapshot is cleared. `Err` = an unexpected or transient fault; it propagates and **never** resets. A collector that refreshed part of its surface never returns `Skipped`, and independently optional sources settle independently — a skipped locks source cannot clear a fresh sibling, and unavailable binlog data cannot clear valid replica state.
  - **Central error classification**: new `collectors::util::{QueryFailure, classify_query_error}` keys on the MariaDB/MySQL **error number** (never on error-message text): absent/unsupported source (`1049`, `1054`, `1109`, `1146`, `1193`, `1235`, `1286`, `1381`) → `Skipped`; permission denied (`1044`, `1142`, `1143`, `1227`, `1370`) → `Skipped`, warned once per process instead of on every scrape; everything else — lost connections, timeouts, deadlocks, malformed data, failed feature probes — → `Err`.
  - **No more laundered failures**: `unwrap_or(0)` table probes, `vec![]` fallbacks on query errors, and swallowed `Err(e) => debug!(…)` paths were removed from `statements`, `userstat`, `metadata`, `query_response_time`, `locks`, `schema`, `tls`, `innodb`, and `replication`. A query failure is no longer indistinguishable from an absent feature.
  - **Reset ordering**: success-path resets that used to run *before* the fallible read now run only after the query succeeded, immediately before publishing, so an error can no longer destroy the last good snapshot.
  - **Skip-capable scalars became zero-label vectors**: gauges/counters owned by a skippable source were converted to the matching `*Vec` with an empty label set (set via `with_label_values(&[])`, removed via `reset()`). Metric name, help, type, labels and wire format are unchanged — a zero-label vector renders byte-for-byte like the scalar it replaced — but the series can now be removed. This covers all 108 `default` status/variable metrics plus `default/plugins`, `replication` (replica status and binlog), `innodb`, `statements`, `tls`, `locks`, and `query_response_time`.
  - **Series that are now absent instead of `0`/stale**: an uninstalled `query_response_time` plugin (bucket, `_count` **and** `_sum` clear together); `userstat` disabled or its table missing; an unreadable/absent `performance_schema` statements source; unreadable metadata-lock, table-wait, InnoDB-status, TLS-status or schema sources; binary logging off or `SHOW BINARY LOGS` denied (previously reported `0` binlog files); a replica status that cannot be read (previously reported zero lag and stopped threads); TLS state that cannot be read (previously claimed "TLS not configured"); certificate timestamps that are missing from an otherwise successful read; and optional InnoDB status lines (LSN, checkpoint age, adaptive hash) missing from a successful status document.
  - **Honest zeros are preserved**: TLS genuinely not in use is still `mariadb_ssl_server_configured 0`, a server with no replication is still `mariadb_replica_configured 0` with the documented `-1/0/0` sentinels, and counters remain monotonic — a counter child is removed only on a genuine skip, never blanket-reset on the success path.
  - **`default` gained a replication leaf**: the `mariadb_slave_status_*` summary moved from `default/status.rs` into its own `default/replication.rs` sub-collector so that an unreadable replica source settles on its own instead of erasing the ~108 global status gauges published in the same scrape. Metric names, help and labels are unchanged.
  - **Registry / HTTP behavior**: `/metrics` still always returns **HTTP 200**. A `Skipped` collector is a successful scrape — only its unavailable series disappear. If any collector returns `Err` after connectivity succeeded, the exporter drains all launched tasks, aggregates the failures, emits `# Error collecting metrics from '<name>': …` comments, keeps `mariadb_up 1`, build information and fresh `mariadb_exporter_*` self-observation metrics, and **withholds every database-dependent family for that scrape** so a preserved snapshot is never timestamped as current. The failed collector's registry state is kept so it can resume. `mariadb_up` is never fabricated to `0` for a collector error, and the encoding-failure path no longer emits a `mariadb_up` sample at all.

### Fixed
- `just bump` now stamps the release version into `grafana/dashboard.json` (rewriting the semver entry in `.tags` and incrementing `.version`), matching the sibling `pg_exporter` flow — previously the dashboard shipped with no indication of which exporter version it targeted. A test asserts the dashboard is tagged with the current crate version. The stamp is applied **before** `just test` runs, because `cargo set-version` has already bumped `CARGO_PKG_VERSION` by that point and stamping afterwards made `dashboard_is_tagged_with_the_crate_version` fail, breaking every `just bump`/`just deploy*` release. A failed bump now also restores `grafana/dashboard.json`, not just the manifests.
- `collector.system` no longer reports the `MariaDB` process group as using zero CPU and zero memory when the host process table cannot be read at all. The sampler now distinguishes "the host was read and no server process runs here" (an honest zero) from "the source is unreadable" (preserve the last good values and warn once), matching the `Err` half of the settlement contract.
- Hand-maintained `--collector.*` lists could silently drift when a collector was added: `just watch`, `scripts/validate-dashboard.sh` and `scripts/benchmark/run-soak.sh` all now enable every registered collector, and `tests/collector_flags_sync.rs` fails if any of them (or `README.md`) omits one.

### Alerting
- Affected series are now **absent** rather than `0` or stale, so threshold alerts on them go quiet instead of firing on frozen values. Move such alerts to `absent()` / `absent_over_time()`, and alert on scrape health with `mariadb_exporter_collector_last_scrape_success == 0` and `rate(mariadb_exporter_collector_scrape_errors_total[5m]) > 0`, which stay exported in the HTTP-200 collector-error mode.

### Added
- **New opt-in `system` collector** (`--collector.system`): host CPU, memory and `MariaDB` process-group statistics for the machine the exporter runs on, ported from the sibling `pg_exporter`. It reads only the operating system — `/proc` on Linux, `kern.cp_times` sysctls on FreeBSD, and `sysinfo` for memory and load average — so it issues no queries, holds no connection from the shared pool and needs no database privileges.
  - `system.cpu`: `mariadb_system_cpu_seconds_total{cpu,mode}` (monotonic per-core deltas that re-baseline across CPU hotplug), `mariadb_system_cpu_cores`, `mariadb_system_cpu_cores_physical`, `mariadb_system_load1` / `_load5` / `_load15`.
  - `system.memory`: `mariadb_system_memory_{total,used,free,available}_bytes` and `mariadb_system_swap_{total,used,free}_bytes`. A swapless host reports a factual `0` rather than removing the series.
  - `system.process`: `mariadb_system_process_group_{cpu_seconds_total,memory_bytes,count}{group="mariadb"}`, aggregating processes whose command name starts with `mariadbd` or `mysqld` (MariaDB 10.5+ renamed the binary; the `mysqld` compatibility name is still shipped by many distributions). Cardinality is fixed at one series per metric. Memory prefers PSS (`/proc/<pid>/smaps_rollup`) with an RSS fallback.
  - **Settlement**: per-core series are *removed* when the platform reports no per-core data, the process group is `Skipped` (all series removed) on unsupported platforms, and a group matching zero processes is a `Fresh` `count=0` — an honest "no server on this host". Deliberately, an OS read error warns and preserves rather than returning `Err`, because a collector `Err` withholds every database-dependent family for the scrape and an optional host collector must never be able to blank out the database metrics.
  - **Grafana**: new collapsed *Host CPU / Memory* row in `grafana/dashboard.json` (CPU utilization by mode, CPU busy normalized across cores, load average against the logical/physical core count, memory & swap, per-CPU utilization, and `MariaDB` process-group CPU/memory/count). All 16 metrics the collector publishes are charted. The row sits above *Exporter Self-Monitoring*, which is pinned last by a test.
  - **Disabled by default and intentionally so**: only enable it when the exporter runs on the same host as `MariaDB`. On managed services (RDS, Aurora, SkySQL, …) or a remote/sidecar exporter it would describe the exporter's host instead. See [`src/collectors/system/README.md`](src/collectors/system/README.md).
- **Settlement tests**: unit tests pinning the trait contract (`Fresh` does not reset, `Skipped` resets, `Err` propagates without resetting, one skipped child cannot clear a fresh sibling) and the zero-label wire format; `tests/collectors/settlement.rs` for successful-snapshot clearing, error-preserves-snapshot, denied-source clearing, TLS certificate absence and `performance_schema` becoming unreadable; `tests/settlement_transitions.rs` for plugin installed → removed and `userstat` enabled → disabled against **isolated** containers; a replica → no-replica channel-clearing assertion in `tests/testcontainers.rs`; and registry tests for the HTTP-200 collector-error mode.
- **Metric metadata golden fixture**: `tests/metric_metadata.rs` and `tests/fixtures/metric_metadata.tsv` pin the name, Prometheus type and `# HELP` string of every exported family. Because settlement makes an unreadable source *absent*, the set of families legitimately varies by server version and privileges — but a family that **is** exported must match the fixture exactly, so a refactor can no longer silently rename a metric, change its type or reword its help text. Adding a metric requires appending the row the test prints.

### Security
- **Dependency audit is clean**: refreshed the locked dependency tree, fixing [RUSTSEC-2026-0258](https://rustsec.org/advisories/RUSTSEC-2026-0258) (`h2` unbounded empty `DATA` frames, a remotely reachable denial-of-service in the HTTP/2 stack that serves `/metrics`) by moving `h2` 0.4.15 → 0.4.18. Also cleared the `event-listener` 5.4.1 unsoundness warning ([RUSTSEC-2026-0221](https://rustsec.org/advisories/RUSTSEC-2026-0221), → 5.4.2) and the yanked `spin` 0.9.8. `cargo audit --deny unsound --deny yanked` now reports no vulnerabilities and no warnings.
- **Security Audit workflow hardened**: added `workflow_dispatch`/`workflow_call` triggers, a `concurrency` group, a pinned `dtolnay/rust-toolchain@stable`, and a cached, guarded `cargo-audit` install matching the other workflows. The audit now runs with `--deny unsound --deny yanked`, and the `push` trigger is path-filtered so the stricter gate cannot break pushes unrelated to dependencies while the daily cron still catches newly published advisories.
- **Security policy added**: `.github/SECURITY.md` documents the supported release line (`0.8.x`), GitHub private vulnerability reporting as the preferred disclosure channel alongside email, and how to reproduce the dependency audit locally. Private vulnerability reporting was enabled on the repository so that channel actually resolves.

### Dependencies
- Added `libc` 0.2 for the `system` collector's `sysconf(_SC_CLK_TCK)` / `sysconf(_SC_PAGESIZE)` reads and the FreeBSD `sysctlbyname` path.
- Updated the locked tree (`cargo update`) and bumped direct requirements: `ulid` 1.2 → 3.0 (`Ulid::new()` → `Ulid::generate()`), `base64` 0.22 → 0.23, `regex` 1.12 → 1.13, `tokio` 1.52.3 → 1.53.1, `sysinfo` 0.39.5 → 0.39.6. `testcontainers` is intentionally held at 0.27.3 because the latest `testcontainers-modules` (0.15) still requires `testcontainers ^0.27`.

## [0.7.0] - 2026-07-06

### Changed
- **Ephemeral Per-Database Connections**: Replaced the dormant per-database *pool cache* in `collectors::util` (`get_or_create_pool_for_db` + a never-evicted `HashMap<String, MySqlPool>`) with an ephemeral `open_db_connection`, which opens a bare connection that is **closed on drop** and never cached. MariaDB reads every schema from the shared pool via `information_schema`, so no collector fans out per database today; this removes a latent foot-gun where a future per-database collector wired to the cached helper would have pinned one persistent connection per database and could exhaust `max_connections` on large or connection-constrained servers. The ephemeral invariant is locked by a new regression test (`tests/collectors/connection.rs`).

### Added
- **aarch64 Release Artifacts**: The release workflow now builds and publishes `aarch64` binaries/packages alongside `x86_64` — Linux static musl (`x86_64`/`aarch64-unknown-linux-musl`) and macOS (`x86_64`/`aarch64-apple-darwin`).
- **Dev Container**: A compose-based [Dev Container](.devcontainer/README.md) (Rust `app` + `mariadb`, plus an optional Prometheus + Grafana `observability` profile). Start with `scripts/dev-up`; `just test` runs against the `mariadb` service with no host database. The `just test` recipe is now devcontainer-aware (uses an already-reachable MariaDB and honors a pre-set `MARIADB_EXPORTER_DSN`).
- **Local Soak Harness**: `scripts/benchmark/` adds a self-contained soak/leak test (`run-soak.sh` + `check-soak.sh` + `soak-dashboard.json`) driven by `scripts/mariadb_loadtest.py` that samples the exporter's own `mariadb_exporter_process_*` metrics (RSS, open FDs, scrape counters) to catch leaks.
- **Developer Tooling & Docs**: `scripts/install-mariadb-client.sh`, `scripts/monitor-exporter.sh`, `scripts/pre-commit-hook.sh`, `scripts/dev-up`/`dev-ssh`/`metrics-dev`, a `mise.toml` toolchain, a new `CONTRIBUTING.md`, and `.github/copilot-instructions.md`.

### Dependencies
- Updated Rust dependencies to their latest versions, including major bumps: `sqlx` 0.8 → 0.9 (adopting the `AssertSqlSafe` API for the few internally-constructed queries), the OpenTelemetry stack 0.31 → 0.32 (`opentelemetry`, `opentelemetry-otlp`, `opentelemetry_sdk`, `opentelemetry-http`), `tracing-opentelemetry` 0.32 → 0.33, `tower-http` 0.6 → 0.7, and `sysinfo` 0.38 → 0.39.
- Bumped GitHub Actions to their latest major versions: `actions/checkout` v6 → v7, `actions/cache` v5 → v6, `codecov/codecov-action` v6 → v7.

### Fixed
- **Dashboard — Collapsed Row Alignment**: All 9 collapsed Grafana rows (Exporter Self-Monitoring, User Statistics, TLS, Statements, Schema, Replication, Locks, Metadata, Query Response Time) stored their child panels' `gridPos.y` as relative values, so Grafana misrendered them (overlapping/misaligned) when expanded. Child panels now use absolute `y` continuing from their row header, matching the intended layout.
- **Devcontainer Observability (`metrics-dev`)**: `scripts/metrics-dev` picked an arbitrary `-app-1` container via `head -1`, so it could target the wrong compose project when another exporter's devcontainer was running. It now selects the project that has both `<project>-app-1` and this repo's `<project>-mariadb-1`. The observability stack's host ports are offset to `3001`/`9091` (in-container ports unchanged) so it coexists with another exporter's Prometheus/Grafana on `3000`/`9090`.

## [0.6.2] - 2026-04-17

### Fixed
- **Linting**: Replaced a suboptimal duration construction in the exporter connection pool so the codebase passes `cargo clippy --all-targets --all-features` under the repo's pedantic lint settings.

### Changed
- **Dependencies**: Refreshed direct Rust crate versions in `Cargo.toml` and regenerated `Cargo.lock` with the latest compatible dependency set.

## [0.6.1] - 2026-04-15

### Fixed
- **Dashboard**: The "Replication Lag (Seconds Behind Master)" panel now shows the current `mariadb_replica_seconds_behind_master_seconds` gauge value instead of the peak value across the selected range, so lag returns to `0` after replica catch-up.

### Changed
- **Dependencies**: Refreshed direct Rust crate versions and regenerated `Cargo.lock` with the current compatible dependency set.
- **CI/CD**: Updated GitHub Actions workflow dependencies and locked Cargo-installed release/coverage helper tools in workflows.

## [0.6.0] - 2026-02-23

### Fixed
- **InnoDB**: Correctly sum all "OS waits" in `mariadb_innodb_semaphore_waits_total` instead of only reporting the last occurrence.
- **Replication**: Report `-1` for lag metrics on `NULL`/stopped/unknown/non-replica states to avoid false "0s healthy" signals on primaries or broken replicas.
- **Replication**: Added upstream-style fallback query support for replica status collection (`SHOW ALL SLAVES STATUS`, `SHOW SLAVE STATUS`, `SHOW REPLICA STATUS` with lock-free suffixes when available).
- **Replication**: Correctly aggregate multi-channel replica status instead of using only the first `SHOW ALL SLAVES STATUS` row.
- **CLI**: Fixed `test_handle_action_signature` to properly test invalid DSN formats without hanging.
- **Correctness**: Added `.reset()` calls to multiple collectors (`Tables`, `UserStat`, `Metadata`, `Statements`, `TLS`, `Version`) to prevent stale labels when entities are dropped.
- **Robustness**: Skip setting metrics if queries fail (e.g. `Performance Schema` missing) rather than reporting misleading zero values.
- **Tests**: Removed unsafe in-test `DOCKER_HOST` mutation to avoid cross-test environment races; container runtime selection is now process-environment driven.

### Changed
- **Resilience**: The exporter now uses lazy database connections and a zero-minimum pool, allowing it to start even when MariaDB is unreachable.
- **Resilience**: The `/metrics` endpoint now always returns `HTTP 200`. During MariaDB outages, it serves a best-effort response with `mariadb_up 0` and omits DB-dependent metrics.
- **Resilience**: MariaDB version detection is now deferred if it fails at startup, retrying during the first scrape.

### Added
- **InnoDB**: New `mariadb_innodb_semaphore_wait_time_ms_total` metric parsing individual thread wait times from `SHOW ENGINE INNODB STATUS`.
- **Tests**: New end-to-end integration test `tests/connectivity_failure.rs` for database outage scenarios.
- **Tests**: Strengthened primary/replica topology coverage for lag, role, and thread-state semantics; CI now requires a runtime for these tests instead of silently skipping.
- **Tests**: Replication topology test now verifies lag progression and recovery (`STOP SLAVE SQL_THREAD` backlog phase, positive lag observation, and recovery to zero).
- **Replication**: New per-channel metrics `mariadb_replica_*_by_channel{channel_name,connection_name}` to expose multi-source channel state without ambiguity.
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
