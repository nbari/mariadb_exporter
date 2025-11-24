use crate::collectors::Collector;
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{IntGauge, Registry};
use sqlx::{MySqlPool, Row};
use std::collections::HashMap;
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;

/// Collects core `MariaDB` status/health metrics (default-on).
#[derive(Clone)]
pub struct StatusCollector {
    // Global status (connections/traffic)
    global_uptime: IntGauge,
    threads_connected: IntGauge,
    threads_running: IntGauge,
    connections: IntGauge,
    max_used_connections: IntGauge,
    aborted_connects: IntGauge,
    aborted_clients: IntGauge,
    bytes_received: IntGauge,
    bytes_sent: IntGauge,
    questions: IntGauge,
    queries: IntGauge,
    slow_queries: IntGauge,
    open_files: IntGauge,
    open_tables: IntGauge,
    table_locks_immediate: IntGauge,
    table_locks_waited: IntGauge,
    created_tmp_disk_tables: IntGauge,
    connection_errors_max_connections: IntGauge,
    connection_errors_too_many_connections: IntGauge,
    connection_errors_refused: IntGauge,
    // InnoDB
    innodb_buffer_pool_pages_data: IntGauge,
    innodb_buffer_pool_pages_dirty: IntGauge,
    innodb_buffer_pool_pages_free: IntGauge,
    innodb_buffer_pool_bytes_data: IntGauge,
    innodb_buffer_pool_bytes_dirty: IntGauge,
    innodb_buffer_pool_read_requests: IntGauge,
    innodb_buffer_pool_reads: IntGauge,
    innodb_buffer_pool_write_requests: IntGauge,
    innodb_log_waits: IntGauge,
    innodb_log_written: IntGauge,
    innodb_log_write_requests: IntGauge,
    innodb_row_lock_time: IntGauge,
    innodb_row_lock_waits: IntGauge,
    innodb_history_list_length: IntGauge,
    innodb_data_pending_reads: IntGauge,
    innodb_data_pending_writes: IntGauge,
    // Replication (replica)
    slave_status_seconds_behind: IntGauge,
    slave_status_sql_running: IntGauge,
    slave_status_io_running: IntGauge,
    // Binlog (primary)
    binlog_bytes_written: IntGauge,
    binlog_cache_disk_use: IntGauge,
    binlog_stmt_cache_disk_use: IntGauge,
    // Config flags
    have_ssl: IntGauge,
    have_openssl: IntGauge,
    performance_schema: IntGauge,
}

