use crate::collectors::{
    Collected, Collector,
    util::{DeniedOnce, QueryFailure, classify_query_error},
};
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{IntGaugeVec, Opts, Registry};
use sqlx::MySqlPool;
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;

/// Table metrics collector for schema information.
#[derive(Clone)]
pub struct TablesCollector {
    table_size_bytes: IntGaugeVec,
    table_rows: IntGaugeVec,
    denied: DeniedOnce,
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
            denied: DeniedOnce::default(),
        }
    }

    /// Collect table size and row count metrics.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails for a reason other than the source
    /// being absent or unreadable.
    #[instrument(skip(self, pool), level = "debug", fields(sub_collector = "tables"))]
    async fn collect_inner(&self, pool: &MySqlPool) -> Result<Collected> {
        // Build exclusion list from constant
        let excluded = crate::collectors::util::SYSTEM_SCHEMAS
            .iter()
            .map(|s| format!("'{s}'"))
            .collect::<Vec<_>>()
            .join(",");

        // Limit to avoid runaway cardinality: sample up to 20 largest tables.
        let span = info_span!(
            "db.query",
            db.system = "mysql",
            db.operation = "SELECT",
            db.statement = "SELECT schema/table sizes",
            otel.kind = "client"
        );

        let query = format!(
            "SELECT TABLE_SCHEMA, TABLE_NAME,
                    CAST(COALESCE(DATA_LENGTH,0) + COALESCE(INDEX_LENGTH,0) AS UNSIGNED) AS size_bytes,
                    CAST(COALESCE(TABLE_ROWS,0) AS UNSIGNED) as rows_est
             FROM information_schema.tables
             WHERE TABLE_SCHEMA NOT IN ({excluded})
             ORDER BY size_bytes DESC
             LIMIT 20"
        );

        let rows = match sqlx::query_as::<_, (String, String, u64, u64)>(sqlx::AssertSqlSafe(query))
            .fetch_all(pool)
            .instrument(span)
            .await
        {
            Ok(r) => r,
            Err(e) => match classify_query_error(&e) {
                QueryFailure::Absent => {
                    debug!(error = %e, "information_schema.tables unavailable; skipping");
                    return Ok(Collected::Skipped);
                }
                QueryFailure::Denied => {
                    self.denied.report("information_schema.tables", &e);
                    return Ok(Collected::Skipped);
                }
                QueryFailure::Fault => return Err(e.into()),
            },
        };

        debug!("Schema collector found {} tables", rows.len());

        // Reset only after the query succeeded and immediately before publishing: a query
        // error must never destroy the last good snapshot, while a successful empty result
        // is a fresh empty snapshot that legitimately clears vanished tables.
        self.table_size_bytes.reset();
        self.table_rows.reset();

        for (schema, table, size_bytes, rows_est) in rows {
            debug!("Setting metrics for {}.{}: size={}, rows={}", schema, table, size_bytes, rows_est);
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

        Ok(Collected::Fresh)
    }

    /// Get the table size metric for registration.
    #[must_use]
    pub const fn table_size_bytes(&self) -> &IntGaugeVec {
        &self.table_size_bytes
    }

    /// Get the table rows metric for registration.
    #[must_use]
    pub const fn table_rows(&self) -> &IntGaugeVec {
        &self.table_rows
    }
}

impl Collector for TablesCollector {
    fn name(&self) -> &'static str {
        "tables"
    }

    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.table_size_bytes.clone()))?;
        registry.register(Box::new(self.table_rows.clone()))?;
        Ok(())
    }

    fn collect_once<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
        Box::pin(async move { self.collect_inner(pool).await })
    }

    fn reset_metrics(&self) {
        self.table_size_bytes.reset();
        self.table_rows.reset();
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}

impl Default for TablesCollector {
    fn default() -> Self {
        Self::new()
    }
}
