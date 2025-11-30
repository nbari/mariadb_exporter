# Testing Guide - mariadb_loadtest.py

Complete guide to test all metrics with the load testing script.

---

## Quick Start (3 Steps)

### **1. Start MariaDB**
```bash
# From project root
just mariadb

# Wait for healthy status
podman ps
```

### **2. Start Exporter (with all collectors)**
```bash
# Terminal 1
cargo run -- \
  --dsn "mysql://root:root@127.0.0.1:3306/mysql" \
  --collector.query_response_time \
  --collector.statements \
  --collector.schema \
  --collector.locks \
  --collector.metadata
```

### **3. Run Load Test**
```bash
# Terminal 2
cd scripts

# Default: 60s duration with stepped ramp-up (recommended for watching metrics)
python3 mariadb_loadtest.py --workload all_metrics --workers 50

# OR single run mode (faster, no duration)
python3 mariadb_loadtest.py --single-run --workload all_metrics --workers 50
```

---

## Watch Metrics Update (Recommended)

Open **3 terminals**:

### **Terminal 1: Exporter**
```bash
cargo run -- \
  --dsn "mysql://root:root@127.0.0.1:3306/mysql" \
  --collector.query_response_time \
  --collector.statements \
  --collector.schema
```

### **Terminal 2: Load Test**
```bash
cd scripts

# Default: Stepped ramp-up (60s, best for watching metrics evolve)
python3 mariadb_loadtest.py --workload all_metrics --workers 50

# Option B: Single run (faster, no duration)
python3 mariadb_loadtest.py --single-run --workload all_metrics --workers 50 --hold-time 5

# Option C: Custom duration
python3 mariadb_loadtest.py \
  --workload all_metrics \
  --duration 120 \
  --step-size 20 \
  --step-interval 10
```

### **Terminal 3: Watch Metrics**
```bash
# Watch total metrics exposed
watch -n 1 'curl -s http://localhost:9306/metrics | grep "^mariadb_" | wc -l'

# Watch query metrics
watch -n 1 'curl -s http://localhost:9306/metrics | grep -E "mariadb_global_status_(queries|slow_queries) "'

# Watch connection metrics
watch -n 1 'curl -s http://localhost:9306/metrics | grep -E "mariadb_global_status_(threads_connected|max_used_connections) "'

# Watch command counters
watch -n 1 'curl -s http://localhost:9306/metrics | grep "mariadb_global_status_com_" | head -10'
```

---

## Test All Workloads

### **1. Test `all_metrics` (Recommended)**
Exercises 95% of all metrics.

```bash
# Default: 60s stepped ramp-up (best for observing metric changes)
python3 mariadb_loadtest.py --workload all_metrics --workers 50

# Single run mode (faster)
python3 mariadb_loadtest.py --single-run --workload all_metrics --workers 50 --hold-time 5
```

**What it tests:**
- ✅ Fast queries (<0.01s)
- ✅ Medium queries (0.1-0.5s)
- ✅ Slow queries (>0.5s with SLEEP)
- ✅ COM_SELECT, COM_INSERT, COM_UPDATE, COM_DELETE
- ✅ InnoDB buffer pool (joins, subqueries)
- ✅ Table locks
- ✅ Metadata locks
- ✅ Schema metrics (table size/rows)
- ✅ Traffic (bytes sent/received)

---

### **2. Test `mixed` (Default)**
Balanced workload for general testing.

```bash
python3 mariadb_loadtest.py --workload mixed --workers 50
```

---

### **3. Test `stress`**
Heavy load with complex queries.

```bash
python3 mariadb_loadtest.py --workload stress --workers 100 --hold-time 10
```

**What it adds:**
- ✅ Cross joins (slow)
- ✅ Large result sets
- ✅ Complex aggregations

---

### **4. Test `connection_exhaustion`**
Test max_connections handling and observe connection limit behavior.

