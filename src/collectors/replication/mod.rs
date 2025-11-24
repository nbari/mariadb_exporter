use crate::collectors::Collector;
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{IntGauge, Registry};
use sqlx::{MySqlPool, Row};
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;

/// Additional replication details (opt-in; noop on non-replicas).
#[derive(Clone)]
pub struct ReplicationCollector {
    replica_relay_log_space: IntGauge,
    replica_relay_log_pos: IntGauge,
    primary_binlog_files: IntGauge,
}

impl ReplicationCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    /// Create a new replication collector.
    ///
    /// # Panics
    ///
    /// Panics if metric names are invalid (should not occur with static names).
    pub fn new() -> Self {
        Self {
            replica_relay_log_space: IntGauge::new(
                "mariadb_replica_relay_log_space_bytes",
                "Total combined size of relay logs on replica",
            )
            .expect("valid mariadb_replica_relay_log_space_bytes metric"),
            replica_relay_log_pos: IntGauge::new(
                "mariadb_replica_relay_log_pos",
                "Current relay log position",
            )
            .expect("valid mariadb_replica_relay_log_pos metric"),
            primary_binlog_files: IntGauge::new(
                "mariadb_primary_binlog_files",
                "Number of binlog files on primary (requires binary logging)",
            )
            .expect("valid mariadb_primary_binlog_files metric"),
        }
    }
}

impl Default for ReplicationCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for ReplicationCollector {
    fn name(&self) -> &'static str {
        "replication"
    }

    #[instrument(
        skip(self, registry),
        level = "info",
        err,
        fields(collector = "replication")
    )]
    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.replica_relay_log_space.clone()))?;
        registry.register(Box::new(self.replica_relay_log_pos.clone()))?;
        registry.register(Box::new(self.primary_binlog_files.clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "replication", otel.kind = "internal"))]
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // Replica details
            let span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SHOW",
                db.statement = "SHOW SLAVE STATUS",
                otel.kind = "client"
            );

            if let Ok(rows) = sqlx::query("SHOW SLAVE STATUS")
                .fetch_all(pool)
                .instrument(span)
                .await
                && let Some(row) = rows.first()
            {
                let relay_space: Option<i64> = row.try_get("Relay_Log_Space").ok();
                let relay_pos: Option<i64> = row.try_get("Exec_Master_Log_Pos").ok();
                self.replica_relay_log_space
                    .set(relay_space.unwrap_or_default());
                self.replica_relay_log_pos
                    .set(relay_pos.unwrap_or_default());
            }

            // Primary binlog count
            let binlog_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SHOW",
                db.statement = "SHOW BINARY LOGS",
                otel.kind = "client"
            );

            match sqlx::query("SHOW BINARY LOGS")
                .fetch_all(pool)
                .instrument(binlog_span)
                .await
            {
                Ok(rows) => self
                    .primary_binlog_files
                    .set(i64::try_from(rows.len()).unwrap_or(i64::MAX)),
                Err(e) => {
                    debug!(error = %e, "binary logging likely disabled; skipping binlog count");
                    self.primary_binlog_files.set(0);
                }
            }

            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
