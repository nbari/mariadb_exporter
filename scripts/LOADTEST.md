# MariaDB Load Tester & Metrics Exerciser

Comprehensive load testing tool that exercises all MariaDB exporter collectors.

## Quick Start

```bash
# Install dependencies
pip install mariadb

# Run with default mixed workload (tests all collectors)
python mariadb_loadtest.py --workers 50

# Or explicitly specify workload type:

# Basic workload (simple queries)
python mariadb_loadtest.py --workload basic --workers 100

# Mixed workload (DEFAULT - recommended for testing all collectors)
python mariadb_loadtest.py --workload mixed --workers 50 --hold-time 5

# Stress test (heavy load)
python mariadb_loadtest.py --workload stress --workers 200 --burst

# Metadata locks (DDL and long transactions)
python mariadb_loadtest.py --workload metadata --workers 20 --hold-time 10
```

## Workload Types

### 1. Basic (`--workload basic`)
**Purpose**: Simple baseline testing
**Exercises**:
- Query response time (fast queries)
- Connection pooling
- Basic INSERT operations
- Simple SELECT queries

**Use when**: Testing basic connectivity and query response time distribution

### 2. Mixed (`--workload mixed`) ⭐ **DEFAULT** - Recommended
**Purpose**: Comprehensive collector testing
**Exercises**:
- Query Response Time: Fast queries (<1ms) and slow queries (>100ms)
- SELECT Operations: Index scans, range scans, full table scans
- Sort Operations: ORDER BY queries
- Temporary Tables: GROUP BY operations
- Handler Statistics: Read/write operations
- Slow Queries: LIKE queries on large datasets
- InnoDB Operations: Multiple row operations
- All CRUD operations: INSERT, SELECT, UPDATE, DELETE

**Use when**: Testing all exporter metrics at once

### 3. Stress (`--workload stress`)
**Purpose**: High-load performance testing
**Exercises**:
- InnoDB Buffer Pool: Heavy read/write operations
- Sort Merge Passes: Large sorting operations
- Temporary Tables on Disk: Complex GROUP BY queries
- Subqueries and complex JOINs
- Multiple concurrent updates
- Slow queries (intentional delays)

**Use when**: Load testing and identifying bottlenecks

### 4. Metadata (`--workload metadata`)
**Purpose**: Metadata lock and DDL testing
**Exercises**:
- Metadata Lock Info: Long-running transactions with locks
- DDL Operations: ALTER TABLE statements
- Lock contention: Concurrent operations on same tables
- Transaction isolation: Long-held locks

**Use when**: Testing metadata lock collector and lock monitoring

## Options

```
--host HOST              Database host (default: 127.0.0.1)
--user USER              Database user (default: root)
--password PASSWORD      Database password (default: root)
--database DATABASE      Database name (default: test)
--port PORT              Database port (default: 3306)
--workers WORKERS        Number of concurrent workers (default: 50)
--hold-time HOLD_TIME    Time to hold connections in seconds (default: 5)
--burst                  Launch all workers at once (no ramp)
--ramp-ms RAMP_MS        Milliseconds between worker launches (default: 10)
--jitter-ms JITTER_MS    Random jitter in milliseconds (default: 0)
--connect-timeout        Connection timeout in seconds (default: 2)
--max-threads            Max thread pool size (default: 200)
--autocommit             Enable autocommit mode
```

## Environment Variables

Can be set instead of command-line options:
- `DB_HOST`, `DB_USER`, `DB_PASSWORD`, `DB_NAME`, `DB_PORT`
- `LT_WORKERS`, `LT_HOLD`, `LT_BURST`, `LT_RAMP_MS`, `LT_JITTER_MS`
- `LT_CONN_TIMEOUT`, `LT_MAX_THREADS`, `LT_AUTOCOMMIT`, `LT_WORKLOAD`

## Metrics Coverage

### Query Response Time Distribution
- **Workloads**: All
- **Metrics**: `mariadb_info_schema_query_response_time_seconds`
- Fast queries (<1ms), slow queries (>100ms), and everything in between

