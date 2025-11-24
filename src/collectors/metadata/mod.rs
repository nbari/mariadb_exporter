use crate::collectors::Collector;
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{IntGaugeVec, Opts, Registry};
use sqlx::MySqlPool;
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;

/// Metadata lock info (opt-in; requires `metadata_lock_info` plugin).
#[derive(Clone)]
pub struct MetadataCollector {
    lock_info_count: IntGaugeVec,
}

impl MetadataCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    /// Create a new metadata collector.
    ///
    /// # Panics
    ///
    /// Panics if metric names are invalid (should not occur with static names).
    pub fn new() -> Self {
        Self {
            lock_info_count: IntGaugeVec::new(
                Opts::new(
                    "mariadb_metadata_lock_info_count",
                    "Count of metadata locks by status/type (metadata_lock_info plugin)",
                ),
                &["status", "type"],
            )
            .expect("valid mariadb_metadata_lock_info_count metric"),
        }
    }
}

impl Default for MetadataCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for MetadataCollector {
    fn name(&self) -> &'static str {
        "metadata"
    }

    #[instrument(
        skip(self, registry),
        level = "info",
        err,
        fields(collector = "metadata")
    )]
    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.lock_info_count.clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "metadata", otel.kind = "internal"))]
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let exists_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "check metadata_lock_info table",
                otel.kind = "client"
            );

            let has_table = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema='information_schema' AND table_name='METADATA_LOCK_INFO'",
            )
            .fetch_one(pool)
            .instrument(exists_span)
            .await
            .unwrap_or(0)
                > 0;

            if !has_table {
                debug!("metadata_lock_info plugin not present; skipping");
                return Ok(());
            }

            let span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "SELECT LOCK_TYPE, LOCK_STATUS, COUNT(*) FROM information_schema.metadata_lock_info GROUP BY LOCK_TYPE, LOCK_STATUS",
                otel.kind = "client"
            );

            let rows = sqlx::query_as::<_, (Option<String>, Option<String>, i64)>(
                "SELECT LOCK_TYPE, LOCK_STATUS, COUNT(*) as cnt FROM information_schema.metadata_lock_info GROUP BY LOCK_TYPE, LOCK_STATUS",
            )
            .fetch_all(pool)
            .instrument(span)
            .await
            .unwrap_or_default();

            for (lock_type, status, cnt) in rows {
                let lt = lock_type.unwrap_or_else(|| "unknown".to_string());
                let st = status.unwrap_or_else(|| "unknown".to_string());
                self.lock_info_count
                    .with_label_values(&[st.as_str(), lt.as_str()])
                    .set(cnt);
            }

            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
