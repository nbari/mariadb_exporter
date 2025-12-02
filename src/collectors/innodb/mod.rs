use crate::collectors::Collector;
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::Registry;
use sqlx::MySqlPool;
use tracing::instrument;

pub mod status;
use status::StatusParser;

/// `InnoDB` engine status collector (requires `SHOW ENGINE INNODB STATUS` privilege).
///
/// Parses output from `SHOW ENGINE INNODB STATUS` to extract advanced metrics:
/// - LSN (Log Sequence Number) and checkpoint age
/// - Transaction states and history
/// - Semaphore information
/// - Adaptive hash index stats
#[derive(Clone)]
pub struct InnodbCollector {
    status: StatusParser,
}

impl InnodbCollector {
    #[must_use]
    /// Create a new `InnoDB` collector.
    pub fn new() -> Self {
        Self {
            status: StatusParser::new(),
        }
    }
}

impl Default for InnodbCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for InnodbCollector {
    fn name(&self) -> &'static str {
        "innodb"
    }

    #[instrument(
        skip(self, registry),
        level = "info",
        err,
        fields(collector = "innodb")
    )]
    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.status.lsn_current().clone()))?;
        registry.register(Box::new(self.status.lsn_flushed().clone()))?;
        registry.register(Box::new(self.status.lsn_checkpoint().clone()))?;
        registry.register(Box::new(self.status.checkpoint_age().clone()))?;
        registry.register(Box::new(self.status.active_transactions().clone()))?;
        registry.register(Box::new(self.status.semaphore_waits().clone()))?;
        registry.register(Box::new(self.status.semaphore_wait_time_ms().clone()))?;
        registry.register(Box::new(self.status.adaptive_hash_searches().clone()))?;
        registry.register(Box::new(self.status.adaptive_hash_searches_btree().clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "innodb", otel.kind = "internal"))]
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.status.collect(pool).await?;
            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
