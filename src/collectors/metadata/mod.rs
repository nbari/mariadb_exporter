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

/// Metadata lock info (opt-in; requires `metadata_lock_info` plugin).
#[derive(Clone)]
pub struct MetadataCollector {
    lock_info_count: IntGaugeVec,
    denied: DeniedOnce,
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
                    "Count of metadata locks by mode and type (metadata_lock_info plugin)",
                ),
                &["mode", "type"],
            )
            .expect("valid mariadb_metadata_lock_info_count metric"),
            denied: DeniedOnce::default(),
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
    fn collect_once<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
        Box::pin(async move {
            let exists_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "check metadata_lock_info table",
                otel.kind = "client"
            );

            // A failing feature probe is a fault, not an absent plugin: it must not be
            // laundered into "the plugin is missing" by an `unwrap_or(0)` fallback.
            let has_table = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema='information_schema' AND table_name='METADATA_LOCK_INFO'",
            )
            .fetch_one(pool)
            .instrument(exists_span)
            .await?
                > 0;

            if !has_table {
                debug!("metadata_lock_info plugin not present; skipping");
                return Ok(Collected::Skipped);
            }

            let span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "SELECT LOCK_MODE, LOCK_TYPE, COUNT(*) FROM information_schema.metadata_lock_info GROUP BY LOCK_MODE, LOCK_TYPE",
                otel.kind = "client"
            );

            let rows = match sqlx::query_as::<_, (Option<String>, Option<String>, i64)>(
                "SELECT LOCK_MODE, LOCK_TYPE, COUNT(*) as cnt FROM information_schema.metadata_lock_info GROUP BY LOCK_MODE, LOCK_TYPE",
            )
            .fetch_all(pool)
            .instrument(span)
            .await
            {
                Ok(r) => r,
                Err(e) => match classify_query_error(&e) {
                    QueryFailure::Absent => {
                        debug!(error = %e, "metadata_lock_info unavailable; skipping");
                        return Ok(Collected::Skipped);
                    }
                    QueryFailure::Denied => {
                        self.denied
                            .report("information_schema.metadata_lock_info", &e);
                        return Ok(Collected::Skipped);
                    }
                    QueryFailure::Fault => return Err(e.into()),
                },
            };

            // Reset only after the fallible read succeeded, immediately before publishing,
            // so an error can never destroy the last good snapshot. A successful empty
            // result is a fresh empty snapshot and correctly clears vanished labels.
            self.lock_info_count.reset();

            for (lock_mode, lock_type, cnt) in rows {
                let mode = lock_mode.unwrap_or_else(|| "unknown".to_string());
                let ltype = lock_type.unwrap_or_else(|| "unknown".to_string());
                self.lock_info_count
                    .with_label_values(&[mode.as_str(), ltype.as_str()])
                    .set(cnt);
            }

            Ok(Collected::Fresh)
        })
    }

    fn reset_metrics(&self) {
        self.lock_info_count.reset();
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
