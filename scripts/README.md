# MariaDB Exporter Test Scripts

## Overview

This directory contains testing scripts for `mariadb_exporter`. The primary goal is to **generate database activity that exercises all exposed metrics** from every collector.

## Files

| File | Purpose |
|------|---------|
| `mariadb_loadtest.py` | Main load testing script |
| `METRICS_COVERAGE.md` | Maps workloads to metrics they exercise |
| `README.md` | This file |

---

## Quick Start

### **Test ALL Metrics (Recommended)**

```bash
# Default: 60s stepped ramp-up (best for watching metrics evolve)
python mariadb_loadtest.py --workload all_metrics --workers 50

# Single run mode (faster, completes in ~10-15 seconds)
python mariadb_loadtest.py --single-run --workload all_metrics --workers 50
```

This generates activity for:
- ✅ `default` collector - queries, connections, InnoDB, traffic
- ✅ `query_response_time` - fast, medium, and slow queries
- ✅ `statements` - varied statement types
- ✅ `schema` - table data and rows
- ✅ `locks` - table locks and metadata locks
- ✅ `metadata` - long transactions with DDL

### **Watch Metrics Update in Real-Time**

```bash
# Terminal 1: Start exporter
cd /path/to/mariadb_exporter
cargo run -- --dsn "mysql://root:root@127.0.0.1:3306/mysql"

# Terminal 2: Run load test (default: 60s stepped ramp-up)
cd scripts
python mariadb_loadtest.py --workload all_metrics --workers 50

# Terminal 3: Watch metrics evolve
watch -n 1 'curl -s http://localhost:9306/metrics | grep -E "mariadb_global_status_queries|mariadb_global_status_slow_queries|mariadb_global_status_com_"'
```

---

## Workload Types

### **1. `all_metrics` - Comprehensive Testing**
**Purpose:** Exercise ALL metrics from all collectors in one workload.

**Metrics covered:**
- Connections: `threads_connected`, `max_used_connections`
- Queries: `queries`, `questions`, `slow_queries`
- Commands: `com_select`, `com_insert`, `com_update`, `com_delete`
- InnoDB: `innodb_buffer_pool_*`, `innodb_row_lock_*`
- Table locks: `table_locks_immediate`, `table_locks_waited`
- Query times: Fast (<0.01s), medium (0.1-0.5s), slow (>0.5s)
- Traffic: `bytes_sent`, `bytes_received`
- Schema: Table rows and sizes
- Locks: Metadata locks, table locks

**Usage:**
```bash
python mariadb_loadtest.py --workload all_metrics --workers 50
```

---

### **2. `mixed` - Balanced Workload (Default Workload)**
**Purpose:** Varied operations for general testing.

**Covers:**
- Fast queries
- INSERT, SELECT, UPDATE, DELETE
- Temporary tables (GROUP BY)
- Range scans
- Full table scans

**Usage:**
```bash
# Default: 60s duration mode
python mariadb_loadtest.py

# Single run mode
python mariadb_loadtest.py --single-run --workload mixed --workers 50
```

---

### **3. `stress` - Heavy Load Testing**
**Purpose:** Complex queries, intentional slow queries, buffer pool stress.

**Covers:**
- Slow queries (`SLEEP`, cross joins)
- Large result sets
- Complex aggregations
- Subqueries
- Self-joins

**Usage:**
```bash
python mariadb_loadtest.py --workload stress --workers 100
```

---

### **4. `metadata` - DDL and Lock Testing**
**Purpose:** Metadata locks, long transactions, DDL operations.

**Covers:**
- Metadata locks (`metadata_lock_info`)
- DDL operations (ALTER TABLE)
- Long-held transactions

**Usage:**
```bash
python mariadb_loadtest.py --workload metadata --workers 20 --hold-time 10
```

---

### **5. `connection_exhaustion` - Connection Limit Testing**
**Purpose:** Test max_connections handling and related metrics.

**Covers:**
- `max_used_connections`
- `aborted_connects`
- `connection_errors_max_connections`
- `threads_connected` at limit

**Usage:**
```bash
# Auto-detects max_connections and exceeds it
python mariadb_loadtest.py --workload connection_exhaustion --workers 200 --burst

# With persistent retries (workers wait for available connections)
# Perfect for observing connections decrease as workers finish
python mariadb_loadtest.py --workload connection_exhaustion --workers 200 --persistent --burst

# Gradual ramp-up to watch metrics change
python mariadb_loadtest.py --workload connection_exhaustion --duration 60 --step-size 10 --step-interval 5 --persistent
```

**Persistent mode (`--persistent` flag):**
- Workers automatically retry for up to 60 seconds when max_connections is reached
- Allows observation of connection increase/decrease patterns
- Ideal for: `--workers <max_connections + 50>` to see queuing behavior

---

### **6. `basic` - Minimal Testing**
**Purpose:** Simple queries for basic connectivity testing.

