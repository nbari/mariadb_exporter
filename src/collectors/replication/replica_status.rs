use anyhow::{Result, anyhow};
use prometheus::{IntGauge, IntGaugeVec, Opts};
use sqlx::mysql::MySqlRow;
use sqlx::{MySqlPool, Row};
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;
use crate::collectors::util::is_mariadb_version_at_least;

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

/// Collector for SHOW SLAVE STATUS metrics.
#[derive(Clone)]
pub struct ReplicaStatusCollector {
    relay_log_space: IntGauge,
    relay_log_pos: IntGauge,
    seconds_behind_master: IntGauge,
    io_running: IntGauge,
    sql_running: IntGauge,
    last_io_errno: IntGauge,
    last_sql_errno: IntGauge,
    master_server_id: IntGauge,
    replica_configured: IntGauge,
    relay_log_space_by_channel: IntGaugeVec,
    relay_log_pos_by_channel: IntGaugeVec,
    seconds_behind_master_by_channel: IntGaugeVec,
    io_running_by_channel: IntGaugeVec,
    sql_running_by_channel: IntGaugeVec,
    last_io_errno_by_channel: IntGaugeVec,
    last_sql_errno_by_channel: IntGaugeVec,
    master_server_id_by_channel: IntGaugeVec,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReplicaChannelStatus {
    channel_name: String,
    connection_name: String,
    relay_log_space: i64,
    relay_log_pos: i64,
    seconds_behind_master: Option<i64>,
    io_running: i64,
    sql_running: i64,
    last_io_errno: i64,
    last_sql_errno: i64,
    master_server_id: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AggregatedReplicaStatus {
    relay_log_space: i64,
    relay_log_pos: i64,
    seconds_behind_master: i64,
    io_running: i64,
    sql_running: i64,
    last_io_errno: i64,
    last_sql_errno: i64,
    master_server_id: i64,
}

impl ReplicaStatusCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    /// Create a new replica status collector.
    ///
    /// # Panics
    ///
    /// Panics if metric names are invalid (should not occur with static names).
    pub fn new() -> Self {
        let channel_labels = &["channel_name", "connection_name"];

        Self {
            relay_log_space: gauge(
                "mariadb_replica_relay_log_space_bytes",
                "Total combined size of relay logs on replica",
            ),
            relay_log_pos: gauge("mariadb_replica_relay_log_pos", "Current relay log position"),
            seconds_behind_master: gauge(
                "mariadb_replica_seconds_behind_master_seconds",
                "Seconds behind master (replication lag, -1 = NULL/stopped)",
            ),
            io_running: gauge(
                "mariadb_replica_io_running",
                "Whether the I/O thread is running (1 = Yes, 0 = No)",
            ),
            sql_running: gauge(
                "mariadb_replica_sql_running",
                "Whether the SQL thread is running (1 = Yes, 0 = No)",
            ),
            last_io_errno: gauge("mariadb_replica_last_io_errno", "Last I/O error code"),
            last_sql_errno: gauge("mariadb_replica_last_sql_errno", "Last SQL error code"),
            master_server_id: gauge("mariadb_replica_master_server_id", "Master server ID"),
            replica_configured: gauge(
                "mariadb_replica_configured",
                "Replica configured (1 = yes, 0 = no)",
            ),
            relay_log_space_by_channel: gauge_by_channel(
                "mariadb_replica_relay_log_space_bytes_by_channel",
                "Relay log size by replication channel",
                channel_labels,
            ),
            relay_log_pos_by_channel: gauge_by_channel(
                "mariadb_replica_relay_log_pos_by_channel",
                "Relay log execution position by replication channel",
                channel_labels,
            ),
            seconds_behind_master_by_channel: gauge_by_channel(
                "mariadb_replica_seconds_behind_master_seconds_by_channel",
                "Replication lag by channel (-1 = unknown)",
                channel_labels,
            ),
            io_running_by_channel: gauge_by_channel(
                "mariadb_replica_io_running_by_channel",
                "Whether replica I/O thread is running per channel (1 = Yes, 0 = No)",
                channel_labels,
            ),
            sql_running_by_channel: gauge_by_channel(
                "mariadb_replica_sql_running_by_channel",
                "Whether replica SQL thread is running per channel (1 = Yes, 0 = No)",
                channel_labels,
            ),
            last_io_errno_by_channel: gauge_by_channel(
                "mariadb_replica_last_io_errno_by_channel",
                "Last I/O error code by replication channel",
                channel_labels,
            ),
            last_sql_errno_by_channel: gauge_by_channel(
                "mariadb_replica_last_sql_errno_by_channel",
                "Last SQL error code by replication channel",
                channel_labels,
            ),
            master_server_id_by_channel: gauge_by_channel(
                "mariadb_replica_master_server_id_by_channel",
                "Source server id by replication channel",
                channel_labels,
            ),
        }
    }

    /// Get relay log space metric.
    #[must_use]
    pub const fn relay_log_space(&self) -> &IntGauge {
        &self.relay_log_space
    }

    /// Get relay log position metric.
    #[must_use]
    pub const fn relay_log_pos(&self) -> &IntGauge {
        &self.relay_log_pos
    }

    /// Get seconds behind master metric.
    #[must_use]
    pub const fn seconds_behind_master(&self) -> &IntGauge {
        &self.seconds_behind_master
    }

    /// Get I/O running metric.
    #[must_use]
    pub const fn io_running(&self) -> &IntGauge {
        &self.io_running
    }

    /// Get SQL running metric.
    #[must_use]
    pub const fn sql_running(&self) -> &IntGauge {
        &self.sql_running
    }

    /// Get last I/O errno metric.
    #[must_use]
    pub const fn last_io_errno(&self) -> &IntGauge {
        &self.last_io_errno
    }

    /// Get last SQL errno metric.
    #[must_use]
    pub const fn last_sql_errno(&self) -> &IntGauge {
        &self.last_sql_errno
    }

    /// Get master server ID metric.
    #[must_use]
    pub const fn master_server_id(&self) -> &IntGauge {
        &self.master_server_id
    }

    /// Get replica configured metric.
    #[must_use]
    pub const fn replica_configured(&self) -> &IntGauge {
        &self.replica_configured
    }

    /// Get per-channel relay log space metric.
    #[must_use]
    pub const fn relay_log_space_by_channel(&self) -> &IntGaugeVec {
        &self.relay_log_space_by_channel
    }

    /// Get per-channel relay log position metric.
    #[must_use]
    pub const fn relay_log_pos_by_channel(&self) -> &IntGaugeVec {
        &self.relay_log_pos_by_channel
    }

    /// Get per-channel lag metric.
    #[must_use]
    pub const fn seconds_behind_master_by_channel(&self) -> &IntGaugeVec {
        &self.seconds_behind_master_by_channel
    }

    /// Get per-channel I/O running metric.
    #[must_use]
    pub const fn io_running_by_channel(&self) -> &IntGaugeVec {
        &self.io_running_by_channel
    }

    /// Get per-channel SQL running metric.
    #[must_use]
    pub const fn sql_running_by_channel(&self) -> &IntGaugeVec {
        &self.sql_running_by_channel
    }

    /// Get per-channel last I/O error metric.
    #[must_use]
    pub const fn last_io_errno_by_channel(&self) -> &IntGaugeVec {
        &self.last_io_errno_by_channel
    }

    /// Get per-channel last SQL error metric.
    #[must_use]
    pub const fn last_sql_errno_by_channel(&self) -> &IntGaugeVec {
        &self.last_sql_errno_by_channel
    }

    /// Get per-channel source server id metric.
    #[must_use]
    pub const fn master_server_id_by_channel(&self) -> &IntGaugeVec {
        &self.master_server_id_by_channel
    }

    fn clear_replica_metrics(&self) {
        self.relay_log_space.set(0);
        self.relay_log_pos.set(0);
        self.seconds_behind_master.set(-1);
        self.io_running.set(0);
        self.sql_running.set(0);
        self.last_io_errno.set(0);
        self.last_sql_errno.set(0);
        self.master_server_id.set(0);
        self.reset_channel_metrics();
    }

    fn reset_channel_metrics(&self) {
        self.relay_log_space_by_channel.reset();
        self.relay_log_pos_by_channel.reset();
        self.seconds_behind_master_by_channel.reset();
        self.io_running_by_channel.reset();
        self.sql_running_by_channel.reset();
        self.last_io_errno_by_channel.reset();
        self.last_sql_errno_by_channel.reset();
        self.master_server_id_by_channel.reset();
    }

    /// Collect replica status metrics from SHOW SLAVE STATUS.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails (though queries are best-effort).
    #[instrument(skip(self, pool), level = "debug", fields(sub_collector = "replica_status"))]
    pub async fn collect(&self, pool: &MySqlPool) -> Result<()> {
        let mut configured = None;

        if is_mariadb_version_at_least(100_600) {
            let config_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "SELECT COUNT(*) FROM performance_schema.replication_connection_configuration",
                otel.kind = "client"
            );

            match sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM performance_schema.replication_connection_configuration",
            )
            .fetch_one(pool)
            .instrument(config_span)
            .await
            {
                Ok(count) => configured = Some(count > 0),
                Err(e) => debug!(error = %e, "replication_connection_configuration not available; falling back to SHOW SLAVE STATUS"),
            }
        }