```bash
# Check current max_connections
mariadb -h127.0.0.1 -uroot -proot -e "SHOW VARIABLES LIKE 'max_connections';"

# Test exhaustion with persistent retries (recommended for observation)
# Workers will wait and retry when connections become available
python3 mariadb_loadtest.py \
  --workload connection_exhaustion \
  --workers 200 \
  --persistent \
  --burst \
  --hold-time 30

# OR: Test without retries (workers fail immediately)
python3 mariadb_loadtest.py \
  --workload connection_exhaustion \
  --workers 200 \
  --burst \
  --hold-time 30
```

**What it tests:**
- ✅ mariadb_global_status_threads_connected
- ✅ mariadb_global_status_max_used_connections
- ✅ mariadb_global_status_aborted_connects
- ✅ mariadb_global_status_connection_errors_max_connections

**With `--persistent` flag:**
- Workers retry for up to 60 seconds when max_connections is reached
- Allows you to observe gradual connection decrease as workers finish
- Perfect for testing with `--workers <max_connections + 50>`

---

### **5. Test `metadata`**
DDL operations and metadata locks.

```bash
python3 mariadb_loadtest.py --workload metadata --workers 20 --hold-time 10
```

**What it tests:**
- ✅ Metadata locks (performance_schema.metadata_locks)
- ✅ DDL operations (ALTER TABLE)
- ✅ Long transactions

---

## Automated Testing

Run the automated test script:

```bash
cd scripts
bash test_metrics.sh
```

**What it does:**
1. ✅ Checks MariaDB is running
2. ✅ Starts exporter with collectors enabled
3. ✅ Captures baseline metrics
4. ✅ Runs `all_metrics` workload
5. ✅ Verifies metrics increased
6. ✅ Reports success/failure

**Expected output:**
```
================================================
Testing mariadb_loadtest.py metric coverage
================================================

✅ MariaDB container is running
✅ Exporter is running (PID: 12345)

📊 Baseline metrics:
  Queries: 1234
  Slow queries: 0

🔥 Running all_metrics workload...

📊 After load test:
  Queries: 2150 (+916)
  Slow queries: 15 (+15)

📈 Verification:
  ✅ Queries increased by 916 (expected > 50)
  ✅ Slow queries increased by 15 (SLEEP queries working!)

📊 Metric variety check:
  Total mariadb_* metrics: 325
  COM_* metrics: 45
  InnoDB metrics: 62
  ✅ Good metric coverage (325 metrics)

================================================
✅ ALL TESTS PASSED
The load test successfully exercises metrics!
================================================
```

---

## Environment Variables

Instead of flags, use environment variables:

```bash
# Database connection
export DB_HOST=127.0.0.1
export DB_PORT=3306
export DB_USER=root
export DB_PASSWORD=root
export DB_NAME=test

# Workload settings
export LT_WORKLOAD=all_metrics
export LT_WORKERS=50
export LT_HOLD=5

# Run test (uses env vars)
python3 mariadb_loadtest.py
```

---

## Duration Mode (Stepped Ramp-Up)

Gradually increase load to watch metrics evolve:

```bash
python3 mariadb_loadtest.py \
  --workload all_metrics \
  --duration 120 \
  --step-size 10 \
  --step-interval 5 \
  --hold-time 10
```

**Timeline:**
- `0:00` → 10 connections created
- `0:05` → 20 connections (10 more)
- `0:10` → 30 connections (10 more)
- `0:15` → 40 connections (10 more)
- ...continues until duration ends

**Perfect for:**
- 📊 Watching Grafana dashboards update
- 📈 Observing metric trends
- 🔍 Finding connection limit threshold

---

## Verify Specific Collectors

### **Query Response Time**
```bash
# Start exporter with collector
cargo run -- --dsn "..." --collector.query_response_time

# Run load with varied query times
python3 mariadb_loadtest.py --workload all_metrics --workers 50

# Check metrics
curl -s http://localhost:9306/metrics | grep "mariadb_query_response_time"
```

