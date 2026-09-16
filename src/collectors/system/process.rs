//! Host resource usage for the `MariaDB` server process group.
//!
//! Aggregates CPU and memory for every OS process whose name is exactly
//! `mariadbd`, `mysqld`, `mariadbd-safe`, or `mysqld_safe` into a single
//! low-cardinality series labeled `group="mariadb"`. This answers a question
//! the host-wide panels cannot: *is `MariaDB` itself eating the box, or is it
//! a co-located neighbour?*
//!
//! Both server names are matched because the server binary is `mariadbd` on
//! modern releases and `mysqld` on older ones (and on installs that keep the
//! compatibility name). The `mariadbd-safe` / `mysqld_safe` wrapper scripts are
//! members in their own right: they are part of the same service and cost
//! almost nothing. Membership is exact rather than a prefix match, so lookalike
//! tools that share the `mysqld` stem — `mysqldump`, or the community
//! `mysqld_exporter` on hosts that run both — never leak into the series.
//!
//! - **CPU** is a cumulative counter,
//!   `mariadb_system_process_group_cpu_seconds_total` (`utime + stime`). It is
//!   built by accumulating per-PID deltas so process churn (a restart, or a
//!   second instance stopping) never makes the group counter go backwards; use
//!   `rate()` to get "cores consumed by `MariaDB`".
//! - **Memory** is `mariadb_system_process_group_memory_bytes`. On Linux this is
//!   **RSS**, read from `/proc/<pid>/statm`; on FreeBSD it is the summed RSS
//!   reported by `sysinfo`. `--system.process-memory=pss` switches Linux to PSS
//!   (`/proc/<pid>/smaps_rollup`), which is **much** more expensive — see
//!   [`ProcessMemorySource`].
//!
//!   RSS is the right default here, and not merely the cheap one. `MariaDB` is
//!   **thread-per-connection**, not process-per-connection: a single `mariadbd`
//!   process serves every session, so the `InnoDB` buffer pool is already counted
//!   exactly once and summing RSS over the group cannot multiply it. This is
//!   unlike `PostgreSQL`, where PSS is what stops `shared_buffers` being counted
//!   once per backend — there the accuracy was worth arguing about, here the two
//!   sources agree to within shared libraries.
//! - **Count** is `mariadb_system_process_group_count`, the number of matched
//!   processes — normally `1` (plus a wrapper script, if used).
//!
//! Like the rest of `--collector.system` this only makes sense when the exporter
//! is co-located with `MariaDB` and never touches the database.

use crate::collectors::{Collected, Collector, blocking};
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

/// Which `/proc` source the process-group memory gauge is built from (Linux only).
///
/// This exists because the two sources differ in cost by orders of magnitude, not just in
/// accuracy. See [`ProcessMemorySource::Pss`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProcessMemorySource {
    /// **Default.** Resident set size, field 2 of `/proc/<pid>/statm`.
    ///
    /// One short, world-readable line per process that the kernel answers from already
    /// maintained counters, so cost is `O(processes)` and independent of how much memory
    /// the server has touched.
    ///
    /// Summing RSS over the group is safe for `MariaDB` specifically: the server is
    /// thread-per-connection, so there is normally exactly one `mariadbd` process (plus a
    /// wrapper script that maps almost nothing) and the buffer pool is counted once. The
    /// double-counting that makes summed RSS meaningless for process-per-connection
    /// databases has no group to double-count over here.
    #[default]
    Rss,
    /// Proportional set size, read from `/proc/<pid>/smaps_rollup`. **Opt-in: expensive.**
    ///
    /// PSS divides each shared page by the number of processes mapping it, so pages shared
    /// between several server instances on one host — or with anything else — are not
    /// counted more than once. The kernel can only produce that number by walking **every
    /// PTE of every VMA** of the process and checking each page's mapcount, which makes the
    /// cost `O(processes × resident pages)` rather than `O(processes)`.
    ///
    /// Measured on the `PostgreSQL` primary in `nbari/pg_exporter#35` — 253 processes with
    /// a 15939 MB shared segment — reading `smaps_rollup` for the group took **13.851 s**
    /// versus **0.016 s** for the equivalent `stat` reads, ~866x, consuming 92% of a 15 s
    /// scrape budget in one sub-collector. `MariaDB` normally runs one server process
    /// rather than hundreds, so the absolute cost here is far lower — but it still scales
    /// with how much of the buffer pool has been faulted in, which is exactly the number
    /// that grows on the hosts where the exporter matters most.
    ///
    /// Enable with `--system.process-memory=pss` only when several `MariaDB` instances
    /// share a host and the shared-page accounting is worth the walk.
    Pss,
}

