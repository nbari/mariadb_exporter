use crate::collectors::Collector;
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::Registry;
use sqlx::MySqlPool;
use tracing::instrument;

pub mod metadata;
pub mod table_waits;

use metadata::MetadataLocksCollector;
use table_waits::TableLockWaitsCollector;

/// Lock/wait visibility from `performance_schema` (opt-in).
#[derive(Clone)]
pub struct LocksCollector {
    metadata_locks: MetadataLocksCollector,
    table_lock_waits: TableLockWaitsCollector,
}

impl LocksCollector {
    #[must_use]
    /// Create a new locks collector.
    pub fn new() -> Self {
        Self {
            metadata_locks: MetadataLocksCollector::new(),
            table_lock_waits: TableLockWaitsCollector::new(),
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
        registry.register(Box::new(self.metadata_locks.lock_count().clone()))?;
        registry.register(Box::new(self.table_lock_waits.lock_waits().clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "locks", otel.kind = "internal"))]
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.metadata_locks.collect(pool).await?;
            self.table_lock_waits.collect(pool).await?;
            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
