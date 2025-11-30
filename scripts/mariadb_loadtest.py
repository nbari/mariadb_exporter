#!/usr/bin/env python3
"""
MariaDB load tester and metrics exerciser.

Creates diverse workloads to test all MariaDB exporter collectors:
- Query response time distribution (fast/slow queries)
- Metadata locks (DDL operations)
- Temporary tables and sorts
- InnoDB operations
- Connection patterns
- SELECT/INSERT/UPDATE/DELETE variations

- `from __future__ import annotations` defers annotation evaluation to runtime; enables builtin
  generics like `tuple[int, ...]` on older Pythons and avoids importing `typing` names.
"""

from __future__ import annotations

import argparse
import asyncio
import concurrent.futures
import os
import random
import signal
import sys
import time
from dataclasses import dataclass
from typing import Callable

import mariadb


@dataclass(frozen=True)
class Config:
    host: str
    user: str
    password: str
    database: str
    port: int
    workers: int
    hold_time: float
    burst_mode: bool
    ramp_ms: int
    jitter_ms: int
    connect_timeout: int
    max_threads: int
    autocommit: bool
    workload_type: str  # basic, mixed, stress, metadata


def parse_args() -> Config:
    """Flags first, env fallback; easy to tweak without code edits."""
    p = argparse.ArgumentParser(description="MariaDB connection/load tester")
    p.add_argument("--host", default=os.getenv("DB_HOST", "127.0.0.1"))
    p.add_argument("--user", default=os.getenv("DB_USER", "root"))  # default changed
    p.add_argument(
        "--password", default=os.getenv("DB_PASSWORD", "root")
    )  # default changed
    p.add_argument("--database", default=os.getenv("DB_NAME", "test"))
    p.add_argument("--port", type=int, default=int(os.getenv("DB_PORT", "3306")))
    p.add_argument("--workers", type=int, default=int(os.getenv("LT_WORKERS", "50")))
    p.add_argument("--hold-time", type=float, default=float(os.getenv("LT_HOLD", "5")))
    p.add_argument(
        "--burst",
        action="store_true",
        default=os.getenv("LT_BURST", "false").lower() == "true",
    )
    p.add_argument("--ramp-ms", type=int, default=int(os.getenv("LT_RAMP_MS", "10")))
    p.add_argument("--jitter-ms", type=int, default=int(os.getenv("LT_JITTER_MS", "0")))
    p.add_argument(
        "--connect-timeout", type=int, default=int(os.getenv("LT_CONN_TIMEOUT", "2"))
    )
    p.add_argument(
        "--max-threads", type=int, default=int(os.getenv("LT_MAX_THREADS", "200"))
    )
    p.add_argument(
        "--autocommit",
        action="store_true",
        default=os.getenv("LT_AUTOCOMMIT", "false").lower() == "true",
    )
    p.add_argument(
        "--workload",
        choices=["basic", "mixed", "stress", "metadata"],
        default=os.getenv("LT_WORKLOAD", "mixed"),
        help="Workload type: basic (simple queries), mixed (varied operations - DEFAULT), stress (heavy load), metadata (DDL/locks)",
    )
    args = p.parse_args()

    if args.workers <= 0 or args.max_threads <= 0:
        p.error("--workers and --max-threads must be > 0")

    return Config(
        host=args.host,
        user=args.user,
        password=args.password,
        database=args.database,
        port=args.port,
        workers=args.workers,
        hold_time=args.hold_time,
        burst_mode=args.burst,
        ramp_ms=args.ramp_ms,
        jitter_ms=args.jitter_ms,
        connect_timeout=args.connect_timeout,
        max_threads=args.max_threads,
        autocommit=args.autocommit,
        workload_type=args.workload,
    )


def _rand_payload(n: int = 16) -> str:
    return "".join(random.choice("abcdef0123456789") for _ in range(n))


