#!/usr/bin/env python3
"""
MariaDB Load Tester - Generate workload to test ALL mariadb_exporter metrics.

Purpose:
    Create database activity that exercises all collectors and their metrics.
    See scripts/METRICS_COVERAGE.md for detailed metric mapping.

Quick Start:
    # Default: Gradual ramp-up (60s duration, stepped increase)
    python mariadb_loadtest.py

    # Comprehensive metric testing (recommended)
    python mariadb_loadtest.py --workload all_metrics --workers 50

    # Single run mode (no duration)
    python mariadb_loadtest.py --single-run --workload mixed

    # Connection exhaustion test
    python mariadb_loadtest.py --workload connection_exhaustion --workers 200 --burst

Workloads:
    basic               - Simple queries, minimal metrics
    mixed               - Varied operations (DEFAULT)
    stress              - Heavy load, slow queries, complex operations
    metadata            - DDL operations, metadata locks
    connection_exhaustion - Max connections testing
    query_response_time - Tests all histogram buckets (100ms-1s, 1s-10s, >10s)
    all_metrics         - Comprehensive coverage of ALL collectors

Environment Variables:
    DB_HOST, DB_PORT, DB_USER, DB_PASSWORD, DB_NAME
    LT_WORKLOAD, LT_WORKERS, LT_HOLD, LT_BURST, LT_SINGLE_RUN
    LT_DURATION (default: 60), LT_STEP_SIZE (default: 10), LT_STEP_INTERVAL (default: 5)

Run --help for full options.
"""

from __future__ import annotations

import argparse
import asyncio
import concurrent.futures
import os
import random
import signal
import sys
import threading
import time
from dataclasses import dataclass
from typing import Callable

import mariadb

# ============================================================================
# CONFIGURATION
# ============================================================================

@dataclass(frozen=True)
class Config:
    """Test configuration."""
    # Connection
    host: str
    port: int
    user: str
    password: str
    database: str
    connect_timeout: int

    # Workload
    workload_type: str
    workers: int
    hold_time: float
    autocommit: bool

    # Execution mode
    burst_mode: bool
    ramp_ms: int
    jitter_ms: int
    max_threads: int
    persistent: bool

    # Duration mode (stepped ramp-up)
    duration: int
    step_size: int
    step_interval: int


