use anyhow::Result;
use prometheus::IntGauge;
use sqlx::MySqlPool;
use tracing::{info_span, instrument};
use tracing_futures::Instrument as _;

/// Collector for metadata locks from `performance_schema`.
#[derive(Clone)]
pub struct MetadataLocksCollector {
    lock_count: IntGauge,
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
            lock_count: IntGauge::new(
                "mariadb_perf_schema_metadata_lock_count",
                "Number of metadata locks currently listed",
            )
            .expect("valid mariadb_perf_schema_metadata_lock_count metric"),
        }
    }

    /// Get metadata lock count metric.
    #[must_use]
    pub const fn lock_count(&self) -> &IntGauge {
        &self.lock_count
    }

    /// Collect metadata lock metrics.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails (though queries are best-effort).
    #[instrument(skip(self, pool), level = "debug", fields(sub_collector = "metadata_locks"))]
    pub async fn collect(&self, pool: &MySqlPool) -> Result<()> {
        let span = info_span!(
            "db.query",
            db.system = "mysql",
            db.operation = "SELECT",
            db.statement = "SELECT COUNT(*) FROM performance_schema.metadata_locks",
            otel.kind = "client"
        );

        let result: Result<i64, _> = sqlx::query_scalar(
            "SELECT COUNT(*) FROM performance_schema.metadata_locks",
        )
        .fetch_one(pool)
        .instrument(span)
        .await;

        match result {
            Ok(count) => {
                self.lock_count.set(count);
            }
            Err(e) => {
                tracing::debug!("Metadata locks (performance_schema) not available: {}", e);
            }
        }

        Ok(())
    }
}

impl Default for MetadataLocksCollector {
    fn default() -> Self {
        Self::new()
    }
}
