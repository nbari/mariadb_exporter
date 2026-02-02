use crate::collectors::Collector;
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::Registry;
use sqlx::MySqlPool;
use tracing::instrument;

pub mod binlog;
pub mod replica_status;

use binlog::BinlogCollector;
use replica_status::ReplicaStatusCollector;

/// Additional replication details (opt-in; noop on non-replicas).
#[derive(Clone)]
pub struct ReplicationCollector {
    replica_status: ReplicaStatusCollector,
    binlog: BinlogCollector,
}

impl ReplicationCollector {
    #[must_use]
    /// Create a new replication collector.
    pub fn new() -> Self {
        Self {
            replica_status: ReplicaStatusCollector::new(),
            binlog: BinlogCollector::new(),
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
        // Replica status metrics
        registry.register(Box::new(self.replica_status.relay_log_space().clone()))?;
        registry.register(Box::new(self.replica_status.relay_log_pos().clone()))?;
        registry.register(Box::new(self.replica_status.seconds_behind_master().clone()))?;
        registry.register(Box::new(self.replica_status.io_running().clone()))?;
        registry.register(Box::new(self.replica_status.sql_running().clone()))?;
        registry.register(Box::new(self.replica_status.last_io_errno().clone()))?;
        registry.register(Box::new(self.replica_status.last_sql_errno().clone()))?;
        registry.register(Box::new(self.replica_status.master_server_id().clone()))?;
        registry.register(Box::new(self.replica_status.replica_configured().clone()))?;

        // Binlog metrics
        registry.register(Box::new(self.binlog.binlog_files().clone()))?;

        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "replication", otel.kind = "internal"))]
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.replica_status.collect(pool).await?;
            self.binlog.collect(pool).await?;
            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
