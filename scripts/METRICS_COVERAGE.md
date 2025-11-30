# Metrics Coverage Map

This document maps `mariadb_loadtest.py` workloads to the metrics they exercise.

## Collectors and Their Metrics

### 1. **default** (enabled by default)
**Purpose:** Core MariaDB status, connections, InnoDB basics, replication basics

**Metrics exercised:**
- `mariadb_up` - Database reachability
- `mariadb_global_status_threads_connected` - Active connections
- `mariadb_global_status_threads_running` - Running threads
- `mariadb_global_status_max_used_connections` - Peak connections
- `mariadb_global_status_aborted_connects` - Failed connection attempts
- `mariadb_global_status_aborted_clients` - Client disconnects
- `mariadb_global_status_connections` - Total connection attempts
- `mariadb_global_status_uptime` - Server uptime
- `mariadb_global_status_queries` - Total queries
- `mariadb_global_status_questions` - Client queries
- `mariadb_global_status_slow_queries` - Slow queries
- `mariadb_global_status_bytes_received` - Bytes received
- `mariadb_global_status_bytes_sent` - Bytes sent
- `mariadb_global_status_com_select` - SELECT count
- `mariadb_global_status_com_insert` - INSERT count
- `mariadb_global_status_com_update` - UPDATE count
- `mariadb_global_status_com_delete` - DELETE count
- `mariadb_global_status_table_locks_immediate` - Table locks acquired
- `mariadb_global_status_table_locks_waited` - Table locks waited
- `mariadb_global_status_innodb_buffer_pool_*` - InnoDB buffer pool stats
- `mariadb_global_status_innodb_row_lock_*` - InnoDB row locks
- `mariadb_version_info` - Server version

**Workloads that exercise:**
- ✅ `basic` - Connections, queries, uptime
- ✅ `mixed` - SELECT, INSERT, UPDATE, DELETE, table locks
- ✅ `stress` - Heavy query load, slow queries
- ✅ `connection_exhaustion` - Max connections, aborted connects

---

### 2. **exporter** (enabled by default)
**Purpose:** Exporter self-monitoring

**Metrics exercised:**
- `mariadb_exporter_build_info` - Build version/commit
- `mariadb_exporter_scrapes_total` - Total scrapes
- `mariadb_exporter_scrape_duration_seconds` - Scrape duration
- `mariadb_exporter_scrape_errors_total` - Scrape errors
- `process_cpu_seconds_total` - CPU usage
- `process_resident_memory_bytes` - Memory usage
- `process_open_fds` - Open file descriptors
- `process_start_time_seconds` - Process start time

**Workloads that exercise:**
- ✅ All workloads (metrics update on every scrape)

---

### 3. **tls** (opt-in)
**Purpose:** TLS connection details

**Metrics exercised:**
- `mariadb_ssl_version_info` - TLS version
- `mariadb_ssl_cipher_info` - Cipher in use
- `mariadb_ssl_verify_mode` - Certificate verification mode

**Workloads that exercise:**
- ❌ None (requires TLS-enabled connection)
- 📝 **TODO:** Add TLS-specific test with ssl-mode=REQUIRED

---

### 4. **query_response_time** (opt-in)
**Purpose:** Query response time distribution

**Metrics exercised:**
- `mariadb_query_response_time_seconds` - Histogram of query times

**Workloads that exercise:**
- ✅ `basic` - Fast queries (<0.1s)
- ✅ `mixed` - Mixed fast/slow queries
- ✅ `stress` - Complex queries (0.1-1s)
- ❌ Missing: Very slow queries (>1s)
- 📝 **TODO:** Add intentionally slow queries (SLEEP, large sorts)

---

### 5. **statements** (opt-in)
**Purpose:** Performance schema statement digests

**Metrics exercised:**
- `mariadb_perf_schema_stmt_digest_*` - Statement execution stats

**Workloads that exercise:**
- ✅ `mixed` - Varied statement types
- ✅ `stress` - Complex statements
- ❌ Missing: Prepared statements
- 📝 **TODO:** Add prepared statement workload

---

### 6. **schema** (opt-in)
**Purpose:** Table sizes and row counts

**Metrics exercised:**
- `mariadb_table_size_bytes` - Table data size
- `mariadb_table_rows` - Estimated row count

**Workloads that exercise:**
- ✅ `mixed` - Creates data in test_load table
- ✅ `stress` - Bulk inserts increase table size
- ❌ Missing: Multiple large tables
- 📝 **TODO:** Create additional large tables for testing

---

### 7. **replication** (opt-in)
**Purpose:** Replication lag and binlog stats

**Metrics exercised:**
- `mariadb_slave_status_*` - Replication status
- `mariadb_binlog_size_bytes` - Binlog file sizes

**Workloads that exercise:**
- ❌ None (requires replica setup)
- 📝 **TODO:** Add replication-specific test setup

---

### 8. **locks** (opt-in)
**Purpose:** Metadata and table lock waits

**Metrics exercised:**
- `mariadb_perf_schema_metadata_lock_waits` - Metadata locks
- `mariadb_perf_schema_table_lock_waits` - Table locks

**Workloads that exercise:**
- ✅ `metadata` - DDL operations create metadata locks
- ⚠️ `mixed` - Table locks (but not tracked without performance_schema)
- 📝 **TODO:** Ensure performance_schema is enabled in tests

---

### 9. **metadata** (opt-in)
**Purpose:** Metadata lock info table counts

**Metrics exercised:**
- `mariadb_metadata_lock_info_count` - Active metadata locks

**Workloads that exercise:**
- ✅ `metadata` - Long transactions with DDL
- 📝 **TODO:** Verify metadata_lock_info plugin is available

---

### 10. **userstat** (opt-in)
**Purpose:** Per-user statistics

**Metrics exercised:**
- `mariadb_user_statistics_*` - User activity stats

**Workloads that exercise:**
- ❌ None (requires userstat=1 and USER_STATISTICS table)
- 📝 **TODO:** Add userstat-enabled test configuration

---

## Current Coverage Summary

| Collector | Coverage | Missing |
|-----------|----------|---------|
| default | ✅ 90% | Slow queries generation |
| exporter | ✅ 100% | - |
| tls | ❌ 0% | TLS connection test |
| query_response_time | ⚠️ 60% | Intentionally slow queries |
| statements | ⚠️ 80% | Prepared statements |
| schema | ⚠️ 70% | Multiple large tables |
| replication | ❌ 0% | Replica setup |
| locks | ⚠️ 50% | Performance schema required |
| metadata | ⚠️ 50% | Plugin verification |
| userstat | ❌ 0% | Userstat configuration |

---

## Recommended Improvements

### **High Priority**
1. **Add slow query generation** to `stress` workload
   - Use `SLEEP(1)` queries
   - Large result sets without LIMIT
   - Complex JOINs without indexes

2. **Add performance_schema checks** to ensure locks collector works
   - Verify `performance_schema` is enabled
   - Check required tables exist

3. **Add table schema diversity** to `schema` workload
   - Create 5-10 tables of varying sizes
   - Different storage engines (InnoDB, MyISAM if available)

### **Medium Priority**
4. **Add prepared statement workload**
   - Use `PREPARE` / `EXECUTE` statements
   - Exercises `statements` collector

5. **Add TLS connection test mode**
   - Requires TLS-enabled MariaDB
   - Exercises `tls` collector

### **Low Priority (requires setup)**
6. **Replication test mode** - Requires replica setup
7. **Userstat test mode** - Requires `userstat=1` configuration
