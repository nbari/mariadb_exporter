use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::Registry;
use sqlx::MySqlPool;
use std::collections::HashMap;

#[macro_use]
mod register_macro;

/// Outcome of one collection attempt.
///
/// `Skipped` means the collector published **nothing at all** this scrape — an uninstalled
/// plugin, an absent `performance_schema` table, a disabled feature, a revoked privilege.
/// A collector that refreshed part of its surface and skipped the rest must report `Fresh`,
/// or [`Collector::collect`] would clear the values it just published.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Collected {
    /// Everything this collector publishes is current.
    Fresh,
    /// Nothing was published; previously published series must not persist.
    Skipped,
}

pub trait Collector {
    fn name(&self) -> &'static str;

    /// Register metrics with the prometheus registry
    ///
    /// # Errors
    ///
    /// Returns an error if metric registration fails
    fn register_metrics(&self, registry: &Registry) -> Result<()>;

    /// Collect once, reporting whether anything was published.
    ///
    /// This is the implementation hook; callers should use [`Collector::collect`], which
    /// also settles a skip.
    ///
    /// # Errors
    ///
    /// Return an error for a **genuine fault**. [`Collector::collect`] does not clear on an
    /// error, so the registry keeps the last good snapshot and the series resumes on the
    /// next successful scrape. The errored scrape itself serves no database sample — the
    /// registry aggregates the failure and withholds every database-dependent metric family
    /// for that scrape (see [`registry::CollectorRegistry::collect_all`]) — so the choice is
    /// not "stale data versus none", it is "the series resumes versus the series was
    /// cleared".
    ///
    /// [`Collected::Skipped`] is *not* the safe default for anything that failed — it
    /// **clears** the collector's metrics, so using it for a transient fault destroys the
    /// previous snapshot instead of preserving it. Reserve it for conditions where the data
    /// is known to be unavailable rather than merely unread:
    ///
    /// - a successful feature probe reporting the plugin/feature as absent or disabled
    /// - a missing optional table, plugin, schema or system variable
    ///   ([`util::QueryFailure::Absent`])
    /// - the account lacks the privilege to read it ([`util::QueryFailure::Denied`])
    ///
    /// Classify with [`util::classify_query_error`], which keys on the `MariaDB` error
    /// number; never treat "the query returned an error" as a skip, and never turn a query
    /// failure into a successful absence with `unwrap_or(0)`, `vec![]` or a similar empty
    /// fallback.
    ///
    /// A server that is already unreachable is handled earlier and more cheaply: the
    /// registry runs a connectivity check first and serves `mariadb_up 0` instead of running
    /// database collectors at all. But that check only proves the server was reachable *at
    /// that moment*: a connection dropped mid-scrape, a killed session, or a restart between
    /// the check and this query all surface here as an ordinary query error. Those are
    /// faults, and reporting them as such is correct — it keeps the previous snapshot, where
    /// a skip would delete it.
    // lifetime 'a is needed to tie the future to the lifetime of self and pool
    fn collect_once<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>>;

    /// Stop asserting values from an earlier scrape.
    ///
    /// Required on purpose: every collector must decide what a skip means for it.
    ///
    /// Labeled metrics: `reset()` removes the children, so the series disappear. Scalar
    /// metrics cannot be removed while registered, so zeroing one is a *claim* rather than
    /// an absence — a zeroed `mariadb_ssl_server_configured` says "TLS is off" and a zeroed
    /// `mariadb_replica_seconds_behind_master_seconds` says "fully caught up". Skip-capable
    /// scalars are therefore declared as zero-label metric vectors (identical on the wire,
    /// removable via `reset()`); a collector with no skip path may implement this as a
    /// documented no-op.
    fn reset_metrics(&self);

    fn enabled_by_default(&self) -> bool {
        false
    }

    /// Collect, then settle the result. **This is the entry point callers should use.**
    ///
    /// Clears the collector's metrics after a [`Collected::Skipped`], so a skip cannot keep
    /// serving stale values, and deliberately does **not** clear after an error: a failed
    /// scrape preserves the last good snapshot rather than blanking it.
    ///
    /// # Errors
    ///
    /// Propagates whatever `collect_once` returned.
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>>
    where
        Self: Sync,
    {
        Box::pin(async move {
            if matches!(self.collect_once(pool).await?, Collected::Skipped) {
                self.reset_metrics();
            }
            Ok(())
        })
    }
}

