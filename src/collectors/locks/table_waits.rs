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

/// Collector for table lock waits from `performance_schema`.
///
/// `performance_schema.table_lock_waits_summary_global` is optional. When it cannot be read
/// the number of waits is unknown, so the zero-label series disappears instead of reporting
/// a fabricated `0`.
#[derive(Clone)]
pub struct TableLockWaitsCollector {
    lock_waits: IntGaugeVec,
    denied: DeniedOnce,
}

impl TableLockWaitsCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    /// Create a new table lock waits collector.
    ///
    /// # Panics
    ///
    /// Panics if metric names are invalid (should not occur with static names).
    pub fn new() -> Self {
        Self {
            lock_waits: IntGaugeVec::new(
                Opts::new(
                    "mariadb_perf_schema_table_lock_waits",
                    "Number of table lock waits observed (performance_schema)",
                ),
                &NO_LABELS,
            )
            .expect("valid mariadb_perf_schema_table_lock_waits metric"),
            denied: DeniedOnce::default(),
        }
    }

    /// Get table lock waits metric.
    #[must_use]
    pub const fn lock_waits(&self) -> &IntGaugeVec {
        &self.lock_waits
    }
}

impl Collector for TableLockWaitsCollector {
    fn name(&self) -> &'static str {
        "table_lock_waits"
    }

    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.lock_waits.clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "debug", err, fields(sub_collector = "table_lock_waits"))]
    fn collect_once<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
        Box::pin(async move {
            let span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "SELECT CAST(SUM(COUNT_STAR) AS UNSIGNED) FROM performance_schema.table_lock_waits_summary_global",
                otel.kind = "client"
            );

            let result: Result<i64, sqlx::Error> = sqlx::query_scalar(
                "SELECT CAST(COALESCE(SUM(COUNT_STAR),0) AS UNSIGNED)
                 FROM performance_schema.table_lock_waits_summary_global",
            )
            .fetch_one(pool)
            .instrument(span)
            .await;

            match result {
                Ok(waits) => {
                    self.lock_waits.with_label_values(&NO_LABELS).set(waits);
                    Ok(Collected::Fresh)
                }
                Err(e) => match classify_query_error(&e) {
                    QueryFailure::Absent => {
                        debug!(error = %e, "table lock waits (performance_schema) not available; skipping");
                        Ok(Collected::Skipped)
                    }
                    QueryFailure::Denied => {
                        self.denied
                            .report("performance_schema.table_lock_waits_summary_global", &e);
                        Ok(Collected::Skipped)
                    }
                    QueryFailure::Fault => Err(e.into()),
                },
            }
        })
    }

    fn reset_metrics(&self) {
        self.lock_waits.reset();
    }
}

impl Default for TableLockWaitsCollector {
    fn default() -> Self {
        Self::new()
    }
}
