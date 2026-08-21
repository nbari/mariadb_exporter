//! Host resource usage for the `MariaDB` server process group.
//!
//! Aggregates CPU and memory for every OS process whose name starts with
//! `mariadbd` or `mysqld` into a single low-cardinality series labeled
//! `group="mariadb"`. This answers a question the host-wide panels cannot: *is
//! `MariaDB` itself eating the box, or is it a co-located neighbour?*
//!
//! Both prefixes are matched because the server binary is `mariadbd` on modern
//! releases and `mysqld` on older ones (and on installs that keep the
//! compatibility name). The prefixes also match the `mariadbd-safe` / `mysqld_safe`
//! wrapper scripts, which are part of the same service and cost almost nothing.
//!
//! - **CPU** is a cumulative counter,
//!   `mariadb_system_process_group_cpu_seconds_total` (`utime + stime`). It is
//!   built by accumulating per-PID deltas so process churn (a restart, or a
//!   second instance stopping) never makes the group counter go backwards; use
//!   `rate()` to get "cores consumed by `MariaDB`".
//! - **Memory** is `mariadb_system_process_group_memory_bytes`. On Linux this is
//!   **PSS** (proportional set size, from `/proc/<pid>/smaps_rollup`), which
//!   divides shared pages proportionally, so pages shared with other processes
//!   (shared libraries, and copy-on-write pages when several instances run on
//!   one host) are not counted more than once. PSS requires the exporter to run
//!   as the `mysql` user or as root; when a process is not readable it falls
//!   back to that process's RSS. On FreeBSD there is no cheap PSS, so this is the
//!   summed **RSS**.
//!
//!   Note that `MariaDB` is **thread-per-connection**, not process-per-connection:
//!   a single `mariadbd` process serves every session, so the `InnoDB` buffer pool
//!   is already counted exactly once and PSS and RSS are usually close. This is
//!   unlike `PostgreSQL`, where PSS is what stops `shared_buffers` being
//!   multiplied across hundreds of backend processes.
//! - **Count** is `mariadb_system_process_group_count`, the number of matched
//!   processes — normally `1` (plus a wrapper script, if used).
//!
//! Like the rest of `--collector.system` this only makes sense when the exporter
//! is co-located with `MariaDB` and never touches the database.

use crate::collectors::{Collected, Collector};
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{CounterVec, IntGaugeVec, Opts, Registry};
use sqlx::MySqlPool;
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tracing::{debug, instrument, warn};

#[cfg(target_os = "freebsd")]
use sysinfo::System;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use super::cpu::ticks_to_seconds;

/// Value of the `group` label.
const GROUP: &str = "mariadb";

/// Process-name prefixes that define the group. `mariadbd` is the modern server
/// binary; `mysqld` covers older releases and compatibility installs.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
const GROUP_PREFIXES: [&str; 2] = ["mariadbd", "mysqld"];

/// Whether per-process sampling is implemented for the current platform.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
const SUPPORTED: bool = true;
#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
const SUPPORTED: bool = false;