### **Statements (Performance Schema)**
```bash
# Enable performance_schema
mariadb -h127.0.0.1 -uroot -proot -e "SET GLOBAL performance_schema = ON;"

# Start exporter with collector
cargo run -- --dsn "..." --collector.statements

# Run load
python3 mariadb_loadtest.py --workload all_metrics --workers 50

# Check metrics
curl -s http://localhost:9306/metrics | grep "mariadb_perf_schema_stmt"
```

### **Schema (Table Sizes)**
```bash
# Start exporter with collector
cargo run -- --dsn "..." --collector.schema

# Run load (inserts data)
python3 mariadb_loadtest.py --workload all_metrics --workers 100

# Check metrics (shows table sizes)
curl -s http://localhost:9306/metrics | grep "mariadb_table_size_bytes"
curl -s http://localhost:9306/metrics | grep "mariadb_table_rows"
```

### **Locks**
```bash
# Ensure performance_schema is enabled
mariadb -h127.0.0.1 -uroot -proot -e "SHOW VARIABLES LIKE 'performance_schema';"

# Start exporter with collector
cargo run -- --dsn "..." --collector.locks

# Run metadata workload (creates locks)
python3 mariadb_loadtest.py --workload metadata --workers 20 --hold-time 10

# Check metrics
curl -s http://localhost:9306/metrics | grep "mariadb_perf_schema.*lock"
```

---

## Troubleshooting

### **No slow queries showing up**

**Problem:** `mariadb_global_status_slow_queries` stays at 0

**Solution:**
```bash
# Check long_query_time setting
mariadb -h127.0.0.1 -uroot -proot -e "SHOW VARIABLES LIKE 'long_query_time';"

# Lower threshold (default is 10 seconds)
mariadb -h127.0.0.1 -uroot -proot -e "SET GLOBAL long_query_time = 0.5;"

# Run test again
python3 mariadb_loadtest.py --workload stress --workers 50
```

---

### **No statement digest metrics**

**Problem:** No `mariadb_perf_schema_stmt_digest_*` metrics

**Solution:**
```bash
# Check if performance_schema is enabled
mariadb -h127.0.0.1 -uroot -proot -e "SHOW VARIABLES LIKE 'performance_schema';"

# If OFF, restart MariaDB with it enabled
# Add to my.cnf: performance_schema = ON
```

---

### **Connection errors**

**Problem:** Script fails with "Too many connections"

**Solution:**
```bash
# Increase max_connections
mariadb -h127.0.0.1 -uroot -proot -e "SET GLOBAL max_connections = 500;"

# Reduce workers
python3 mariadb_loadtest.py --workload all_metrics --workers 20
```

---

### **Deadlock errors / Replication conflicts**

**Problem:** Errors like:
- `Deadlock found when trying to get lock`
- `Record has changed since last read in table 'test_load'`

**Expected behavior** - These errors are **normal and handled automatically**:

The script is optimized for high-concurrency and replication environments:
- ✅ All UPDATE/DELETE operations wrapped in try/except blocks
- ✅ Replication conflicts are caught and ignored gracefully
- ✅ Uses specific WHERE clauses with random values to spread load
- ✅ LIMIT 1 on all potentially conflicting operations
- ✅ Automatic rollback on commit failures

**Worker completion rate:**
- ✅ **90-100% success rate**: Excellent, normal operation
- ✅ **80-90% success rate**: Good for Galera/replication setups
- ⚠️ **<80% success rate**: Consider reducing workers or slower ramp-up

**Note:** Even with failures, all metrics are still properly exercised because:
- INSERTs always succeed (no conflicts)
- SELECTs always succeed (no locks)
- Failed UPDATEs/DELETEs are expected and don't affect metric collection

