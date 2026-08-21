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

/// Collector for primary binlog metrics (SHOW BINARY LOGS).
///
/// Binary logging is optional and `SHOW BINARY LOGS` needs the `BINLOG MONITOR` privilege.
/// Reporting `0` files in either case would be a lie — a primary with binary logging off has
/// no file count at all, and an unprivileged exporter simply does not know it. The metric is
/// therefore a zero-label vector that disappears on a skip.
#[derive(Clone)]
pub struct BinlogCollector {
    binlog_files: IntGaugeVec,
    denied: DeniedOnce,
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
            binlog_files: IntGaugeVec::new(
                Opts::new(
                    "mariadb_primary_binlog_files",
                    "Number of binlog files on primary (requires binary logging)",
                ),
                &NO_LABELS,
            )
            .expect("valid mariadb_primary_binlog_files metric"),
            denied: DeniedOnce::default(),
        }
    }

    /// Get binlog files metric.
    #[must_use]
    pub const fn binlog_files(&self) -> &IntGaugeVec {
        &self.binlog_files
    }
}

impl Collector for BinlogCollector {
    fn name(&self) -> &'static str {
        "binlog"
    }

    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.binlog_files.clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "debug", err, fields(sub_collector = "binlog"))]
    fn collect_once<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
        Box::pin(async move {
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
                Ok(rows) => {
                    self.binlog_files
                        .with_label_values(&NO_LABELS)
                        .set(i64::try_from(rows.len()).unwrap_or(i64::MAX));
                    Ok(Collected::Fresh)
                }
                Err(e) => match classify_query_error(&e) {
                    // `ER_NO_BINARY_LOGGING` (1381) and friends: the source does not exist.
                    QueryFailure::Absent => {
                        debug!(error = %e, "binary logging disabled; skipping binlog count");
                        Ok(Collected::Skipped)
                    }
                    QueryFailure::Denied => {
                        self.denied.report("SHOW BINARY LOGS", &e);
                        Ok(Collected::Skipped)
                    }
                    QueryFailure::Fault => Err(e.into()),
                },
            }
        })
    }

    fn reset_metrics(&self) {
        self.binlog_files.reset();
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}

impl Default for BinlogCollector {
    fn default() -> Self {
        Self::new()
    }
}