impl ProcessMemorySource {
    /// CLI/env spelling of this variant.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rss => "rss",
            Self::Pss => "pss",
        }
    }

    /// Parses the CLI/env spelling, case-insensitively.
    ///
    /// # Errors
    ///
    /// Returns a message naming the accepted values if `value` is neither.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "rss" => Ok(Self::Rss),
            "pss" => Ok(Self::Pss),
            other => Err(format!(
                "process memory source must be 'rss' or 'pss', got '{other}'"
            )),
        }
    }
}

/// Process names that define the group, matched exactly (after trimming and
/// lowercasing). `mariadbd` is the modern server binary; `mysqld` covers older
/// releases and compatibility installs; the two wrapper scripts are part of the
/// same service. Exact names, not prefixes, so that `mysqldump` and
/// `mysqld_exporter` — which share the `mysqld` stem — never join the group.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
const GROUP_NAMES: [&str; 4] = ["mariadbd", "mysqld", "mariadbd-safe", "mysqld_safe"];

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
    GROUP_NAMES.contains(&name.as_str())
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
///
/// **Expensive.** Reachable only through `--system.process-memory=pss`; see
/// [`ProcessMemorySource::Pss`].
#[cfg(target_os = "linux")]
fn read_pss_bytes(pid: u32) -> Option<u64> {
    let content = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok()?;
    parse_pss_kb(&content).map(|kb| kb.saturating_mul(1024))
}

/// Reads RSS (bytes) for one PID from the world-readable `statm`. This is the default
/// source; see [`ProcessMemorySource::Rss`].
#[cfg(target_os = "linux")]
fn read_rss_bytes(pid: u32, page_size: u64) -> Option<u64> {
    let content = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    parse_statm_resident_pages(&content).map(|pages| pages.saturating_mul(page_size))
}

/// Chooses between the two readers for `source`, and applies the PSS fallback.
///
/// Both readers are taken lazily and that is the whole point: in [`ProcessMemorySource::Rss`]
/// mode `pss` must never be called, because calling it is the `O(processes × resident pages)`
/// page-table walk of `nbari/pg_exporter#35`. Taking them as arguments also makes the
/// dispatch testable without reading a live process, whose footprint moves between reads.
#[cfg(target_os = "linux")]
fn select_memory_source<P, R>(source: ProcessMemorySource, pss: P, statm: R) -> u64
where
    P: FnOnce() -> Option<u64>,
    R: FnOnce() -> Option<u64>,
{
    match source {
        ProcessMemorySource::Rss => statm(),
        // PSS is unreadable without privileges on that process; fall back rather than
        // reporting nothing.
        ProcessMemorySource::Pss => pss().or_else(statm),
    }
    .unwrap_or(0)
}

/// Reads the memory figure for one PID from the configured source.
#[cfg(target_os = "linux")]
fn read_memory_bytes(pid: u32, page_size: u64, source: ProcessMemorySource) -> u64 {
    select_memory_source(
        source,
        || read_pss_bytes(pid),
        || read_rss_bytes(pid, page_size),
    )
}

/// Samples every `mariadbd`/`mysqld` process on Linux by reading `/proc` directly.
///
/// Returns `None` when the process table itself could not be read. That is
/// deliberately distinct from `Some(vec![])`: an empty vector means "the host was
/// read and no server process is running here", while `None` means "the source is
/// unreadable", which must never be published as a factual zero.
///
/// Blocking, synchronous I/O: callers must run this on the blocking pool via
/// [`blocking::offload_coalesced`], never inline on a runtime worker
/// (`nbari/pg_exporter#35`).
#[cfg(target_os = "linux")]
fn sample_processes(source: ProcessMemorySource) -> Option<Vec<ProcSample>> {
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

        let mem_bytes = read_memory_bytes(pid, bytes_per_page, source);

        out.push(ProcSample {
            pid,
            cpu_seconds,
            mem_bytes,
        });
    }

    Some(out)
}

