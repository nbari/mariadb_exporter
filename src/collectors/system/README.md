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
| `mariadb_system_process_group_memory_bytes` | gauge | `group="mariadb"` | Aggregate resident memory of the group |
| `mariadb_system_process_group_count` | gauge | `group="mariadb"` | Number of processes in the group |

Cardinality is fixed at one series per metric, whatever the host runs.

**Group membership** matches processes whose command name starts with
`mariadbd` or `mysqld`. `mariadbd` is the real binary name on MariaDB 10.5+;
`mysqld` covers older releases and distributions that keep the compatibility
symlink. The prefix match also catches the `mariadbd-safe` / `mysqld_safe`
wrapper scripts — this is intentional, since they are part of the server's
process group and their resource usage is negligible.

**Memory accounting** prefers PSS (`/proc/<pid>/smaps_rollup`) and falls back to
RSS (`/proc/<pid>/statm`). On MariaDB the two are usually close: the server is
thread-per-connection, so a single `mariadbd` process serves every session and
the `InnoDB` buffer pool is counted once. (This differs from PostgreSQL, where
one process per backend makes shared memory appear repeatedly in a naive RSS
sum.) PSS still matters when several server instances share pages, or when the
`mariadbd-safe` wrapper is counted alongside the server.

## Settlement behaviour

| Situation | Outcome |
| --- | --- |
| Normal scrape on Linux/FreeBSD | `Fresh` |
| Host reports no per-core CPU data | `Fresh`, per-core series removed |
| Process group matches zero processes | `Fresh` with `count=0` — an honest "no server here" |
| Unsupported platform (`system.process`) | `Skipped`, all group series removed |
| OS read error (unreadable `/proc`, …) | warn + preserve, **not** `Err` |

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
| Linux | full (`/proc/stat`) | full | full (`/proc`) |
| FreeBSD | full (`kern.cp_times`) | full | full |
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

# Server RSS/PSS as a fraction of host memory
mariadb_system_process_group_memory_bytes / mariadb_system_memory_total_bytes

# Alert when host metrics stop being reported at all
absent(mariadb_system_memory_total_bytes)
```