def _connect(cfg: Config) -> mariadb.Connection:
    """Connect to the target database."""
    return mariadb.connect(
        host=cfg.host,
        user=cfg.user,
        password=cfg.password,
        database=cfg.database,
        port=cfg.port,
        connect_timeout=cfg.connect_timeout,
        autocommit=cfg.autocommit,
    )


def _connect_no_db(cfg: Config) -> mariadb.Connection:
    """Connect to server without selecting a database (for DB bootstrap)."""
    return mariadb.connect(
        host=cfg.host,
        user=cfg.user,
        password=cfg.password,
        port=cfg.port,
        connect_timeout=cfg.connect_timeout,
    )


def ensure_database(cfg: Config) -> None:
    """Create the database if missing. Requires CREATE privilege."""
    try:
        conn = _connect_no_db(cfg)
        cur = conn.cursor()
        safe_db = cfg.database.replace("`", "``")
        cur.execute(
            f"CREATE DATABASE IF NOT EXISTS `{safe_db}` "
            "CHARACTER SET utf8mb4 COLLATE utf8mb4_general_ci"
        )
        conn.commit()
        cur.close()
        conn.close()
        print(f"[INIT] Database ensured: {cfg.database}")
    except Exception as e:
        print(f"[INIT] ERROR ensuring database '{cfg.database}': {e}", file=sys.stderr)
        raise


def ensure_test_table(cfg: Config) -> None:
    """Ensure database + table exist."""
    ensure_database(cfg)
    try:
        conn = _connect(cfg)
        cur = conn.cursor()
        cur.execute(
            """
            CREATE TABLE IF NOT EXISTS test_load (
                id INT AUTO_INCREMENT PRIMARY KEY,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                payload VARCHAR(255),
                counter INT DEFAULT 0,
                status VARCHAR(50),
                INDEX idx_status (status),
                INDEX idx_counter (counter)
            )
            """
        )
        # Table for metadata lock testing
        cur.execute(
            """
            CREATE TABLE IF NOT EXISTS test_metadata (
                id INT AUTO_INCREMENT PRIMARY KEY,
                data TEXT
            )
            """
        )
        conn.commit()
        cur.close()
        conn.close()
        print("[INIT] test tables ensured")
    except Exception as e:
        print(f"[INIT] ERROR ensuring table: {e}", file=sys.stderr)
        raise


def workload_basic(cur: mariadb.Cursor, conn: mariadb.Connection, cfg: Config) -> None:
    """Basic workload: simple queries."""
    payload = _rand_payload(16)
    cur.execute("INSERT INTO test_load (payload, status) VALUES (?, 'active')", (payload,))
    if not cfg.autocommit:
        conn.commit()

    for _ in range(3):
        a = random.randint(1, 1000)
        b = random.randint(1, 1000)
        cur.execute("SELECT ? + ?", (a, b))
        _ = cur.fetchone()
        time.sleep(0.05)


def workload_mixed(cur: mariadb.Cursor, conn: mariadb.Connection, cfg: Config) -> None:
    """Mixed workload: SELECTs, INSERTs, UPDATEs, temporary tables, sorts."""
    # Fast queries
    cur.execute("SELECT 1")
    _ = cur.fetchone()
    
    # INSERT
    payload = _rand_payload(32)
    cur.execute("INSERT INTO test_load (payload, status, counter) VALUES (?, ?, ?)", 
                (payload, random.choice(['active', 'pending', 'done']), random.randint(1, 100)))
    
    # SELECT with index
    cur.execute("SELECT * FROM test_load WHERE status = 'active' LIMIT 10")
    _ = cur.fetchall()
    
    # SELECT with range scan
    cur.execute("SELECT * FROM test_load WHERE counter BETWEEN 10 AND 50 LIMIT 20")
    _ = cur.fetchall()
    
    # Full table scan (slow query)
    cur.execute("SELECT COUNT(*) FROM test_load WHERE payload LIKE '%abc%'")
    _ = cur.fetchone()
    
    # UPDATE
    cur.execute("UPDATE test_load SET counter = counter + 1 WHERE status = 'active' LIMIT 5")
    
    # Sort operation
    cur.execute("SELECT * FROM test_load ORDER BY created_at DESC LIMIT 10")
    _ = cur.fetchall()
    
    # Temporary table (GROUP BY)
    cur.execute("SELECT status, COUNT(*) FROM test_load GROUP BY status")
    _ = cur.fetchall()
    
    # DELETE
    cur.execute("DELETE FROM test_load WHERE status = 'done' AND counter > 90 LIMIT 5")
    
    if not cfg.autocommit:
        conn.commit()


