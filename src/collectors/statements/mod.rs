use crate::collectors::Collector;
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{IntGauge, IntGaugeVec, Opts, Registry};
use sqlx::MySqlPool;
use tracing::{info_span, instrument};
use tracing_futures::Instrument as _;

/// Statements summary from `performance_schema` (opt-in, lightweight aggregate).
#[derive(Clone)]
pub struct StatementsCollector {
    digest_total: IntGauge,
    digest_errors: IntGauge,
    digest_warnings: IntGauge,
    digest_rows_examined: IntGauge,
    digest_rows_sent: IntGauge,
    digest_latency_seconds: IntGauge,
    top_digest_latencies: IntGaugeVec,
}

impl StatementsCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    /// Create a new statements collector.
    ///
    /// # Panics
    ///
    /// Panics if metric names are invalid (should not occur with static names).
    pub fn new() -> Self {
        let g = |name: &str, help: &str| {
            IntGauge::new(name, help).expect("valid statement metric")
        };

        let top_digest_latencies = IntGaugeVec::new(
            Opts::new(
                "mariadb_perf_schema_digest_latency_seconds",
                "Top statement digests by total latency (seconds)",
            ),
            &["digest", "schema"],
        )
        .expect("valid mariadb_perf_schema_digest_latency_seconds metric");

        Self {
            digest_total: g(
                "mariadb_perf_schema_digest_total",
                "Total statements counted in performance_schema digests",
            ),
            digest_errors: g(
                "mariadb_perf_schema_digest_errors_total",
                "Total errors across statement digests",
            ),
            digest_warnings: g(
                "mariadb_perf_schema_digest_warnings_total",
                "Total warnings across statement digests",
            ),
            digest_rows_examined: g(
                "mariadb_perf_schema_digest_rows_examined_total",
                "Total rows examined across statement digests",
            ),
            digest_rows_sent: g(
                "mariadb_perf_schema_digest_rows_sent_total",
                "Total rows sent across statement digests",
            ),
            digest_latency_seconds: g(
                "mariadb_perf_schema_digest_latency_seconds_total",
                "Total latency across statement digests in picoseconds converted to seconds",
            ),
            top_digest_latencies,
        }
    }
}

impl Default for StatementsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for StatementsCollector {
    fn name(&self) -> &'static str {
        "statements"
    }

    #[instrument(
        skip(self, registry),
        level = "info",
        err,
        fields(collector = "statements")
    )]
    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.digest_total.clone()))?;
        registry.register(Box::new(self.digest_errors.clone()))?;
        registry.register(Box::new(self.digest_warnings.clone()))?;
        registry.register(Box::new(self.digest_rows_examined.clone()))?;
        registry.register(Box::new(self.digest_rows_sent.clone()))?;
        registry.register(Box::new(self.digest_latency_seconds.clone()))?;
        registry.register(Box::new(self.top_digest_latencies.clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "statements", otel.kind = "internal"))]
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // Aggregate totals
            let totals_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "aggregate statement digests",
                otel.kind = "client"
            );

            let totals = sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64)>(
                "SELECT
                    COALESCE(SUM(COUNT_STAR),0) as total,
                    COALESCE(SUM(SUM_ERRORS),0) as errors,
                    COALESCE(SUM(SUM_WARNINGS),0) as warnings,
                    COALESCE(SUM(SUM_ROWS_EXAMINED),0) as rows_examined,
                    COALESCE(SUM(SUM_ROWS_SENT),0) as rows_sent,
                    COALESCE(SUM(SUM_TIMER_WAIT),0) as latency_ps
                FROM performance_schema.events_statements_summary_by_digest",
            )
            .fetch_one(pool)
            .instrument(totals_span)
            .await
            .unwrap_or((0, 0, 0, 0, 0, 0));

            let latency_seconds = totals.5 / 1_000_000_000_000; // pico -> seconds

            self.digest_total.set(totals.0);
            self.digest_errors.set(totals.1);
            self.digest_warnings.set(totals.2);
            self.digest_rows_examined.set(totals.3);
            self.digest_rows_sent.set(totals.4);
            self.digest_latency_seconds.set(latency_seconds);

            // Top digests by latency (limit 5 to keep cardinality sane)
            let top_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "top digest latencies",
                otel.kind = "client"
            );

            let rows = sqlx::query_as::<_, (Option<String>, Option<String>, i64)>(
                "SELECT DIGEST_TEXT, SCHEMA_NAME, SUM_TIMER_WAIT
                 FROM performance_schema.events_statements_summary_by_digest
                 ORDER BY SUM_TIMER_WAIT DESC
                 LIMIT 5",
            )
            .fetch_all(pool)
            .instrument(top_span)
            .await
            .unwrap_or_default();

            for (digest, schema, latency_ps) in rows {
                let digest_label = digest.unwrap_or_else(|| "unknown".to_string());
                let schema_label = schema.unwrap_or_else(|| "unknown".to_string());
                self.top_digest_latencies
                    .with_label_values(&[digest_label.as_str(), schema_label.as_str()])
                    .set(latency_ps / 1_000_000_000_000);
            }

            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
