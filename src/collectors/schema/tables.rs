use anyhow::Result;
use prometheus::{IntGaugeVec, Opts};
use sqlx::MySqlPool;
use tracing::{info_span, instrument};
use tracing_futures::Instrument as _;

/// Table metrics collector for schema information.
#[derive(Clone)]
pub struct TablesCollector {
    table_size_bytes: IntGaugeVec,
    table_rows: IntGaugeVec,
}

impl TablesCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    /// Create a new tables collector.
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

    /// Collect table size and row count metrics.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[instrument(skip(self, pool), level = "debug", fields(sub_collector = "tables"))]
    pub async fn collect(&self, pool: &MySqlPool) -> Result<()> {
        // Limit to avoid runaway cardinality: sample up to 20 largest tables.
        let span = info_span!(
            "db.query",
            db.system = "mysql",
            db.operation = "SELECT",
            db.statement = "SELECT schema/table sizes",
            otel.kind = "client"
        );

        let rows = match sqlx::query_as::<_, (String, String, u64, u64)>(
            "SELECT TABLE_SCHEMA, TABLE_NAME,
                    CAST(COALESCE(DATA_LENGTH,0) + COALESCE(INDEX_LENGTH,0) AS UNSIGNED) AS size_bytes,
                    CAST(COALESCE(TABLE_ROWS,0) AS UNSIGNED) as rows_est
             FROM information_schema.tables
             WHERE TABLE_SCHEMA NOT IN ('mysql', 'performance_schema', 'information_schema', 'sys')
             ORDER BY size_bytes DESC
             LIMIT 20",
        )
        .fetch_all(pool)
        .instrument(span)
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Schema collector query failed: {}", e);
                vec![]
            }
        };

        tracing::debug!("Schema collector found {} tables", rows.len());

        for (schema, table, size_bytes, rows_est) in rows {
            tracing::debug!("Setting metrics for {}.{}: size={}, rows={}", schema, table, size_bytes, rows_est);
            #[allow(clippy::cast_possible_wrap)]
            let size_i64 = size_bytes as i64;
            #[allow(clippy::cast_possible_wrap)]
            let rows_i64 = rows_est as i64;
            
            self.table_size_bytes
                .with_label_values(&[schema.as_str(), table.as_str()])
                .set(size_i64);
            self.table_rows
                .with_label_values(&[schema.as_str(), table.as_str()])
                .set(rows_i64);
        }

        Ok(())
    }

    /// Get the table size metric for registration.
    #[must_use]
    pub fn table_size_bytes(&self) -> &IntGaugeVec {
        &self.table_size_bytes
    }

    /// Get the table rows metric for registration.
    #[must_use]
    pub fn table_rows(&self) -> &IntGaugeVec {
        &self.table_rows
    }
}

impl Default for TablesCollector {
    fn default() -> Self {
        Self::new()
    }
}
