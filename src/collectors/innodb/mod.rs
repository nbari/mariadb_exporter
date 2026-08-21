use crate::collectors::{Collected, Collector};
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

    /// Access the underlying status parser.
    #[must_use]
    pub const fn status(&self) -> &StatusParser {
        &self.status
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
        self.status.register_metrics(registry)?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "innodb", otel.kind = "internal"))]
    fn collect_once<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
        Box::pin(async move {
            self.status.collect(pool).await?;
            Ok(Collected::Fresh)
        })
    }

    /// Fans out to the sub-collector; this umbrella owns no metrics itself.
    fn reset_metrics(&self) {
        Collector::reset_metrics(&self.status);
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
