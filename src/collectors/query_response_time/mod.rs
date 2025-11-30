use crate::collectors::Collector;
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{IntGaugeVec, Opts, Registry};
use sqlx::MySqlPool;
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;

/// Query response time plugin metrics (opt-in; skipped if plugin not installed).
#[derive(Clone)]
pub struct QueryResponseTimeCollector {
    response_time_seconds: IntGaugeVec,
}

impl QueryResponseTimeCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    /// Create a new query response time collector.
    ///
    /// # Panics
    ///
    /// Panics if metric names are invalid (should not occur with static names).
    pub fn new() -> Self {
        let response_time_seconds = IntGaugeVec::new(
            Opts::new(
                "mariadb_info_schema_query_response_time_seconds",
                "Query response time histogram buckets from query_response_time plugin",
            ),
            &["le"],
        )
        .expect("valid mariadb_info_schema_query_response_time_seconds metric");

        Self {
            response_time_seconds,
        }
    }
}

impl Default for QueryResponseTimeCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for QueryResponseTimeCollector {
    fn name(&self) -> &'static str {
        "query_response_time"
    }

    #[instrument(
        skip(self, registry),
        level = "info",
        err,
        fields(collector = "query_response_time")
    )]
    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.response_time_seconds.clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "query_response_time", otel.kind = "internal"))]
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // Confirm plugin table exists.
            let exists_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "check QUERY_RESPONSE_TIME table",
                otel.kind = "client"
            );

            let has_table = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema='information_schema' AND table_name='QUERY_RESPONSE_TIME'",
            )
            .fetch_one(pool)
            .instrument(exists_span)
            .await
            .unwrap_or(0)
                > 0;

            if !has_table {
                debug!("query_response_time plugin not present; skipping collection");
                return Ok(());
            }

            let span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "SELECT TIME, COUNT FROM information_schema.QUERY_RESPONSE_TIME",
                otel.kind = "client"
            );

            let rows = match sqlx::query_as::<_, (String, u64)>(
                "SELECT TIME, COUNT FROM information_schema.QUERY_RESPONSE_TIME",
            )
            .fetch_all(pool)
            .instrument(span)
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("Query response time query failed: {}", e);
                    vec![]
                }
            };

            for (bucket, count) in rows {
                self.response_time_seconds
                    .with_label_values(&[bucket.as_str()])
                    .set(i64::try_from(count).unwrap_or(i64::MAX));
            }

            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
