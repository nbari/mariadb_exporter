use crate::collectors::{Collected, Collector, NO_LABELS};
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{IntCounterVec, IntGaugeVec, Opts, Registry};
use sqlx::{MySqlPool, Row};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;

/// Collects core `MariaDB` status/health metrics (default-on).
#[derive(Clone)]
pub struct StatusCollector {
    // Global status (connections/traffic)
    global_uptime: IntGaugeVec,
    threads_connected: IntGaugeVec,
    threads_running: IntGaugeVec,
    connections: IntGaugeVec,
    max_used_connections: IntGaugeVec,
    aborted_connects: IntGaugeVec,
    aborted_clients: IntGaugeVec,
    bytes_received: IntGaugeVec,
    bytes_sent: IntGaugeVec,
    questions_total: IntCounterVec,
    queries_total: IntCounterVec,
    questions_last: Arc<AtomicI64>,
    queries_last: Arc<AtomicI64>,
    slow_queries: IntGaugeVec,
    open_files: IntGaugeVec,
    open_tables: IntGaugeVec,
    table_locks_immediate: IntGaugeVec,
    table_locks_waited: IntGaugeVec,
    created_tmp_disk_tables: IntGaugeVec,
    created_tmp_tables: IntGaugeVec,
    created_tmp_files: IntGaugeVec,
    connection_errors_max_connections: IntGaugeVec,
    connection_errors_too_many_connections: IntGaugeVec,
    connection_errors_refused: IntGaugeVec,
    // Query execution and sorts
    sort_merge_passes: IntGaugeVec,
    sort_range: IntGaugeVec,
    sort_rows: IntGaugeVec,
    sort_scan: IntGaugeVec,
    select_full_join: IntGaugeVec,
    select_full_range_join: IntGaugeVec,
    select_range: IntGaugeVec,
    select_range_check: IntGaugeVec,
    select_scan: IntGaugeVec,
    // Handler statistics (index usage)
    handler_read_first: IntGaugeVec,
    handler_read_key: IntGaugeVec,
    handler_read_next: IntGaugeVec,
    handler_read_prev: IntGaugeVec,
    handler_read_rnd: IntGaugeVec,
    handler_read_rnd_next: IntGaugeVec,
    handler_write: IntGaugeVec,
    handler_update: IntGaugeVec,
    handler_delete: IntGaugeVec,
    // Command statistics (SQL-level)
    com_select: IntGaugeVec,
    com_insert: IntGaugeVec,
    com_update: IntGaugeVec,
    com_delete: IntGaugeVec,
    com_replace: IntGaugeVec,
    // Table cache
    opened_tables: IntGaugeVec,
    opened_files: IntGaugeVec,
    table_open_cache_hits: IntGaugeVec,
    table_open_cache_misses: IntGaugeVec,
    table_open_cache_overflows: IntGaugeVec,
    // Thread cache
    threads_created: IntGaugeVec,
    threads_cached: IntGaugeVec,
    // Key buffer (MyISAM)
    key_read_requests: IntGaugeVec,
    key_reads: IntGaugeVec,
    key_write_requests: IntGaugeVec,
    key_writes: IntGaugeVec,
    key_blocks_unused: IntGaugeVec,
    key_blocks_used: IntGaugeVec,
    key_blocks_not_flushed: IntGaugeVec,
    // InnoDB
    innodb_buffer_pool_pages_data: IntGaugeVec,
    innodb_buffer_pool_pages_dirty: IntGaugeVec,
    innodb_buffer_pool_pages_free: IntGaugeVec,
    innodb_buffer_pool_size_bytes: IntGaugeVec,
    innodb_buffer_pool_bytes_dirty: IntGaugeVec,
    innodb_buffer_pool_read_requests: IntGaugeVec,
    innodb_buffer_pool_reads: IntGaugeVec,
    innodb_buffer_pool_write_requests: IntGaugeVec,
    innodb_log_waits: IntGaugeVec,
    innodb_log_written: IntGaugeVec,
    innodb_log_write_requests: IntGaugeVec,
    innodb_row_lock_time: IntGaugeVec,
    innodb_row_lock_waits: IntGaugeVec,
    innodb_row_lock_current_waits: IntGaugeVec,
    innodb_history_list_length: IntGaugeVec,
    innodb_data_pending_reads: IntGaugeVec,
    innodb_data_pending_writes: IntGaugeVec,
    innodb_data_pending_fsyncs: IntGaugeVec,
    // InnoDB row operations
    innodb_rows_read: IntGaugeVec,
    innodb_rows_inserted: IntGaugeVec,
    innodb_rows_updated: IntGaugeVec,
    innodb_rows_deleted: IntGaugeVec,
    // InnoDB data I/O
    innodb_data_reads: IntGaugeVec,
    innodb_data_writes: IntGaugeVec,
    innodb_data_read_bytes: IntGaugeVec,
    innodb_data_written_bytes: IntGaugeVec,
    innodb_data_fsyncs: IntGaugeVec,
    // InnoDB deadlocks and lock timeouts
    innodb_deadlocks: IntGaugeVec,
    innodb_lock_timeouts: IntGaugeVec,
    // InnoDB buffer pool efficiency
    innodb_buffer_pool_pages_misc: IntGaugeVec,
    innodb_buffer_pool_pages_total: IntGaugeVec,
    innodb_buffer_pool_wait_free: IntGaugeVec,
    innodb_buffer_pool_read_ahead: IntGaugeVec,
    innodb_buffer_pool_read_ahead_evicted: IntGaugeVec,
    // InnoDB log
    innodb_os_log_written_bytes: IntGaugeVec,
    innodb_os_log_fsyncs: IntGaugeVec,
    innodb_os_log_pending_writes: IntGaugeVec,
    innodb_os_log_pending_fsyncs: IntGaugeVec,
    innodb_log_write_ratio: IntGaugeVec,
    // Replication (replica)
    // Binlog (primary)
    binlog_bytes_written: IntGaugeVec,
    binlog_cache_disk_use: IntGaugeVec,
    binlog_stmt_cache_disk_use: IntGaugeVec,
    // Config flags
    have_ssl: IntGaugeVec,
    have_openssl: IntGaugeVec,
    performance_schema: IntGaugeVec,
    max_connections: IntGaugeVec,
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
        // Zero-label vectors throughout: a status or variable key that disappears from an
        // otherwise successful read must remove its series, and a scalar gauge cannot be
        // removed once registered.
        let g = |name: &str, help: &str| {
            IntGaugeVec::new(Opts::new(name, help), &NO_LABELS).expect("valid metric name")
        };
        let c = |name: &str, help: &str| {
            IntCounterVec::new(Opts::new(name, help), &NO_LABELS).expect("valid metric name")
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
            // Command statistics (SQL-level)
            com_select: g(
                "mariadb_global_status_com_select",
                "Number of SELECT statements executed",
            ),
            com_insert: g(
                "mariadb_global_status_com_insert",
                "Number of INSERT statements executed",
            ),
            com_update: g(
                "mariadb_global_status_com_update",
                "Number of UPDATE statements executed",
            ),
            com_delete: g(
                "mariadb_global_status_com_delete",
                "Number of DELETE statements executed",
            ),
            com_replace: g(
                "mariadb_global_status_com_replace",
                "Number of REPLACE statements executed",
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
            max_connections: g(
                "mariadb_global_variables_max_connections",
                "Maximum number of simultaneous client connections allowed",
            ),
        }
    }