/// Samples every `mariadbd`/`mysqld` process on FreeBSD via `sysinfo`. There is
/// no cheap PSS, so memory is RSS (`Process::memory`) regardless of
/// [`ProcessMemorySource`].
///
/// Always returns `Some`: `sysinfo` reports an empty process list rather than a
/// read failure, so there is no unreadable-source case to distinguish here.
///
/// Blocking, synchronous I/O: callers must run this on the blocking pool via
/// [`blocking::offload_coalesced`], never inline on a runtime worker
/// (`nbari/pg_exporter#35`).
#[cfg(target_os = "freebsd")]
#[expect(
    clippy::unnecessary_wraps,
    reason = "platform implementations share Option so Linux can report an unreadable process source"
)]
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
/// - `mariadb_system_process_group_memory_bytes` (gauge; RSS by default, PSS via
///   `--system.process-memory=pss` on Linux)
/// - `mariadb_system_process_group_count` (gauge)
#[derive(Clone)]
pub struct ProcessGroupCollector {
    cpu_seconds: CounterVec,
    memory_bytes: IntGaugeVec,
    proc_count: IntGaugeVec,
    /// Which `/proc` file the memory gauge is read from. Only the Linux sampler
    /// consults it: FreeBSD has no cheap PSS, and other platforms do not sample.
    /// Which `/proc` file the Linux sampler reads a process's memory footprint from.
    ///
    /// Only Linux offers a choice: FreeBSD sampling goes through `sysinfo`, which exposes
    /// RSS alone, and every other platform collects nothing at all, so the field is inert
    /// there rather than genuinely dead. `#[expect(dead_code)]` would be wrong — the
    /// `#[cfg(test)]` tests do read it, so `--all-targets` would trip
    /// `unfulfilled_lint_expectations` and move the failure to the Linux clippy job.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    memory_source: ProcessMemorySource,
    /// Last observed cumulative CPU seconds per live PID, used to accumulate a
    /// monotonic group counter across process churn. Held across the sample so
    /// two collections cannot publish their baselines out of order.
    prev_cpu: Arc<Mutex<HashMap<u32, f64>>>,
    /// Caps the collector at one in-flight blocking sample; see
    /// [`blocking::offload_coalesced`].
    sample_slot: Arc<tokio::sync::Mutex<()>>,
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
    /// Creates a new `ProcessGroupCollector` reading the default memory source.
    #[must_use]
    pub fn new() -> Self {
        Self::with_memory_source(ProcessMemorySource::default())
    }

