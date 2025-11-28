use crate::collectors::Collector;
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{IntCounter, IntGauge, Registry};
use sqlx::{MySqlPool, Row};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
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
    questions_total: IntCounter,
    queries_total: IntCounter,
    questions_last: Arc<AtomicI64>,
    queries_last: Arc<AtomicI64>,
    slow_queries: IntGauge,
    open_files: IntGauge,
    open_tables: IntGauge,
    table_locks_immediate: IntGauge,
    table_locks_waited: IntGauge,
    created_tmp_disk_tables: IntGauge,
    created_tmp_tables: IntGauge,
    created_tmp_files: IntGauge,
    connection_errors_max_connections: IntGauge,
    connection_errors_too_many_connections: IntGauge,
    connection_errors_refused: IntGauge,
    // Query execution and sorts
    sort_merge_passes: IntGauge,
    sort_range: IntGauge,
    sort_rows: IntGauge,
    sort_scan: IntGauge,
    select_full_join: IntGauge,
    select_full_range_join: IntGauge,
    select_range: IntGauge,
    select_range_check: IntGauge,
    select_scan: IntGauge,
    // Handler statistics (index usage)
    handler_read_first: IntGauge,
    handler_read_key: IntGauge,
    handler_read_next: IntGauge,
    handler_read_prev: IntGauge,
    handler_read_rnd: IntGauge,
    handler_read_rnd_next: IntGauge,
    handler_write: IntGauge,
    handler_update: IntGauge,
    handler_delete: IntGauge,
    // Table cache
    opened_tables: IntGauge,
    opened_files: IntGauge,
    table_open_cache_hits: IntGauge,
    table_open_cache_misses: IntGauge,
    table_open_cache_overflows: IntGauge,
    // Thread cache
    threads_created: IntGauge,
    threads_cached: IntGauge,
    // Key buffer (MyISAM)
    key_read_requests: IntGauge,
    key_reads: IntGauge,
    key_write_requests: IntGauge,
    key_writes: IntGauge,
    key_blocks_unused: IntGauge,
    key_blocks_used: IntGauge,
    key_blocks_not_flushed: IntGauge,
    // InnoDB
    innodb_buffer_pool_pages_data: IntGauge,
    innodb_buffer_pool_pages_dirty: IntGauge,
    innodb_buffer_pool_pages_free: IntGauge,
    innodb_buffer_pool_size_bytes: IntGauge,
    innodb_buffer_pool_bytes_dirty: IntGauge,
    innodb_buffer_pool_read_requests: IntGauge,
    innodb_buffer_pool_reads: IntGauge,
    innodb_buffer_pool_write_requests: IntGauge,
    innodb_log_waits: IntGauge,
    innodb_log_written: IntGauge,
    innodb_log_write_requests: IntGauge,
    innodb_row_lock_time: IntGauge,
    innodb_row_lock_waits: IntGauge,
    innodb_row_lock_current_waits: IntGauge,
    innodb_history_list_length: IntGauge,
    innodb_data_pending_reads: IntGauge,
    innodb_data_pending_writes: IntGauge,
    innodb_data_pending_fsyncs: IntGauge,
    // InnoDB row operations
    innodb_rows_read: IntGauge,
    innodb_rows_inserted: IntGauge,
    innodb_rows_updated: IntGauge,
    innodb_rows_deleted: IntGauge,
    // InnoDB data I/O
    innodb_data_reads: IntGauge,
    innodb_data_writes: IntGauge,
    innodb_data_read_bytes: IntGauge,
    innodb_data_written_bytes: IntGauge,
    innodb_data_fsyncs: IntGauge,
    // InnoDB deadlocks and lock timeouts
    innodb_deadlocks: IntGauge,
    innodb_lock_timeouts: IntGauge,
    // InnoDB buffer pool efficiency
    innodb_buffer_pool_pages_misc: IntGauge,
    innodb_buffer_pool_pages_total: IntGauge,
    innodb_buffer_pool_wait_free: IntGauge,
    innodb_buffer_pool_read_ahead: IntGauge,
    innodb_buffer_pool_read_ahead_evicted: IntGauge,
    // InnoDB log
    innodb_os_log_written_bytes: IntGauge,
    innodb_os_log_fsyncs: IntGauge,
    innodb_os_log_pending_writes: IntGauge,
    innodb_os_log_pending_fsyncs: IntGauge,
    innodb_log_write_ratio: IntGauge,
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
        // Small helpers to create metrics consistently.
        let g = |name: &str, help: &str| IntGauge::new(name, help).expect("valid metric name");
        let c = |name: &str, help: &str| IntCounter::new(name, help).expect("valid metric name");

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
            questions_total: c(
                "mariadb_global_status_questions_total",
                "Statements executed by clients (includes stored program calls)",
            ),
            queries_total: c(
                "mariadb_global_status_queries_total",
                "Statements executed by the server (includes replication)",
            ),
            questions_last: Arc::new(AtomicI64::new(0)),
            queries_last: Arc::new(AtomicI64::new(0)),
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
            created_tmp_tables: g(
                "mariadb_global_status_created_tmp_tables",
                "Number of internal temporary tables created",
            ),
            created_tmp_files: g(
                "mariadb_global_status_created_tmp_files",
                "Number of temporary files created",
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
            // Query execution and sorts
            sort_merge_passes: g(
                "mariadb_global_status_sort_merge_passes",
                "Number of merge passes for sort operations",
            ),
            sort_range: g(
                "mariadb_global_status_sort_range",
                "Number of sorts done using ranges",
            ),
            sort_rows: g(
                "mariadb_global_status_sort_rows",
                "Number of rows sorted",
            ),
            sort_scan: g(
                "mariadb_global_status_sort_scan",
                "Number of sorts done by scanning the table",
            ),
            select_full_join: g(
                "mariadb_global_status_select_full_join",
                "Joins without indexes (should be 0)",
            ),
            select_full_range_join: g(
                "mariadb_global_status_select_full_range_join",
                "Joins using range search on reference table",
            ),
            select_range: g(
                "mariadb_global_status_select_range",
                "Joins using ranges on the first table",
            ),
            select_range_check: g(
                "mariadb_global_status_select_range_check",
                "Joins without keys that check for key usage after each row",
            ),
            select_scan: g(
                "mariadb_global_status_select_scan",
                "Joins done by scanning the first table",
            ),
            // Handler statistics (index usage)
            handler_read_first: g(
                "mariadb_global_status_handler_read_first",
                "Times first entry in index was read",
            ),
            handler_read_key: g(
                "mariadb_global_status_handler_read_key",
                "Requests to read a row based on a key",
            ),
            handler_read_next: g(
                "mariadb_global_status_handler_read_next",
                "Requests to read next row in key order",
            ),
            handler_read_prev: g(
                "mariadb_global_status_handler_read_prev",
                "Requests to read previous row in key order",
            ),
            handler_read_rnd: g(
                "mariadb_global_status_handler_read_rnd",
                "Requests to read a row based on a fixed position",
            ),
            handler_read_rnd_next: g(
                "mariadb_global_status_handler_read_rnd_next",
                "Requests to read next row in data file",
            ),
            handler_write: g(
                "mariadb_global_status_handler_write",
                "Requests to insert a row into a table",
            ),
            handler_update: g(
                "mariadb_global_status_handler_update",
                "Requests to update a row in a table",
            ),
            handler_delete: g(
                "mariadb_global_status_handler_delete",
                "Requests to delete a row from a table",
            ),
            // Table cache
            opened_tables: g(
                "mariadb_global_status_opened_tables",
                "Number of tables that have been opened",
            ),
            opened_files: g(
                "mariadb_global_status_opened_files",
                "Number of files that have been opened",
            ),
            table_open_cache_hits: g(
                "mariadb_global_status_table_open_cache_hits",
                "Number of table cache hits",
            ),
            table_open_cache_misses: g(
                "mariadb_global_status_table_open_cache_misses",
                "Number of table cache misses",
            ),
            table_open_cache_overflows: g(
                "mariadb_global_status_table_open_cache_overflows",
                "Number of table cache overflows",
            ),
            // Thread cache
            threads_created: g(
                "mariadb_global_status_threads_created",
                "Number of threads created to handle connections",
            ),
            threads_cached: g(
                "mariadb_global_status_threads_cached",
                "Number of threads in the thread cache",
            ),
            // Key buffer (MyISAM)
            key_read_requests: g(
                "mariadb_global_status_key_read_requests",
                "Number of requests to read a key block from cache",
            ),
            key_reads: g(
                "mariadb_global_status_key_reads",
                "Number of physical reads of a key block from disk",
            ),
            key_write_requests: g(
                "mariadb_global_status_key_write_requests",
                "Number of requests to write a key block to cache",
            ),
            key_writes: g(
                "mariadb_global_status_key_writes",
                "Number of physical writes of a key block to disk",
            ),
            key_blocks_unused: g(
                "mariadb_global_status_key_blocks_unused",
                "Number of unused blocks in the key cache",
            ),
            key_blocks_used: g(
                "mariadb_global_status_key_blocks_used",
                "Number of used blocks in the key cache",
            ),
            key_blocks_not_flushed: g(
                "mariadb_global_status_key_blocks_not_flushed",
                "Number of key blocks that have changed but not flushed to disk",
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
            innodb_buffer_pool_size_bytes: g(
                "mariadb_innodb_buffer_pool_size_bytes",
                "Configured size of the InnoDB buffer pool in bytes",
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
            innodb_row_lock_current_waits: g(
                "mariadb_innodb_row_lock_current_waits",
                "Number of row locks currently being waited for",
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
            innodb_data_pending_fsyncs: g(
                "mariadb_innodb_data_pending_fsyncs",
                "Pending InnoDB fsync() calls",
            ),
            // InnoDB row operations
            innodb_rows_read: g(
                "mariadb_innodb_rows_read",
                "Number of rows read from InnoDB tables",
            ),
            innodb_rows_inserted: g(
                "mariadb_innodb_rows_inserted",
                "Number of rows inserted into InnoDB tables",
            ),
            innodb_rows_updated: g(
                "mariadb_innodb_rows_updated",
                "Number of rows updated in InnoDB tables",
            ),
            innodb_rows_deleted: g(
                "mariadb_innodb_rows_deleted",
                "Number of rows deleted from InnoDB tables",
            ),
            // InnoDB data I/O
            innodb_data_reads: g(
                "mariadb_innodb_data_reads",
                "Number of data reads",
            ),
            innodb_data_writes: g(
                "mariadb_innodb_data_writes",
                "Number of data writes",
            ),
            innodb_data_read_bytes: g(
                "mariadb_innodb_data_read_bytes",
                "Amount of data read in bytes",
            ),
            innodb_data_written_bytes: g(
                "mariadb_innodb_data_written_bytes",
                "Amount of data written in bytes",
            ),
            innodb_data_fsyncs: g(
                "mariadb_innodb_data_fsyncs",
                "Number of fsync() operations",
            ),
            // InnoDB deadlocks and lock timeouts
            innodb_deadlocks: g(
                "mariadb_innodb_deadlocks_total",
                "Total number of InnoDB deadlocks",
            ),
            innodb_lock_timeouts: g(
                "mariadb_innodb_lock_timeouts_total",
                "Total number of InnoDB lock timeouts",
            ),
            // InnoDB buffer pool efficiency
            innodb_buffer_pool_pages_misc: g(
                "mariadb_innodb_buffer_pool_pages_misc",
                "InnoDB buffer pool pages for misc use",
            ),
            innodb_buffer_pool_pages_total: g(
                "mariadb_innodb_buffer_pool_pages_total",
                "Total number of InnoDB buffer pool pages",
            ),
            innodb_buffer_pool_wait_free: g(
                "mariadb_innodb_buffer_pool_wait_free",
                "Number of times waited for free buffer pool page",
            ),
            innodb_buffer_pool_read_ahead: g(
                "mariadb_innodb_buffer_pool_read_ahead",
                "Number of pages read ahead",
            ),
            innodb_buffer_pool_read_ahead_evicted: g(
                "mariadb_innodb_buffer_pool_read_ahead_evicted",
                "Number of read ahead pages evicted without being accessed",
            ),
            // InnoDB log
            innodb_os_log_written_bytes: g(
                "mariadb_innodb_os_log_written_bytes",
                "Bytes written to InnoDB log files",
            ),
            innodb_os_log_fsyncs: g(
                "mariadb_innodb_os_log_fsyncs",
                "Number of fsync() writes to InnoDB log files",
            ),
            innodb_os_log_pending_writes: g(
                "mariadb_innodb_os_log_pending_writes",
                "Number of pending InnoDB log writes",
            ),
            innodb_os_log_pending_fsyncs: g(
                "mariadb_innodb_os_log_pending_fsyncs",
                "Number of pending InnoDB log fsyncs",
            ),
            innodb_log_write_ratio: g(
                "mariadb_innodb_log_write_ratio",
                "InnoDB log write ratio (log writes / write requests)",
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

    #[allow(clippy::too_many_lines)]
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
            &self.slow_queries,
            &self.open_files,
            &self.open_tables,
            &self.table_locks_immediate,
            &self.table_locks_waited,
            &self.created_tmp_disk_tables,
            &self.created_tmp_tables,
            &self.created_tmp_files,
            &self.connection_errors_max_connections,
            &self.connection_errors_too_many_connections,
            &self.connection_errors_refused,
            // Query execution and sorts
            &self.sort_merge_passes,
            &self.sort_range,
            &self.sort_rows,
            &self.sort_scan,
            &self.select_full_join,
            &self.select_full_range_join,
            &self.select_range,
            &self.select_range_check,
            &self.select_scan,
            // Handler statistics
            &self.handler_read_first,
            &self.handler_read_key,
            &self.handler_read_next,
            &self.handler_read_prev,
            &self.handler_read_rnd,
            &self.handler_read_rnd_next,
            &self.handler_write,
            &self.handler_update,
            &self.handler_delete,
            // Table cache
            &self.opened_tables,
            &self.opened_files,
            &self.table_open_cache_hits,
            &self.table_open_cache_misses,
            &self.table_open_cache_overflows,
            // Thread cache
            &self.threads_created,
            &self.threads_cached,
            // Key buffer (MyISAM)
            &self.key_read_requests,
            &self.key_reads,
            &self.key_write_requests,
            &self.key_writes,
            &self.key_blocks_unused,
            &self.key_blocks_used,
            &self.key_blocks_not_flushed,
            // InnoDB
            &self.innodb_buffer_pool_pages_data,
            &self.innodb_buffer_pool_pages_dirty,
            &self.innodb_buffer_pool_pages_free,
            &self.innodb_buffer_pool_size_bytes,
            &self.innodb_buffer_pool_bytes_dirty,
            &self.innodb_buffer_pool_read_requests,
            &self.innodb_buffer_pool_reads,
            &self.innodb_buffer_pool_write_requests,
            &self.innodb_log_waits,
            &self.innodb_log_written,
            &self.innodb_log_write_requests,
            &self.innodb_row_lock_time,
            &self.innodb_row_lock_waits,
            &self.innodb_row_lock_current_waits,
            &self.innodb_history_list_length,
            &self.innodb_data_pending_reads,
            &self.innodb_data_pending_writes,
            &self.innodb_data_pending_fsyncs,
            // InnoDB row operations
            &self.innodb_rows_read,
            &self.innodb_rows_inserted,
            &self.innodb_rows_updated,
            &self.innodb_rows_deleted,
            // InnoDB data I/O
            &self.innodb_data_reads,
            &self.innodb_data_writes,
            &self.innodb_data_read_bytes,
            &self.innodb_data_written_bytes,
            &self.innodb_data_fsyncs,
            // InnoDB deadlocks
            &self.innodb_deadlocks,
            &self.innodb_lock_timeouts,
            // InnoDB buffer pool efficiency
            &self.innodb_buffer_pool_pages_misc,
            &self.innodb_buffer_pool_pages_total,
            &self.innodb_buffer_pool_wait_free,
            &self.innodb_buffer_pool_read_ahead,
            &self.innodb_buffer_pool_read_ahead_evicted,
            // InnoDB log
            &self.innodb_os_log_written_bytes,
            &self.innodb_os_log_fsyncs,
            &self.innodb_os_log_pending_writes,
            &self.innodb_os_log_pending_fsyncs,
            &self.innodb_log_write_ratio,
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

        registry.register(Box::new(self.questions_total.clone()))?;
        registry.register(Box::new(self.queries_total.clone()))?;

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

    fn set_counter_from_status(
        status: &HashMap<String, String>,
        key: &str,
        counter: &IntCounter,
        last_seen: &AtomicI64,
    ) {
        if let Some(raw) = status.get(&key.to_ascii_uppercase()) {
            if let Ok(v) = raw.parse::<i64>() {
                let previous = last_seen.swap(v, Ordering::Relaxed);
                if v >= 0 {
                    if previous <= 0 {
                        counter.reset();
                        if let Ok(incr) = u64::try_from(v) {
                            counter.inc_by(incr);
                        }
                    } else if v >= previous {
                        let delta = v.saturating_sub(previous);
                        if let Ok(incr) = u64::try_from(delta) {
                            counter.inc_by(incr);
                        }
                    } else {
                        counter.reset();
                        if let Ok(incr) = u64::try_from(v) {
                            counter.inc_by(incr);
                        }
                    }
                }
            } else {
                debug!(metric = key, value = raw, "could not parse status value");
            }
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
        Self::set_counter_from_status(
            status,
            "Questions",
            &self.questions_total,
            &self.questions_last,
        );
        Self::set_counter_from_status(status, "Queries", &self.queries_total, &self.queries_last);
        Self::set_from_status(status, "Slow_queries", &self.slow_queries);
        Self::set_from_status(status, "Open_files", &self.open_files);
        Self::set_from_status(status, "Open_tables", &self.open_tables);
        Self::set_from_status(status, "Table_locks_immediate", &self.table_locks_immediate);
        Self::set_from_status(status, "Table_locks_waited", &self.table_locks_waited);
        Self::set_from_status(status, "Created_tmp_disk_tables", &self.created_tmp_disk_tables);
        Self::set_from_status(status, "Created_tmp_tables", &self.created_tmp_tables);
        Self::set_from_status(status, "Created_tmp_files", &self.created_tmp_files);
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

        // Query execution and sorts
        Self::set_from_status(status, "Sort_merge_passes", &self.sort_merge_passes);
        Self::set_from_status(status, "Sort_range", &self.sort_range);
        Self::set_from_status(status, "Sort_rows", &self.sort_rows);
        Self::set_from_status(status, "Sort_scan", &self.sort_scan);
        Self::set_from_status(status, "Select_full_join", &self.select_full_join);
        Self::set_from_status(status, "Select_full_range_join", &self.select_full_range_join);
        Self::set_from_status(status, "Select_range", &self.select_range);
        Self::set_from_status(status, "Select_range_check", &self.select_range_check);
        Self::set_from_status(status, "Select_scan", &self.select_scan);

        // Handler statistics
        Self::set_from_status(status, "Handler_read_first", &self.handler_read_first);
        Self::set_from_status(status, "Handler_read_key", &self.handler_read_key);
        Self::set_from_status(status, "Handler_read_next", &self.handler_read_next);
        Self::set_from_status(status, "Handler_read_prev", &self.handler_read_prev);
        Self::set_from_status(status, "Handler_read_rnd", &self.handler_read_rnd);
        Self::set_from_status(status, "Handler_read_rnd_next", &self.handler_read_rnd_next);
        Self::set_from_status(status, "Handler_write", &self.handler_write);
        Self::set_from_status(status, "Handler_update", &self.handler_update);
        Self::set_from_status(status, "Handler_delete", &self.handler_delete);

        // Table cache
        Self::set_from_status(status, "Opened_tables", &self.opened_tables);
        Self::set_from_status(status, "Opened_files", &self.opened_files);
        Self::set_from_status(status, "Table_open_cache_hits", &self.table_open_cache_hits);
        Self::set_from_status(status, "Table_open_cache_misses", &self.table_open_cache_misses);
        Self::set_from_status(status, "Table_open_cache_overflows", &self.table_open_cache_overflows);

        // Thread cache
        Self::set_from_status(status, "Threads_created", &self.threads_created);
        Self::set_from_status(status, "Threads_cached", &self.threads_cached);

        // Key buffer (MyISAM)
        Self::set_from_status(status, "Key_read_requests", &self.key_read_requests);
        Self::set_from_status(status, "Key_reads", &self.key_reads);
        Self::set_from_status(status, "Key_write_requests", &self.key_write_requests);
        Self::set_from_status(status, "Key_writes", &self.key_writes);
        Self::set_from_status(status, "Key_blocks_unused", &self.key_blocks_unused);
        Self::set_from_status(status, "Key_blocks_used", &self.key_blocks_used);
        Self::set_from_status(status, "Key_blocks_not_flushed", &self.key_blocks_not_flushed);
    }

    #[allow(clippy::too_many_lines)]
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
        // Note: innodb_buffer_pool_size_bytes is set from GLOBAL VARIABLES in collect_variables()
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
        Self::set_from_status(status, "Innodb_row_lock_current_waits", &self.innodb_row_lock_current_waits);
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
        Self::set_from_status(
            status,
            "Innodb_data_pending_fsyncs",
            &self.innodb_data_pending_fsyncs,
        );

        // InnoDB row operations
        Self::set_from_status(status, "Innodb_rows_read", &self.innodb_rows_read);
        Self::set_from_status(status, "Innodb_rows_inserted", &self.innodb_rows_inserted);
        Self::set_from_status(status, "Innodb_rows_updated", &self.innodb_rows_updated);
        Self::set_from_status(status, "Innodb_rows_deleted", &self.innodb_rows_deleted);

        // InnoDB data I/O
        Self::set_from_status(status, "Innodb_data_reads", &self.innodb_data_reads);
        Self::set_from_status(status, "Innodb_data_writes", &self.innodb_data_writes);
        Self::set_from_status(status, "Innodb_data_read", &self.innodb_data_read_bytes);
        Self::set_from_status(status, "Innodb_data_written", &self.innodb_data_written_bytes);
        Self::set_from_status(status, "Innodb_data_fsyncs", &self.innodb_data_fsyncs);

        // InnoDB deadlocks and lock timeouts
        Self::set_from_status(status, "Innodb_deadlocks", &self.innodb_deadlocks);
        Self::set_from_status(status, "Innodb_row_lock_time_max", &self.innodb_lock_timeouts);

        // InnoDB buffer pool efficiency
        Self::set_from_status(status, "Innodb_buffer_pool_pages_misc", &self.innodb_buffer_pool_pages_misc);
        Self::set_from_status(status, "Innodb_buffer_pool_pages_total", &self.innodb_buffer_pool_pages_total);
        Self::set_from_status(status, "Innodb_buffer_pool_wait_free", &self.innodb_buffer_pool_wait_free);
        Self::set_from_status(status, "Innodb_buffer_pool_read_ahead", &self.innodb_buffer_pool_read_ahead);
        Self::set_from_status(status, "Innodb_buffer_pool_read_ahead_evicted", &self.innodb_buffer_pool_read_ahead_evicted);

        // InnoDB log
        Self::set_from_status(status, "Innodb_os_log_written", &self.innodb_os_log_written_bytes);
        Self::set_from_status(status, "Innodb_os_log_fsyncs", &self.innodb_os_log_fsyncs);
        Self::set_from_status(status, "Innodb_os_log_pending_writes", &self.innodb_os_log_pending_writes);
        Self::set_from_status(status, "Innodb_os_log_pending_fsyncs", &self.innodb_os_log_pending_fsyncs);

        // Calculate InnoDB log write ratio (avoid division by zero)
        if let Some(write_requests) = status.get("INNODB_LOG_WRITE_REQUESTS")
            && let Ok(requests) = write_requests.parse::<i64>()
            && requests > 0
            && let Some(log_writes) = status.get("INNODB_LOG_WRITES")
            && let Ok(writes) = log_writes.parse::<i64>()
        {
            let ratio = (writes * 100) / requests;
            self.innodb_log_write_ratio.set(ratio);
        }
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

        // Set innodb_buffer_pool_size from global variable
        if let Some(raw) = vars.get(&"innodb_buffer_pool_size".to_string()) {
            if let Ok(v) = raw.parse::<i64>() {
                self.innodb_buffer_pool_size_bytes.set(v);
            } else {
                debug!(metric = "innodb_buffer_pool_size", value = raw, "could not parse variable value");
            }
        }
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
                db.statement = "SELECT VARIABLE_NAME, VARIABLE_VALUE FROM information_schema.global_variables WHERE VARIABLE_NAME IN ('have_ssl','have_openssl','performance_schema','innodb_buffer_pool_size')",
                otel.kind = "client"
            );
            let vars_rows = sqlx::query(
                "SELECT VARIABLE_NAME, VARIABLE_VALUE FROM information_schema.global_variables WHERE VARIABLE_NAME IN ('have_ssl','have_openssl','performance_schema','innodb_buffer_pool_size')",
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
