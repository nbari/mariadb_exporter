use crate::collectors::{
    Collected, Collector, NO_LABELS,
    util::{DeniedOnce, QueryFailure, classify_query_error},
};
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{CounterVec, IntCounterVec, Opts, Registry};
use sqlx::MySqlPool;
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;

/// A fully aggregated snapshot of the plugin's response-time histogram.
struct Histogram {
    under_hundred_millis: u64,
    under_one_second: u64,
    under_ten_seconds: u64,
    total: u64,
    count: u64,
    sum: f64,
}

/// Query response time plugin metrics (opt-in; skipped if plugin not installed).
/// Exposes histogram-style buckets: le="0.1" (<=100ms), le="1.0" (<=1s), le="10.0" (<=10s), le="+Inf"
#[derive(Clone)]
#[allow(clippy::struct_field_names)]
pub struct QueryResponseTimeCollector {
    response_time_bucket: IntCounterVec,
    response_time_count: IntCounterVec,
    response_time_sum: CounterVec,
    denied: DeniedOnce,
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

        // `_count` and `_sum` are zero-label vectors so that an absent plugin can remove
        // them entirely; a bare counter can never be unregistered once published.
        let response_time_count = IntCounterVec::new(
            Opts::new(
                "mariadb_info_schema_query_response_time_seconds_count",
                "Total count of queries tracked",
            ),
            &NO_LABELS,
        )
        .expect("valid mariadb_info_schema_query_response_time_seconds_count metric");

        let response_time_sum = CounterVec::new(
            Opts::new(
                "mariadb_info_schema_query_response_time_seconds_sum",
                "Total sum of query response times in seconds",
            ),
            &NO_LABELS,
        )
        .expect("valid mariadb_info_schema_query_response_time_seconds_sum metric");

        Self {
            response_time_bucket,
            response_time_count,
            response_time_sum,
            denied: DeniedOnce::default(),
        }
    }

    /// Read the plugin table and publish the histogram.
    ///
    /// # Errors
    ///
    /// Returns an error when the feature probe or the data query fails for a reason other
    /// than the plugin being absent or unreadable.
    #[instrument(skip(self, pool), level = "debug", fields(sub_collector = "query_response_time"))]
    async fn collect_inner(&self, pool: &MySqlPool) -> Result<Collected> {
        // Confirm plugin table exists. A failing probe is a fault, not an absent plugin.
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
        .await?
            > 0;

        if !has_table {
            debug!("query_response_time plugin not present; skipping collection");
            return Ok(Collected::Skipped);
        }

        let span = info_span!(
            "db.query",
            db.system = "mysql",
            db.operation = "SELECT",
            db.statement = "SELECT TIME, COUNT FROM information_schema.QUERY_RESPONSE_TIME",
            otel.kind = "client"
        );

        let rows = match sqlx::query_as::<_, (String, u64)>(
            "SELECT TIME, CAST(COUNT AS UNSIGNED) FROM information_schema.QUERY_RESPONSE_TIME",
        )
        .fetch_all(pool)
        .instrument(span)
        .await
        {
            Ok(r) => r,
            Err(e) => match classify_query_error(&e) {
                QueryFailure::Absent => {
                    debug!(error = %e, "query_response_time table vanished; skipping");
                    return Ok(Collected::Skipped);
                }
                QueryFailure::Denied => {
                    self.denied
                        .report("information_schema.QUERY_RESPONSE_TIME", &e);
                    return Ok(Collected::Skipped);
                }
                QueryFailure::Fault => return Err(e.into()),
            },
        };

        let histogram = Self::aggregate(&rows);
        self.publish(&histogram);

        debug!(
            "Query response time: processed {} raw buckets, total count={}, sum={:.2}s",
            rows.len(),
            histogram.count,
            histogram.sum
        );

        Ok(Collected::Fresh)
    }

    /// Fold the plugin's raw `(TIME, COUNT)` rows into the exported cumulative histogram.
    ///
    /// Rows whose `TIME` cannot be parsed (the plugin emits `TOO LONG` for its overflow
    /// row) are skipped individually: they are a documented artefact of an otherwise
    /// healthy source, not evidence that the source is unreadable.
    fn aggregate(rows: &[(String, u64)]) -> Histogram {
        let mut under_hundred_millis: u64 = 0;
        let mut under_one_second: u64 = 0;
        let mut under_ten_seconds: u64 = 0;
        let mut slower_than_10s: u64 = 0;
        let mut count: u64 = 0;
        let mut sum: f64 = 0.0;

        for (time_str, row_count) in rows {
            let Ok(time_secs) = time_str.trim().parse::<f64>() else {
                continue;
            };

            if *row_count == 0 {
                continue;
            }

            count += row_count;
            #[allow(clippy::cast_precision_loss)]
            let row_count_f64 = *row_count as f64;
            sum += time_secs * row_count_f64;

            // Place into non-overlapping ranges first.
            if time_secs <= 0.1 {
                under_hundred_millis += row_count;
            } else if time_secs <= 1.0 {
                under_one_second += row_count;
            } else if time_secs <= 10.0 {
                under_ten_seconds += row_count;
            } else {
                slower_than_10s += row_count;
            }
        }

        // Make the buckets cumulative: each includes everything up to its threshold.
        under_one_second += under_hundred_millis;
        under_ten_seconds += under_one_second;

        Histogram {
            under_hundred_millis,
            under_one_second,
            under_ten_seconds,
            total: under_ten_seconds + slower_than_10s,
            count,
            sum,
        }
    }

    /// Publish a freshly aggregated histogram, replacing the previous snapshot.
    fn publish(&self, histogram: &Histogram) {
        self.response_time_bucket.reset();
        self.response_time_bucket
            .with_label_values(&["0.1"])
            .inc_by(histogram.under_hundred_millis);
        self.response_time_bucket
            .with_label_values(&["1.0"])
            .inc_by(histogram.under_one_second);
        self.response_time_bucket
            .with_label_values(&["10.0"])
            .inc_by(histogram.under_ten_seconds);
        self.response_time_bucket
            .with_label_values(&["+Inf"])
            .inc_by(histogram.total);

        self.response_time_count.reset();
        self.response_time_count
            .with_label_values(&NO_LABELS)
            .inc_by(histogram.count);
        self.response_time_sum.reset();
        self.response_time_sum
            .with_label_values(&NO_LABELS)
            .inc_by(histogram.sum);
    }

    /// Get the bucket metric for registration.
    #[must_use]
    pub const fn response_time_bucket(&self) -> &IntCounterVec {
        &self.response_time_bucket
    }

    /// Get the count metric for registration.
    #[must_use]
    pub const fn response_time_count(&self) -> &IntCounterVec {
        &self.response_time_count
    }

    /// Get the sum metric for registration.
    #[must_use]
    pub const fn response_time_sum(&self) -> &CounterVec {
        &self.response_time_sum
    }
}

impl Collector for QueryResponseTimeCollector {
    fn name(&self) -> &'static str {
        "query_response_time"
    }

    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.response_time_bucket.clone()))?;
        registry.register(Box::new(self.response_time_count.clone()))?;
        registry.register(Box::new(self.response_time_sum.clone()))?;
        Ok(())
    }

    fn collect_once<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
        Box::pin(async move { self.collect_inner(pool).await })
    }

    /// An absent plugin must clear the bucket, count and sum together — a partial histogram
    /// is worse than no histogram.
    fn reset_metrics(&self) {
        self.response_time_bucket.reset();
        self.response_time_count.reset();
        self.response_time_sum.reset();
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}

