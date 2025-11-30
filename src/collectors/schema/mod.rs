use crate::collectors::Collector;
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
        registry.register(Box::new(self.tables.table_size_bytes().clone()))?;
        registry.register(Box::new(self.tables.table_rows().clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "schema", otel.kind = "internal"))]
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.tables.collect(pool).await?;
            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