impl StatusCollector {
    #[must_use]
    #[allow(clippy::expect_used, clippy::too_many_lines)]
    /// Create a new status collector.
    ///
    /// # Panics
    ///
    /// Panics if metric registration opts are invalid (should never happen with static names).
    pub fn new() -> Self {
        // Small helper to create gauges consistently.
        let g = |name: &str, help: &str| {
            IntGauge::new(name, help).expect("valid metric name")
        };

        Self {
            global_uptime: g("mariadb_global_status_uptime_seconds", "Server uptime in seconds"),
            threads_connected: g(
                "mariadb_global_status_threads_connected",
                "Number of currently open connections",
            ),
            threads_running: g(
                "mariadb_global_status_threads_running",
                "Number of threads that are not sleeping",
            ),
            connections: g(
                "mariadb_global_status_connections",
                "Number of connection attempts (successful or not)",
            ),
            max_used_connections: g(
                "mariadb_global_status_max_used_connections",
                "Highest number of concurrent connections since server start",
            ),
            aborted_connects: g(
                "mariadb_global_status_aborted_connects",
                "Connections rejected due to errors",
            ),
            aborted_clients: g(
                "mariadb_global_status_aborted_clients",
                "Connections aborted because the client died without closing",
            ),
            bytes_received: g(
                "mariadb_global_status_bytes_received",
                "Bytes received from all clients",
            ),
            bytes_sent: g("mariadb_global_status_bytes_sent", "Bytes sent to all clients"),
            questions: g(
                "mariadb_global_status_questions",
                "Statements executed by clients (includes stored program calls)",
            ),
            queries: g(
                "mariadb_global_status_queries",
                "Statements executed by the server (includes replication)",
            ),
            slow_queries: g(
                "mariadb_global_status_slow_queries",
                "Number of queries longer than long_query_time",
            ),
            open_files: g(
                "mariadb_global_status_open_files",
                "Number of files open by the server",
            ),
            open_tables: g(
                "mariadb_global_status_open_tables",
                "Number of tables currently open",
            ),
            table_locks_immediate: g(
                "mariadb_global_status_table_locks_immediate",
                "Table locks granted immediately",
            ),
            table_locks_waited: g(
                "mariadb_global_status_table_locks_waited",
                "Table locks that had to wait",
            ),
            created_tmp_disk_tables: g(
                "mariadb_global_status_created_tmp_disk_tables",
                "Number of on-disk temporary tables created automatically",
            ),
            connection_errors_max_connections: g(
                "mariadb_global_status_connection_errors_max_connections",
                "Failed connections because max_connections was reached",
            ),
            connection_errors_too_many_connections: g(
                "mariadb_global_status_connection_errors_too_many_connections",
                "Failed connections because too many connections",
            ),
            connection_errors_refused: g(
                "mariadb_global_status_connection_errors_refused",
                "Failed connections because server refused them",
            ),
            innodb_buffer_pool_pages_data: g(
                "mariadb_innodb_buffer_pool_pages_data",
                "InnoDB buffer pool pages containing data",
            ),
            innodb_buffer_pool_pages_dirty: g(
                "mariadb_innodb_buffer_pool_pages_dirty",
                "InnoDB buffer pool pages currently dirty",
            ),
            innodb_buffer_pool_pages_free: g(
                "mariadb_innodb_buffer_pool_pages_free",
                "Free InnoDB buffer pool pages",
            ),
            innodb_buffer_pool_bytes_data: g(
                "mariadb_innodb_buffer_pool_bytes_data",
                "Bytes of data in InnoDB buffer pool",
            ),
            innodb_buffer_pool_bytes_dirty: g(
                "mariadb_innodb_buffer_pool_bytes_dirty",
                "Bytes of dirty data in InnoDB buffer pool",
            ),
            innodb_buffer_pool_read_requests: g(
                "mariadb_innodb_buffer_pool_read_requests",
                "Logical read requests served by the buffer pool",
            ),
            innodb_buffer_pool_reads: g(
                "mariadb_innodb_buffer_pool_reads",
                "Physical reads from disk into the buffer pool",
            ),
            innodb_buffer_pool_write_requests: g(
                "mariadb_innodb_buffer_pool_write_requests",
                "Write requests for the buffer pool",
            ),
            innodb_log_waits: g(
                "mariadb_innodb_log_waits",
                "Log writes that had to wait for a log flush",
            ),
            innodb_log_written: g(
                "mariadb_innodb_log_written",
                "Bytes written to InnoDB redo log",
            ),
            innodb_log_write_requests: g(
                "mariadb_innodb_log_write_requests",
                "InnoDB redo log write requests",
            ),
            innodb_row_lock_time: g(
                "mariadb_innodb_row_lock_time_seconds",
                "Time spent in acquiring row locks (seconds)",
            ),
            innodb_row_lock_waits: g(
                "mariadb_innodb_row_lock_waits",
                "Number of times a row lock had to wait",
            ),
            innodb_history_list_length: g(
                "mariadb_innodb_history_list_length",
                "Undo log history list length",
            ),
            innodb_data_pending_reads: g(
                "mariadb_innodb_data_pending_reads",
                "Pending InnoDB data file reads",
            ),
            innodb_data_pending_writes: g(
                "mariadb_innodb_data_pending_writes",
                "Pending InnoDB data file writes",
            ),
            slave_status_seconds_behind: g(
                "mariadb_slave_status_seconds_behind_master",
                "Seconds the replica is behind the primary",
            ),
            slave_status_sql_running: g(
                "mariadb_slave_status_sql_running",
                "Replica SQL thread running (1/0)",
            ),
            slave_status_io_running: g(
                "mariadb_slave_status_io_running",
                "Replica IO thread running (1/0)",
            ),
            binlog_bytes_written: g(
                "mariadb_binlog_bytes_written",
                "Bytes written to the binary log",
            ),
            binlog_cache_disk_use: g(
                "mariadb_binlog_cache_disk_use",
                "Number of transactions that used binlog cache disk",
            ),
            binlog_stmt_cache_disk_use: g(
                "mariadb_binlog_stmt_cache_disk_use",
                "Number of statements that used binlog stmt cache disk",
            ),
            have_ssl: g("mariadb_global_variables_have_ssl", "Server has SSL available (1/0)"),
            have_openssl: g(
                "mariadb_global_variables_have_openssl",
                "Server built with OpenSSL (1/0)",
            ),
            performance_schema: g(
                "mariadb_global_variables_performance_schema",
                "Performance schema enabled (1/0)",
            ),
        }
    }

