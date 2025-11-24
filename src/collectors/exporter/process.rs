use crate::collectors::Collector;
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{Gauge, IntGauge, Opts, Registry};
use sqlx::MySqlPool;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use sysinfo::{Pid, System};
use tracing::{debug, instrument, warn};

/// Monitors the `mariadb_exporter` process itself
#[derive(Clone)]
pub struct ProcessCollector {
    cpu_percent: Gauge,
    cpu_cores: IntGauge,
    resident_memory_bytes: IntGauge,
    virtual_memory_bytes: IntGauge,
    open_fds: IntGauge,
    start_time_seconds: Gauge,
    system: Arc<Mutex<SystemState>>,
    pid: Pid,
}

struct SystemState {
    system: System,
    last_refresh: Option<Instant>,
}

impl Default for ProcessCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    ///
    /// # Panics
    ///
    /// Panics if metric creation fails.
    pub fn new() -> Self {
        let cpu_percent = Gauge::with_opts(Opts::new(
            "mariadb_exporter_process_cpu_percent",
            "Current CPU usage percentage (matches ps %cpu, can exceed 100%)",
        ))
        .expect("mariadb_exporter_process_cpu_percent");

        let cpu_cores = IntGauge::with_opts(Opts::new(
            "mariadb_exporter_process_cpu_cores",
            "Number of CPU cores available on the system",
        ))
        .expect("mariadb_exporter_process_cpu_cores");

        let resident_memory_bytes = IntGauge::with_opts(Opts::new(
            "mariadb_exporter_process_resident_memory_bytes",
            "Resident memory size in bytes (RSS)",
        ))
        .expect("mariadb_exporter_process_resident_memory_bytes");

        let virtual_memory_bytes = IntGauge::with_opts(Opts::new(
            "mariadb_exporter_process_virtual_memory_bytes",
            "Virtual memory size in bytes (VSZ)",
        ))
        .expect("mariadb_exporter_process_virtual_memory_bytes");

        let open_fds = IntGauge::with_opts(Opts::new(
            "mariadb_exporter_process_open_fds",
            "Number of open file descriptors",
        ))
        .expect("mariadb_exporter_process_open_fds");

        let start_time_seconds = Gauge::with_opts(Opts::new(
            "mariadb_exporter_process_start_time_seconds",
            "Start time of the process since unix epoch in seconds",
        ))
        .expect("mariadb_exporter_process_start_time_seconds");

        let system = System::new_all();
        let num_cpus = system.cpus().len().max(1);

        let system = Arc::new(Mutex::new(SystemState {
            system,
            last_refresh: None,
        }));
        let pid = Pid::from(std::process::id() as usize);

        cpu_cores.set(i64::try_from(num_cpus).unwrap_or(0));

        let start_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        start_time_seconds.set(start_time);

        Self {
            cpu_percent,
            cpu_cores,
            resident_memory_bytes,
            virtual_memory_bytes,
            open_fds,
            start_time_seconds,
            system,
            pid,
        }
    }

    fn collect_stats(&self) {
        let now = Instant::now();

        let mut state = match self.system.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                warn!("System mutex was poisoned, recovering");
                poisoned.into_inner()
            }
        };

        let should_wait = state
            .last_refresh
            .is_some_and(|last| now.duration_since(last) < sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);

        if should_wait {
            if let Some(process) = state.system.process(self.pid) {
                let rss = process.memory();
                let vsz = process.virtual_memory();

                self.resident_memory_bytes
                    .set(i64::try_from(rss).unwrap_or(0));
                self.virtual_memory_bytes
                    .set(i64::try_from(vsz).unwrap_or(0));
            }
            return;
        }

        state.system.refresh_all();
        state.last_refresh = Some(now);

        if let Some(process) = state.system.process(self.pid) {
            let cpu = f64::from(process.cpu_usage());
            self.cpu_percent.set(cpu);

            let rss = process.memory();
            let vsz = process.virtual_memory();

            self.resident_memory_bytes
                .set(i64::try_from(rss).unwrap_or(0));
            self.virtual_memory_bytes
                .set(i64::try_from(vsz).unwrap_or(0));

            #[cfg(target_os = "linux")]
            {
                if let Ok(entries) = std::fs::read_dir(format!("/proc/{}/fd", self.pid)) {
                    let fd_count = i64::try_from(entries.count()).unwrap_or(0);
                    self.open_fds.set(fd_count);
                }
            }

            #[cfg(not(target_os = "linux"))]
            {
                self.open_fds.set(0);
            }

            debug!(
                cpu_percent = cpu,
                rss_mb = rss / 1024 / 1024,
                vsz_mb = vsz / 1024 / 1024,
                fds = self.open_fds.get(),
                "collected process metrics"
            );
        }
    }
}

impl Collector for ProcessCollector {
    fn name(&self) -> &'static str {
        "metrics.process"
    }

    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.cpu_percent.clone()))?;
        registry.register(Box::new(self.cpu_cores.clone()))?;
        registry.register(Box::new(self.resident_memory_bytes.clone()))?;
        registry.register(Box::new(self.virtual_memory_bytes.clone()))?;
        registry.register(Box::new(self.open_fds.clone()))?;
        registry.register(Box::new(self.start_time_seconds.clone()))?;
        Ok(())
    }

    #[instrument(skip(self, _pool), level = "debug")]
    fn collect<'a>(&'a self, _pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.collect_stats();
            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