        let rows = match query_replica_status_rows(pool).await {
            Ok(rows) => rows,
            Err(e) => {
                debug!(error = %e, "replica status query failed; marking replica metrics unknown");
                self.clear_replica_metrics();
                self.replica_configured
                    .set(i64::from(configured.unwrap_or(false)));
                return Ok(());
            }
        };

        configured = Some(configured.unwrap_or(false) || !rows.is_empty());

        if rows.is_empty() {
            self.clear_replica_metrics();
        } else {
            self.reset_channel_metrics();

            let channels: Vec<_> = rows.iter().map(parse_channel_status).collect();
            for channel in &channels {
                let labels = [
                    channel.channel_name.as_str(),
                    channel.connection_name.as_str(),
                ];
                self.relay_log_space_by_channel
                    .with_label_values(&labels)
                    .set(channel.relay_log_space);
                self.relay_log_pos_by_channel
                    .with_label_values(&labels)
                    .set(channel.relay_log_pos);
                self.seconds_behind_master_by_channel
                    .with_label_values(&labels)
                    .set(channel.seconds_behind_master.unwrap_or(-1));
                self.io_running_by_channel
                    .with_label_values(&labels)
                    .set(channel.io_running);
                self.sql_running_by_channel
                    .with_label_values(&labels)
                    .set(channel.sql_running);
                self.last_io_errno_by_channel
                    .with_label_values(&labels)
                    .set(channel.last_io_errno);
                self.last_sql_errno_by_channel
                    .with_label_values(&labels)
                    .set(channel.last_sql_errno);
                self.master_server_id_by_channel
                    .with_label_values(&labels)
                    .set(channel.master_server_id);
            }

            let aggregate = aggregate_channel_statuses(&channels);
            self.relay_log_space.set(aggregate.relay_log_space);
            self.relay_log_pos.set(aggregate.relay_log_pos);
            self.seconds_behind_master
                .set(aggregate.seconds_behind_master);
            self.io_running.set(aggregate.io_running);
            self.sql_running.set(aggregate.sql_running);
            self.last_io_errno.set(aggregate.last_io_errno);
            self.last_sql_errno.set(aggregate.last_sql_errno);
            self.master_server_id.set(aggregate.master_server_id);
        }

