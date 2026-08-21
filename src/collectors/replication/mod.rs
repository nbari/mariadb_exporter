use crate::collectors::{Collected, Collector};
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
        self.replica_status.register_metrics(registry)?;
        self.binlog.register_metrics(registry)?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "replication", otel.kind = "internal"))]
    fn collect_once<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
        Box::pin(async move {
            // Replica state and binary-log state are independent sources: unavailable binlog
            // data must not clear valid replica-state data, and vice versa. Calling the safe
            // `collect` on each child settles them separately.
            self.replica_status.collect(pool).await?;
            self.binlog.collect(pool).await?;
            Ok(Collected::Fresh)
        })
    }

    /// Fans out to the sub-collectors; this umbrella owns no metrics itself.
    fn reset_metrics(&self) {
        Collector::reset_metrics(&self.replica_status);
        Collector::reset_metrics(&self.binlog);
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