/// Label values for a zero-label metric vector.
///
/// Skip-capable scalar metrics are declared as `*Vec` with an empty label set: the wire
/// format is identical to a plain scalar (prometheus emits no `{}` for an empty label set),
/// but `reset()` removes the child so the series genuinely disappears on a skip instead of
/// being zeroed — a zeroed gauge is a claim, an absent one is not. An empty slice literal
/// cannot infer its element type, hence the named constant.
pub const NO_LABELS: [&str; 0] = [];

/// Number of samples currently published by a metric, for settlement assertions.
///
/// `Collector::collect` (this crate) and `prometheus::core::Collector::collect` share a name,
/// so tests use this helper instead of importing the prometheus trait everywhere.
#[cfg(test)]
pub(crate) fn published_samples<M: prometheus::core::Collector>(metric: &M) -> usize {
    metric
        .collect()
        .iter()
        .map(|family| family.get_metric().len())
        .sum()
}

// Make utils available to all collectors (exclusions, etc.)
pub mod util;

/// Convert i64 to f64 for Prometheus metrics.
///
/// This conversion is safe for `MariaDB` metric values because:
/// - Values are typically small (row counts, connections, etc.)
/// - f64 has 52-bit mantissa precision, accurate up to 2^53 (9 quadrillion)
/// - `MariaDB` metrics will never realistically exceed this threshold
///
/// # Arguments
/// * `value` - The i64 value to convert
///
/// # Returns
/// The f64 representation of the value
#[inline]
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub const fn i64_to_f64(value: i64) -> f64 {
    value as f64
}

// THIS IS THE ONLY PLACE YOU NEED TO ADD NEW COLLECTORS
register_collectors! {
    default => DefaultCollector,
    exporter => ExporterCollector,
    tls => TlsCollector,
    query_response_time => QueryResponseTimeCollector,
    statements => StatementsCollector,
    schema => SchemaCollector,
    replication => ReplicationCollector,
    locks => LocksCollector,
    metadata => MetadataCollector,
    userstat => UserStatCollector,
    innodb => InnodbCollector,
    system => SystemCollector,
    // Add more collectors here - just follow the same pattern!
}

// Other modules
pub mod config;
pub mod registry;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod settlement_contract {
    use super::{Collected, Collector, NO_LABELS};
    use anyhow::{Result, anyhow};
    use futures::future::BoxFuture;
    use prometheus::{Encoder as _, IntGauge, IntGaugeVec, Opts, Registry, TextEncoder};
    use sqlx::MySqlPool;
    use sqlx::mysql::MySqlPoolOptions;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// A collector whose outcome the test dictates, so settlement can be observed directly.
    struct Probe {
        metric: IntGaugeVec,
        outcome: Box<dyn Fn() -> Result<Collected> + Send + Sync>,
        resets: AtomicUsize,
    }

    impl Probe {
        fn new(
            name: &str,
            outcome: impl Fn() -> Result<Collected> + Send + Sync + 'static,
        ) -> Self {
            let metric = IntGaugeVec::new(Opts::new(name, "probe"), &["key"]).unwrap();
            metric.with_label_values(&["a"]).set(7);
            Self {
                metric,
                outcome: Box::new(outcome),
                resets: AtomicUsize::new(0),
            }
        }

        fn samples(&self) -> usize {
            super::published_samples(&self.metric)
        }
    }

    impl Collector for Probe {
        fn name(&self) -> &'static str {
            "probe"
        }

        fn register_metrics(&self, registry: &Registry) -> Result<()> {
            registry.register(Box::new(self.metric.clone()))?;
            Ok(())
        }