        self.replica_configured
            .set(i64::from(configured.unwrap_or(false)));

        Ok(())
    }
}

#[allow(clippy::expect_used)]
fn gauge(name: &str, help: &str) -> IntGauge {
    IntGauge::new(name, help).expect("valid replication metric")
}

#[allow(clippy::expect_used)]
fn gauge_by_channel(name: &str, help: &str, labels: &[&str]) -> IntGaugeVec {
    IntGaugeVec::new(Opts::new(name, help), labels).expect("valid channel replication metric")
}

async fn query_replica_status_rows(pool: &MySqlPool) -> Result<Vec<MySqlRow>> {
    let mut last_error = None;
    let mut had_empty_success = false;

    for query in REPLICA_STATUS_QUERY_CANDIDATES {
        let span = info_span!(
            "db.query",
            db.system = "mysql",
            db.operation = "SHOW",
            db.statement = *query,
            otel.kind = "client"
        );

        match sqlx::query(query).fetch_all(pool).instrument(span).await {
            Ok(rows) => {
                if rows.is_empty() {
                    had_empty_success = true;
                    continue;
                }
                return Ok(rows);
            }
            Err(e) => {
                debug!(query, error = %e, "replica status query form not supported");
                last_error = Some(e);
            }
        }
    }

    if had_empty_success {
        return Ok(Vec::new());
    }

    Err(anyhow!(
        "all replica status query forms failed: {}",
        last_error
            .map_or_else(|| "unknown error".to_string(), |e| e.to_string())
    ))
}