/// Converts a `u64` byte count into the `i64` a Prometheus `IntGauge` stores,
/// saturating instead of wrapping on the (practically impossible) overflow.
#[inline]
fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Returns true when a process name belongs to the `MariaDB` server group.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn is_group_member(process_name: &str) -> bool {
    let name = process_name.trim_end().to_ascii_lowercase();
    GROUP_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// One sampled process: its PID, cumulative CPU seconds, and resident bytes.
struct ProcSample {
    pid: u32,
    cpu_seconds: f64,
    mem_bytes: u64,
}

/// Parses the summed `utime + stime` clock ticks from a `/proc/<pid>/stat` line.
///
/// The `comm` field (2) is wrapped in parentheses and may itself contain spaces
/// or parentheses, so fields are read after the **last** `)`: the first token
/// after it is `state` (field 3), making `utime` (field 14) index 11 and `stime`
/// (field 15) index 12.
#[cfg(target_os = "linux")]
fn parse_stat_cpu_ticks(stat: &str) -> Option<u64> {
    let rparen = stat.rfind(')')?;
    let rest = stat.get(rparen + 1..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let utime = fields.get(11)?.parse::<u64>().ok()?;
    let stime = fields.get(12)?.parse::<u64>().ok()?;
    Some(utime.saturating_add(stime))
}

/// Extracts the `Pss:` value (in kB) from a `/proc/<pid>/smaps_rollup` dump.
#[cfg(target_os = "linux")]
fn parse_pss_kb(smaps_rollup: &str) -> Option<u64> {
    smaps_rollup
        .lines()
        .find_map(|line| line.strip_prefix("Pss:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
}

/// Extracts resident pages (field 2) from a `/proc/<pid>/statm` line.
#[cfg(target_os = "linux")]
fn parse_statm_resident_pages(statm: &str) -> Option<u64> {
    statm.split_whitespace().nth(1)?.parse::<u64>().ok()
}

/// Returns the clock-tick frequency (`_SC_CLK_TCK`) used to scale `/proc` CPU
/// counters, defaulting to the near-universal 100 Hz.
#[cfg(target_os = "linux")]
fn clk_tck() -> f64 {
    // SAFETY: `sysconf` is a pure, thread-safe query with no side effects.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    u32::try_from(ticks).map_or(100.0, f64::from)
}

/// Returns the system page size in bytes, defaulting to 4096.
#[cfg(target_os = "linux")]
fn page_size() -> u64 {
    // SAFETY: `sysconf` is a pure, thread-safe query with no side effects.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    u64::try_from(size).unwrap_or(4096)
}

/// Reads PSS (bytes) for one PID, or `None` when `smaps_rollup` is unavailable
/// (older kernels) or unreadable (insufficient privileges for that process).
#[cfg(target_os = "linux")]
fn read_pss_bytes(pid: u32) -> Option<u64> {
    let content = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok()?;
    parse_pss_kb(&content).map(|kb| kb.saturating_mul(1024))
}

/// Reads RSS (bytes) for one PID from the world-readable `statm`, the fallback
/// when PSS is not available.
#[cfg(target_os = "linux")]
fn read_rss_bytes(pid: u32, page_size: u64) -> Option<u64> {
    let content = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    parse_statm_resident_pages(&content).map(|pages| pages.saturating_mul(page_size))
}

/// Samples every `mariadbd`/`mysqld` process on Linux by reading `/proc` directly.
///
/// Returns `None` when the process table itself could not be read. That is
/// deliberately distinct from `Some(vec![])`: an empty vector means "the host was
/// read and no server process is running here", while `None` means "the source is
/// unreadable", which must never be published as a factual zero.
#[cfg(target_os = "linux")]
fn sample_processes() -> Option<Vec<ProcSample>> {
    let hz = clk_tck();
    let bytes_per_page = page_size();
    let mut out = Vec::new();

    let entries = std::fs::read_dir("/proc").ok()?;

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(pid) = file_name.to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };

        let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) else {
            continue;
        };
        if !is_group_member(&comm) {
            continue;
        }

        let cpu_seconds = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| parse_stat_cpu_ticks(&stat))
            .map_or(0.0, |ticks| ticks_to_seconds(ticks, hz));

        let mem_bytes = read_pss_bytes(pid)
            .or_else(|| read_rss_bytes(pid, bytes_per_page))
            .unwrap_or(0);

        out.push(ProcSample {
            pid,
            cpu_seconds,
            mem_bytes,
        });
    }

    Some(out)
}

/// Samples every `mariadbd`/`mysqld` process on FreeBSD via `sysinfo`. There is
/// no cheap PSS, so memory is RSS (`Process::memory`).
///
/// Always returns `Some`: `sysinfo` reports an empty process list rather than a
/// read failure, so there is no unreadable-source case to distinguish here.
#[cfg(target_os = "freebsd")]
fn sample_processes(system: &Mutex<System>) -> Option<Vec<ProcSample>> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate};

    let mut system = match system.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            warn!("system process mutex was poisoned, recovering");
            poisoned.into_inner()
        }
    };

    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_memory().with_cpu(),
    );

    let mut out = Vec::new();
    for (pid, process) in system.processes() {
        let name = process.name().to_string_lossy();
        if !is_group_member(&name) {
            continue;
        }
        out.push(ProcSample {
            pid: pid.as_u32(),
            // accumulated_cpu_time() is in CPU-milliseconds.
            cpu_seconds: ticks_to_seconds(process.accumulated_cpu_time(), 1000.0),
            mem_bytes: process.memory(),
        });
    }

    Some(out)
}

