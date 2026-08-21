use crate::collectors::util::{
    DeniedOnce, ER_PARSE_ERROR, QueryFailure, classify_query_error, mysql_error_number,
};
use crate::collectors::{Collected, Collector, NO_LABELS};
use anyhow::{Result, anyhow};
use futures::future::BoxFuture;
use prometheus::{IntGaugeVec, Opts, Registry};
use sqlx::mysql::MySqlRow;
use sqlx::{MySqlPool, Row};
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;

// Keep query semantics aligned with upstream mysqld_exporter:
// try old/new forms and lock-free suffixes where supported.
const REPLICA_STATUS_QUERY_CANDIDATES: &[&str] = &[
    "SHOW ALL SLAVES STATUS",
    "SHOW ALL SLAVES STATUS NONBLOCKING",
    "SHOW ALL SLAVES STATUS NOLOCK",
    "SHOW SLAVE STATUS",
    "SHOW SLAVE STATUS NONBLOCKING",
    "SHOW SLAVE STATUS NOLOCK",
    "SHOW REPLICA STATUS",
    "SHOW REPLICA STATUS NONBLOCKING",
    "SHOW REPLICA STATUS NOLOCK",
];

/// Default-on replication summary (`mariadb_slave_status_*`).
///
/// Split out of [`super::status::StatusCollector`] so that an unreadable replica source
/// settles on its own. Inside the status collector a skip would have erased the ~108 global
/// status gauges that had just been published successfully; here it only clears the three
/// series it actually owns. Metric names, help text and labels are unchanged.
#[derive(Clone)]
pub struct ReplicationCollector {
    seconds_behind: IntGaugeVec,
    sql_running: IntGaugeVec,
    io_running: IntGaugeVec,
    denied: DeniedOnce,
}

/// Result of probing the supported `SHOW ... STATUS` statement forms.
enum ReplicaStatusOutcome {
    /// A statement form succeeded. An empty vector is a valid "not a replica" answer.
    Rows(Vec<MySqlRow>),
    /// No form could be read: either the server supports none of them or access was denied.
    Unavailable { denied: Option<sqlx::Error> },
}