    fn register_gauges(&self, registry: &Registry) -> Result<()> {
        let metrics: &[&IntGauge] = &[
            &self.global_uptime,
            &self.threads_connected,
            &self.threads_running,
            &self.connections,
            &self.max_used_connections,
            &self.aborted_connects,
            &self.aborted_clients,
            &self.bytes_received,
            &self.bytes_sent,
            &self.questions,
            &self.queries,
            &self.slow_queries,
            &self.open_files,
            &self.open_tables,
            &self.table_locks_immediate,
            &self.table_locks_waited,
            &self.created_tmp_disk_tables,
            &self.connection_errors_max_connections,
            &self.connection_errors_too_many_connections,
            &self.connection_errors_refused,
            &self.innodb_buffer_pool_pages_data,
            &self.innodb_buffer_pool_pages_dirty,
            &self.innodb_buffer_pool_pages_free,
            &self.innodb_buffer_pool_bytes_data,
            &self.innodb_buffer_pool_bytes_dirty,
            &self.innodb_buffer_pool_read_requests,
            &self.innodb_buffer_pool_reads,
            &self.innodb_buffer_pool_write_requests,
            &self.innodb_log_waits,
            &self.innodb_log_written,
            &self.innodb_log_write_requests,
            &self.innodb_row_lock_time,
            &self.innodb_row_lock_waits,
            &self.innodb_history_list_length,
            &self.innodb_data_pending_reads,
            &self.innodb_data_pending_writes,
            &self.slave_status_seconds_behind,
            &self.slave_status_sql_running,
            &self.slave_status_io_running,
            &self.binlog_bytes_written,
            &self.binlog_cache_disk_use,
            &self.binlog_stmt_cache_disk_use,
            &self.have_ssl,
            &self.have_openssl,
            &self.performance_schema,
        ];

        for m in metrics {
            registry.register(Box::new((*m).clone()))?;
        }

        Ok(())
    }

    fn set_from_status(status: &HashMap<String, String>, key: &str, gauge: &IntGauge) {
        if let Some(raw) = status.get(&key.to_ascii_uppercase()) {
            if let Ok(v) = raw.parse::<i64>() {
                gauge.set(v);
            } else {
                debug!(metric = key, value = raw, "could not parse status value");
            }
        }
    }

    fn set_from_status_ms_to_seconds(status: &HashMap<String, String>, key: &str, gauge: &IntGauge) {
        if let Some(raw) = status.get(&key.to_ascii_uppercase()) {
            if let Ok(v) = raw.parse::<i64>() { gauge.set(v / 1_000) } else { debug!(metric = key, value = raw, "could not parse status value") }
        }
    }

