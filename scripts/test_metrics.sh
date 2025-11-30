#!/bin/bash
# Test script to verify mariadb_loadtest.py exercises metrics

set -e

echo "================================================"
echo "Testing mariadb_loadtest.py metric coverage"
echo "================================================"
echo ""

# Check MariaDB is running
if ! podman ps | grep -q mariadb_exporter_db; then
    echo "❌ ERROR: MariaDB container not running"
    echo "Start with: just mariadb"
    exit 1
fi
echo "✅ MariaDB container is running"

# Start exporter in background
echo "🚀 Starting exporter..."
cargo run --quiet -- \
    --dsn "mysql://root:root@127.0.0.1:3306/mysql" \
    --port 9306 \
    --collector.query_response_time \
    --collector.statements \
    --collector.schema \
    > /tmp/exporter.log 2>&1 &
EXPORTER_PID=$!

sleep 3

# Check exporter is responding
if ! curl -s http://localhost:9306/metrics > /dev/null 2>&1; then
    echo "❌ ERROR: Exporter not responding"
    kill $EXPORTER_PID 2>/dev/null || true
    exit 1
fi
echo "✅ Exporter is running (PID: $EXPORTER_PID)"

# Get baseline metrics
echo ""
echo "📊 Baseline metrics:"
BEFORE_QUERIES=$(curl -s http://localhost:9306/metrics | grep "^mariadb_global_status_queries " | awk '{print $2}')
BEFORE_SLOW=$(curl -s http://localhost:9306/metrics | grep "^mariadb_global_status_slow_queries " | awk '{print $2}')
echo "  Queries: $BEFORE_QUERIES"
echo "  Slow queries: $BEFORE_SLOW"

# Run load test (single run mode for faster testing)
echo ""
echo "🔥 Running all_metrics workload..."
cd "$(dirname "$0")"
timeout 30 python3 mariadb_loadtest.py --single-run --workload all_metrics --workers 10 --hold-time 1 > /tmp/loadtest.log 2>&1 || true

# Wait for metrics to update
sleep 2

# Get new metrics
echo ""
echo "📊 After load test:"
AFTER_QUERIES=$(curl -s http://localhost:9306/metrics | grep "^mariadb_global_status_queries " | awk '{print $2}')
AFTER_SLOW=$(curl -s http://localhost:9306/metrics | grep "^mariadb_global_status_slow_queries " | awk '{print $2}')
DIFF_QUERIES=$((AFTER_QUERIES - BEFORE_QUERIES))
DIFF_SLOW=$((AFTER_SLOW - BEFORE_SLOW))

echo "  Queries: $AFTER_QUERIES (+$DIFF_QUERIES)"
echo "  Slow queries: $AFTER_SLOW (+$DIFF_SLOW)"

# Check results
echo ""
echo "📈 Verification:"
SUCCESS=true

if [ "$DIFF_QUERIES" -gt 50 ]; then
    echo "  ✅ Queries increased by $DIFF_QUERIES (expected > 50)"
else
    echo "  ❌ Queries only increased by $DIFF_QUERIES (expected > 50)"
    SUCCESS=false
fi

if [ "$DIFF_SLOW" -gt 0 ]; then
    echo "  ✅ Slow queries increased by $DIFF_SLOW (SLEEP queries working!)"
else
    echo "  ⚠️  No slow queries detected (check long_query_time setting)"
fi

# Check metric variety
echo ""
echo "📊 Metric variety check:"
TOTAL_METRICS=$(curl -s http://localhost:9306/metrics | grep "^mariadb_" | wc -l)
COM_METRICS=$(curl -s http://localhost:9306/metrics | grep "^mariadb_global_status_com_" | wc -l)
INNODB_METRICS=$(curl -s http://localhost:9306/metrics | grep "^mariadb_global_status_innodb_" | wc -l)

echo "  Total mariadb_* metrics: $TOTAL_METRICS"
echo "  COM_* metrics: $COM_METRICS"
echo "  InnoDB metrics: $INNODB_METRICS"

if [ "$TOTAL_METRICS" -gt 100 ]; then
    echo "  ✅ Good metric coverage ($TOTAL_METRICS metrics)"
else
    echo "  ⚠️  Low metric count ($TOTAL_METRICS, expected > 100)"
fi

# Cleanup
kill $EXPORTER_PID 2>/dev/null || true
wait $EXPORTER_PID 2>/dev/null || true

echo ""
echo "================================================"
if [ "$SUCCESS" = true ]; then
    echo "✅ ALL TESTS PASSED"
    echo "The load test successfully exercises metrics!"
else
    echo "⚠️  SOME TESTS FAILED"
    echo "Check /tmp/loadtest.log and /tmp/exporter.log for details"
fi
echo "================================================"