impl ReplicationCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    /// Create a new replication summary collector.
    ///
    /// # Panics
    ///
    /// Panics if metric registration opts are invalid (should never happen with static names).
    pub fn new() -> Self {
        // Zero-label vectors: "0 lag" and "threads stopped" are factual claims that must
        // never be published for a source the exporter could not read.
        let g = |name: &str, help: &str| {
            IntGaugeVec::new(Opts::new(name, help), &NO_LABELS).expect("valid metric name")
        };

        Self {
            seconds_behind: g(
                "mariadb_slave_status_seconds_behind_master",
                "Seconds the replica is behind the primary",
            ),
            sql_running: g(
                "mariadb_slave_status_sql_running",
                "Replica SQL thread running (1/0)",
            ),
            io_running: g(
                "mariadb_slave_status_io_running",
                "Replica IO thread running (1/0)",
            ),
            denied: DeniedOnce::default(),
        }
    }

    /// Publish the documented "not a replica" sentinels.
    ///
    /// `-1` lag means "NULL / stopped / not a replica" and the running flags mean "not
    /// running". They describe a *successful* read that found no replica, and are never
    /// published for a source that could not be read.
    fn set_no_replica_sentinels(&self) {
        self.seconds_behind.with_label_values(&NO_LABELS).set(-1);
        self.sql_running.with_label_values(&NO_LABELS).set(0);
        self.io_running.with_label_values(&NO_LABELS).set(0);
    }

    fn parse_i64_from_columns(row: &MySqlRow, columns: &[&str]) -> Option<i64> {
        for column in columns {
            let unsigned = row.try_get::<Option<u64>, _>(*column).ok().flatten();
            let signed = row.try_get::<Option<i64>, _>(*column).ok().flatten();
            let text = row.try_get::<Option<String>, _>(*column).ok().flatten();

            if let Some(value) = Self::parse_i64_from_values(unsigned, signed, text) {
                return Some(value);
            }
        }

        None
    }

    fn parse_i64_from_values(
        unsigned: Option<u64>,
        signed: Option<i64>,
        text: Option<String>,
    ) -> Option<i64> {
        unsigned
            .and_then(|v| i64::try_from(v).ok())
            .or(signed)
            .or_else(|| text.and_then(|value| value.parse::<i64>().ok()))
    }

    fn parse_string_from_columns(row: &MySqlRow, columns: &[&str]) -> Option<String> {
        for column in columns {
            if let Some(value) = row.try_get::<Option<String>, _>(*column).ok().flatten() {
                return Some(value);
            }
        }

        None
    }

    fn as_running(val: Option<&str>) -> i32 {
        match val.map(str::to_ascii_lowercase).as_deref() {
            Some("yes" | "on" | "running") => 1,
            _ => 0,
        }
    }

    fn aggregate_replica_channel_states(channels: &[(Option<i64>, i32, i32)]) -> (i64, i64, i64) {
        let mut lag: Option<i64> = None;
        let mut io_running = true;
        let mut sql_running = true;

        for (channel_lag, channel_io_running, channel_sql_running) in channels {
            if let Some(value) = channel_lag {
                lag = Some(lag.map_or(*value, |current| current.max(*value)));
            }
            io_running &= *channel_io_running == 1;
            sql_running &= *channel_sql_running == 1;
        }

        (
            lag.unwrap_or(-1),
            i64::from(io_running),
            i64::from(sql_running),
        )
    }

    async fn query_replica_status_rows(pool: &MySqlPool) -> Result<ReplicaStatusOutcome> {
        let mut had_empty_success = false;
        let mut denied: Option<sqlx::Error> = None;
        let mut fault: Option<sqlx::Error> = None;

        for query in REPLICA_STATUS_QUERY_CANDIDATES {
            let span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SHOW",
                db.statement = *query,
                otel.kind = "client"
            );

            match sqlx::query(*query).fetch_all(pool).instrument(span).await {
                Ok(rows) => {
                    if rows.is_empty() {
                        had_empty_success = true;
                        continue;
                    }
                    return Ok(ReplicaStatusOutcome::Rows(rows));
                }
                Err(e) => {
                    // This loop is a capability probe over statement *forms*: a parse error
                    // only means "this server does not know this spelling", so it is treated
                    // as an absent form rather than as a fault in our own SQL.
                    if mysql_error_number(&e) == Some(ER_PARSE_ERROR) {
                        debug!(query, error = %e, "replica status query form not supported");
                        continue;
                    }
                    match classify_query_error(&e) {
                        QueryFailure::Absent => {
                            debug!(query, error = %e, "replica status source not available");
                        }
                        QueryFailure::Denied => {
                            debug!(query, error = %e, "replica status not permitted");
                            denied = Some(e);
                        }
                        QueryFailure::Fault => fault = Some(e),
                    }
                }
            }
        }

        if had_empty_success {
            return Ok(ReplicaStatusOutcome::Rows(Vec::new()));
        }

        if let Some(e) = fault {
            return Err(anyhow!("all replica status query forms failed: {e}"));
        }

        Ok(ReplicaStatusOutcome::Unavailable { denied })
    }
}

