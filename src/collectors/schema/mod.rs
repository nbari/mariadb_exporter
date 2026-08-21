use crate::collectors::{Collected, Collector};
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::Registry;
use sqlx::MySqlPool;
use tracing::instrument;

pub mod tables;
use tables::TablesCollector;

/// Basic schema/table size metrics (opt-in; limited to avoid high cardinality).
#[derive(Clone)]
pub struct SchemaCollector {
    tables: TablesCollector,
}

impl SchemaCollector {
    #[must_use]
    /// Create a new schema collector.
    pub fn new() -> Self {
        Self {
            tables: TablesCollector::new(),
        }
    }

    /// Access the tables sub-collector.
    #[must_use]
    pub const fn tables(&self) -> &TablesCollector {
        &self.tables
    }
}

impl Default for SchemaCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for SchemaCollector {
    fn name(&self) -> &'static str {
        "schema"
    }

    #[instrument(
        skip(self, registry),
        level = "info",
        err,
        fields(collector = "schema")
    )]
    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        self.tables.register_metrics(registry)?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "schema", otel.kind = "internal"))]
    fn collect_once<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
        Box::pin(async move {
            self.tables.collect(pool).await?;
            Ok(Collected::Fresh)
        })
    }

    /// Fans out to the sub-collector; this umbrella owns no metrics itself.
    fn reset_metrics(&self) {
        Collector::reset_metrics(&self.tables);
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