def workload_stress(cur: mariadb.Cursor, conn: mariadb.Connection, cfg: Config) -> None:
    """Stress workload: complex queries, joins, subqueries."""
    # Multiple inserts
    for _ in range(5):
        payload = _rand_payload(64)
        cur.execute("INSERT INTO test_load (payload, status, counter) VALUES (?, ?, ?)",
                    (payload, random.choice(['active', 'pending', 'processing', 'done']), 
                     random.randint(1, 1000)))
    
    # Complex query with sorting and grouping
    cur.execute("""
        SELECT status, COUNT(*) as cnt, AVG(counter) as avg_counter
        FROM test_load
        GROUP BY status
        HAVING cnt > 0
        ORDER BY cnt DESC
    """)
    _ = cur.fetchall()
    
    # Large sort (causes sort merge passes if data is big)
    cur.execute("SELECT * FROM test_load ORDER BY payload, counter LIMIT 100")
    _ = cur.fetchall()
    
    # Subquery
    cur.execute("""
        SELECT * FROM test_load
        WHERE counter > (SELECT AVG(counter) FROM test_load)
        LIMIT 10
    """)
    _ = cur.fetchall()
    
    # Multiple updates
    cur.execute("UPDATE test_load SET status = 'processing' WHERE status = 'pending' LIMIT 10")
    cur.execute("UPDATE test_load SET counter = counter * 2 WHERE status = 'active' LIMIT 10")
    
    if not cfg.autocommit:
        conn.commit()
    
    # Introduce a slow query
    time.sleep(random.uniform(0.1, 0.5))


def workload_metadata(cur: mariadb.Cursor, conn: mariadb.Connection, cfg: Config) -> None:
    """Metadata lock workload: DDL operations, long transactions."""
    # Long transaction that holds locks
    if not cfg.autocommit:
        cur.execute("START TRANSACTION")
    
    # Read with lock (creates metadata lock)
    cur.execute("SELECT * FROM test_metadata LIMIT 1")
    _ = cur.fetchall()
    
    # Hold the lock for a bit
    time.sleep(random.uniform(0.5, 2.0))
    
    # DDL operation (if not in transaction)
    if random.random() < 0.3:  # 30% chance
        try:
            cur.execute("ALTER TABLE test_metadata ADD COLUMN temp_col VARCHAR(10)")
            cur.execute("ALTER TABLE test_metadata DROP COLUMN temp_col")
        except Exception:
            pass  # Column might already exist/not exist
    
    # More operations while holding locks
    payload = _rand_payload(128)
    cur.execute("INSERT INTO test_load (payload, status) VALUES (?, 'locked')", (payload,))
    
    time.sleep(random.uniform(0.5, 1.5))
    
    if not cfg.autocommit:
        conn.commit()


def sync_worker(i: int, cfg: Config) -> bool:
    """One connection lifecycle. True on success; False on errors."""
    try:
        conn = _connect(cfg)
        cur = conn.cursor()
        print(f"[{i}] Connected ({cfg.workload_type} workload)")

        # Select workload based on configuration
        workload_map: dict[str, Callable] = {
            "basic": workload_basic,
            "mixed": workload_mixed,
            "stress": workload_stress,
            "metadata": workload_metadata,
        }
        
        workload_func = workload_map.get(cfg.workload_type, workload_basic)
        workload_func(cur, conn, cfg)

        time.sleep(cfg.hold_time)

        cur.close()
        conn.close()
        print(f"[{i}] Closed")
        return True

    except mariadb.OperationalError as e:
        print(f"[{i}] FAILED (OperationalError): {e}", file=sys.stderr)
        return False
    except Exception as e:
        print(f"[{i}] FAILED (Unexpected): {e}", file=sys.stderr)
        return False


