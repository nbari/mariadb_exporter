use anyhow::Result;
use prometheus::IntGauge;
use sqlx::MySqlPool;
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;

/// Collector for primary binlog metrics (SHOW BINARY LOGS).
#[derive(Clone)]
pub struct BinlogCollector {
    binlog_files: IntGauge,
}

impl BinlogCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    /// Create a new binlog collector.
    ///
    /// # Panics
    ///
    /// Panics if metric names are invalid (should not occur with static names).
    pub fn new() -> Self {
        Self {
            binlog_files: IntGauge::new(
                "mariadb_primary_binlog_files",
                "Number of binlog files on primary (requires binary logging)",
            )
            .expect("valid mariadb_primary_binlog_files metric"),
        }
    }

    /// Get binlog files metric.
    #[must_use]
    pub const fn binlog_files(&self) -> &IntGauge {
        &self.binlog_files
    }

    /// Collect binlog metrics from SHOW BINARY LOGS.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails (though queries are best-effort).
    #[instrument(skip(self, pool), level = "debug", fields(sub_collector = "binlog"))]
    pub async fn collect(&self, pool: &MySqlPool) -> Result<()> {
        let span = info_span!(
            "db.query",
            db.system = "mysql",
            db.operation = "SHOW",
            db.statement = "SHOW BINARY LOGS",
            otel.kind = "client"
        );

        match sqlx::query("SHOW BINARY LOGS")
            .fetch_all(pool)
            .instrument(span)
            .await
        {
            Ok(rows) => self
                .binlog_files
                .set(i64::try_from(rows.len()).unwrap_or(i64::MAX)),
            Err(e) => {
                debug!(error = %e, "binary logging likely disabled; skipping binlog count");
                self.binlog_files.set(0);
            }
        }

        Ok(())
    }
}

impl Default for BinlogCollector {
    fn default() -> Self {
        Self::new()
    }
}