    /// Every gauge this collector owns, in registration order.
    #[allow(clippy::too_many_lines)]
    fn all_gauges(&self) -> Vec<&IntGaugeVec> {
        vec![
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
            // Command statistics (SQL-level)
            &self.com_select,
            &self.com_insert,
            &self.com_update,
            &self.com_delete,
            &self.com_replace,
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
            &self.binlog_bytes_written,
            &self.binlog_cache_disk_use,
            &self.binlog_stmt_cache_disk_use,
            &self.have_ssl,
            &self.have_openssl,
            &self.performance_schema,
            &self.max_connections,
        ]
    }

    fn register_gauges(&self, registry: &Registry) -> Result<()> {
        for m in self.all_gauges() {
            registry.register(Box::new(m.clone()))?;
        }

        registry.register(Box::new(self.questions_total.clone()))?;
        registry.register(Box::new(self.queries_total.clone()))?;

        Ok(())
    }

    fn reset_all_gauges(&self) {
        for m in self.all_gauges() {
            m.reset();
        }
    }

    /// Publish a status value, scaling the parsed number.
    ///
    /// A key that is absent or unparseable in an *otherwise successful* read means the server
    /// no longer reports it (a storage engine was unloaded, a variable was renamed between
    /// versions), so the series is removed rather than left showing the previous scrape.
    fn set_scaled_from_status(
        status: &HashMap<String, String>,
        key: &str,
        gauge: &IntGaugeVec,
        divisor: i64,
    ) {
        match status
            .get(&key.to_ascii_uppercase())
            .map(|raw| (raw, raw.parse::<i64>()))
        {
            Some((_, Ok(v))) => gauge.with_label_values(&NO_LABELS).set(v / divisor),
            Some((raw, Err(_))) => {
                debug!(metric = key, value = raw, "could not parse status value");
                let _ = gauge.remove_label_values(&NO_LABELS);
            }
            None => {
                let _ = gauge.remove_label_values(&NO_LABELS);
            }
        }
    }

