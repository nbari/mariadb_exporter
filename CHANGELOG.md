# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.9.0]

### Fixed
- **`/metrics` could wedge at `503` permanently** (port of [`nbari/pg_exporter#34`][pg-34]). The scrape gate is now held by the *request*, not by the scrape task. The sibling project observed 66 minutes of uninterrupted `503`: the single permit had been moved into the spawned scrape task, and a timeout drops the `JoinHandle`, which **detaches** the task rather than cancelling it — so the permit was released only if the abandoned task eventually unwound, which it never did.
  - The permit is acquired by the request future and released by ordinary `Drop` on **every** exit path: success, collector error, panic, timeout, and client disconnect.
  - A timed-out scrape is now **aborted** rather than detached (`AbortOnDrop`), so its in-flight `sqlx` futures are dropped and their pooled connections returned instead of being leaked.
  - Aborting is only half the fix: a task can be cancelled only at an `await` point, and the `system` collector's `/proc` walk had none. Offloading those reads (below) is what makes the abort effective.
  - `tests/collector_safety.rs` pins the shape (the permit must not be named at or after `tokio::spawn`), and `gate_tests` in `src/collectors/registry.rs` pins the behaviour from each exit path. Two of them fail if the permit is moved back into the task.
- **`--collector.system` could cost ~14 s per scrape and stall every other collector** (port of [`nbari/pg_exporter#35`][pg-35]).
  - Process-group memory now reads **RSS from `/proc/<pid>/statm` by default**. It previously preferred PSS from `/proc/<pid>/smaps_rollup`, which the kernel computes on demand by walking every page-table entry of every mapping and checking each page's mapcount — `O(processes × resident pages)`. Measured on a sibling deployment with a 15939 MB shared segment: **13.851 s** via `smaps_rollup` versus **0.016 s** via `statm`, about 866×, consuming almost an entire 15 s scrape budget.
  - RSS is also the *correct* default for MariaDB: the server is thread-per-connection, so one `mariadbd` process serves every session and the `InnoDB` buffer pool is counted exactly once. There is no double-counting for PSS to correct. PSS remains available via the new `--system.process-memory=pss` for hosts running several instances that share pages, and still falls back to RSS for any process whose `smaps_rollup` is unreadable.
  - **All synchronous OS reads moved off the runtime workers.** `system.cpu`, `system.memory`, `system.process` and `metrics.process` previously ran their `/proc`, `sysctl` and `sysinfo` reads inline in `collect_once`, occupying a Tokio worker for the whole read. That stopped the `sqlx` pool's futures from being polled, so unrelated collectors failed with the badly misleading `pool timed out while waiting for an open connection`. They now run on `tokio::task::spawn_blocking` via the new `collectors::blocking` module.
  - **At most one sample per collector is in flight.** A started `spawn_blocking` task cannot be cancelled, so without a bound each aborted scrape would enqueue another multi-second sample and grow the blocking pool without limit. `blocking::offload_coalesced` takes a one-slot guard *before* spawning and skips the sample when the previous one is still running.
  - An OS sample that does not complete (the blocking task panicked or was cancelled) **warns and preserves** the previous values instead of returning `Err`. Offloading makes the sample fallible for the first time, and a collector `Err` withholds every database-dependent family for that scrape — an optional host-metrics collector must never be able to blank out the database metrics. `tests/collector_safety.rs` fails if any `collect_once` applies `?` to the offload.
  - `system.process` takes its CPU baseline lock **before** sampling. Otherwise two overlapping passes can publish baselines out of order, making `mariadb_system_process_group_cpu_seconds_total` over-report.
