use crate::collectors::{Collected, Collector};
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
        self.metadata_locks.register_metrics(registry)?;
        self.table_lock_waits.register_metrics(registry)?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "locks", otel.kind = "internal"))]
    fn collect_once<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
        Box::pin(async move {
            // Each lock source settles on its own: a missing `metadata_locks` table must not
            // erase freshly collected table-lock-wait data, and vice versa.
            self.metadata_locks.collect(pool).await?;
            self.table_lock_waits.collect(pool).await?;
            Ok(Collected::Fresh)
        })
    }

    /// Fans out to the sub-collectors; this umbrella owns no metrics itself.
    fn reset_metrics(&self) {
        Collector::reset_metrics(&self.metadata_locks);
        Collector::reset_metrics(&self.table_lock_waits);
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
