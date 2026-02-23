use crate::collectors::{util::PICO_TO_SECONDS, Collector};
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{Gauge, GaugeVec, IntGauge, Opts, Registry};
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
    digest_latency_seconds: Gauge,
    top_digest_latencies: GaugeVec,
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

        let top_digest_latencies = GaugeVec::new(
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
            digest_latency_seconds: Gauge::new(
                "mariadb_perf_schema_digest_latency_seconds_total",
                "Total latency across statement digests in picoseconds converted to seconds",
            )
            .expect("valid mariadb_perf_schema_digest_latency_seconds_total metric"),
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
            // Reset top digests to avoid stale data
            self.top_digest_latencies.reset();

            // Confirm table exists (Performance Schema might be off)
            let exists_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "check events_statements_summary_by_digest table",
                otel.kind = "client"
            );

            let has_table = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema='performance_schema' AND table_name='events_statements_summary_by_digest'",
            )
            .fetch_one(pool)
            .instrument(exists_span)
            .await
            .unwrap_or(0)
                > 0;

            if !has_table {
                tracing::debug!("events_statements_summary_by_digest not available; skipping collection");
                return Ok(());
            }

            // Aggregate totals
            let totals_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "aggregate statement digests",
                otel.kind = "client"
            );

            let totals = sqlx::query_as::<_, (u64, u64, u64, u64, u64, u64)>(
                "SELECT
                    CAST(COALESCE(SUM(COUNT_STAR),0) AS UNSIGNED) as total,
                    CAST(COALESCE(SUM(SUM_ERRORS),0) AS UNSIGNED) as errors,
                    CAST(COALESCE(SUM(SUM_WARNINGS),0) AS UNSIGNED) as warnings,
                    CAST(COALESCE(SUM(SUM_ROWS_EXAMINED),0) AS UNSIGNED) as rows_examined,
                    CAST(COALESCE(SUM(SUM_ROWS_SENT),0) AS UNSIGNED) as rows_sent,
                    CAST(COALESCE(SUM(SUM_TIMER_WAIT),0) AS UNSIGNED) as latency_ps
                FROM performance_schema.events_statements_summary_by_digest",
            )
            .fetch_one(pool)
            .instrument(totals_span)
            .await?;

            #[allow(clippy::cast_precision_loss)]
            let latency_seconds = (totals.5 as f64) / PICO_TO_SECONDS;

            #[allow(clippy::cast_possible_wrap)]
            {
                self.digest_total.set(totals.0 as i64);
                self.digest_errors.set(totals.1 as i64);
                self.digest_warnings.set(totals.2 as i64);
                self.digest_rows_examined.set(totals.3 as i64);
                self.digest_rows_sent.set(totals.4 as i64);
                self.digest_latency_seconds.set(latency_seconds);
            }

            // Top digests by latency (limit 5 to keep cardinality sane)
            let top_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "top digest latencies",
                otel.kind = "client"
            );

            let rows = match sqlx::query_as::<_, (Option<String>, Option<String>, u64)>(
                "SELECT DIGEST_TEXT, SCHEMA_NAME, CAST(SUM_TIMER_WAIT AS UNSIGNED)
                 FROM performance_schema.events_statements_summary_by_digest
                 ORDER BY SUM_TIMER_WAIT DESC
                 LIMIT 5",
            )
            .fetch_all(pool)
            .instrument(top_span)
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("Statements digest query failed: {}", e);
                    vec![]
                }
            };

            for (digest, schema, latency_ps) in rows {
                let digest_label = digest.unwrap_or_else(|| "unknown".to_string());
                let schema_label = schema.unwrap_or_else(|| "unknown".to_string());
                #[allow(clippy::cast_precision_loss)]
                let latency_seconds = (latency_ps as f64) / PICO_TO_SECONDS;
                self.top_digest_latencies
                    .with_label_values(&[digest_label.as_str(), schema_label.as_str()])
                    .set(latency_seconds);
            }

            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