async def async_worker(
    i: int, cfg: Config, executor: concurrent.futures.ThreadPoolExecutor
) -> bool:
    """Offload blocking DB work to a thread."""
    loop = asyncio.get_running_loop()
    return await loop.run_in_executor(executor, sync_worker, i, cfg)


async def run_test(cfg: Config) -> tuple[int, int, float]:
    """Orchestrate workers and print summary."""
    ensure_test_table(cfg)

    print(
        f"[INIT] Starting {cfg.workers} workers | workload={cfg.workload_type} | "
        f"burst={cfg.burst_mode} | ramp={cfg.ramp_ms}ms | jitter={cfg.jitter_ms}ms | "
        f"threads={cfg.max_threads}"
    )

    executor = concurrent.futures.ThreadPoolExecutor(max_workers=cfg.max_threads)
    stop = asyncio.Event()

    def _handle_sig(signum, _frame):
        print(f"\n[CTRL] Received signal {signum}; cancelling pending tasks...")
        stop.set()

    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            signal.signal(sig, _handle_sig)
        except Exception:
            pass

    tasks: list[asyncio.Task[bool]] = []
    start = time.time()

    try:
        for i in range(cfg.workers):
            if stop.is_set():
                break
            tasks.append(asyncio.create_task(async_worker(i, cfg, executor)))
            if not cfg.burst_mode:
                base = cfg.ramp_ms / 1000.0
                jitter = (
                    (random.randint(0, cfg.jitter_ms) / 1000.0)
                    if cfg.jitter_ms > 0
                    else 0.0
                )
                await asyncio.sleep(base + jitter)
        results = await asyncio.gather(*tasks, return_exceptions=True)
    finally:
        executor.shutdown(wait=True)

    successful = 0
    failed = 0
    for r in results:
        if isinstance(r, BaseException):
            failed += 1
        elif r:
            successful += 1
        else:
            failed += 1

    duration = round(time.time() - start, 2)

    print("\n===== TEST SUMMARY =====")
    print(f"Total attempted: {len(tasks)}")
    print(f"Successful:      {successful}")
    print(f"Failed:          {failed}")
    print(f"Duration:        {duration}s")
    print("========================\n")

    return successful, failed, duration


def main() -> None:
    """
    Main entry point.
    
    Usage examples:
    
    # Basic workload (simple queries)
    python mariadb_loadtest.py --workload basic --workers 100
    
    # Mixed workload (varied operations - recommended for testing all collectors)
    python mariadb_loadtest.py --workload mixed --workers 50 --hold-time 5
    
    # Stress test (heavy load with complex queries)
    python mariadb_loadtest.py --workload stress --workers 200 --burst
    
    # Metadata locks (DDL operations and long transactions)
    python mariadb_loadtest.py --workload metadata --workers 20 --hold-time 10
    
    Metrics exercised by each workload:
    - basic:    Query response time, connections
    - mixed:    All collectors (SELECT/INSERT/UPDATE/DELETE, sorts, temp tables, slow queries)
    - stress:   InnoDB operations, buffer pool, sorts, temp tables, slow queries
    - metadata: Metadata locks, long transactions, DDL operations
    """
    cfg = parse_args()
    print("\n" + "="*60)
    print("MariaDB Load Tester & Metrics Exerciser")
    print("="*60)
    print(f"Workload type: {cfg.workload_type}")
    print(f"Workers: {cfg.workers}")
    print(f"Hold time: {cfg.hold_time}s")
    print("="*60 + "\n")
    asyncio.run(run_test(cfg))


if __name__ == "__main__":
    main()
