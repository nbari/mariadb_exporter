use crate::collectors::{
    Collected, Collector, NO_LABELS,
    util::{DeniedOnce, QueryFailure, classify_query_error},
};
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{IntGaugeVec, Opts, Registry};
use sqlx::MySqlPool;
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;

/// Collector for metadata locks from `performance_schema`.
///
/// `performance_schema.metadata_locks` is optional: it is absent when the Performance Schema
/// is compiled out and unreadable without `SELECT` on `performance_schema`. Either way the
/// count is unknown, not zero, so the metric is a zero-label vector that disappears rather
/// than asserting "no metadata locks are held".
#[derive(Clone)]
pub struct MetadataLocksCollector {
    lock_count: IntGaugeVec,
    denied: DeniedOnce,
}

impl MetadataLocksCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    /// Create a new metadata locks collector.
    ///
    /// # Panics
    ///
    /// Panics if metric names are invalid (should not occur with static names).
    pub fn new() -> Self {
        Self {
            lock_count: IntGaugeVec::new(
                Opts::new(
                    "mariadb_perf_schema_metadata_lock_count",
                    "Number of metadata locks currently listed",
                ),
                &NO_LABELS,
            )
            .expect("valid mariadb_perf_schema_metadata_lock_count metric"),
            denied: DeniedOnce::default(),
        }
    }

    /// Get metadata lock count metric.
    #[must_use]
    pub const fn lock_count(&self) -> &IntGaugeVec {
        &self.lock_count
    }
}

impl Collector for MetadataLocksCollector {
    fn name(&self) -> &'static str {
        "metadata_locks"
    }

    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.lock_count.clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "debug", err, fields(sub_collector = "metadata_locks"))]
    fn collect_once<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
        Box::pin(async move {
            let span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "SELECT COUNT(*) FROM performance_schema.metadata_locks",
                otel.kind = "client"
            );

            let result: Result<i64, sqlx::Error> =
                sqlx::query_scalar("SELECT COUNT(*) FROM performance_schema.metadata_locks")
                    .fetch_one(pool)
                    .instrument(span)
                    .await;

            match result {
                Ok(count) => {
                    self.lock_count.with_label_values(&NO_LABELS).set(count);
                    Ok(Collected::Fresh)
                }
                Err(e) => match classify_query_error(&e) {
                    QueryFailure::Absent => {
                        debug!(error = %e, "performance_schema.metadata_locks not available; skipping");
                        Ok(Collected::Skipped)
                    }
                    QueryFailure::Denied => {
                        self.denied.report("performance_schema.metadata_locks", &e);
                        Ok(Collected::Skipped)
                    }
                    QueryFailure::Fault => Err(e.into()),
                },
            }
        })
    }

    fn reset_metrics(&self) {
        self.lock_count.reset();
    }
}

impl Default for MetadataLocksCollector {
    fn default() -> Self {
        Self::new()
    }
}