### Query Performance & Execution
- **Workloads**: Mixed, Stress
- **Metrics**: 
  - `mariadb_global_status_slow_queries`
  - `mariadb_global_status_select_scan/range/full_join`
  - `mariadb_global_status_sort_scan/range/rows`
  - `mariadb_global_status_handler_*`
  - `mariadb_global_status_created_tmp_tables/disk_tables`

### InnoDB Buffer Pool
- **Workloads**: Mixed, Stress
- **Metrics**:
  - `mariadb_innodb_buffer_pool_*`
  - `mariadb_innodb_rows_*`
  - `mariadb_innodb_data_*`

### Metadata Locks
- **Workloads**: Metadata
- **Metrics**: `mariadb_metadata_lock_info_count`
- Creates actual metadata locks for testing the collector

### User Statistics
- **Workloads**: All
- **Metrics**: `mariadb_info_schema_userstats_*`
- Connections, bytes sent/received per user

## Example: Full Exporter Testing

```bash
# Terminal 1: Start exporter with all collectors
mariadb_exporter \
  --collector.query_response_time \
  --collector.metadata \
  --collector.statements \
  --dsn="mysql://root:root@127.0.0.1:3306/mysql"

# Terminal 2: Run mixed workload
python mariadb_loadtest.py \
  --workload mixed \
  --workers 50 \
  --hold-time 5 \
  --ramp-ms 50

# Terminal 3: Check metrics
curl -s http://localhost:9104/metrics | grep mariadb_
```

## Example: Metadata Lock Testing

```bash
# Terminal 1: Start exporter with metadata collector
mariadb_exporter --collector.metadata --dsn="mysql://root:root@127.0.0.1:3306/mysql"

# Terminal 2: Generate metadata locks
python mariadb_loadtest.py \
  --workload metadata \
  --workers 10 \
  --hold-time 15 \
  --ramp-ms 500

# Terminal 3: Watch metadata locks appear
watch -n 1 'curl -s http://localhost:9104/metrics | grep mariadb_metadata_lock_info_count'
```

## Example: Stress Testing

```bash
# Heavy load with burst mode
python mariadb_loadtest.py \
  --workload stress \
  --workers 500 \
  --burst \
  --hold-time 30 \
  --max-threads 100

# Monitor slow queries
watch -n 1 'curl -s http://localhost:9104/metrics | grep slow_queries'
```

## Troubleshooting

### "Too many connections"
```bash
# Reduce workers or increase MariaDB max_connections
--workers 100 --max-threads 50
```

### "No metadata locks appearing"
```bash
# Increase hold time and reduce workers for metadata workload
--workload metadata --workers 5 --hold-time 20
```

### Script exits immediately
```bash
# Check database connectivity
mysql -u root -proot -h 127.0.0.1 -e "SELECT 1"
```

## Tables Created

The script creates these tables in the specified database:
- `test_load`: Main testing table with payload, status, counter
- `test_metadata`: Table for metadata lock testing

Tables are created automatically on first run.

## Signal Handling

- `Ctrl+C` (SIGINT): Graceful shutdown of pending workers
- `SIGTERM`: Graceful shutdown

## Performance Tips

1. **For realistic metrics**: Use `--workload mixed` with moderate workers (50-100)
2. **For load testing**: Use `--workload stress` with `--burst` mode
3. **For metadata locks**: Use `--workload metadata` with low workers (5-20) and high hold-time (10-30s)
4. **For connection testing**: Use `--workload basic` with high workers (500+)

## Output Example

```
============================================================
MariaDB Load Tester & Metrics Exerciser
============================================================
Workload type: mixed
Workers: 50
Hold time: 5.0s
============================================================

[INIT] Database ensured: test
[INIT] test tables ensured
[INIT] Starting 50 workers | workload=mixed | burst=False | ramp=10ms | jitter=0ms | threads=200
[0] Connected (mixed workload)
[1] Connected (mixed workload)
...
[49] Closed

===== TEST SUMMARY =====
Total attempted: 50
Successful:      50
Failed:          0
Duration:        5.52s
========================
```

## Integration with Grafana

After running the load test, check your Grafana dashboard:
- Query Response Time panel should show distribution across buckets
- Slow Queries should increment
- Temporary Tables counter should increase
- InnoDB metrics should show activity
- Metadata locks should appear (if using metadata workload)

All panels should now have data instead of "No data" messages!