        fn collect_once<'a>(&'a self, _pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
            Box::pin(async move { (self.outcome)() })
        }

        fn reset_metrics(&self) {
            self.resets.fetch_add(1, Ordering::Relaxed);
            self.metric.reset();
        }
    }

    fn dummy_pool() -> MySqlPool {
        // Never connected to: every probe resolves without touching the database.
        MySqlPoolOptions::new()
            .connect_lazy("mysql://root@127.0.0.1:1/none")
            .unwrap()
    }

    #[tokio::test]
    async fn fresh_does_not_reset() {
        let probe = Probe::new("probe_fresh", || Ok(Collected::Fresh));

        probe.collect(&dummy_pool()).await.unwrap();

        assert_eq!(probe.resets.load(Ordering::Relaxed), 0);
        assert_eq!(probe.samples(), 1, "a fresh scrape keeps its samples");
    }

    #[tokio::test]
    async fn skipped_resets_and_removes_samples() {
        let probe = Probe::new("probe_skipped", || Ok(Collected::Skipped));

        probe.collect(&dummy_pool()).await.unwrap();

        assert_eq!(probe.resets.load(Ordering::Relaxed), 1);
        assert_eq!(
            probe.samples(),
            0,
            "a skipped source must not keep serving its previous values"
        );
    }

    #[tokio::test]
    async fn error_propagates_without_resetting() {
        let probe = Probe::new("probe_error", || Err(anyhow!("transient fault")));

        let err = probe.collect(&dummy_pool()).await.unwrap_err();

        assert!(err.to_string().contains("transient fault"));
        assert_eq!(probe.resets.load(Ordering::Relaxed), 0);
        assert_eq!(
            probe.samples(),
            1,
            "an error preserves the last good snapshot"
        );
    }

    /// An umbrella that settles each child through the safe `collect`, exactly as the real
    /// composite collectors do.
    struct Umbrella {
        children: Vec<Arc<dyn Collector + Send + Sync>>,
    }

    impl Collector for Umbrella {
        fn name(&self) -> &'static str {
            "umbrella"
        }

        fn register_metrics(&self, registry: &Registry) -> Result<()> {
            for child in &self.children {
                child.register_metrics(registry)?;
            }
            Ok(())
        }

        fn collect_once<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
            Box::pin(async move {
                for child in &self.children {
                    child.collect(pool).await?;
                }
                Ok(Collected::Fresh)
            })
        }

        fn reset_metrics(&self) {
            for child in &self.children {
                child.reset_metrics();
            }
        }
    }

    #[tokio::test]
    async fn one_skipped_child_cannot_clear_a_fresh_sibling() {
        let skipped = Arc::new(Probe::new("probe_child_skipped", || Ok(Collected::Skipped)));
        let fresh = Arc::new(Probe::new("probe_child_fresh", || Ok(Collected::Fresh)));
        let umbrella = Umbrella {
            children: vec![skipped.clone(), fresh.clone()],
        };

        umbrella.collect(&dummy_pool()).await.unwrap();

        assert_eq!(skipped.samples(), 0, "the skipped source is cleared");
        assert_eq!(
            fresh.samples(),
            1,
            "a skipped sibling must not erase freshly published data"
        );
    }

    #[test]
    fn zero_label_vector_is_wire_compatible_with_a_scalar_and_removable() {
        let scalar_registry = Registry::new();
        let scalar = IntGauge::new("mariadb_probe_metric", "probe help").unwrap();
        scalar.set(42);
        scalar_registry.register(Box::new(scalar)).unwrap();

        let vector_registry = Registry::new();
        let vector =
            IntGaugeVec::new(Opts::new("mariadb_probe_metric", "probe help"), &NO_LABELS).unwrap();
        vector.with_label_values(&NO_LABELS).set(42);
        vector_registry.register(Box::new(vector.clone())).unwrap();

        let encode = |registry: &Registry| {
            let mut buffer = Vec::new();
            TextEncoder::new()
                .encode(&registry.gather(), &mut buffer)
                .unwrap();
            String::from_utf8(buffer).unwrap()
        };

        let scalar_text = encode(&scalar_registry);
        assert_eq!(
            scalar_text,
            encode(&vector_registry),
            "converting a scalar to a zero-label vector must not change the wire format"
        );
        assert!(scalar_text.contains("mariadb_probe_metric 42\n"));
        assert!(
            !scalar_text.contains("{}"),
            "an empty label set must not render braces"
        );

        // And unlike a scalar, it can be removed: the family gathers empty, so nothing at
        // all is exposed for it.
        vector.reset();
        assert_eq!(
            encode(&vector_registry),
            "",
            "reset() must make the series disappear from the exposition"
        );
    }

    /// Guards the trait shape: `reset_metrics` has no default, so every implementation is
    /// forced to state what a skip means for it. A regression here (adding a default) would
    /// silently reintroduce stale metrics for any new collector.
    #[test]
    fn reset_is_wired_through_the_safe_entry_point() {
        static CALLED: AtomicBool = AtomicBool::new(false);

        struct Marker;
        impl Collector for Marker {
            fn name(&self) -> &'static str {
                "marker"
            }
            fn register_metrics(&self, _registry: &Registry) -> Result<()> {
                Ok(())
            }
            fn collect_once<'a>(
                &'a self,
                _pool: &'a MySqlPool,
            ) -> BoxFuture<'a, Result<Collected>> {
                Box::pin(async move { Ok(Collected::Skipped) })
            }
            fn reset_metrics(&self) {
                CALLED.store(true, Ordering::Relaxed);
            }
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(async { Marker.collect(&dummy_pool()).await })
            .unwrap();

        assert!(CALLED.load(Ordering::Relaxed));
    }
}
