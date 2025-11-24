use crate::collectors::Collector;
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{IntGauge, Registry};
use sqlx::MySqlPool;
use tracing::{info_span, instrument};
use tracing_futures::Instrument as _;

/// Lock/wait visibility from `performance_schema` (opt-in).
#[derive(Clone)]
pub struct LocksCollector {
    metadata_lock_count: IntGauge,
    table_lock_waits: IntGauge,
}

impl LocksCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    /// Create a new locks collector.
    ///
    /// # Panics
    ///
    /// Panics if metric names are invalid (should not occur with static names).
    pub fn new() -> Self {
        Self {
            metadata_lock_count: IntGauge::new(
                "mariadb_perf_schema_metadata_lock_count",
                "Number of metadata locks currently listed",
            )
            .expect("valid mariadb_perf_schema_metadata_lock_count metric"),
            table_lock_waits: IntGauge::new(
                "mariadb_perf_schema_table_lock_waits",
                "Number of table lock waits observed (performance_schema)",
            )
            .expect("valid mariadb_perf_schema_table_lock_waits metric"),
        }
    }
}

impl Default for LocksCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for LocksCollector {
    fn name(&self) -> &'static str {
        "locks"
    }

    #[instrument(
        skip(self, registry),
        level = "info",
        err,
        fields(collector = "locks")
    )]
    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.metadata_lock_count.clone()))?;
        registry.register(Box::new(self.table_lock_waits.clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "locks", otel.kind = "internal"))]
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let meta_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "count metadata locks",
                otel.kind = "client"
            );

            let meta_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM performance_schema.metadata_locks",
            )
            .fetch_one(pool)
            .instrument(meta_span)
            .await
            .unwrap_or(0);

            self.metadata_lock_count.set(meta_count);

            let table_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "sum table lock waits",
                otel.kind = "client"
            );

            let table_waits: i64 = sqlx::query_scalar(
                "SELECT COALESCE(SUM(COUNT_STAR),0)
                 FROM performance_schema.table_lock_waits_summary_global",
            )
            .fetch_one(pool)
            .instrument(table_span)
            .await
            .unwrap_or(0);

            self.table_lock_waits.set(table_waits);

            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