impl Default for ReplicationCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for ReplicationCollector {
    fn name(&self) -> &'static str {
        "replication_summary"
    }

    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.seconds_behind.clone()))?;
        registry.register(Box::new(self.sql_running.clone()))?;
        registry.register(Box::new(self.io_running.clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "replication_summary", otel.kind = "internal"))]
    fn collect_once<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
        Box::pin(async move {
            let rows = match Self::query_replica_status_rows(pool).await? {
                ReplicaStatusOutcome::Rows(rows) => rows,
                ReplicaStatusOutcome::Unavailable { denied } => {
                    if let Some(e) = denied {
                        self.denied.report("SHOW REPLICA STATUS", &e);
                    } else {
                        debug!("no supported replica status statement; skipping replication summary");
                    }
                    // A permission or capability failure must never become "0 lag, threads
                    // stopped": the series disappear instead.
                    return Ok(Collected::Skipped);
                }
            };

            if rows.is_empty() {
                // Not a replica: a successful, current answer.
                self.set_no_replica_sentinels();
                return Ok(Collected::Fresh);
            }

            let channel_states: Vec<_> = rows
                .iter()
                .map(|row| {
                    let lag = Self::parse_i64_from_columns(
                        row,
                        &["Seconds_Behind_Master", "Seconds_Behind_Source"],
                    );
                    let io_running = Self::parse_string_from_columns(
                        row,
                        &["Slave_IO_Running", "Replica_IO_Running"],
                    );
                    let sql_running = Self::parse_string_from_columns(
                        row,
                        &["Slave_SQL_Running", "Replica_SQL_Running"],
                    );

                    (
                        lag,
                        Self::as_running(io_running.as_deref()),
                        Self::as_running(sql_running.as_deref()),
                    )
                })
                .collect();

            let (lag, io_running, sql_running) =
                Self::aggregate_replica_channel_states(&channel_states);
            self.seconds_behind.with_label_values(&NO_LABELS).set(lag);
            self.io_running
                .with_label_values(&NO_LABELS)
                .set(io_running);
            self.sql_running
                .with_label_values(&NO_LABELS)
                .set(sql_running);

            Ok(Collected::Fresh)
        })
    }

    fn reset_metrics(&self) {
        self.seconds_behind.reset();
        self.sql_running.reset();
        self.io_running.reset();
    }

    fn enabled_by_default(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::ReplicationCollector;
    use crate::collectors::{Collector, NO_LABELS, published_samples};

    #[test]
    fn running_state_handles_common_replication_values() {
        assert_eq!(ReplicationCollector::as_running(Some("Yes")), 1);
        assert_eq!(ReplicationCollector::as_running(Some("ON")), 1);
        assert_eq!(ReplicationCollector::as_running(Some("Running")), 1);
        assert_eq!(ReplicationCollector::as_running(Some("No")), 0);
        assert_eq!(ReplicationCollector::as_running(Some("Connecting")), 0);
        assert_eq!(ReplicationCollector::as_running(None), 0);
    }

    #[test]
    fn parses_unsigned_and_text_replication_numbers() {
        assert_eq!(
            ReplicationCollector::parse_i64_from_values(Some(42), None, None),
            Some(42)
        );
        assert_eq!(
            ReplicationCollector::parse_i64_from_values(None, Some(9), None),
            Some(9)
        );
        assert_eq!(
            ReplicationCollector::parse_i64_from_values(None, None, Some("13".to_string())),
            Some(13)
        );
        assert_eq!(
            ReplicationCollector::parse_i64_from_values(None, None, Some("x".to_string())),
            None
        );
    }

    #[test]
    fn aggregate_replication_channels_uses_worst_case_semantics() {
        let channels = vec![(Some(0), 1, 1), (Some(7), 1, 0), (None, 0, 0)];
        let (lag, io_running, sql_running) =
            ReplicationCollector::aggregate_replica_channel_states(&channels);

        assert_eq!(lag, 7);
        assert_eq!(io_running, 0);
        assert_eq!(sql_running, 0);
    }

    #[test]
    fn aggregate_replication_channels_reports_unknown_when_all_lag_null() {
        let channels = vec![(None, 1, 1), (None, 1, 1)];
        let (lag, io_running, sql_running) =
            ReplicationCollector::aggregate_replica_channel_states(&channels);

        assert_eq!(lag, -1);
        assert_eq!(io_running, 1);
        assert_eq!(sql_running, 1);
    }

    #[test]
    fn no_replica_sentinels_are_published_and_removable() {
        let collector = ReplicationCollector::new();
        collector.set_no_replica_sentinels();

        assert_eq!(
            collector.seconds_behind.with_label_values(&NO_LABELS).get(),
            -1
        );

        Collector::reset_metrics(&collector);
        assert_eq!(published_samples(&collector.seconds_behind), 0);
    }
}