fn parse_channel_status(row: &MySqlRow) -> ReplicaChannelStatus {
    let (channel_name, connection_name) = parse_channel_labels(row);

    ReplicaChannelStatus {
        channel_name,
        connection_name,
        relay_log_space: parse_i64_from_columns(row, &["Relay_Log_Space"]).unwrap_or_default(),
        relay_log_pos: parse_i64_from_columns(row, &["Exec_Master_Log_Pos", "Exec_Source_Log_Pos"])
            .unwrap_or_default(),
        seconds_behind_master: parse_i64_from_columns(
            row,
            &["Seconds_Behind_Master", "Seconds_Behind_Source"],
        ),
        io_running: parse_running(
            parse_string_from_columns(row, &["Slave_IO_Running", "Replica_IO_Running"]).as_deref(),
        ),
        sql_running: parse_running(
            parse_string_from_columns(row, &["Slave_SQL_Running", "Replica_SQL_Running"])
                .as_deref(),
        ),
        last_io_errno: parse_i64_from_columns(row, &["Last_IO_Errno"]).unwrap_or_default(),
        last_sql_errno: parse_i64_from_columns(row, &["Last_SQL_Errno"]).unwrap_or_default(),
        master_server_id: parse_i64_from_columns(row, &["Master_Server_Id", "Source_Server_Id"])
            .unwrap_or_default(),
    }
}

fn parse_channel_labels(row: &MySqlRow) -> (String, String) {
    let channel_name = parse_string_from_columns(row, &["Channel_Name"]).unwrap_or_default();
    let connection_name =
        parse_string_from_columns(row, &["Connection_name", "Connection_Name"])
            .unwrap_or_default();

    if channel_name.is_empty() && connection_name.is_empty() {
        return ("default".to_string(), "default".to_string());
    }
    if channel_name.is_empty() {
        return (connection_name.clone(), connection_name);
    }
    if connection_name.is_empty() {
        return (channel_name.clone(), channel_name);
    }

    (channel_name, connection_name)
}

fn aggregate_channel_statuses(channels: &[ReplicaChannelStatus]) -> AggregatedReplicaStatus {
    let mut relay_log_space = 0_i64;
    let mut relay_log_pos = 0_i64;
    let mut seconds_behind_master: Option<i64> = None;
    let mut io_running = true;
    let mut sql_running = true;
    let mut last_io_errno = 0_i64;
    let mut last_sql_errno = 0_i64;
    let mut master_server_id = None;
    let mut mixed_master_server_id = false;

    for channel in channels {
        relay_log_space = relay_log_space.saturating_add(channel.relay_log_space);
        relay_log_pos = relay_log_pos.max(channel.relay_log_pos);
        if let Some(lag) = channel.seconds_behind_master {
            seconds_behind_master = Some(seconds_behind_master.map_or(lag, |current| current.max(lag)));
        }
        io_running &= channel.io_running == 1;
        sql_running &= channel.sql_running == 1;
        last_io_errno = last_io_errno.max(channel.last_io_errno);
        last_sql_errno = last_sql_errno.max(channel.last_sql_errno);

        if channel.master_server_id > 0 {
            if let Some(current) = master_server_id {
                if current != channel.master_server_id {
                    mixed_master_server_id = true;
                }
            } else {
                master_server_id = Some(channel.master_server_id);
            }
        }
    }

    AggregatedReplicaStatus {
        relay_log_space,
        relay_log_pos,
        seconds_behind_master: seconds_behind_master.unwrap_or(-1),
        io_running: i64::from(io_running),
        sql_running: i64::from(sql_running),
        last_io_errno,
        last_sql_errno,
        master_server_id: if mixed_master_server_id {
            0
        } else {
            master_server_id.unwrap_or_default()
        },
    }
}