    fn collect_global_status(&self, status: &HashMap<String, String>) {
        Self::set_from_status(status, "Uptime", &self.global_uptime);
        Self::set_from_status(status, "Threads_connected", &self.threads_connected);
        Self::set_from_status(status, "Threads_running", &self.threads_running);
        Self::set_from_status(status, "Connections", &self.connections);
        Self::set_from_status(status, "Max_used_connections", &self.max_used_connections);
        Self::set_from_status(status, "Aborted_connects", &self.aborted_connects);
        Self::set_from_status(status, "Aborted_clients", &self.aborted_clients);
        Self::set_from_status(status, "Bytes_received", &self.bytes_received);
        Self::set_from_status(status, "Bytes_sent", &self.bytes_sent);
        Self::set_from_status(status, "Questions", &self.questions);
        Self::set_from_status(status, "Queries", &self.queries);
        Self::set_from_status(status, "Slow_queries", &self.slow_queries);
        Self::set_from_status(status, "Open_files", &self.open_files);
        Self::set_from_status(status, "Open_tables", &self.open_tables);
        Self::set_from_status(status, "Table_locks_immediate", &self.table_locks_immediate);
        Self::set_from_status(status, "Table_locks_waited", &self.table_locks_waited);
        Self::set_from_status(status, "Created_tmp_disk_tables", &self.created_tmp_disk_tables);
        Self::set_from_status(
            status,
            "Connection_errors_max_connections",
            &self.connection_errors_max_connections,
        );
        Self::set_from_status(
            status,
            "Connection_errors_too_many_connections",
            &self.connection_errors_too_many_connections,
        );
        Self::set_from_status(
            status,
            "Connection_errors_refused",
            &self.connection_errors_refused,
        );
    }

    fn collect_innodb(&self, status: &HashMap<String, String>) {
        Self::set_from_status(
            status,
            "Innodb_buffer_pool_pages_data",
            &self.innodb_buffer_pool_pages_data,
        );
        Self::set_from_status(
            status,
            "Innodb_buffer_pool_pages_dirty",
            &self.innodb_buffer_pool_pages_dirty,
        );
        Self::set_from_status(
            status,
            "Innodb_buffer_pool_pages_free",
            &self.innodb_buffer_pool_pages_free,
        );
        Self::set_from_status(
            status,
            "Innodb_buffer_pool_bytes_data",
            &self.innodb_buffer_pool_bytes_data,
        );
        Self::set_from_status(
            status,
            "Innodb_buffer_pool_bytes_dirty",
            &self.innodb_buffer_pool_bytes_dirty,
        );
        Self::set_from_status(
            status,
            "Innodb_buffer_pool_read_requests",
            &self.innodb_buffer_pool_read_requests,
        );
        Self::set_from_status(status, "Innodb_buffer_pool_reads", &self.innodb_buffer_pool_reads);
        Self::set_from_status(
            status,
            "Innodb_buffer_pool_write_requests",
            &self.innodb_buffer_pool_write_requests,
        );
        Self::set_from_status(status, "Innodb_log_waits", &self.innodb_log_waits);
        Self::set_from_status(status, "Innodb_log_written", &self.innodb_log_written);
        Self::set_from_status(
            status,
            "Innodb_log_write_requests",
            &self.innodb_log_write_requests,
        );
        Self::set_from_status_ms_to_seconds(status, "Innodb_row_lock_time", &self.innodb_row_lock_time);
        Self::set_from_status(status, "Innodb_row_lock_waits", &self.innodb_row_lock_waits);
        Self::set_from_status(
            status,
            "Innodb_history_list_length",
            &self.innodb_history_list_length,
        );
        Self::set_from_status(
            status,
            "Innodb_data_pending_reads",
            &self.innodb_data_pending_reads,
        );
        Self::set_from_status(
            status,
            "Innodb_data_pending_writes",
            &self.innodb_data_pending_writes,
        );
    }

    async fn collect_replication(&self, pool: &MySqlPool) -> Result<()> {
        let span = info_span!(
            "db.query",
            db.system = "mysql",
            db.operation = "SHOW",
            db.statement = "SHOW SLAVE STATUS",
            otel.kind = "client"
        );

        let rows = sqlx::query("SHOW SLAVE STATUS")
            .fetch_all(pool)
            .instrument(span)
            .await?;

        if rows.is_empty() {
            // Not a replica; clear replica-only gauges.
            self.slave_status_seconds_behind.set(0);
            self.slave_status_sql_running.set(0);
            self.slave_status_io_running.set(0);
            return Ok(());
        }

        // Use first row (MariaDB typically has one channel unless multi-source).
        if let Some(row) = rows.first() {
            let seconds: Option<i64> = row.try_get("Seconds_Behind_Master").ok();
            let io_running: Option<String> = row.try_get("Slave_IO_Running").ok();
            let sql_running: Option<String> = row.try_get("Slave_SQL_Running").ok();

            self.slave_status_seconds_behind
                .set(seconds.unwrap_or_default());
            self.slave_status_io_running
                .set(i64::from(Self::as_running(io_running.as_ref())));
            self.slave_status_sql_running
                .set(i64::from(Self::as_running(sql_running.as_ref())));
        }

        Ok(())
    }

