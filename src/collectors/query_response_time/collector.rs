use anyhow::Result;
use prometheus::{IntCounterVec, Opts};
use sqlx::MySqlPool;
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;

/// Query response time plugin metrics (opt-in; skipped if plugin not installed).
/// Exposes histogram-style buckets: le="0.1" (<=100ms), le="1.0" (<=1s), le="10.0" (<=10s), le="+Inf"
#[derive(Clone)]
#[allow(clippy::struct_field_names)]
pub struct QueryResponseTimeCollector {
    response_time_bucket: IntCounterVec,
    response_time_count: prometheus::IntCounter,
    response_time_sum: prometheus::Counter,
}

impl Default for QueryResponseTimeCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryResponseTimeCollector {
    /// Creates a new `QueryResponseTimeCollector`
    ///
    /// # Panics
    ///
    /// Panics if metric creation fails (should never happen with valid metric names)
    #[must_use]
    #[allow(clippy::expect_used)]
    pub fn new() -> Self {
        // Create histogram-style _bucket metric with le label
        let response_time_bucket = IntCounterVec::new(
            Opts::new(
                "mariadb_info_schema_query_response_time_seconds_bucket",
                "Cumulative counters for query response time histogram buckets",
            ),
            &["le"],
        )
        .expect("valid mariadb_info_schema_query_response_time_seconds_bucket metric");

        // Create _count metric (total number of queries)
        let response_time_count = prometheus::IntCounter::with_opts(
            Opts::new(
                "mariadb_info_schema_query_response_time_seconds_count",
                "Total count of queries tracked",
            ),
        )
        .expect("valid mariadb_info_schema_query_response_time_seconds_count metric");

        // Create _sum metric (total sum of query times)
        let response_time_sum = prometheus::Counter::with_opts(
            Opts::new(
                "mariadb_info_schema_query_response_time_seconds_sum",
                "Total sum of query response times in seconds",
            ),
        )
        .expect("valid mariadb_info_schema_query_response_time_seconds_sum metric");

        Self {
            response_time_bucket,
            response_time_count,
            response_time_sum,
        }
    }

    /// Collect query response time metrics.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[allow(clippy::similar_names)]
    #[allow(clippy::manual_let_else)]
    #[instrument(skip(self, pool), level = "debug", fields(sub_collector = "query_response_time"))]
    pub async fn collect(&self, pool: &MySqlPool) -> Result<()> {
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

        // Aggregate into our 4 histogram buckets (cumulative)
        // Each bucket counts queries up to (and including) that threshold
        let mut cumulative_0_1: u64 = 0;    // le="0.1" - queries <= 0.1s (100ms)
        let mut cumulative_1_0: u64 = 0;    // le="1.0" - queries <= 1s
        let mut cumulative_10_0: u64 = 0;   // le="10.0" - queries <= 10s
        let mut over_10s: u64 = 0;          // queries > 10s
        let mut total_count: u64 = 0;
        let mut total_sum: f64 = 0.0;

        for (time_str, count) in &rows {
            let time_secs = match time_str.trim().parse::<f64>() {
                Ok(t) => t,
                Err(_) => continue, // Skip rows with unparseable TIME values (e.g., 'TOO LONG')
            };

            // Skip zero counts
            if *count == 0 {
                continue;
            }

            // Add to total count and sum
            total_count += count;
            #[allow(clippy::cast_precision_loss)]
            let count_f64 = *count as f64;
            total_sum += time_secs * count_f64;

            // Place into non-overlapping ranges first
            if time_secs <= 0.1 {
                cumulative_0_1 += count;
            } else if time_secs <= 1.0 {
                cumulative_1_0 += count;
            } else if time_secs <= 10.0 {
                cumulative_10_0 += count;
            } else {
                over_10s += count;
            }
        }

        // Now make cumulative: each bucket includes all queries up to that threshold
        cumulative_1_0 += cumulative_0_1;   // 1s bucket includes everything <= 1s
        cumulative_10_0 += cumulative_1_0;  // 10s bucket includes everything <= 10s
        let cumulative_inf = cumulative_10_0 + over_10s; // +Inf includes everything

        // Set histogram buckets (using reset() and inc_by() for counters)
        self.response_time_bucket.reset();
        self.response_time_bucket
            .with_label_values(&["0.1"])
            .inc_by(cumulative_0_1);
        self.response_time_bucket
            .with_label_values(&["1.0"])
            .inc_by(cumulative_1_0);
        self.response_time_bucket
            .with_label_values(&["10.0"])
            .inc_by(cumulative_10_0);
        self.response_time_bucket
            .with_label_values(&["+Inf"])
            .inc_by(cumulative_inf);

        // Set count and sum
        self.response_time_count.reset();
        self.response_time_count.inc_by(total_count);
        self.response_time_sum.reset();
        self.response_time_sum.inc_by(total_sum);

        debug!(
            "Query response time: processed {} raw buckets, total count={}, sum={:.2}s",
            rows.len(),
            total_count,
            total_sum
        );

        Ok(())
    }

    /// Get the bucket metric for registration.
    #[must_use]
    pub fn response_time_bucket(&self) -> &IntCounterVec {
        &self.response_time_bucket
    }

    /// Get the count metric for registration.
    #[must_use]
    pub fn response_time_count(&self) -> &prometheus::IntCounter {
        &self.response_time_count
    }

    /// Get the sum metric for registration.
    #[must_use]
    pub fn response_time_sum(&self) -> &prometheus::Counter {
        &self.response_time_sum
    }
}