/// Aggregate host CPU and memory for the `MariaDB` server process group.
///
/// **Metrics (labeled `group="mariadb"`):**
/// - `mariadb_system_process_group_cpu_seconds_total` (counter, seconds)
/// - `mariadb_system_process_group_memory_bytes` (gauge; PSS on Linux, RSS on FreeBSD)
/// - `mariadb_system_process_group_count` (gauge)
#[derive(Clone)]
pub struct ProcessGroupCollector {
    cpu_seconds: CounterVec,
    memory_bytes: IntGaugeVec,
    proc_count: IntGaugeVec,
    /// Last observed cumulative CPU seconds per live PID, used to accumulate a
    /// monotonic group counter across process churn.
    prev_cpu: Arc<Mutex<HashMap<u32, f64>>>,
    /// Persistent `sysinfo` state for FreeBSD sampling (unused on Linux, which
    /// reads `/proc` directly).
    #[cfg(target_os = "freebsd")]
    system: Arc<Mutex<System>>,
    /// Ensures the "unsupported platform" warning is logged at most once.
    unsupported_warned: Arc<AtomicBool>,
    /// Ensures the "unreadable process table" warning is logged at most once
    /// rather than on every scrape.
    unreadable_warned: Arc<AtomicBool>,
}

impl Default for ProcessGroupCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessGroupCollector {
    /// Creates a new `ProcessGroupCollector`.
    ///
    /// # Panics
    ///
    /// Panics if metric creation fails, which only happens with an invalid
    /// metric name or label set and therefore never at runtime.
    #[must_use]
    #[allow(clippy::expect_used)]
    pub fn new() -> Self {
        let cpu_seconds = CounterVec::new(
            Opts::new(
                "mariadb_system_process_group_cpu_seconds_total",
                "Cumulative CPU time in seconds (user + system) consumed by host processes in the \
                 group, since the exporter started tracking; use rate() for cores consumed",
            ),
            &["group"],
        )
        .expect("mariadb_system_process_group_cpu_seconds_total");

        let memory_bytes = IntGaugeVec::new(
            Opts::new(
                "mariadb_system_process_group_memory_bytes",
                "Resident memory of the host process group in bytes (Linux: PSS, so pages shared \
                 with other processes are not double-counted; FreeBSD: summed RSS)",
            ),
            &["group"],
        )
        .expect("mariadb_system_process_group_memory_bytes");

        let proc_count = IntGaugeVec::new(
            Opts::new(
                "mariadb_system_process_group_count",
                "Number of host processes matched in the group",
            ),
            &["group"],
        )
        .expect("mariadb_system_process_group_count");

        Self {
            cpu_seconds,
            memory_bytes,
            proc_count,
            prev_cpu: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(target_os = "freebsd")]
            system: Arc::new(Mutex::new(System::new())),
            unsupported_warned: Arc::new(AtomicBool::new(false)),
            unreadable_warned: Arc::new(AtomicBool::new(false)),
        }
    }

    fn collect_stats(&self) {
        if !SUPPORTED {
            if !self.unsupported_warned.swap(true, Ordering::Relaxed) {
                warn!(
                    "collector.system process-group metrics are not supported on this platform \
                     (Linux/FreeBSD only)"
                );
            }
            return;
        }

        #[cfg(target_os = "linux")]
        let observed = sample_processes();
        #[cfg(target_os = "freebsd")]
        let observed = sample_processes(&self.system);
        #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
        let observed: Option<Vec<ProcSample>> = None;

        self.apply_samples(observed);
    }

    /// Publishes a process-group snapshot.
    ///
    /// `None` means the process table could not be read and preserves the last good
    /// snapshot, matching the `Err` half of the settlement contract. Publishing zeros
    /// there would claim "MariaDB is using no CPU or memory", which is a factual
    /// assertion the exporter cannot make when it could not read the source at all.
    /// `Some(vec![])` is different: the host was read and no server process runs here,
    /// so an honest zero is published.
    fn apply_samples(&self, observed: Option<Vec<ProcSample>>) {
        let Some(samples) = observed else {
            if !self.unreadable_warned.swap(true, Ordering::Relaxed) {
                warn!(
                    "collector.system could not read the host process table; MariaDB \
                     process-group metrics keep their last good values"
                );
            }
            return;
        };

        let mut prev = match self.prev_cpu.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                warn!("process-group cpu mutex was poisoned, recovering");
                poisoned.into_inner()
            }
        };

        let mut delta_total = 0.0_f64;
        let mut mem_total = 0_u64;
        let mut current = HashMap::with_capacity(samples.len());

        for sample in &samples {
            // Only positive deltas count: a missing PID (exited) simply stops
            // contributing, and a reused PID with a lower total is treated as a
            // reset (new baseline), so the group counter never decreases.
            if let Some(&previous) = prev.get(&sample.pid)
                && sample.cpu_seconds >= previous
            {
                delta_total += sample.cpu_seconds - previous;
            }
            mem_total = mem_total.saturating_add(sample.mem_bytes);
            current.insert(sample.pid, sample.cpu_seconds);
        }

        let count = i64::try_from(samples.len()).unwrap_or(i64::MAX);
        *prev = current;
        drop(prev);

        // Materialise the counter on every fresh scrape, even when the delta is
        // zero: having observed the group, "0 additional CPU seconds" is a
        // truthful current reading, and it gives `rate()` a series to work with
        // from the first scrape instead of only after the first busy interval.
        let cpu = self.cpu_seconds.with_label_values(&[GROUP]);
        if delta_total > 0.0 {
            cpu.inc_by(delta_total);
        }
        self.memory_bytes
            .with_label_values(&[GROUP])
            .set(to_i64(mem_total));
        self.proc_count.with_label_values(&[GROUP]).set(count);

        debug!(
            count,
            mem_bytes = mem_total,
            "updated mariadb process-group metrics"
        );
    }
}