fn parse_i64_from_columns(row: &MySqlRow, columns: &[&str]) -> Option<i64> {
    for column in columns {
        let unsigned = row.try_get::<Option<u64>, _>(*column).ok().flatten();
        let signed = row.try_get::<Option<i64>, _>(*column).ok().flatten();
        let text = row.try_get::<Option<String>, _>(*column).ok().flatten();

        if let Some(value) = parse_i64_from_values(unsigned, signed, text) {
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

fn parse_running(value: Option<&str>) -> i64 {
    match value.map(str::to_ascii_lowercase).as_deref() {
        Some("yes" | "on" | "running") => 1,
        _ => 0,
    }
}

impl Default for ReplicaStatusCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        REPLICA_STATUS_QUERY_CANDIDATES, ReplicaChannelStatus, aggregate_channel_statuses,
        parse_i64_from_values, parse_running,
    };

    #[test]
    fn parses_unsigned_master_server_id() {
        let zero_id = parse_i64_from_values(Some(0), None, None);
        assert_eq!(zero_id, Some(0));

        let nonzero_id = parse_i64_from_values(Some(123), None, None);
        assert_eq!(nonzero_id, Some(123));
    }

    #[test]
    fn parses_text_and_signed_values() {
        assert_eq!(parse_i64_from_values(None, Some(11), None), Some(11));
        assert_eq!(
            parse_i64_from_values(None, None, Some("12".to_string())),
            Some(12)
        );
        assert_eq!(
            parse_i64_from_values(None, None, Some("not-a-number".to_string())),
            None
        );
    }

    #[test]
    fn running_state_matches_upstream_expectations() {
        assert_eq!(parse_running(Some("Yes")), 1);
        assert_eq!(parse_running(Some("ON")), 1);
        assert_eq!(parse_running(Some("Running")), 1);
        assert_eq!(parse_running(Some("No")), 0);
        assert_eq!(parse_running(Some("OFF")), 0);
        assert_eq!(parse_running(Some("Disabled")), 0);
        assert_eq!(parse_running(Some("Connecting")), 0);
        assert_eq!(parse_running(None), 0);
    }

    #[test]
    fn replica_query_candidates_cover_upstream_forms() {
        assert!(
            REPLICA_STATUS_QUERY_CANDIDATES.contains(&"SHOW ALL SLAVES STATUS")
        );
        assert!(REPLICA_STATUS_QUERY_CANDIDATES.contains(&"SHOW SLAVE STATUS"));
        assert!(REPLICA_STATUS_QUERY_CANDIDATES.contains(&"SHOW REPLICA STATUS"));
        assert!(
            REPLICA_STATUS_QUERY_CANDIDATES
                .contains(&"SHOW SLAVE STATUS NOLOCK")
        );
    }

    #[test]
    fn aggregate_replica_status_uses_safe_multi_channel_semantics() {
        let channels = vec![
            ReplicaChannelStatus {
                channel_name: "a".to_string(),
                connection_name: "a".to_string(),
                relay_log_space: 20,
                relay_log_pos: 50,
                seconds_behind_master: Some(3),
                io_running: 1,
                sql_running: 1,
                last_io_errno: 0,
                last_sql_errno: 0,
                master_server_id: 11,
            },
            ReplicaChannelStatus {
                channel_name: "b".to_string(),
                connection_name: "b".to_string(),
                relay_log_space: 30,
                relay_log_pos: 100,
                seconds_behind_master: Some(8),
                io_running: 1,
                sql_running: 0,
                last_io_errno: 0,
                last_sql_errno: 123,
                master_server_id: 11,
            },
            ReplicaChannelStatus {
                channel_name: "c".to_string(),
                connection_name: "c".to_string(),
                relay_log_space: 5,
                relay_log_pos: 90,
                seconds_behind_master: None,
                io_running: 0,
                sql_running: 0,
                last_io_errno: 9,
                last_sql_errno: 0,
                master_server_id: 11,
            },
        ];

        let aggregate = aggregate_channel_statuses(&channels);
        assert_eq!(aggregate.relay_log_space, 55);
        assert_eq!(aggregate.relay_log_pos, 100);
        assert_eq!(aggregate.seconds_behind_master, 8);
        assert_eq!(aggregate.io_running, 0);
        assert_eq!(aggregate.sql_running, 0);
        assert_eq!(aggregate.last_io_errno, 9);
        assert_eq!(aggregate.last_sql_errno, 123);
        assert_eq!(aggregate.master_server_id, 11);
    }

    #[test]
    fn aggregate_replica_status_marks_master_id_ambiguous_for_multi_source() {
        let channels = vec![
            ReplicaChannelStatus {
                channel_name: "a".to_string(),
                connection_name: "a".to_string(),
                relay_log_space: 1,
                relay_log_pos: 1,
                seconds_behind_master: Some(0),
                io_running: 1,
                sql_running: 1,
                last_io_errno: 0,
                last_sql_errno: 0,
                master_server_id: 11,
            },
            ReplicaChannelStatus {
                channel_name: "b".to_string(),
                connection_name: "b".to_string(),
                relay_log_space: 1,
                relay_log_pos: 1,
                seconds_behind_master: Some(0),
                io_running: 1,
                sql_running: 1,
                last_io_errno: 0,
                last_sql_errno: 0,
                master_server_id: 22,
            },
        ];

        let aggregate = aggregate_channel_statuses(&channels);
        assert_eq!(aggregate.master_server_id, 0);
    }
}