**Covers:**
- Basic queries
- Simple INSERT
- Connection tracking

**Usage:**
```bash
python mariadb_loadtest.py --workload basic --workers 20
```

---

## Duration Mode (Stepped Ramp-Up) - DEFAULT

**NEW:** Duration mode is now the default behavior (60s with stepped ramp-up).

This gradually increases connections over time to watch metrics evolve.

```bash
# Default: 60s with steps of 10 every 5 seconds
python mariadb_loadtest.py --workload all_metrics --workers 50

# Custom duration and steps
python mariadb_loadtest.py \
  --workload all_metrics \
  --duration 120 \
  --step-size 20 \
  --step-interval 10

# Disable duration mode (single run)
python mariadb_loadtest.py --single-run --workload all_metrics
```

**Default Timeline (60s, 10 workers/step, 5s intervals):**
- `0:00` → 10 connections
- `0:05` → 20 connections
- `0:10` → 30 connections
- ...
- `0:55` → 50 connections (max)
- `1:00` → Test complete

---

## Configuration

### **Environment Variables**

```bash
# Database connection
export DB_HOST=127.0.0.1
export DB_PORT=3306
export DB_USER=root
export DB_PASSWORD=root
export DB_NAME=test

# Load test settings
export LT_WORKLOAD=all_metrics
export LT_WORKERS=50
export LT_HOLD=5
export LT_DURATION=60        # Default: 60 (stepped ramp-up mode)
export LT_STEP_SIZE=10       # Default: 10
export LT_STEP_INTERVAL=5    # Default: 5
export LT_SINGLE_RUN=false   # Set to "true" to disable duration mode
```

### **Command-Line Flags**

All environment variables can be overridden with flags:

```bash
python mariadb_loadtest.py \
  --host 192.168.1.100 \
  --port 3306 \
  --user exporter \
  --password secret \
  --database test \
  --workload all_metrics \
  --workers 100 \
  --hold-time 10 \
  --duration 120 \
  --step-size 20 \
  --step-interval 10
```

---

## Metrics Coverage

See `METRICS_COVERAGE.md` for detailed mapping of workloads to metrics.

**Summary:**

| Collector | Coverage | Best Workload |
|-----------|----------|---------------|
| default | ✅ 95% | `all_metrics`, `stress` |
| exporter | ✅ 100% | Any workload |
| query_response_time | ✅ 90% | `all_metrics`, `stress` |
| statements | ✅ 85% | `all_metrics`, `mixed` |
| schema | ✅ 80% | `all_metrics`, `stress` |
| locks | ⚠️ 60% | `metadata`, `all_metrics` |
| metadata | ⚠️ 60% | `metadata`, `all_metrics` |
| tls | ❌ 0% | (requires TLS setup) |
| replication | ❌ 0% | (requires replica) |
| userstat | ❌ 0% | (requires userstat=1) |

---

## Tips

### **Run exporter with all collectors enabled**
```bash
cargo run -- \
  --dsn "mysql://root:root@127.0.0.1:3306/mysql" \
  --collector.query_response_time \
  --collector.statements \
  --collector.schema \
  --collector.locks \
  --collector.metadata
```

### **Monitor specific metrics**
```bash
# Watch query metrics
watch -n 1 'curl -s http://localhost:9306/metrics | grep mariadb_global_status_queries'

# Watch connection metrics
watch -n 1 'curl -s http://localhost:9306/metrics | grep -E "threads_connected|max_used_connections"'

# Watch InnoDB buffer pool
watch -n 1 'curl -s http://localhost:9306/metrics | grep innodb_buffer_pool'

# Count total metrics
curl -s http://localhost:9306/metrics | grep "^mariadb_" | grep -v "^#" | wc -l
```

### **Test for specific duration**
```bash
# Run for 5 minutes with all_metrics workload
timeout 300 python mariadb_loadtest.py --workload all_metrics --duration 300
```

---

## Troubleshooting

### **No slow queries appearing**
- Check `long_query_time` setting: `SHOW VARIABLES LIKE 'long_query_time';`
- Set lower threshold: `SET GLOBAL long_query_time = 0.5;`

### **No statement digest metrics**
- Enable performance_schema: `SET GLOBAL performance_schema = ON;` (requires restart)
- Check: `SHOW VARIABLES LIKE 'performance_schema';`

### **No metadata lock metrics**
- Check if `metadata_lock_info` plugin is available
- Install: `INSTALL SONAME 'metadata_lock_info';`

### **Connection limit too low**
- Increase: `SET GLOBAL max_connections = 500;`
- Make permanent in `/etc/mysql/mariadb.conf.d/50-server.cnf`

---

## Contributing

When adding new collectors to `mariadb_exporter`:

1. **Update `METRICS_COVERAGE.md`** with new metrics
2. **Extend workloads** to exercise new metrics
3. **Test coverage** with `all_metrics` workload
4. **Document** expected behavior

The goal is **100% metric coverage** - every metric should be testable via the load script.
