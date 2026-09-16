//! `system` collector umbrella (host CPU / memory / `MariaDB` process group).
//!
//! `mod.rs` is the entry point: it wires up the `cpu`, `memory` and `process`
//! sub-collectors and exposes them under the `--collector.system` CLI flag. The
//! actual metric definitions and OS reads live in the sibling [`cpu`],
//! [`memory`] and [`process`] modules.
//!
//! This collector reports **host-wide** CPU and memory usage for the machine the
//! exporter runs on, plus the resource usage of the **`MariaDB` server process
//! group** on that host. It never touches `MariaDB`: it reads only the operating
//! system (`/proc` on Linux, `kern.cp_times` sysctls on FreeBSD, and `sysinfo`
//! for memory and load average), so it adds no query or connection load to the
//! database and holds no connection from the shared pool.
//!
//! # When to enable it
//!
//! It is **disabled by default** and only meaningful when `mariadb_exporter` runs
//! on the **same host** as `MariaDB`. Do **not** enable it for managed services
//! such as Amazon RDS or `SkySQL`: there the exporter runs on a separate machine,
//! so the CPU/memory numbers describe the exporter's host, not the database
//! server, and would be misleading.

use crate::collectors::{Collected, Collector, registry::panic_payload_message};
use anyhow::Result;
use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};
use futures::FutureExt as _;
use prometheus::Registry;
use sqlx::MySqlPool;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use tracing::{debug, info_span, instrument, warn};
use tracing_futures::Instrument as _;

pub mod cpu;
pub mod memory;
pub mod process;

use cpu::CpuCollector;
use memory::MemoryCollector;
pub use process::ProcessMemorySource;
use process::ProcessGroupCollector;

/// Host CPU, memory and `MariaDB` process-group statistics for the machine
/// running the exporter.
///
/// This is the umbrella collector selected by `--collector.system`. It fans
/// registration and collection out to a [`CpuCollector`], a [`MemoryCollector`],
/// and a [`ProcessGroupCollector`], matching the structure used by the other
/// composite collectors (`default`, `replication`, `locks`). It is disabled by
/// default and intended only for exporters co-located with `MariaDB`.
#[derive(Clone)]
pub struct SystemCollector {
    subs: Vec<Arc<dyn Collector + Send + Sync>>,
}

impl Default for SystemCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemCollector {
    /// Creates a new `SystemCollector` with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(ProcessMemorySource::default())
    }

    /// Creates a new `SystemCollector` reading process-group memory from
    /// `process_memory`.
    #[must_use]
    pub fn with_config(process_memory: ProcessMemorySource) -> Self {
        Self {
            subs: vec![
                Arc::new(CpuCollector::new()),
                Arc::new(MemoryCollector::new()),
                Arc::new(ProcessGroupCollector::with_memory_source(process_memory)),
            ],
        }
    }
}

