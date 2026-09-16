# `system` collector

Host CPU, memory and `MariaDB` process-group statistics for the machine the
exporter runs on.

> **Disabled by default.** Enable with `--collector.system`.

This collector never queries `MariaDB`. It reads only the operating system —
`/proc` on Linux, `kern.cp_times` sysctls on FreeBSD, and `sysinfo` for memory
and load average — so it adds no query load, holds no connection from the shared
pool, and needs no database privileges.

## When to enable it

Enable it **only when `mariadb_exporter` runs on the same host as `MariaDB`**.

Do **not** enable it for managed services (Amazon RDS, Aurora, Azure Database for
MariaDB, SkySQL, …) or any setup where the exporter is remote: there the numbers
describe the *exporter's* host, not the database server, and are actively
misleading. The same applies when the exporter runs in a sidecar container
without `hostPID`/`/proc` access to the database process.

## Metrics

### `system.cpu`

| Metric | Type | Labels | Description |
| --- | --- | --- | --- |
| `mariadb_system_cpu_seconds_total` | counter | `cpu`, `mode` | Cumulative CPU time per logical core and mode |
| `mariadb_system_cpu_cores` | gauge | — | Logical cores visible to the OS |
| `mariadb_system_cpu_cores_physical` | gauge | — | Physical cores (best effort) |
| `mariadb_system_load1` | gauge | — | 1-minute load average |
| `mariadb_system_load5` | gauge | — | 5-minute load average |
| `mariadb_system_load15` | gauge | — | 15-minute load average |

`mode` is `user`, `nice`, `system`, `idle`, `iowait`, `irq`, `softirq`, `steal`
on Linux, and `user`, `nice`, `system`, `interrupt`, `idle` on FreeBSD.

The counter is derived from monotonic per-core deltas, so a CPU going offline
and returning (hotplug, cgroup reshuffle) re-baselines instead of producing a
negative jump. When the platform reports no per-core data at all, the per-core
series are **removed** rather than frozen at their last value.

### `system.memory`

| Metric | Type | Description |
| --- | --- | --- |
| `mariadb_system_memory_total_bytes` | gauge | Total physical memory |
| `mariadb_system_memory_used_bytes` | gauge | Used physical memory |
| `mariadb_system_memory_free_bytes` | gauge | Free physical memory |
| `mariadb_system_memory_available_bytes` | gauge | Memory available to new allocations |
| `mariadb_system_swap_total_bytes` | gauge | Total swap |
| `mariadb_system_swap_used_bytes` | gauge | Used swap |
| `mariadb_system_swap_free_bytes` | gauge | Free swap |

A swapless host reports `0` for the swap gauges. That is a factual reading, not a
missing source, so the series stay published.

> **Note:** the `default` collector already exports
> `mariadb_exporter_system_memory_total_bytes` as part of the exporter's build /
> self-observation metrics. `mariadb_system_memory_total_bytes` is the
> host-metrics equivalent; they report the same quantity under different
> namespaces and both are safe to scrape.

### `system.process`

| Metric | Type | Labels | Description |
| --- | --- | --- | --- |
| `mariadb_system_process_group_cpu_seconds_total` | counter | `group="mariadb"` | Aggregate CPU time of the server process group |
| `mariadb_system_process_group_memory_bytes` | gauge | `group="mariadb"` | Aggregate resident memory of the group (RSS by default, PSS with `--system.process-memory=pss`) |
| `mariadb_system_process_group_count` | gauge | `group="mariadb"` | Number of processes in the group |

Cardinality is fixed at one series per metric, whatever the host runs.

**Group membership** matches processes whose command name is exactly
`mariadbd`, `mysqld`, `mariadbd-safe`, or `mysqld_safe` (case-insensitive).
`mariadbd` is the real binary name on MariaDB 10.5+; `mysqld` covers older
releases and distributions that keep the compatibility symlink, and the two
wrapper scripts are part of the server's process group while using negligible
resources. The match is exact rather than a prefix, so lookalike tools such as
`mysqldump` or the community `mysqld_exporter` never join the group.

**Memory accounting** reads RSS from `/proc/<pid>/statm` by default, and can be
switched to PSS (`/proc/<pid>/smaps_rollup`) with `--system.process-memory=pss`.

RSS is the right default here. MariaDB is thread-per-connection, so a single
`mariadbd` process serves every session and the `InnoDB` buffer pool is counted
exactly once — there is no double-counting to correct. (This differs from
PostgreSQL, where one process per backend makes shared memory appear repeatedly
in a naive RSS sum.)

