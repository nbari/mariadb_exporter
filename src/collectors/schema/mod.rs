use crate::collectors::Collector;
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{IntGaugeVec, Opts, Registry};
use sqlx::MySqlPool;
use tracing::{info_span, instrument};
use tracing_futures::Instrument as _;

/// Basic schema/table size metrics (opt-in; limited to avoid high cardinality).
#[derive(Clone)]
pub struct SchemaCollector {
    table_size_bytes: IntGaugeVec,
    table_rows: IntGaugeVec,
}

impl SchemaCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    /// Create a new schema collector.
    ///
    /// # Panics
    ///
    /// Panics if metric names are invalid (should not occur with static names).
    pub fn new() -> Self {
        let table_size_bytes = IntGaugeVec::new(
            Opts::new(
                "mariadb_info_schema_table_size_bytes",
                "Approximate table size (data+index) in bytes",
            ),
            &["schema", "table"],
        )
        .expect("valid mariadb_info_schema_table_size_bytes metric");

        let table_rows = IntGaugeVec::new(
            Opts::new(
                "mariadb_info_schema_table_rows",
                "Approximate row count per table",
            ),
            &["schema", "table"],
        )
        .expect("valid mariadb_info_schema_table_rows metric");

        Self {
            table_size_bytes,
            table_rows,
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
        registry.register(Box::new(self.table_size_bytes.clone()))?;
        registry.register(Box::new(self.table_rows.clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "schema", otel.kind = "internal"))]
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // Limit to avoid runaway cardinality: sample up to 20 largest tables.
            let span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "SELECT schema/table sizes",
                otel.kind = "client"
            );

            let rows = sqlx::query_as::<_, (String, String, i64, i64)>(
                "SELECT TABLE_SCHEMA, TABLE_NAME,
                        COALESCE(DATA_LENGTH,0) + COALESCE(INDEX_LENGTH,0) AS size_bytes,
                        COALESCE(TABLE_ROWS,0) as rows_est
                 FROM information_schema.tables
                 WHERE TABLE_SCHEMA NOT IN ('mysql', 'performance_schema', 'information_schema', 'sys')
                 ORDER BY size_bytes DESC
                 LIMIT 20",
            )
            .fetch_all(pool)
            .instrument(span)
            .await
            .unwrap_or_default();

            for (schema, table, size_bytes, rows_est) in rows {
                self.table_size_bytes
                    .with_label_values(&[schema.as_str(), table.as_str()])
                    .set(size_bytes);
                self.table_rows
                    .with_label_values(&[schema.as_str(), table.as_str()])
                    .set(rows_est);
            }

            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