def parse_args() -> Config:
    """Parse command-line arguments and environment variables."""
    p = argparse.ArgumentParser(
        description="MariaDB connection and load tester",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  # Default: 60s duration with gradual ramp-up
  %(prog)s

  # Single run mode (no duration)
  %(prog)s --single-run --workload mixed

  # Connection exhaustion
  %(prog)s --workload connection_exhaustion --workers 200 --burst

  # Custom duration
  %(prog)s --duration 120 --step-size 20 --step-interval 10
        """
    )

    # Connection settings
    conn = p.add_argument_group('Connection')
    conn.add_argument("--host", default=os.getenv("DB_HOST", "127.0.0.1"))
    conn.add_argument("--port", type=int, default=int(os.getenv("DB_PORT", "3306")))
    conn.add_argument("--user", default=os.getenv("DB_USER", "root"))
    conn.add_argument("--password", default=os.getenv("DB_PASSWORD", "root"))
    conn.add_argument("--database", default=os.getenv("DB_NAME", "test"))
    conn.add_argument("--connect-timeout", type=int, default=int(os.getenv("LT_CONN_TIMEOUT", "2")))

    # Workload settings
    workload = p.add_argument_group('Workload')
    workload.add_argument(
        "--workload",
        choices=["basic", "mixed", "stress", "metadata", "connection_exhaustion", "query_response_time", "all_metrics"],
        default=os.getenv("LT_WORKLOAD", "mixed"),
        help="Workload type (default: mixed). Use 'query_response_time' to test histogram buckets, 'all_metrics' for comprehensive testing."
    )
    workload.add_argument("--workers", type=int, default=int(os.getenv("LT_WORKERS", "50")),
                         help="Number of concurrent workers (default: 50, auto-adjusts to 200 for connection_exhaustion)")
    workload.add_argument("--hold-time", type=float, default=float(os.getenv("LT_HOLD", "5")),
                         help="Seconds to hold each connection (default: 5)")
    workload.add_argument("--autocommit", action="store_true",
                         default=os.getenv("LT_AUTOCOMMIT", "false").lower() == "true")

    # Execution mode
    execution = p.add_argument_group('Execution')
    execution.add_argument("--burst", action="store_true",
                          default=os.getenv("LT_BURST", "false").lower() == "true",
                          help="Create all connections at once")
    execution.add_argument("--ramp-ms", type=int, default=int(os.getenv("LT_RAMP_MS", "10")),
                          help="Milliseconds between worker starts (default: 10)")
    execution.add_argument("--jitter-ms", type=int, default=int(os.getenv("LT_JITTER_MS", "0")),
                          help="Random jitter added to ramp (default: 0)")
    execution.add_argument("--max-threads", type=int, default=int(os.getenv("LT_MAX_THREADS", "200")),
                          help="Thread pool size (default: 200)")
    execution.add_argument("--persistent", action="store_true",
                          default=os.getenv("LT_PERSISTENT", "false").lower() == "true",
                          help="Retry connections when max_connections reached (for observing connection limit behavior)")

    # Duration mode (default) or single-run mode
    duration_group = p.add_argument_group('Duration Mode (default: stepped ramp-up)')
    duration_group.add_argument("--duration", type=int, default=int(os.getenv("LT_DURATION", "60")),
                               help="Total test duration in seconds (default: 60s, use --single-run to disable)")
    duration_group.add_argument("--step-size", type=int, default=int(os.getenv("LT_STEP_SIZE", "10")),
                               help="Connections to add per step (default: 10)")
    duration_group.add_argument("--step-interval", type=int, default=int(os.getenv("LT_STEP_INTERVAL", "5")),
                               help="Seconds between steps (default: 5)")
    duration_group.add_argument("--single-run", action="store_true",
                               default=os.getenv("LT_SINGLE_RUN", "false").lower() == "true",
                               help="Disable duration mode and run once (overrides --duration)")

    args = p.parse_args()

    # Validation
    if args.workers <= 0 or args.max_threads <= 0:
        p.error("--workers and --max-threads must be > 0")

    # Apply single-run flag (overrides duration)
    duration = 0 if args.single_run else args.duration

    if duration > 0 and (args.step_size <= 0 or args.step_interval <= 0):
        p.error("--step-size and --step-interval must be > 0 when using --duration")

    # Auto-adjust workers for connection exhaustion
    workers = args.workers
    if (args.workload == "connection_exhaustion" and
        not any(a.startswith("--workers") for a in sys.argv) and
        "LT_WORKERS" not in os.environ):
        workers = 200
        print(f"[INFO] Auto-setting workers to {workers} for connection_exhaustion (override with --workers)")

    return Config(
        host=args.host,
        port=args.port,
        user=args.user,
        password=args.password,
        database=args.database,
        connect_timeout=args.connect_timeout,
        workload_type=args.workload,
        workers=workers,
        hold_time=args.hold_time,
        autocommit=args.autocommit,
        burst_mode=args.burst,
        ramp_ms=args.ramp_ms,
        jitter_ms=args.jitter_ms,
        max_threads=args.max_threads,
        persistent=args.persistent,
        duration=duration,  # Use computed duration (respects --single-run)
        step_size=args.step_size,
        step_interval=args.step_interval,
    )


# ============================================================================
# DATABASE UTILITIES
# ============================================================================

def connect(cfg: Config) -> mariadb.Connection:
    """Create database connection."""
    return mariadb.connect(
        host=cfg.host,
        user=cfg.user,
        password=cfg.password,
        database=cfg.database,
        port=cfg.port,
        connect_timeout=cfg.connect_timeout,
        autocommit=cfg.autocommit,
    )


def get_max_connections(cfg: Config) -> int:
    """Get MariaDB max_connections setting."""
    try:
        conn = connect(cfg)
        cur = conn.cursor()
        cur.execute("SHOW VARIABLES LIKE 'max_connections'")
        result = cur.fetchone()
        cur.close()
        conn.close()
        return int(result[1]) if result else 151
    except Exception:
        return 151


def ensure_database(cfg: Config) -> None:
    """Create database if missing."""
    try:
        conn = mariadb.connect(
            host=cfg.host, user=cfg.user, password=cfg.password,
            port=cfg.port, connect_timeout=cfg.connect_timeout
        )
        cur = conn.cursor()
        safe_db = cfg.database.replace("`", "``")
        cur.execute(f"CREATE DATABASE IF NOT EXISTS `{safe_db}` CHARACTER SET utf8mb4 COLLATE utf8mb4_general_ci")
        conn.commit()
        cur.close()
        conn.close()
        print(f"[INIT] Database ensured: {cfg.database}")
    except Exception as e:
        print(f"[INIT] ERROR ensuring database: {e}", file=sys.stderr)
        raise


def ensure_test_tables(cfg: Config) -> None:
    """Create test tables."""
    ensure_database(cfg)
    try:
        conn = connect(cfg)
        cur = conn.cursor()
        cur.execute("""
            CREATE TABLE IF NOT EXISTS test_load (
                id INT AUTO_INCREMENT PRIMARY KEY,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                payload VARCHAR(255),
                counter INT DEFAULT 0,
                status VARCHAR(50),
                INDEX idx_status (status),
                INDEX idx_counter (counter)
            )
        """)
        cur.execute("""
            CREATE TABLE IF NOT EXISTS test_metadata (
                id INT AUTO_INCREMENT PRIMARY KEY,
                data TEXT
            )
        """)
        conn.commit()
        cur.close()
        conn.close()
        print("[INIT] Test tables ensured")
    except Exception as e:
        print(f"[INIT] ERROR ensuring tables: {e}", file=sys.stderr)
        raise


# ============================================================================
# WORKLOADS
# ============================================================================

def rand_payload(n: int = 16) -> str:
    """Generate random hex payload."""
    return "".join(random.choice("abcdef0123456789") for _ in range(n))


def workload_basic(cur: mariadb.Cursor, conn: mariadb.Connection, cfg: Config) -> None:
    """Basic workload: simple queries."""
    payload = rand_payload(16)
    cur.execute("INSERT INTO test_load (payload, status) VALUES (?, 'active')", (payload,))
    if not cfg.autocommit:
        try:
            conn.commit()
        except mariadb.OperationalError:
            conn.rollback()

    for _ in range(3):
        a, b = random.randint(1, 1000), random.randint(1, 1000)
        cur.execute("SELECT ? + ?", (a, b))
        _ = cur.fetchone()
        time.sleep(0.05)


def workload_mixed(cur: mariadb.Cursor, conn: mariadb.Connection, cfg: Config) -> None:
    """Mixed workload: varied operations."""
    # Fast query
    cur.execute("SELECT 1")
    _ = cur.fetchone()

    # INSERT (no contention)
    payload = rand_payload(32)
    cur.execute("INSERT INTO test_load (payload, status, counter) VALUES (?, ?, ?)",
                (payload, random.choice(['active', 'pending', 'done']), random.randint(1, 100)))

    # SELECT with index (no locks)
    cur.execute("SELECT * FROM test_load WHERE status = 'active' LIMIT 10")
    _ = cur.fetchall()

    # Full table scan (slow, but no locks)
    cur.execute("SELECT COUNT(*) FROM test_load WHERE payload LIKE '%abc%'")
    _ = cur.fetchone()

    # UPDATE - use random counter to spread contention (wrapped in try/except for replication conflicts)
    try:
        random_counter = random.randint(1, 1000)
        cur.execute("UPDATE test_load SET counter = counter + 1 WHERE counter = ? LIMIT 1", (random_counter,))
    except mariadb.OperationalError:
        pass  # Ignore replication conflicts

    # Sort + GROUP BY (temp table, no locks)
    cur.execute("SELECT status, COUNT(*) FROM test_load GROUP BY status")
    _ = cur.fetchall()

    # DELETE - use specific counter to reduce contention (wrapped in try/except)
    try:
        delete_counter = random.randint(1, 50)
        cur.execute("DELETE FROM test_load WHERE status = 'done' AND counter < ? LIMIT 1", (delete_counter,))
    except mariadb.OperationalError:
        pass  # Ignore replication conflicts

    if not cfg.autocommit:
        try:
            conn.commit()
        except mariadb.OperationalError:
            conn.rollback()


def workload_stress(cur: mariadb.Cursor, conn: mariadb.Connection, cfg: Config) -> None:
    """Stress workload: complex queries, intentional slow queries."""
    # Multiple inserts (no contention)
    for _ in range(5):
        payload = rand_payload(64)
        cur.execute("INSERT INTO test_load (payload, status, counter) VALUES (?, ?, ?)",
                    (payload, random.choice(['active', 'pending', 'processing', 'done']),
                     random.randint(1, 1000)))

    # Complex aggregation (no locks)
    cur.execute("""
        SELECT status, COUNT(*) as cnt, AVG(counter) as avg_counter
        FROM test_load
        GROUP BY status
        HAVING cnt > 0
        ORDER BY cnt DESC
    """)
    _ = cur.fetchall()

    # Subquery (no locks)
    cur.execute("""
        SELECT * FROM test_load
        WHERE counter > (SELECT AVG(counter) FROM test_load)
        LIMIT 10
    """)
    _ = cur.fetchall()

    # Intentional slow query (exercises slow_queries metric)
    slow_duration = random.uniform(0.5, 1.2)
    cur.execute("SELECT SLEEP(?)", (slow_duration,))
    _ = cur.fetchone()

    # Large result set (no locks)
    cur.execute("SELECT * FROM test_load WHERE counter > 0 ORDER BY created_at DESC LIMIT 1000")
    _ = cur.fetchall()

    # Cross join with LIMIT (exercises buffer pool without excessive locks)
    # Use specific counter range to reduce contention
    counter_min = random.randint(1, 500)
    counter_max = counter_min + 100
    cur.execute("""
        SELECT COUNT(*)
        FROM (SELECT * FROM test_load WHERE counter BETWEEN ? AND ? LIMIT 50) t1,
             (SELECT * FROM test_load WHERE counter BETWEEN ? AND ? LIMIT 50) t2
        WHERE t1.counter = t2.counter
    """, (counter_min, counter_max, counter_min, counter_max))
    _ = cur.fetchone()

    # Updates with specific targeting to reduce contention (wrapped in try/except)
    try:
        random_status = random.choice(['pending', 'active', 'processing'])
        random_counter = random.randint(1, 1000)
        cur.execute("UPDATE test_load SET status = 'processing' WHERE status = ? AND counter = ? LIMIT 1",
                    (random_status, random_counter))
        cur.execute("UPDATE test_load SET counter = counter + 1 WHERE counter BETWEEN ? AND ? LIMIT 1",
                    (random_counter, random_counter + 10))
    except mariadb.OperationalError:
        pass  # Ignore replication conflicts

    if not cfg.autocommit:
        try:
            conn.commit()
        except mariadb.OperationalError:
            conn.rollback()


def workload_metadata(cur: mariadb.Cursor, conn: mariadb.Connection, cfg: Config) -> None:
    """Metadata workload: locks and DDL."""
    if not cfg.autocommit:
        cur.execute("START TRANSACTION")

    cur.execute("SELECT * FROM test_metadata LIMIT 1")
    _ = cur.fetchall()
    time.sleep(random.uniform(0.5, 2.0))

    # DDL operation (30% chance)
    if random.random() < 0.3:
        try:
            cur.execute("ALTER TABLE test_metadata ADD COLUMN temp_col VARCHAR(10)")
            cur.execute("ALTER TABLE test_metadata DROP COLUMN temp_col")
        except Exception:
            pass

    payload = rand_payload(128)
    cur.execute("INSERT INTO test_load (payload, status) VALUES (?, 'locked')", (payload,))
    time.sleep(random.uniform(0.5, 1.5))

    if not cfg.autocommit:
        try:
            conn.commit()
        except mariadb.OperationalError:
            conn.rollback()


def workload_connection_exhaustion(cur: mariadb.Cursor, conn: mariadb.Connection, cfg: Config) -> None:
    """Connection exhaustion: hold connections with minimal work."""
    cur.execute("SHOW STATUS LIKE 'Threads_connected'")
    threads_connected = cur.fetchone()
    cur.execute("SHOW STATUS LIKE 'Max_used_connections'")
    max_used = cur.fetchone()
    cur.execute("SHOW VARIABLES LIKE 'max_connections'")
    max_connections = cur.fetchone()

    if threads_connected and max_used and max_connections:
        print(f"[CONN] Threads: {threads_connected[1]} | Max used: {max_used[1]} | Limit: {max_connections[1]}")

    # Keep connection alive
    for _ in range(int(cfg.hold_time)):
        cur.execute("SELECT 1")
        _ = cur.fetchone()
        time.sleep(1)


def workload_query_response_time(cur: mariadb.Cursor, conn: mariadb.Connection, cfg: Config) -> None:
    """
    Query response time workload: generates queries in all histogram buckets.

    Bucket coverage:
    - le="0.1": Queries 100ms-1s (200ms, 500ms, 800ms)
    - le="1.0": Queries 1s-10s (2s, 5s)
    - le="10.0": Queries >10s (12s)
    - Fast queries <100ms (not in buckets, but in _count and _sum)
    """
    # Enable query_response_time if needed
    try:
        cur.execute("SET GLOBAL query_response_time_stats = ON")
    except Exception:
        pass  # May not have permission or plugin not available

    # Fast queries (< 100ms) - appear in _count and _sum but not buckets
    for _ in range(3):
        cur.execute("SELECT 1")
        _ = cur.fetchone()
        cur.execute("SELECT ? + ?", (random.randint(1, 100), random.randint(1, 100)))
        _ = cur.fetchone()

    # Bucket le="0.1" (100ms-1s)
    cur.execute("SELECT SLEEP(0.2)")  # 200ms
    _ = cur.fetchone()

    cur.execute("SELECT SLEEP(0.5)")  # 500ms
    _ = cur.fetchone()

    cur.execute("SELECT SLEEP(0.8)")  # 800ms
    _ = cur.fetchone()

    # Bucket le="1.0" (1s-10s)
    cur.execute("SELECT SLEEP(2)")  # 2s
    _ = cur.fetchone()

    if random.random() < 0.5:  # 50% chance for 5s query
        cur.execute("SELECT SLEEP(5)")  # 5s
        _ = cur.fetchone()

    # Bucket le="10.0" (>10s) - less frequent to avoid excessive slowness
    if random.random() < 0.2:  # 20% chance for very slow query
        cur.execute("SELECT SLEEP(12)")  # 12s
        _ = cur.fetchone()

    # Some data manipulation to make it realistic
    payload = rand_payload(32)
    cur.execute("INSERT INTO test_load (payload, status) VALUES (?, 'test')", (payload,))

    if not cfg.autocommit:
        try:
            conn.commit()
        except mariadb.OperationalError:
            conn.rollback()


def workload_all_metrics(cur: mariadb.Cursor, conn: mariadb.Connection, cfg: Config) -> None:
    """
    Comprehensive workload that exercises ALL metrics from all collectors.

    Metrics coverage:
    - default: connections, queries, com_*, table_locks, InnoDB buffer pool, slow queries
    - query_response_time: fast, medium, and slow queries across all buckets
    - statements: varied statement types (performance_schema)
    - schema: table data and row inserts
    - locks: table locks and metadata locks
    - metadata: long transactions with DDL
    """
    # === PART 1: Query response time bucket testing ===
    # Fast queries (< 100ms) - appear in _count/_sum but not buckets
    for _ in range(2):
        cur.execute("SELECT 1")
        _ = cur.fetchone()

    cur.execute("SELECT ? + ?", (random.randint(1, 100), random.randint(1, 100)))
    _ = cur.fetchone()

    # Medium query: le="0.1" bucket (100ms-1s)
    cur.execute("SELECT SLEEP(0.3)")  # 300ms
    _ = cur.fetchone()

    # Slow query: le="1.0" bucket (1s-10s) - 30% chance
    if random.random() < 0.3:
        cur.execute("SELECT SLEEP(2)")  # 2s
        _ = cur.fetchone()

    # === PART 2: Data manipulation (com_insert, com_update, com_delete, schema) ===
    # INSERT - exercises: com_insert, table rows, table size (no contention)
    for _ in range(3):
        payload = rand_payload(128)
        cur.execute("INSERT INTO test_load (payload, status, counter) VALUES (?, ?, ?)",
                    (payload, random.choice(['active', 'pending', 'done']), random.randint(1, 500)))

    # SELECT - exercises: com_select, table_locks_immediate (no locks)
    cur.execute("SELECT * FROM test_load WHERE status = 'active' LIMIT 20")
    _ = cur.fetchall()

    # UPDATE - exercises: com_update, innodb_row_lock_* (targeted to reduce contention)
    try:
        update_counter = random.randint(1, 500)
        cur.execute("UPDATE test_load SET counter = counter + 1 WHERE counter = ? LIMIT 1", (update_counter,))
    except mariadb.OperationalError:
        pass  # Ignore replication conflicts

    # DELETE - exercises: com_delete (specific targeting)
    try:
        delete_counter = random.randint(400, 500)
        cur.execute("DELETE FROM test_load WHERE status = 'done' AND counter = ? LIMIT 1", (delete_counter,))
    except mariadb.OperationalError:
        pass  # Ignore replication conflicts

    # === PART 3: Slow queries (query_response_time: > 0.5s, slow_queries) ===
    # Intentional slow query
    cur.execute("SELECT SLEEP(0.6)")
    _ = cur.fetchone()

    # Large scan (query_response_time: 0.1-0.5s)
    cur.execute("SELECT COUNT(*) FROM test_load WHERE payload LIKE '%a%'")
    _ = cur.fetchone()

    # === PART 4: Complex queries (statements, buffer pool) ===
    # Aggregation with GROUP BY (temporary tables, no locks)
    cur.execute("""
        SELECT status, COUNT(*) as cnt, MIN(counter), MAX(counter), AVG(counter)
        FROM test_load
        GROUP BY status
        ORDER BY cnt DESC
    """)
    _ = cur.fetchall()

    # Subquery (exercises buffer pool, nested queries, no locks)
    cur.execute("""
        SELECT * FROM test_load
        WHERE counter > (SELECT AVG(counter) FROM test_load)
        ORDER BY created_at DESC
        LIMIT 15
    """)
    _ = cur.fetchall()

    # Self-join (exercises buffer pool, join operations)
    # Use limited subqueries to reduce contention and avoid deadlocks
    try:
        counter_range_start = random.randint(1, 400)
        counter_range_end = counter_range_start + 50
        cur.execute("""
            SELECT t1.status, COUNT(*)
            FROM (SELECT status, counter FROM test_load
                  WHERE counter BETWEEN ? AND ? LIMIT 30) t1
            JOIN (SELECT status, counter FROM test_load
                  WHERE counter BETWEEN ? AND ? LIMIT 30) t2
                ON t1.counter = t2.counter
            WHERE t1.status != t2.status
            GROUP BY t1.status
        """, (counter_range_start, counter_range_end, counter_range_start, counter_range_end))
        _ = cur.fetchall()
    except Exception:
        # Ignore rare deadlocks in concurrent scenarios
        pass

    # === PART 5: Metadata operations (metadata, locks) ===
    if random.random() < 0.3:  # 30% chance to avoid excessive DDL
        try:
            # Start transaction to create metadata lock
            if not cfg.autocommit:
                cur.execute("START TRANSACTION")

            # DDL operation (metadata locks)
            cur.execute("SELECT * FROM test_metadata FOR UPDATE")
            _ = cur.fetchall()

            # Hold lock briefly
            time.sleep(0.3)

            if not cfg.autocommit:
                cur.execute("COMMIT")
        except Exception:
            pass

    # === PART 6: Bytes sent/received (traffic metrics) ===
    # Large result set to generate traffic
    cur.execute("SELECT * FROM test_load LIMIT 100")
    _ = cur.fetchall()

    # Commit all changes
    if not cfg.autocommit:
        try:
            conn.commit()
        except mariadb.OperationalError:
            conn.rollback()


WORKLOADS: dict[str, Callable] = {
    "basic": workload_basic,
    "mixed": workload_mixed,
    "stress": workload_stress,
    "metadata": workload_metadata,
    "connection_exhaustion": workload_connection_exhaustion,
    "query_response_time": workload_query_response_time,  # Tests all histogram buckets
    "all_metrics": workload_all_metrics,  # Comprehensive coverage
}


# ============================================================================
# WORKER EXECUTION
# ============================================================================

def sync_worker(worker_id: int, cfg: Config, stop_event: threading.Event | None = None) -> bool:
    """Execute one worker connection lifecycle."""
    max_retries = 60 if cfg.persistent else 1  # Retry for 60s in persistent mode
    retry_delay = 1.0  # 1 second between retries

    for attempt in range(max_retries):
        conn = None
        try:
            conn = connect(cfg)
            cur = conn.cursor()

            if attempt > 0:
                print(f"[{worker_id}] Connected after {attempt} retries ({cfg.workload_type} workload)")
            else:
                print(f"[{worker_id}] Connected ({cfg.workload_type} workload)")

            workload_func = WORKLOADS.get(cfg.workload_type, workload_basic)
            workload_func(cur, conn, cfg)

            # In duration mode, hold connection until stop event is set
            # In single-run mode, hold for configured time
            if stop_event:
                # Duration mode: hold connection until test ends
                stop_event.wait()  # Block until event is set
            else:
                # Single-run mode: hold for configured time
                time.sleep(cfg.hold_time)

            cur.close()
            conn.close()
            print(f"[{worker_id}] Closed")
            return True

        except mariadb.OperationalError as e:
            error_msg = str(e)

            # Close connection on error
            if conn:
                try:
                    conn.close()
                except Exception:
                    pass

            # Check if it's a connection limit error
            if "Too many connections" in error_msg or "max_connections" in error_msg:
                if cfg.persistent and attempt < max_retries - 1:
                    if attempt == 0:
                        print(f"[{worker_id}] Max connections reached, waiting for available slot...")
                    time.sleep(retry_delay)
                    continue  # Retry
                else:
                    print(f"[{worker_id}] FAILED (max_connections reached)", file=sys.stderr)
                    return False
            else:
                # Other operational errors (replication conflicts, etc.)
                print(f"[{worker_id}] FAILED (OperationalError): {error_msg}", file=sys.stderr)
                return False

        except Exception as e:
            # Close connection on unexpected error
            if conn:
                try:
                    conn.close()
                except Exception:
                    pass

            print(f"[{worker_id}] FAILED (Unexpected): {e}", file=sys.stderr)
            return False

    # Exhausted all retries
    print(f"[{worker_id}] FAILED (gave up after {max_retries} retries)", file=sys.stderr)
    return False


async def async_worker(worker_id: int, cfg: Config, executor: concurrent.futures.ThreadPoolExecutor, stop_event: threading.Event | None = None) -> bool:
    """Async wrapper for sync worker."""
    loop = asyncio.get_running_loop()
    return await loop.run_in_executor(executor, sync_worker, worker_id, cfg, stop_event)


# ============================================================================
# TEST ORCHESTRATION
# ============================================================================

def setup_signal_handlers(stop_event: asyncio.Event) -> None:
    """Setup SIGINT/SIGTERM handlers."""
    def handle_signal(signum, _frame):
        print(f"\n[CTRL] Received signal {signum}; stopping...")
        stop_event.set()

    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            signal.signal(sig, handle_signal)
        except Exception:
            pass


def print_summary(attempted: int, successful: int, failed: int, duration: float) -> None:
    """Print test summary."""
    print("\n===== TEST SUMMARY =====")
    print(f"Total attempted: {attempted}")
    print(f"Successful:      {successful}")
    print(f"Failed:          {failed}")
    print(f"Duration:        {duration}s")
    print("========================\n")


async def run_single_test(cfg: Config) -> tuple[int, int, float]:
    """Run single test (default mode)."""
    ensure_test_tables(cfg)

    # Show connection info for exhaustion testing
    max_conns = get_max_connections(cfg)
    if cfg.workload_type == "connection_exhaustion":
        print(f"[INFO] MariaDB max_connections: {max_conns}")
        if cfg.workers >= max_conns:
            print(f"[WARN] Workers ({cfg.workers}) >= max_connections ({max_conns})")
            print(f"[WARN] Some connections will fail! This is expected.")
        else:
            print(f"[WARN] Workers ({cfg.workers}) < max_connections ({max_conns})")
            print(f"[WARN] Consider --workers {max_conns + 10} to exceed limit")

    persistent_msg = " | persistent=true" if cfg.persistent else ""
    print(f"[INIT] Starting {cfg.workers} workers | workload={cfg.workload_type} | "
          f"burst={cfg.burst_mode} | ramp={cfg.ramp_ms}ms | threads={cfg.max_threads}{persistent_msg}")

    executor = concurrent.futures.ThreadPoolExecutor(max_workers=cfg.max_threads)
    stop = asyncio.Event()
    setup_signal_handlers(stop)

    tasks: list[asyncio.Task[bool]] = []
    start = time.time()

    try:
        for i in range(cfg.workers):
            if stop.is_set():
                break
            tasks.append(asyncio.create_task(async_worker(i, cfg, executor, None)))  # No stop event in single-run
            if not cfg.burst_mode:
                delay = cfg.ramp_ms / 1000.0
                if cfg.jitter_ms > 0:
                    delay += random.randint(0, cfg.jitter_ms) / 1000.0
                await asyncio.sleep(delay)

        results = await asyncio.gather(*tasks, return_exceptions=True)
    finally:
        executor.shutdown(wait=True)

    successful = sum(1 for r in results if r is True)
    failed = len(results) - successful
    duration = round(time.time() - start, 2)

    print_summary(len(tasks), successful, failed, duration)
    return successful, failed, duration


async def run_duration_test(cfg: Config) -> tuple[int, int, float]:
    """Run duration-based test with stepped ramp-up."""
    ensure_test_tables(cfg)

    max_conns = get_max_connections(cfg)
    print(f"[INFO] MariaDB max_connections: {max_conns}")
    print(f"[INIT] Duration mode: {cfg.duration}s total")
    print(f"[INIT] Step size: {cfg.step_size} connections every {cfg.step_interval}s")
    persistent_msg = " | persistent retries enabled" if cfg.persistent else ""
    print(f"[INIT] Target max: {cfg.workers} workers | workload={cfg.workload_type}{persistent_msg}")

    executor = concurrent.futures.ThreadPoolExecutor(max_workers=cfg.max_threads)
    stop = asyncio.Event()
    worker_stop = threading.Event()  # Threading event for workers
    setup_signal_handlers(stop)

    all_tasks: list[asyncio.Task[bool]] = []
    start_time = time.time()
    current_workers = 0
    step_number = 0

    try:
        while time.time() - start_time < cfg.duration and not stop.is_set():
            # Calculate workers for this step
            target_workers = min((step_number + 1) * cfg.step_size, cfg.workers)
            new_workers = target_workers - current_workers

            if new_workers > 0:
                print(f"\n[STEP {step_number + 1}] Adding {new_workers} connections "
                      f"(total: {target_workers}/{cfg.workers})")

                for i in range(current_workers, target_workers):
                    if stop.is_set():
                        break
                    all_tasks.append(asyncio.create_task(async_worker(i, cfg, executor, worker_stop)))  # Pass threading event
                    if not cfg.burst_mode:
                        await asyncio.sleep(cfg.ramp_ms / 1000.0)

                current_workers = target_workers

            step_number += 1

            # Wait for next step
            if current_workers < cfg.workers:
                remaining = cfg.duration - (time.time() - start_time)
                wait_time = min(cfg.step_interval, remaining)
                if wait_time > 0:
                    await asyncio.sleep(wait_time)
            else:
                # Reached max, hold until duration ends
                remaining = cfg.duration - (time.time() - start_time)
                if remaining > 0:
                    print(f"\n[INFO] Max workers reached, holding for {remaining:.1f}s...")
                    await asyncio.sleep(remaining)
                break

        print(f"\n[INFO] Duration complete, signaling workers to close...")
        worker_stop.set()  # Signal all workers to close their connections

        results = await asyncio.gather(*all_tasks, return_exceptions=True)

    finally:
        worker_stop.set()  # Ensure it's set even if interrupted
        executor.shutdown(wait=True)

    successful = sum(1 for r in results if r is True)
    failed = len(results) - successful
    duration = round(time.time() - start_time, 2)

    print_summary(len(all_tasks), successful, failed, duration)
    return successful, failed, duration


async def run_test(cfg: Config) -> tuple[int, int, float]:
    """Run test (routes to single or duration mode)."""
    if cfg.duration > 0:
        return await run_duration_test(cfg)
    else:
        return await run_single_test(cfg)


# ============================================================================
# MAIN
# ============================================================================

def main() -> None:
    """Main entry point."""
    cfg = parse_args()

    print("\n" + "="*60)
    print("MariaDB Load Tester & Metrics Exerciser")
    print("="*60)
    print(f"Workload: {cfg.workload_type}")
    print(f"Workers: {cfg.workers}")
    if cfg.duration > 0:
        print(f"Mode: Duration ({cfg.duration}s, steps of {cfg.step_size} every {cfg.step_interval}s)")
    else:
        print(f"Mode: Single run (hold {cfg.hold_time}s)")
    print("="*60 + "\n")

    asyncio.run(run_test(cfg))


if __name__ == "__main__":
    main()