> **Do not port [`nbari/pg_exporter#36`][issue-36] here.** `pg_exporter` changed
> its `statm` reader from field 2 (`resident`) to `resident - shared`, because
> summing resident pages across backends charged `shared_buffers` to every one
> of them: 312 GiB reported on a 93.8 GiB host with 208 backends. That failure
> mode needs one *process* per connection, which MariaDB does not have.
> Measured on `mariadb:11.4`:
>
> | | matched processes | Σ resident | Σ private | gap |
> | --- | --- | --- | --- | --- |
> | idle | 1 | 406 MB | 382 MB | 24 MB |
> | 61 connections | 1 | 412 MB | 387 MB | 24 MB |
>
> The gap is the server binary and its shared libraries. It stays flat as
> sessions arrive instead of scaling with them, so there is nothing to divide
> out — and subtracting it would *under*-report the group by excluding
> file-backed pages the server genuinely has resident. `parse_statm_resident_pages`
> therefore reads field 2 unmodified, pinned by
> `parse_statm_resident_pages_does_not_subtract_shared_pages`.

PSS is **opt-in because it is expensive**, not because it is inaccurate. `statm`
is a short line the kernel already maintains, so a sample costs
`O(processes)`. `smaps_rollup` is computed on demand: the kernel walks every
page-table entry of every mapping and inspects each page's mapcount, making a
sample cost `O(processes × resident pages)`. On a sibling deployment with a
15939 MB shared segment, one group sample took **13.851 s** via `smaps_rollup`
against **0.016 s** via `statm` — roughly 866× — which consumed almost the whole
scrape budget on its own ([`nbari/pg_exporter#35`][issue-35]).

Enable PSS only when you need proportional shared-page accounting: several
server instances sharing pages on one host, or a `mariadbd-safe` wrapper counted
alongside the server. When PSS is unreadable for a process (it needs privileges)
the collector falls back to that process's RSS rather than reporting nothing.

```sh
# Default: cheap, and correct for a single MariaDB instance.
mariadb_exporter --collector.system

# Opt in to proportional accounting, and budget for the page-table walk.
mariadb_exporter --collector.system --system.process-memory=pss
```

Both sources are sampled on a blocking thread, never on a Tokio runtime worker,
and at most one sample per collector is in flight at a time. A slow `/proc` read
therefore delays only the `system` collector's own numbers; it cannot starve the
database collectors or pile work onto the blocking pool
([`nbari/pg_exporter#35`][issue-35]).

> **Reading the cost.** `mariadb_exporter_collector_scrape_duration_seconds` tops out
> at a 5 s bucket, so a PSS walk simply lands in `+Inf`; read the magnitude from
> `_sum / _count` instead. `_count` covers successes **and** aborted scrapes (an abort
> observes its time-until-abort, an error observes nothing), so during a run of scrape
> timeouts that ratio is pulled towards `--scrape.timeout-ms`. Subtract
> `mariadb_exporter_collector_scrape_aborted_total{collector="system"}` from the count
> to see what the scrapes that actually finished cost.

[issue-35]: https://github.com/nbari/pg_exporter/issues/35
[issue-36]: https://github.com/nbari/pg_exporter/issues/36

## Settlement behaviour

| Situation | Outcome |
| --- | --- |
| Normal scrape on Linux/FreeBSD | `Fresh` |
| Host reports no per-core CPU data | `Fresh`, per-core series removed |
| Process group matches zero processes | `Fresh` with `count=0` — an honest "no server here" |
| Unsupported platform (`system.process`) | `Skipped`, all group series removed |
| OS read error (unreadable `/proc`, …) | warn + preserve, **not** `Err` |
| OS sample did not complete (panicked/cancelled blocking task) | warn + preserve, **not** `Err` |
| A previous sample is still running | previous values kept; no duplicate work submitted |

The last row is a deliberate deviation from the usual `Err` classification. In
`mariadb_exporter` a collector `Err` makes the registry withhold **every**
database-dependent metric family for that scrape; an optional host-metrics
collector must never be able to blank out the database metrics, so it degrades
in place instead.

`system.memory` is always `Fresh` (host memory is always readable through
`sysinfo`), so its `reset_metrics` is a documented no-op.

## Platform support

| Platform | CPU | Memory | Process group |
| --- | --- | --- | --- |
| Linux | full (`/proc/stat`) | full | full (`/proc`, RSS or PSS) |
| FreeBSD | full (`kern.cp_times`) | full | full (RSS only) |
| macOS / Windows / other | cores + load only | full | not supported (`Skipped`) |

## Example

```sh
mariadb_exporter --collector.system
```

```promql
# Per-core busy fraction
1 - rate(mariadb_system_cpu_seconds_total{mode="idle"}[5m])

# CPU seconds burned by the MariaDB server itself
rate(mariadb_system_process_group_cpu_seconds_total[5m])

# Server memory as a fraction of host memory
mariadb_system_process_group_memory_bytes / mariadb_system_memory_total_bytes

# Alert when host metrics stop being reported at all
absent(mariadb_system_memory_total_bytes)
```