    fn as_running(val: Option<&String>) -> i32 {
        match val.map(String::as_str) {
            Some("Yes" | "ON" | "Running") => 1,
            _ => 0,
        }
    }

    fn collect_binlog(&self, status: &HashMap<String, String>) {
        Self::set_from_status(status, "Binlog_bytes_written", &self.binlog_bytes_written);
        Self::set_from_status(status, "Binlog_cache_disk_use", &self.binlog_cache_disk_use);
        Self::set_from_status(
            status,
            "Binlog_stmt_cache_disk_use",
            &self.binlog_stmt_cache_disk_use,
        );
    }

    fn collect_variables(&self, vars: &HashMap<String, String>) {
        let to_flag = |val: Option<&String>| match val.map(|s| s.to_ascii_lowercase()) {
            Some(v) if v == "yes" || v == "on" || v == "true" || v == "1" => 1,
            _ => 0,
        };

        self.have_ssl
            .set(i64::from(to_flag(vars.get(&"have_ssl".to_string()))));
        self.have_openssl
            .set(i64::from(to_flag(vars.get(&"have_openssl".to_string()))));
        self.performance_schema
            .set(i64::from(to_flag(vars.get(&"performance_schema".to_string()))));
    }
}

impl Collector for StatusCollector {
    fn name(&self) -> &'static str {
        "status"
    }

    #[instrument(
        skip(self, registry),
        level = "info",
        err,
        fields(collector = "status")
    )]
    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        self.register_gauges(registry)
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "status", otel.kind = "internal"))]
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let status_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "SELECT VARIABLE_NAME, VARIABLE_VALUE FROM information_schema.global_status",
                otel.kind = "client"
            );
            let status_rows = sqlx::query(
                "SELECT VARIABLE_NAME, VARIABLE_VALUE FROM information_schema.global_status",
            )
            .fetch_all(pool)
            .instrument(status_span)
            .await?;

            let status_map: HashMap<String, String> = status_rows
                .into_iter()
                .filter_map(|row| {
                    let name: Option<String> = row.try_get("VARIABLE_NAME").ok();
                    let val: Option<String> = row.try_get("VARIABLE_VALUE").ok();
                    name.zip(val)
                        .map(|(n, v)| (n.to_ascii_uppercase(), v))
                })
                .collect();

            self.collect_global_status(&status_map);
            self.collect_innodb(&status_map);
            self.collect_binlog(&status_map);

            let vars_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "SELECT VARIABLE_NAME, VARIABLE_VALUE FROM information_schema.global_variables WHERE VARIABLE_NAME IN ('have_ssl','have_openssl','performance_schema')",
                otel.kind = "client"
            );
            let vars_rows = sqlx::query(
                "SELECT VARIABLE_NAME, VARIABLE_VALUE FROM information_schema.global_variables WHERE VARIABLE_NAME IN ('have_ssl','have_openssl','performance_schema')",
            )
            .fetch_all(pool)
            .instrument(vars_span)
            .await?;

            let vars_map: HashMap<String, String> = vars_rows
                .into_iter()
                .filter_map(|row| {
                    let name: Option<String> = row.try_get("VARIABLE_NAME").ok();
                    let val: Option<String> = row.try_get("VARIABLE_VALUE").ok();
                    name.zip(val).map(|(n, v)| (n.to_ascii_lowercase(), v))
                })
                .collect();

            self.collect_variables(&vars_map);
            self.collect_replication(pool).await?;
            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        true
    }
}
impl Default for StatusCollector {
    fn default() -> Self {
        Self::new()
    }
}
