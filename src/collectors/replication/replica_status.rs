use anyhow::Result;
use prometheus::IntGauge;
use sqlx::mysql::MySqlRow;
use sqlx::{MySqlPool, Row};
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;
use crate::collectors::util::is_mariadb_version_at_least;

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
        Self {
            relay_log_space: IntGauge::new(
                "mariadb_replica_relay_log_space_bytes",
                "Total combined size of relay logs on replica",
            )
            .expect("valid mariadb_replica_relay_log_space_bytes metric"),
            relay_log_pos: IntGauge::new(
                "mariadb_replica_relay_log_pos",
                "Current relay log position",
            )
            .expect("valid mariadb_replica_relay_log_pos metric"),
            seconds_behind_master: IntGauge::new(
                "mariadb_replica_seconds_behind_master_seconds",
                "Seconds behind master (replication lag, -1 = NULL/stopped)",
            )
            .expect("valid mariadb_replica_seconds_behind_master_seconds metric"),
            io_running: IntGauge::new(
                "mariadb_replica_io_running",
                "Whether the I/O thread is running (1 = Yes, 0 = No)",
            )
            .expect("valid mariadb_replica_io_running metric"),
            sql_running: IntGauge::new(
                "mariadb_replica_sql_running",
                "Whether the SQL thread is running (1 = Yes, 0 = No)",
            )
            .expect("valid mariadb_replica_sql_running metric"),
            last_io_errno: IntGauge::new(
                "mariadb_replica_last_io_errno",
                "Last I/O error code",
            )
            .expect("valid mariadb_replica_last_io_errno metric"),
            last_sql_errno: IntGauge::new(
                "mariadb_replica_last_sql_errno",
                "Last SQL error code",
            )
            .expect("valid mariadb_replica_last_sql_errno metric"),
            master_server_id: IntGauge::new(
                "mariadb_replica_master_server_id",
                "Master server ID",
            )
            .expect("valid mariadb_replica_master_server_id metric"),
            replica_configured: IntGauge::new(
                "mariadb_replica_configured",
                "Replica configured (1 = yes, 0 = no)",
            )
            .expect("valid mariadb_replica_configured metric"),
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

        let span = info_span!(
            "db.query",
            db.system = "mysql",
            db.operation = "SHOW",
            db.statement = "SHOW SLAVE STATUS",
            otel.kind = "client"
        );

        match sqlx::query("SHOW SLAVE STATUS")
            .fetch_all(pool)
            .instrument(span)
            .await
        {
            Ok(rows) => {
                if configured.is_none() {
                    configured = Some(!rows.is_empty());
                }

                if let Some(row) = rows.first() {
                    // Relay log metrics
                    let relay_space: Option<i64> = row.try_get("Relay_Log_Space").ok();
                    let relay_pos: Option<i64> = row.try_get("Exec_Master_Log_Pos").ok();

                    // Try as u64 first (MariaDB returns unsigned), fall back to i64, then NULL
                    let seconds_behind: Option<i64> = row
                        .try_get::<Option<u64>, _>("Seconds_Behind_Master")
                        .ok()
                        .flatten()
                        .and_then(|v| i64::try_from(v).ok())
                        .or_else(|| {
                            row.try_get::<Option<i64>, _>("Seconds_Behind_Master")
                                .ok()
                                .flatten()
                        });

                    self.relay_log_space.set(relay_space.unwrap_or_default());
                    self.relay_log_pos.set(relay_pos.unwrap_or_default());
                    // Set to -1 when NULL (replication stopped/broken), otherwise use actual value
                    self.seconds_behind_master
                        .set(seconds_behind.unwrap_or(-1));

                    // Health status metrics
                    let io_running: Option<String> = row.try_get("Slave_IO_Running").ok();
                    let sql_running: Option<String> = row.try_get("Slave_SQL_Running").ok();
                    let last_io_errno: Option<i64> = row.try_get("Last_IO_Errno").ok();
                    let last_sql_errno: Option<i64> = row.try_get("Last_SQL_Errno").ok();
                    let master_server_id = parse_master_server_id(row);

                    // Convert Yes/No to 1/0
                    self.io_running
                        .set(i64::from(io_running.as_deref() == Some("Yes")));
                    self.sql_running
                        .set(i64::from(sql_running.as_deref() == Some("Yes")));
                    self.last_io_errno.set(last_io_errno.unwrap_or_default());
                    self.last_sql_errno
                        .set(last_sql_errno.unwrap_or_default());
                    self.master_server_id
                        .set(master_server_id.unwrap_or_default());
                }
            }
            Err(e) => debug!(error = %e, "SHOW SLAVE STATUS failed"),
        }

        self.replica_configured
            .set(i64::from(configured.unwrap_or(false)));

        Ok(())
    }
}

fn parse_master_server_id(row: &MySqlRow) -> Option<i64> {
    let unsigned = row
        .try_get::<Option<u64>, _>("Master_Server_Id")
        .ok()
        .flatten();
    let signed = row
        .try_get::<Option<i64>, _>("Master_Server_Id")
        .ok()
        .flatten();
    let text = row
        .try_get::<Option<String>, _>("Master_Server_Id")
        .ok()
        .flatten();

    parse_master_server_id_from_values(unsigned, signed, text)
}

fn parse_master_server_id_from_values(
    unsigned: Option<u64>,
    signed: Option<i64>,
    text: Option<String>,
) -> Option<i64> {
    unsigned
        .and_then(|v| i64::try_from(v).ok())
        .or(signed)
        .or_else(|| text.and_then(|value| value.parse::<i64>().ok()))
}

impl Default for ReplicaStatusCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::parse_master_server_id_from_values;

    #[test]
    fn parses_unsigned_master_server_id() {
        let zero_id = parse_master_server_id_from_values(Some(0), None, None);
        assert_eq!(zero_id, Some(0));

        let nonzero_id = parse_master_server_id_from_values(Some(123), None, None);
        assert_eq!(nonzero_id, Some(123));
    }
}