    /// Creates a new `ProcessGroupCollector` reading `memory_source`.
    ///
    /// # Panics
    ///
    /// Panics if metric creation fails, which only happens with an invalid
    /// metric name or label set and therefore never at runtime.
    #[must_use]
    #[allow(clippy::expect_used)]
    pub fn with_memory_source(memory_source: ProcessMemorySource) -> Self {
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
                "Resident memory of the host process group in bytes (summed RSS; MariaDB is \
                 thread-per-connection so the buffer pool is counted once, set \
                 --system.process-memory=pss for proportional shared-page accounting)",
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
            memory_source,
            prev_cpu: Arc::new(Mutex::new(HashMap::new())),
            sample_slot: Arc::new(tokio::sync::Mutex::new(())),
            #[cfg(target_os = "freebsd")]
            system: Arc::new(Mutex::new(System::new())),
            unsupported_warned: Arc::new(AtomicBool::new(false)),
            unreadable_warned: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Samples the host and publishes the result.
    ///
    /// Blocking: only ever reached through [`blocking::offload_coalesced`].
    fn collect_stats(&self) {
        #[cfg(target_os = "linux")]
        self.collect_stats_with(|| sample_processes(self.memory_source));
        #[cfg(target_os = "freebsd")]
        self.collect_stats_with(|| sample_processes(&self.system));
        #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
        self.collect_stats_with(|| None);
    }

    /// Guards the platform check, then samples and publishes under the CPU baseline lock.
    fn collect_stats_with(&self, sample: impl FnOnce() -> Option<Vec<ProcSample>>) {
        if !SUPPORTED {
            if !self.unsupported_warned.swap(true, Ordering::Relaxed) {
                warn!(
                    "collector.system process-group metrics are not supported on this platform \
                     (Linux/FreeBSD only)"
                );
            }
            return;
        }

        self.sample_and_publish(sample);
    }

    /// Takes the CPU baseline lock, **then** samples, then publishes.
    ///
    /// The order is load-bearing. `mariadb_system_process_group_cpu_seconds_total` is
    /// accumulated from per-PID deltas against `prev_cpu`, so if the sample happened
    /// outside the lock a newer collection could publish its baseline first, the older one
    /// would then find every total lower than the baseline, count no delta and overwrite
    /// the baseline with its own older values — and the next pass would re-count the
    /// interval between them, inflating the counter above the CPU actually consumed.
    ///
    /// `try_lock`, not `lock`: an overlapping collection skips rather than queueing behind
    /// a `/proc` walk. The outer one-slot guard in [`blocking::offload_coalesced`] already
    /// prevents scrape-driven overlap; this is the defence for direct calls.
    fn sample_and_publish(&self, sample: impl FnOnce() -> Option<Vec<ProcSample>>) {
        let mut prev = match self.prev_cpu.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::WouldBlock) => {
                debug!("a previous process-group sample is still running; skipping this one");
                return;
            }
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                warn!("process-group cpu mutex was poisoned, recovering");
                poisoned.into_inner()
            }
        };

        let observed = sample();
        self.publish(&mut prev, observed);
    }

    /// Publishes a process-group snapshot against the already-locked CPU baseline.
    ///
    /// `None` means the process table could not be read and preserves the last good
    /// snapshot, matching the `Err` half of the settlement contract. Publishing zeros
    /// there would claim "MariaDB is using no CPU or memory", which is a factual
    /// assertion the exporter cannot make when it could not read the source at all.
    /// `Some(vec![])` is different: the host was read and no server process runs here,
    /// so an honest zero is published.
    fn publish(&self, prev: &mut HashMap<u32, f64>, observed: Option<Vec<ProcSample>>) {
        let Some(samples) = observed else {
            if !self.unreadable_warned.swap(true, Ordering::Relaxed) {
                warn!(
                    "collector.system could not read the host process table; MariaDB \
                     process-group metrics keep their last good values"
                );
            }
            return;
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

    /// Publishes `observed` directly, bypassing the platform gate.
    ///
    /// Used by tests to drive the settlement paths (`None` versus `Some(vec![])`) on hosts
    /// where per-process sampling is not implemented.
    #[cfg(test)]
    fn apply_samples(&self, observed: Option<Vec<ProcSample>>) {
        self.sample_and_publish(|| observed);
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
            // Blocking `/proc` walk: never run this on a runtime worker. Inline it and a
            // slow walk stops every other collector's futures being polled, which surfaces
            // as a bogus "pool timed out while waiting for an open connection"
            // (`nbari/pg_exporter#35`).
            let collector = self.clone();
            // Deliberately not `?`: a collector `Err` makes the registry withhold every
            // database-dependent family for the scrape, and an optional host-metrics
            // collector must never be able to blank out the database metrics. A sample
            // that did not complete warns and preserves the last good values, exactly
            // like an unreadable `/proc`.
            if let Err(error) = blocking::offload_coalesced(
                "system.process",
                &self.sample_slot,
                move || collector.collect_stats(),
            )
            .await
            {
                warn!("collector.system process-group sample did not complete: {error}");
            }

            if SUPPORTED {
                Ok(Collected::Fresh)
            } else {
                Ok(Collected::Skipped)
            }
        })
    }

    /// Removes every labeled series this collector owns.
    fn reset_metrics(&self) {
        self.cpu_seconds.reset();
        self.memory_bytes.reset();
        self.proc_count.reset();
        match self.prev_cpu.try_lock() {
            Ok(mut guard) => guard.clear(),
            Err(std::sync::TryLockError::WouldBlock) => {
                // A sample holds the baseline. Leaving it is harmless: the series were
                // removed, and the next sample re-establishes them from real deltas.
                debug!("process-group sample in flight; leaving the CPU baseline in place");
            }
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
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
        assert!(!is_group_member("mariadb"), "the client is not the server");
        assert!(!is_group_member(""));
        // Exact names, not prefixes: tools sharing the `mysqld` stem stay out.
        assert!(!is_group_member("mariadb_exporter"));
        assert!(!is_group_member("mariadbd-extra"));
        assert!(!is_group_member("mysqldump"));
        assert!(!is_group_member("mysqld_exporter"));
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

    /// `pg_exporter` reads `resident - shared` from this same line, because summing
    /// resident pages across its one-process-per-backend model charged `shared_buffers`
    /// to every backend (`nbari/pg_exporter#36`: 312 GiB reported on a 93.8 GiB host).
    ///
    /// MariaDB is thread-per-connection, so the group is a single `mariadbd` whose shared
    /// pages are the binary and its libraries — a flat ~24 MB that does not grow with
    /// sessions (measured on `mariadb:11.4`: 1 process and the same 24 MB gap at idle and
    /// at 61 connections). Subtracting it would under-report resident memory the server
    /// really holds, so field 2 is used unmodified.
    ///
    /// This test exists so that porting `#36` is a deliberate act rather than a silent
    /// sync: it fails the moment the shared field starts being subtracted.
    #[cfg(target_os = "linux")]
    #[test]
    fn parse_statm_resident_pages_does_not_subtract_shared_pages() {
        // size=2048, resident=512, shared=128. `resident - shared` would be 384.
        let statm = "2048 512 128 1 0 300 0";
        assert_eq!(
            parse_statm_resident_pages(statm),
            Some(512),
            "the RSS source must report resident pages as-is; subtracting the shared field \
             is pg_exporter's fix for per-backend shared memory, which thread-per-connection \
             MariaDB does not have"
        );

        // A process whose pages are almost entirely shared must still report them.
        assert_eq!(parse_statm_resident_pages("4096 900 890 1 0 300 0"), Some(900));
    }

    #[test]
    fn memory_source_defaults_to_rss() {
        assert_eq!(ProcessMemorySource::default(), ProcessMemorySource::Rss);
        assert_eq!(ProcessGroupCollector::new().memory_source, ProcessMemorySource::Rss);
    }

    #[test]
    fn memory_source_round_trips_through_its_cli_spelling() {
        for source in [ProcessMemorySource::Rss, ProcessMemorySource::Pss] {
            assert_eq!(ProcessMemorySource::parse(source.as_str()), Ok(source));
        }
        assert_eq!(
            ProcessMemorySource::parse("  PSS "),
            Ok(ProcessMemorySource::Pss),
            "parsing must be case- and whitespace-insensitive"
        );
        assert!(ProcessMemorySource::parse("smaps").is_err());
    }

    #[test]
    fn with_memory_source_is_honoured() {
        assert_eq!(
            ProcessGroupCollector::with_memory_source(ProcessMemorySource::Pss).memory_source,
            ProcessMemorySource::Pss
        );
    }

    /// The whole point of the opt-in: in the default mode the `smaps_rollup` reader must
    /// never even be *called*, because calling it is the page-table walk.
    #[cfg(target_os = "linux")]
    #[test]
    fn rss_mode_never_touches_the_smaps_rollup_reader() {
        let pss_calls = std::cell::Cell::new(0_u32);

        let bytes = select_memory_source(
            ProcessMemorySource::Rss,
            || {
                pss_calls.set(pss_calls.get() + 1);
                Some(999)
            },
            || Some(4096),
        );

        assert_eq!(bytes, 4096, "rss mode must report the statm reader's value");
        assert_eq!(
            pss_calls.get(),
            0,
            "rss mode called the smaps_rollup reader: that is the O(processes x resident \
             pages) page-table walk of nbari/pg_exporter#35, which the default must never \
             perform"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pss_mode_reads_pss_and_falls_back_to_statm_when_it_is_unreadable() {
        assert_eq!(
            select_memory_source(ProcessMemorySource::Pss, || Some(999), || Some(4096)),
            999,
            "pss mode must report the smaps_rollup reader's value, or \
             --system.process-memory=pss silently does nothing"
        );
        assert_eq!(
            select_memory_source(ProcessMemorySource::Pss, || None, || Some(4096)),
            4096,
            "pss mode must fall back to statm when smaps_rollup is unreadable"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn both_sources_report_zero_when_nothing_is_readable() {
        for source in [ProcessMemorySource::Rss, ProcessMemorySource::Pss] {
            assert_eq!(select_memory_source(source, || None, || None), 0);
        }
    }

    /// An overlapping direct collection must skip rather than interleave: two passes that
    /// published their CPU baselines out of order would make the group counter over-report.
    #[test]
    fn a_concurrent_collection_skips_instead_of_interleaving_samples() {
        let collector = ProcessGroupCollector::new();

        collector.apply_samples(Some(vec![ProcSample {
            pid: 4242,
            cpu_seconds: 5.0,
            mem_bytes: 1_000_000,
        }]));

        let sampled = std::cell::Cell::new(false);
        #[allow(clippy::unwrap_used)]
        let held = collector.prev_cpu.lock().unwrap();

        collector.sample_and_publish(|| {
            sampled.set(true);
            Some(Vec::new())
        });

        assert!(
            !sampled.get(),
            "a second collection sampled while the CPU baseline was locked: overlapping \
             passes can then publish baselines out of order and the group CPU counter \
             over-reports"
        );
        drop(held);

        assert_eq!(
            collector.memory_bytes.with_label_values(&[GROUP]).get(),
            1_000_000,
            "the skipped collection must not have overwritten the previous snapshot"
        );
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
