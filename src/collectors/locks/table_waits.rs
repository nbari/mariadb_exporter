use anyhow::Result;
use prometheus::IntGauge;
use sqlx::MySqlPool;
use tracing::{info_span, instrument};
use tracing_futures::Instrument as _;

/// Collector for table lock waits from `performance_schema`.
#[derive(Clone)]
pub struct TableLockWaitsCollector {
    lock_waits: IntGauge,
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
            lock_waits: IntGauge::new(
                "mariadb_perf_schema_table_lock_waits",
                "Number of table lock waits observed (performance_schema)",
            )
            .expect("valid mariadb_perf_schema_table_lock_waits metric"),
        }
    }

    /// Get table lock waits metric.
    #[must_use]
    pub const fn lock_waits(&self) -> &IntGauge {
        &self.lock_waits
    }

    /// Collect table lock wait metrics.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails (though queries are best-effort).
    #[instrument(skip(self, pool), level = "debug", fields(sub_collector = "table_lock_waits"))]
    pub async fn collect(&self, pool: &MySqlPool) -> Result<()> {
        let span = info_span!(
            "db.query",
            db.system = "mysql",
            db.operation = "SELECT",
            db.statement = "SELECT CAST(SUM(COUNT_STAR) AS UNSIGNED) FROM performance_schema.table_lock_waits_summary_global",
            otel.kind = "client"
        );

        let result: Result<i64, _> = sqlx::query_scalar(
            "SELECT CAST(COALESCE(SUM(COUNT_STAR),0) AS UNSIGNED)
             FROM performance_schema.table_lock_waits_summary_global",
        )
        .fetch_one(pool)
        .instrument(span)
        .await;

        match result {
            Ok(waits) => {
                self.lock_waits.set(waits);
            }
            Err(e) => {
                tracing::debug!("Table lock waits (performance_schema) not available: {}", e);
            }
        }

        Ok(())
    }
}

impl Default for TableLockWaitsCollector {
    fn default() -> Self {
        Self::new()
    }
}