- **A panicking collector took down the whole exporter.** A panic anywhere in a collector — or in a dependency it calls — unwound the entire scrape, which the gate reported as `TaskFailed`: `/metrics` answered `503` and withheld every *healthy* collector's metrics. Each in-flight `ScrapeTimer` was also dropped unobserved, so the incident was filed as a scrape *abort* rather than naming the collector that broke. Collector futures now run inside a `catch_unwind` boundary that turns a panic into an ordinary collector error, so the documented contract holds: HTTP 200, `# Error collecting metrics from '<name>': collector panicked: …`, honest `mariadb_up`, and fresh self-observation metrics. Construction of the collector future happens inside the boundary too, so a panic before the future is even returned is still an error.
- **`panic = "abort"` is no longer set for release builds.** `catch_unwind` catches unwinding panics only, so aborting made the collector panic boundary, `ScrapeError::TaskFailed`, and `blocking::offload`'s panic contract silently inert — and *only* in the builds that ship, because the `test` profile inherits `dev` and unwinds. Every test covering those paths passed while the shipped binary would `SIGABRT` on the first collector panic. `tests/collector_safety.rs::panic_containment_is_not_disabled_in_release` fails if a Cargo profile or checked-in `.cargo/config.toml` rustflags restore aborting panics.
- **Aborted scrapes were recorded as successes.** When a scrape exceeded `--scrape.timeout-ms` (or the client disconnected), every collector still in flight had its timer dropped without an outcome, which defaulted to *success* — so `mariadb_exporter_collector_last_scrape_success` read `1` with a timeout-sized "successful" duration during exactly the incident these metrics exist to diagnose. Such attempts are now counted in the new `mariadb_exporter_collector_scrape_aborted_total` with `last_scrape_success 0`. The time-until-abort is still observed, so a stalled collector remains visible in `..._duration_seconds` — which means `_sum / _count` is pulled towards the timeout during a run of aborts; subtract the aborted count to see what the scrapes that finished actually cost.
- **Spawned scrapes were detached from the request's trace.** `tokio::spawn` does not propagate `tracing` context, so moving the scrape into a task (above) orphaned every `collector.collect` span from the `http.server.request` span built from the inbound `traceparent`: distributed traces no longer linked a `/metrics` request to the work it caused, and collector logs lost their `request_id` and `http.route` fields. The scrape is now instrumented with the caller's span before it is spawned.
- **A late abort record could overwrite a newer scrape's success.** The gate reopens the moment a scrape times out, but the aborted task's per-collector timers are dropped whenever the runtime reaps it — so a fast follow-up scrape could record `last_scrape_success 1` for a collector and then have the stale timer's `Drop` overwrite it with `0`. Abort records now carry the scrape epoch: the aborted counter and the duration are always recorded, but the success gauge and timestamp yield to any outcome a newer scrape already published. The epoch comparison and both gauges are published under one lock, so a stale writer cannot pass the check and then resume after a newer outcome.
- **A panicking `system` sub-collector could still blank the database metrics.** The umbrella swallowed a sub-collector `Err` (warn and continue) but a *panic* unwound to the registry boundary, failing the whole `system` collector and withholding every database-dependent family for that scrape. Each sub-collector now runs inside its own `catch_unwind`, so a panic degrades to the same per-sub-collector warning as an error.
- `test_memory_metrics_reasonable` no longer asserts an upper bound on virtual memory. VSZ counts address-space reservations rather than memory, and macOS on arm64 maps hundreds of gigabytes into every process, so the ceiling tested the platform allocator rather than the collector.