    fn set_from_status(status: &HashMap<String, String>, key: &str, gauge: &IntGaugeVec) {
        Self::set_scaled_from_status(status, key, gauge, 1);
    }

    fn set_from_status_ms_to_seconds(
        status: &HashMap<String, String>,
        key: &str,
        gauge: &IntGaugeVec,
    ) {
        Self::set_scaled_from_status(status, key, gauge, 1_000);
    }

    /// Publish a monotonic counter from a status value.
    ///
    /// The counter tracks deltas so a server restart (value going backwards) is republished
    /// as a reset rather than as a huge negative jump. A key missing from a successful read
    /// removes the series; Prometheus reads the later reappearance as a counter reset, which
    /// is exactly what happened.
    fn set_counter_from_status(
        status: &HashMap<String, String>,
        key: &str,
        counter: &IntCounterVec,
        last_seen: &AtomicI64,
    ) {
        let Some(raw) = status.get(&key.to_ascii_uppercase()) else {
            let _ = counter.remove_label_values(&NO_LABELS);
            last_seen.store(0, Ordering::Relaxed);
            return;
        };

        {
            if let Ok(v) = raw.parse::<i64>() {
                let previous = last_seen.swap(v, Ordering::Relaxed);
                if v >= 0 {
                    if previous > 0 && v >= previous {
                        let delta = v.saturating_sub(previous);
                        if let Ok(incr) = u64::try_from(delta) {
                            counter.with_label_values(&NO_LABELS).inc_by(incr);
                        }
                    } else {
                        counter.reset();
                        if let Ok(incr) = u64::try_from(v) {
                            counter.with_label_values(&NO_LABELS).inc_by(incr);
                        }
                    }
                }
            } else {
                debug!(metric = key, value = raw, "could not parse status value");
                let _ = counter.remove_label_values(&NO_LABELS);
                last_seen.store(0, Ordering::Relaxed);
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

        // Command statistics (SQL-level)
        Self::set_from_status(status, "Com_select", &self.com_select);
        Self::set_from_status(status, "Com_insert", &self.com_insert);
        Self::set_from_status(status, "Com_update", &self.com_update);
        Self::set_from_status(status, "Com_delete", &self.com_delete);
        Self::set_from_status(status, "Com_replace", &self.com_replace);

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

        // Calculate InnoDB log write ratio. With no write requests the ratio is undefined,
        // not zero, so the series is removed instead of claiming a 0% write ratio.
        let ratio = status
            .get("INNODB_LOG_WRITE_REQUESTS")
            .and_then(|raw| raw.parse::<i64>().ok())
            .filter(|requests| *requests > 0)
            .zip(
                status
                    .get("INNODB_LOG_WRITES")
                    .and_then(|raw| raw.parse::<i64>().ok()),
            )
            .map(|(requests, writes)| (writes * 100) / requests);

        match ratio {
            Some(v) => self
                .innodb_log_write_ratio
                .with_label_values(&NO_LABELS)
                .set(v),
            None => {
                let _ = self.innodb_log_write_ratio.remove_label_values(&NO_LABELS);
            }
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

    /// Publish a boolean-ish server variable, removing the series when the current read no
    /// longer reports it.
    fn set_flag_from_variables(
        vars: &HashMap<String, String>,
        key: &str,
        gauge: &IntGaugeVec,
    ) {
        match vars.get(key).map(|s| s.to_ascii_lowercase()) {
            Some(v) => {
                let flag = i64::from(matches!(v.as_str(), "yes" | "on" | "true" | "1"));
                gauge.with_label_values(&NO_LABELS).set(flag);
            }
            None => {
                let _ = gauge.remove_label_values(&NO_LABELS);
            }
        }
    }

    /// Publish a numeric server variable, removing the series when it is absent or
    /// unparseable in the current successful read.
    fn set_number_from_variables(
        vars: &HashMap<String, String>,
        key: &str,
        gauge: &IntGaugeVec,
    ) {
        match vars.get(key).map(|raw| (raw, raw.parse::<i64>())) {
            Some((_, Ok(v))) => {
                gauge.with_label_values(&NO_LABELS).set(v);
                debug!(metric = key, value = v, "updated variable");
            }
            Some((raw, Err(_))) => {
                debug!(metric = key, value = raw, "could not parse variable value");
                let _ = gauge.remove_label_values(&NO_LABELS);
            }
            None => {
                let _ = gauge.remove_label_values(&NO_LABELS);
            }
        }
    }

    /// Republish every configuration variable from each successful read.
    ///
    /// These were previously latched on the first scrape, which meant a variable that
    /// vanished (or a server replaced behind the same address) kept reporting the very first
    /// value the exporter ever saw.
    fn collect_variables(&self, vars: &HashMap<String, String>) {
        Self::set_flag_from_variables(vars, "have_ssl", &self.have_ssl);
        Self::set_flag_from_variables(vars, "have_openssl", &self.have_openssl);
        Self::set_flag_from_variables(vars, "performance_schema", &self.performance_schema);
        Self::set_number_from_variables(
            vars,
            "innodb_buffer_pool_size",
            &self.innodb_buffer_pool_size_bytes,
        );
        Self::set_number_from_variables(vars, "max_connections", &self.max_connections);
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
    fn collect_once<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
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
                db.statement = "SELECT VARIABLE_NAME, VARIABLE_VALUE FROM information_schema.global_variables WHERE VARIABLE_NAME IN ('have_ssl','have_openssl','performance_schema','innodb_buffer_pool_size','max_connections')",
                otel.kind = "client"
            );
            let vars_rows = sqlx::query(
                "SELECT VARIABLE_NAME, VARIABLE_VALUE FROM information_schema.global_variables WHERE VARIABLE_NAME IN ('have_ssl','have_openssl','performance_schema','innodb_buffer_pool_size','max_connections')",
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

            // `information_schema.global_status` and `global_variables` are core server
            // surfaces: both reads above use `?`, so a failure is an error that preserves
            // the previous snapshot rather than a skip.
            Ok(Collected::Fresh)
        })
    }

    /// Clears every status, `InnoDB`, binlog and configuration series this collector owns.
    fn reset_metrics(&self) {
        self.reset_all_gauges();
        self.questions_total.reset();
        self.queries_total.reset();
        self.questions_last.store(0, Ordering::Relaxed);
        self.queries_last.store(0, Ordering::Relaxed);
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

#[cfg(test)]
mod tests {
    use super::StatusCollector;
    use crate::collectors::{NO_LABELS, published_samples};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicI64, Ordering};

    fn status(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_ascii_uppercase(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn status_value_disappears_when_the_key_vanishes() {
        let collector = StatusCollector::new();

        StatusCollector::set_from_status(
            &status(&[("Uptime", "42")]),
            "Uptime",
            &collector.global_uptime,
        );
        assert_eq!(collector.global_uptime.with_label_values(&NO_LABELS).get(), 42);

        // A successful read that no longer reports the key must remove the series, not keep
        // serving 42 as if it were current.
        StatusCollector::set_from_status(&status(&[]), "Uptime", &collector.global_uptime);
        assert_eq!(published_samples(&collector.global_uptime), 0);
    }

    #[test]
    fn unparseable_status_value_removes_the_series() {
        let collector = StatusCollector::new();

        StatusCollector::set_from_status(
            &status(&[("Uptime", "42")]),
            "Uptime",
            &collector.global_uptime,
        );
        StatusCollector::set_from_status(
            &status(&[("Uptime", "not-a-number")]),
            "Uptime",
            &collector.global_uptime,
        );

        assert_eq!(published_samples(&collector.global_uptime), 0);
    }

    #[test]
    fn counter_tracks_deltas_and_restarts() {
        let collector = StatusCollector::new();
        let last = AtomicI64::new(0);

        StatusCollector::set_counter_from_status(
            &status(&[("Queries", "10")]),
            "Queries",
            &collector.queries_total,
            &last,
        );
        assert_eq!(collector.queries_total.with_label_values(&NO_LABELS).get(), 10);

        StatusCollector::set_counter_from_status(
            &status(&[("Queries", "25")]),
            "Queries",
            &collector.queries_total,
            &last,
        );
        assert_eq!(collector.queries_total.with_label_values(&NO_LABELS).get(), 25);

        // Server restart: the source counter goes backwards, so the exported counter resets.
        StatusCollector::set_counter_from_status(
            &status(&[("Queries", "3")]),
            "Queries",
            &collector.queries_total,
            &last,
        );
        assert_eq!(collector.queries_total.with_label_values(&NO_LABELS).get(), 3);
        assert_eq!(last.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn counter_disappears_when_the_key_vanishes() {
        let collector = StatusCollector::new();
        let last = AtomicI64::new(0);

        StatusCollector::set_counter_from_status(
            &status(&[("Queries", "10")]),
            "Queries",
            &collector.queries_total,
            &last,
        );
        StatusCollector::set_counter_from_status(
            &status(&[]),
            "Queries",
            &collector.queries_total,
            &last,
        );

        assert_eq!(published_samples(&collector.queries_total), 0);
        assert_eq!(last.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn undefined_log_write_ratio_is_absent_rather_than_zero() {
        let collector = StatusCollector::new();

        collector.collect_innodb(&status(&[
            ("Innodb_log_write_requests", "200"),
            ("Innodb_log_writes", "50"),
        ]));
        assert_eq!(
            collector
                .innodb_log_write_ratio
                .with_label_values(&NO_LABELS)
                .get(),
            25
        );

        // Zero write requests makes the ratio undefined; 0% would be a false claim.
        collector.collect_innodb(&status(&[
            ("Innodb_log_write_requests", "0"),
            ("Innodb_log_writes", "50"),
        ]));
        assert_eq!(published_samples(&collector.innodb_log_write_ratio), 0);
    }

    #[test]
    fn configuration_variables_are_republished_every_scrape() {
        let collector = StatusCollector::new();
        let mut vars = HashMap::new();
        vars.insert("have_ssl".to_string(), "YES".to_string());
        vars.insert("max_connections".to_string(), "151".to_string());

        collector.collect_variables(&vars);
        assert_eq!(collector.have_ssl.with_label_values(&NO_LABELS).get(), 1);
        assert_eq!(
            collector.max_connections.with_label_values(&NO_LABELS).get(),
            151
        );

        // Previously latched on the first scrape; a later read must win.
        vars.insert("have_ssl".to_string(), "DISABLED".to_string());
        vars.insert("max_connections".to_string(), "500".to_string());
        collector.collect_variables(&vars);
        assert_eq!(collector.have_ssl.with_label_values(&NO_LABELS).get(), 0);
        assert_eq!(
            collector.max_connections.with_label_values(&NO_LABELS).get(),
            500
        );

        // A variable missing from a successful read disappears.
        collector.collect_variables(&HashMap::new());
        assert_eq!(published_samples(&collector.have_ssl), 0);
    }
}