**If seeing very low success rate (<50%):**
```bash
# Reduce workers
python3 mariadb_loadtest.py --workload all_metrics --workers 10

# Or use slower ramp-up
python3 mariadb_loadtest.py --workload all_metrics --workers 50 --ramp-ms 200
```

---

## Advanced: Testing Connection Limit Behavior

**Goal:** Observe connections increase to max_connections, then gradually decrease.

This is perfect for validating Grafana dashboards and understanding connection patterns.

```bash
# 1. Get max_connections value
MAX_CONNS=$(mariadb -h127.0.0.1 -uroot -proot -sN -e "SELECT @@max_connections")
echo "Max connections: $MAX_CONNS"

# 2. Calculate target workers (max + 50 for queuing)
WORKERS=$((MAX_CONNS + 50))
echo "Will use $WORKERS workers"

# 3. Terminal 1: Start exporter
cargo run -- --dsn "mysql://root:root@127.0.0.1:3306/mysql"

# 4. Terminal 2: Watch connection metrics in real-time
watch -n 1 'curl -s http://localhost:9306/metrics | grep -E "mariadb_global_status_(threads_connected|max_used_connections) "'

# 5. Terminal 3: Run test with persistent retries
cd scripts
python3 mariadb_loadtest.py \
  --workload connection_exhaustion \
  --workers $WORKERS \
  --persistent \
  --burst \
  --hold-time 30
```

**What you'll observe:**
1. **0-5s**: Connections ramp up quickly (burst mode)
2. **5-10s**: Connections hit max_connections limit
3. **10-30s**: Some workers waiting (persistent retry), ~max_connections active
4. **30s+**: Workers complete, connections gradually decrease
5. **End**: All workers finished, connections back to baseline

**Expected output:**
```
[0] Connected (connection_exhaustion workload)
[1] Connected (connection_exhaustion workload)
...
[150] Connected (connection_exhaustion workload)
[151] Max connections reached, waiting for available slot...
[152] Max connections reached, waiting for available slot...
...
[151] Connected after 5 retries (connection_exhaustion workload)
[0] Closed
[1] Closed
...
```

---

## Complete Example Session

```bash
# 1. Start MariaDB
just mariadb
# Wait ~10 seconds for healthy

# 2. Set slow query threshold
mariadb -h127.0.0.1 -uroot -proot -e "SET GLOBAL long_query_time = 0.5;"

# 3. Start exporter (Terminal 1)
cargo run -- \
  --dsn "mysql://root:root@127.0.0.1:3306/mysql" \
  --collector.query_response_time \
  --collector.statements \
  --collector.schema \
  --collector.locks

# 4. Watch metrics (Terminal 2)
watch -n 1 'curl -s http://localhost:9306/metrics | grep -E "mariadb_global_status_(queries|slow_queries|threads_connected) " | grep -v "^#"'

# 5. Run comprehensive test (Terminal 3)
cd scripts
python3 mariadb_loadtest.py \
  --workload all_metrics \
  --duration 60 \
  --step-size 10 \
  --step-interval 5 \
  --hold-time 5

# 6. Check results
curl -s http://localhost:9306/metrics | grep "^mariadb_" | wc -l
# Should see 300+ metrics

# 7. Cleanup
podman stop mariadb_exporter_db
```

---

## Expected Metrics Count

| Collectors Enabled | Expected Metric Count |
|--------------------|----------------------|
| Default only | 150-200 |
| + query_response_time | 200-250 |
| + statements | 250-300 |
| + schema | 280-320 |
| + locks | 300-350 |
| All collectors | 350-400 |

---

## Next Steps

After testing locally, try:

1. **Test with Grafana dashboard** - Import `grafana/dashboard.json`
2. **Test with Prometheus** - Add exporter as target
3. **Run long-term load** - Use duration mode for hours
4. **Test in production-like setup** - Real workload patterns

See `METRICS_COVERAGE.md` for detailed metric mapping.