### Added
- `--scrape.timeout-ms` (env `MARIADB_EXPORTER_SCRAPE_TIMEOUT_MS`, default `15000`): wall-clock budget for one scrape. Set it below your Prometheus `scrape_timeout` so the exporter, not the scraper, decides when to give up. Note the default does not itself satisfy that advice — Prometheus's own `scrape_timeout` defaults to `10s`, so out of the box the scraper disconnects first and the exporter's `504` never fires.
- `--system.process-memory=rss|pss` (env `MARIADB_EXPORTER_SYSTEM_PROCESS_MEMORY`, default `rss`): source for the `system` collector's process-group memory gauge.
- `mariadb_exporter_collector_scrape_aborted_total{collector}`: scrape attempts that were aborted mid-flight. Alert on `rate(...) > 0` to catch a collector that is not finishing within `--scrape.timeout-ms`; the collector is not necessarily at fault, since the abort bounds the whole scrape.
- `mariadb_exporter_scrape_aborted_total` (no `collector` label): scrapes abandoned as a whole. Per-collector timers only start *after* the connectivity check, so a scrape that exceeded its budget while `SELECT 1` was still in flight previously left no trace anywhere. The two counters are asymmetric on purpose: a client disconnect drops the request future and therefore runs no exporter code, so those aborts remain visible only per collector, and only for collectors already started.
- `tests/collector_safety.rs`: structural guards for both invariants. The OS-read guard is a whole-crate call-graph check, not a line match, so moving a read down into a helper — or into a module outside `src/collectors/` entirely — still fails the test, and so does passing the blocking function by value instead of calling it.
- The metric-metadata golden fixture now also pins the conditional `mariadb_exporter_collector_scrape_{errors,aborted}_total` families and `mariadb_info_schema_query_response_time_seconds_bucket`, so their names, types and help strings are checked the moment a scrape materialises them.

### Changed
- **`/metrics` now answers `503` and `504` in two new situations** *(behaviour change for scrapers)*. The documented "always HTTP 200" property held for *database* problems and still does: an unreachable server is `200` with `mariadb_up 0`, and collector failures remain `200` with `# Error collecting metrics from '<name>'` comments. The two new codes describe the **exporter**, where no honest exposition exists to return:
  - `503 Service Unavailable` — a scrape is already in flight. `/metrics` is single-flight, so overlapping scrapes are refused rather than piled onto an already-slow server.
  - `504 Gateway Timeout` — the scrape exceeded `--scrape.timeout-ms` and was aborted.
  - A gate release is not a promise that all work has finished: a running `spawn_blocking` sample cannot be cancelled, and the server may still be finishing a statement. The gate bounds *exporter* concurrency; a role-level connection limit is the hard backstop for server-side work.
- `mariadb_system_process_group_memory_bytes` help text now names the default source (summed RSS) and the `--system.process-memory=pss` opt-in. Its name, type and labels are unchanged.
- `system.process` group membership is now an exact process-name match — `mariadbd`, `mysqld`, and the `mariadbd-safe`/`mysqld_safe` wrappers — instead of a `mariadbd`/`mysqld` prefix match, so a `mysqldump` run or a co-located `mysqld_exporter` no longer transiently joins the `group="mariadb"` series.
- `exporter::new` and `Action::Run` take a `CollectorConfig` instead of a `Vec<String>` of collector names, so per-collector settings and the scrape budget reach the registry alongside the enabled set.
- A failing `system` sub-collector no longer fails the `system` collector. The umbrella propagated the first sub-collector `Err`, which the registry answers by withholding every database-dependent family — so an optional host-metrics collector could have blanked out the MariaDB metrics, the exact inversion this release exists to prevent. Failures are now warned about per sub-collector and the scrape continues.
- CI compiles the non-Linux `cfg` arms. The FreeBSD sysctl readers and the `not(any(linux, freebsd))` fallbacks were built by no job, while `warnings = "deny"` turns an unused field on one platform into a compile error, so a break that only exists off Linux could reach users unnoticed.

### Security
- Updated the locked dependency tree to fix [RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285), moving `rustls` 0.23.43 → 0.23.45. The advisory concerns TLS 1.3 handshake messages accepted at the wrong encryption level; `cargo audit --deny unsound --deny yanked` is clean after the update.

[pg-34]: https://github.com/nbari/pg_exporter/issues/34
[pg-35]: https://github.com/nbari/pg_exporter/issues/35

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
  - `system.process`: `mariadb_system_process_group_{cpu_seconds_total,memory_bytes,count}{group="mariadb"}`, aggregating processes whose command name starts with `mariadbd` or `mysqld` (MariaDB 10.5+ renamed the binary; the `mysqld` compatibility name is still shipped by many distributions). Cardinality is fixed at one series per metric. Memory prefers PSS (`/proc/<pid>/smaps_rollup`) with an RSS fallback. *(Superseded in 0.9.0: membership is an exact-name match and RSS is the default memory source.)*
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