impl Collector for SystemCollector {
    fn name(&self) -> &'static str {
        "system"
    }

    #[instrument(skip(self, registry), level = "info", err, fields(collector = "system"))]
    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        for sub in &self.subs {
            let span = info_span!("collector.register_metrics", sub_collector = %sub.name());
            let res = sub.register_metrics(registry);
            match res {
                Ok(()) => debug!(collector = sub.name(), "registered metrics"),
                Err(ref e) => {
                    warn!(collector = sub.name(), error = %e, "failed to register metrics");
                }
            }
            res?;
            drop(span);
        }
        Ok(())
    }

    /// Always `Fresh`: this umbrella owns no metrics of its own, and each child
    /// settles independently through the safe `collect` wrapper, so a skipped
    /// child can never clear a fresh sibling.
    #[instrument(
        skip(self, pool),
        level = "info",
        err,
        fields(collector = "system", otel.kind = "internal")
    )]
    fn collect_once<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
        Box::pin(async move {
            let mut tasks = FuturesUnordered::new();

            for sub in &self.subs {
                let span = info_span!(
                    "collector.collect",
                    sub_collector = %sub.name(),
                    otel.kind = "internal"
                );
                let name = sub.name();
                tasks.push(
                    async move {
                        // Panic boundary mirroring the registry's `collect_with_outcome`.
                        // The real subs run their risky OS reads inside
                        // `blocking::offload_coalesced` (a panic there surfaces as a
                        // `JoinError` → `Err`), but a panic in the async glue — metric
                        // publication, future construction — would otherwise unwind
                        // through the drain below up to the registry's boundary, marking
                        // the whole `system` collector failed and withholding every
                        // database-dependent family for the scrape. Convert it to an
                        // ordinary `Err` so the warn-and-continue path below covers
                        // panics exactly like errors.
                        let future = async move { sub.collect(pool).await };
                        let result = match AssertUnwindSafe(future).catch_unwind().await {
                            Ok(result) => result,
                            Err(payload) => Err(anyhow::anyhow!(
                                "sub-collector panicked: {}",
                                panic_payload_message(payload.as_ref())
                            )),
                        };
                        (name, result)
                    }
                    .instrument(span),
                );
            }

            // Deliberately not `?`. A sub-collector Err would fail the whole `system`
            // collector, and the registry withholds every database-dependent family for a
            // scrape in which any collector errored — so an optional host-metrics collector
            // could blank out the MariaDB metrics. Each sub already settles its own metrics
            // through `Collector::collect`, and the `catch_unwind` above routes a panicked
            // sub through this same path; report the failure and keep the rest.
            while let Some((name, res)) = tasks.next().await {
                if let Err(error) = res {
                    warn!(
                        sub_collector = name,
                        %error,
                        "system sub-collector failed; continuing so host metrics cannot blank \
                         out the database metrics"
                    );
                }
            }

            Ok(Collected::Fresh)
        })
    }

    /// Fans out to the sub-collectors; this umbrella owns no metrics itself.
    /// Each sub already settles via the safe `collect`, so this exists only so a
    /// caller holding the umbrella has something to call.
    fn reset_metrics(&self) {
        for sub in &self.subs {
            sub.reset_metrics();
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
    fn system_collector_is_named_system() {
        assert_eq!(SystemCollector::new().name(), "system");
    }

    #[test]
    fn system_collector_is_not_enabled_by_default() {
        assert!(!SystemCollector::new().enabled_by_default());
    }

    #[test]
    fn system_collector_registers_without_error() {
        let registry = Registry::new();
        assert!(SystemCollector::new().register_metrics(&registry).is_ok());
    }

    #[test]
    fn system_collector_wires_up_every_leaf() {
        let collector = SystemCollector::new();
        let names: Vec<&str> = collector.subs.iter().map(|s| s.name()).collect();
        assert_eq!(names, vec!["system.cpu", "system.memory", "system.process"]);
    }

    #[test]
    fn reset_metrics_fans_out_to_children() {
        let collector = SystemCollector::new();
        let registry = Registry::new();
        collector.register_metrics(&registry).unwrap();

        // Nothing panics and the umbrella delegates to every child.
        Collector::reset_metrics(&collector);
    }

    /// A sub-collector `Err` must not fail the umbrella.
    ///
    /// The registry withholds every database-dependent family for a scrape in which any
    /// collector returned `Err`, so propagating here would let an optional host-metrics
    /// collector blank out the MariaDB metrics — the exact inversion this change set exists
    /// to prevent. All three real subs are infallible today; this pins the contract for the
    /// next one that is not.
    #[tokio::test]
    async fn a_failing_sub_collector_does_not_fail_the_system_scrape() {
        struct Failing;

        impl Collector for Failing {
            fn name(&self) -> &'static str {
                "system.failing"
            }

            fn register_metrics(&self, _registry: &Registry) -> Result<()> {
                Ok(())
            }

            fn collect_once<'a>(&'a self, _pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
                Box::pin(async move { Err(anyhow::anyhow!("sub-collector exploded")) })
            }

            fn reset_metrics(&self) {}
        }

        let collector = SystemCollector {
            subs: vec![Arc::new(Failing)],
        };

        // Lazy: the stub never touches the pool, so no server has to exist.
        let pool = MySqlPool::connect_lazy("mysql://root@127.0.0.1:3306/mysql")
            .expect("a lazy pool needs no server");

        let result = collector.collect_once(&pool).await;

        assert!(
            result.is_ok(),
            "a failing system sub-collector must not fail the umbrella, or the registry would \
             withhold every database-dependent family: {result:?}"
        );
    }

    /// A sub-collector *panic* must not fail the umbrella either.
    ///
    /// The Err-swallowing contract above only holds for panics because the umbrella wraps
    /// each sub future in `catch_unwind`: an uncaught panic unwinds through the
    /// `FuturesUnordered` drain to the registry's own boundary, which marks the whole
    /// `system` collector failed — and a failed collector withholds every
    /// database-dependent family for that scrape. The real subs panic only inside
    /// `blocking::offload_coalesced` (which converts to `Err` on its own); this stub
    /// panics in the async glue, the one place a real sub could still unwind through the
    /// umbrella.
    #[tokio::test]
    #[allow(clippy::panic)]
    async fn a_panicking_sub_collector_does_not_fail_the_system_scrape() {
        struct Panicking;

        impl Collector for Panicking {
            fn name(&self) -> &'static str {
                "system.panicking"
            }

            fn register_metrics(&self, _registry: &Registry) -> Result<()> {
                Ok(())
            }

            fn collect_once<'a>(&'a self, _pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
                Box::pin(async move { panic!("sub exploded") })
            }

            fn reset_metrics(&self) {}
        }

        let collector = SystemCollector {
            subs: vec![Arc::new(Panicking)],
        };

        // Lazy: the stub never touches the pool, so no server has to exist.
        let pool = MySqlPool::connect_lazy("mysql://root@127.0.0.1:3306/mysql")
            .expect("a lazy pool needs no server");

        let result = collector.collect_once(&pool).await;

        assert!(
            result.is_ok(),
            "a panicking system sub-collector must not fail the umbrella: {result:?}"
        );
    }

    /// A sub-collector that panics *synchronously* while constructing its future must be
    /// contained just like a panic inside the future.
    ///
    /// Wrapping only the returned future is not enough: evaluating `sub.collect(pool)`
    /// happens before any future exists, so an overridden `collect` — or a synchronous
    /// panic while constructing that future — escapes the umbrella. Deferring the call
    /// into the wrapped `async` block puts construction inside the boundary.
    #[tokio::test]
    #[allow(clippy::panic)]
    async fn a_sub_panic_while_constructing_its_future_is_contained() {
        struct PanickingConstructor;

        impl Collector for PanickingConstructor {
            fn name(&self) -> &'static str {
                "system.panicking_constructor"
            }

            fn register_metrics(&self, _registry: &Registry) -> Result<()> {
                Ok(())
            }

            fn collect_once<'a>(&'a self, _pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
                Box::pin(async move { Ok(Collected::Fresh) })
            }

            fn reset_metrics(&self) {}

            fn collect<'a>(&'a self, _pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
                panic!("sub exploded while constructing collect future")
            }
        }

        let collector = SystemCollector {
            subs: vec![Arc::new(PanickingConstructor)],
        };

        // Lazy: the stub never touches the pool, so no server has to exist.
        let pool = MySqlPool::connect_lazy("mysql://root@127.0.0.1:3306/mysql")
            .expect("a lazy pool needs no server");

        let result = collector.collect_once(&pool).await;

        assert!(
            result.is_ok(),
            "future-construction panic must degrade to a sub-collector warning: {result:?}"
        );
    }
}