impl Collector for ProcessGroupCollector {
    fn name(&self) -> &'static str {
        "system.process"
    }

    #[instrument(skip(self, registry), level = "info", err, fields(collector = "system.process"))]
    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.cpu_seconds.clone()))?;
        registry.register(Box::new(self.memory_bytes.clone()))?;
        registry.register(Box::new(self.proc_count.clone()))?;
        Ok(())
    }

    /// `Skipped` when per-process sampling is not implemented for this platform,
    /// so the group series disappear instead of freezing at their last value;
    /// `Fresh` otherwise. A successful scan that matches **no** process is a
    /// genuine current snapshot (`count=0`), not a skip: it truthfully reports
    /// that no `MariaDB` server is running on this host.
    #[instrument(skip(self, _pool), level = "debug")]
    fn collect_once<'a>(&'a self, _pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
        Box::pin(async move {
            if !SUPPORTED {
                self.collect_stats();
                return Ok(Collected::Skipped);
            }
            self.collect_stats();
            Ok(Collected::Fresh)
        })
    }

    /// Removes every labeled series this collector owns.
    fn reset_metrics(&self) {
        self.cpu_seconds.reset();
        self.memory_bytes.reset();
        self.proc_count.reset();
        match self.prev_cpu.lock() {
            Ok(mut guard) => guard.clear(),
            Err(poisoned) => {
                warn!("process-group cpu mutex was poisoned, recovering");
                poisoned.into_inner().clear();
            }
        }
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn collector_name_is_system_process() {
        assert_eq!(ProcessGroupCollector::new().name(), "system.process");
    }

    #[test]
    fn collector_is_disabled_by_default() {
        assert!(!ProcessGroupCollector::new().enabled_by_default());
    }

    #[test]
    fn register_metrics_succeeds() {
        let registry = Registry::new();
        assert!(
            ProcessGroupCollector::new()
                .register_metrics(&registry)
                .is_ok()
        );
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn group_matching_accepts_both_server_binaries() {
        assert!(is_group_member("mariadbd"));
        assert!(is_group_member("mariadbd\n"));
        assert!(is_group_member("mysqld"));
        assert!(is_group_member("mysqld_safe"));
        assert!(is_group_member("mariadbd-safe"));
        assert!(is_group_member("MariaDBd"), "matching is case-insensitive");
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[test]
    fn group_matching_rejects_unrelated_processes() {
        assert!(!is_group_member("postgres"));
        assert!(!is_group_member("mariadb_exporter"));
        assert!(!is_group_member("mariadb"), "the client is not the server");
        assert!(!is_group_member(""));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_stat_cpu_ticks_handles_parentheses_in_comm() {
        // comm containing spaces and a ')' must not shift the field offsets.
        let mut stat = String::from("1234 (weird ) name) S 1 1 1 0 -1 0 0 0 0 0");
        // fields after state: ppid,pgrp,session,tty,tpgid,flags,minflt,cminflt,majflt,cmajflt
        // then utime (index 11) and stime (index 12).
        stat.push_str(" 700 300 0 0 20 0 1 0 0");
        assert_eq!(parse_stat_cpu_ticks(&stat), Some(1000));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_stat_cpu_ticks_rejects_truncated_lines() {
        assert_eq!(parse_stat_cpu_ticks("1 (x) S 1 2 3"), None);
        assert_eq!(parse_stat_cpu_ticks("no parenthesis here"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_pss_kb_reads_the_rollup_field() {
        let rollup = "Rss:  1024 kB\nPss:   512 kB\nShared_Clean: 0 kB\n";
        assert_eq!(parse_pss_kb(rollup), Some(512));
        assert_eq!(parse_pss_kb("Rss: 1024 kB\n"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_statm_resident_pages_reads_second_field() {
        assert_eq!(parse_statm_resident_pages("2048 512 128 1 0 300 0"), Some(512));
        assert_eq!(parse_statm_resident_pages("2048"), None);
    }

    #[test]
    fn to_i64_saturates_instead_of_wrapping() {
        assert_eq!(to_i64(0), 0);
        assert_eq!(to_i64(4096), 4096);
        assert_eq!(to_i64(u64::MAX), i64::MAX);
    }

    #[test]
    fn collect_stats_publishes_a_bounded_label_set() {
        let collector = ProcessGroupCollector::new();
        collector.collect_stats();

        // Cardinality is fixed: exactly one series per metric, whatever the host runs.
        assert!(collector.proc_count.with_label_values(&[GROUP]).get() >= 0);
        assert!(collector.memory_bytes.with_label_values(&[GROUP]).get() >= 0);
    }

    #[test]
    fn reset_metrics_removes_the_group_series() {
        let collector = ProcessGroupCollector::new();
        let registry = Registry::new();
        collector.register_metrics(&registry).unwrap();
        collector.collect_stats();

        Collector::reset_metrics(&collector);

        let names: Vec<String> = registry
            .gather()
            .iter()
            .map(|f| f.name().to_owned())
            .collect();
        assert!(
            !names
                .iter()
                .any(|n| n == "mariadb_system_process_group_count"),
            "group series must disappear after a settled skip, got {names:?}"
        );
    }

    #[test]
    fn cpu_counter_never_decreases_across_repeated_scrapes() {
        let collector = ProcessGroupCollector::new();
        collector.collect_stats();
        let first = collector.cpu_seconds.with_label_values(&[GROUP]).get();
        collector.collect_stats();
        let second = collector.cpu_seconds.with_label_values(&[GROUP]).get();

        assert!(
            second >= first,
            "group CPU counter must be monotonic: {second} < {first}"
        );
    }

    #[test]
    fn unreadable_process_table_preserves_the_last_good_snapshot() {
        let collector = ProcessGroupCollector::new();

        collector.apply_samples(Some(vec![ProcSample {
            pid: 4242,
            cpu_seconds: 12.0,
            mem_bytes: 3_000_000,
        }]));
        let mem_before = collector.memory_bytes.with_label_values(&[GROUP]).get();
        let count_before = collector.proc_count.with_label_values(&[GROUP]).get();
        assert_eq!(mem_before, 3_000_000);
        assert_eq!(count_before, 1);

        // An unreadable process table must not claim "MariaDB is using nothing".
        collector.apply_samples(None);

        assert_eq!(
            collector.memory_bytes.with_label_values(&[GROUP]).get(),
            mem_before,
            "unreadable process table must preserve memory, not publish 0"
        );
        assert_eq!(
            collector.proc_count.with_label_values(&[GROUP]).get(),
            count_before,
            "unreadable process table must preserve count, not publish 0"
        );
    }

    #[test]
    fn readable_host_with_no_server_process_publishes_an_honest_zero() {
        let collector = ProcessGroupCollector::new();

        collector.apply_samples(Some(vec![ProcSample {
            pid: 4242,
            cpu_seconds: 12.0,
            mem_bytes: 3_000_000,
        }]));

        // Distinct from `None`: the host *was* read and no server process runs here.
        collector.apply_samples(Some(Vec::new()));

        assert_eq!(
            collector.proc_count.with_label_values(&[GROUP]).get(),
            0,
            "a readable host with no server process is an honest zero"
        );
        assert_eq!(
            collector.memory_bytes.with_label_values(&[GROUP]).get(),
            0,
            "a readable host with no server process reports zero memory"
        );
    }
}
