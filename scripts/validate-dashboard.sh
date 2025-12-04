#!/bin/bash
# Dashboard validation - ensures metrics in dashboard match actual exporter output
#
# This script:
# 1. Builds the exporter
# 2. Starts it with all collectors enabled
# 3. Scrapes actual metrics from /metrics endpoint
# 4. Compares dashboard queries against actual metrics
# 5. Reports invalid/missing metrics

set -e

DASHBOARD="grafana/dashboard.json"
EXPORTER_PORT=19307
EXPORTER_PID=""
ERRORS=0
DSN="${MARIADB_EXPORTER_DSN:-mysql://root:root@127.0.0.1/mysql}"

cleanup() {
    if [ -n "$EXPORTER_PID" ] && kill -0 "$EXPORTER_PID" 2>/dev/null; then
        echo "Stopping exporter (PID: $EXPORTER_PID)..."
        kill "$EXPORTER_PID" 2>/dev/null || true
        wait "$EXPORTER_PID" 2>/dev/null || true
    fi
    rm -f /tmp/validate_dashboard_*.txt
}
trap cleanup EXIT

echo "╔════════════════════════════════════════════════════════╗"
echo "║        Dashboard Validation - Comprehensive           ║"
echo "╚════════════════════════════════════════════════════════╝"
echo ""

# Change to project root
cd "$(dirname "$0")/.." || exit 1

# Step 1: Build exporter
echo "⚙️  Step 1: Building exporter..."
if ! cargo build --release --quiet 2>&1 | tail -5; then
    echo "❌ Build failed"
    exit 1
fi
echo "   ✅ Build successful"
echo ""

# Step 2: Start exporter with all collectors
echo "🚀 Step 2: Starting exporter with all collectors..."
echo "   DSN: ${DSN}"
echo "   Port: ${EXPORTER_PORT}"
EXPORTER_LOG="/tmp/validate_dashboard_exporter.log"
./target/release/mariadb_exporter \
    --dsn="${DSN}" \
    --port="${EXPORTER_PORT}" \
    --collector.exporter \
    --collector.tls \
    --collector.query_response_time \
    --collector.statements \
    --collector.schema \
    --collector.replication \
    --collector.locks \
    --collector.metadata \
    --collector.userstat \
    --collector.innodb \
    >"$EXPORTER_LOG" 2>&1 &
EXPORTER_PID=$!

# Wait for exporter to be ready
echo "   Waiting for exporter to start..."
for i in {1..15}; do
    if curl -s "http://127.0.0.1:${EXPORTER_PORT}/metrics" >/dev/null 2>&1; then
        echo "   ✅ Exporter running (PID: $EXPORTER_PID)"
        break
    fi
    # Check if process is still alive
    if ! kill -0 "$EXPORTER_PID" 2>/dev/null; then
        echo "   ❌ Exporter process died. Last log lines:"
        tail -20 "$EXPORTER_LOG"
        exit 1
    fi
    if [ "$i" -eq 15 ]; then
        echo "   ❌ Exporter failed to start after 15 seconds"
        exit 1
    fi
    sleep 1
done
echo ""

# Step 3: Scrape actual metrics
echo "📊 Step 3: Scraping actual exported metrics..."
curl -s "http://127.0.0.1:${EXPORTER_PORT}/metrics" 2>/dev/null | \
    grep '^mariadb_' | \
    grep -v '^#' | \
    awk '{print $1}' | \
    sed 's/{.*//' | \
    sort -u > /tmp/validate_dashboard_actual.txt

ACTUAL_COUNT=$(wc -l < /tmp/validate_dashboard_actual.txt)
echo "   ✅ Found: ${ACTUAL_COUNT} actual exported metrics"
echo ""

# Step 4: Extract dashboard metrics
echo "📋 Step 4: Extracting dashboard metrics..."
jq -r '.panels[].panels[]?.targets[]?.expr, .panels[].targets[]?.expr' "$DASHBOARD" 2>/dev/null | \
    grep -v '^null$' | \
    grep -oP '\b(mariadb_)[a-z_0-9]+' | \
    sort -u > /tmp/validate_dashboard_referenced.txt

DASHBOARD_COUNT=$(wc -l < /tmp/validate_dashboard_referenced.txt)
echo "   ✅ Found: ${DASHBOARD_COUNT} unique metrics in dashboard"
echo ""

# Step 5: Validate dashboard metrics against actual
echo "✓  Step 5: Validating dashboard metrics..."

# Define optional metrics that may not be present (plugin/config dependent)
cat > /tmp/validate_dashboard_optional.txt << 'EOF'
mariadb_metadata_lock_info_count
EOF

comm -13 /tmp/validate_dashboard_actual.txt /tmp/validate_dashboard_referenced.txt > /tmp/validate_dashboard_invalid.txt

# Separate truly invalid from optional metrics
comm -23 /tmp/validate_dashboard_invalid.txt /tmp/validate_dashboard_optional.txt > /tmp/validate_dashboard_truly_invalid.txt
comm -12 /tmp/validate_dashboard_invalid.txt /tmp/validate_dashboard_optional.txt > /tmp/validate_dashboard_optional_missing.txt

if [ -s /tmp/validate_dashboard_truly_invalid.txt ]; then
    echo "   ❌ Invalid metrics found in dashboard:"
    while IFS= read -r metric; do
        # Find which panels use this metric
        PANELS=$(jq -r --arg metric "$metric" '.panels[] | 
            select(.type == "row") | 
            .panels[] | 
            select(.targets[]?.expr? // "" | contains($metric)) | 
            "\(.title) (ID: \(.id))"' "$DASHBOARD" 2>/dev/null | sort -u)
        
        echo "      • $metric"
        if [ -n "$PANELS" ]; then
            echo "$PANELS" | sed 's/^/          → /'
        fi
        ERRORS=$((ERRORS + 1))
    done < /tmp/validate_dashboard_truly_invalid.txt
fi

if [ -s /tmp/validate_dashboard_optional_missing.txt ]; then
    echo "   ⚠️  Optional metrics (not present, requires plugins/config):"
    while IFS= read -r metric; do
        echo "      • $metric"
    done < /tmp/validate_dashboard_optional_missing.txt
fi

if [ ! -s /tmp/validate_dashboard_truly_invalid.txt ]; then
    echo "   ✅ All dashboard metrics are valid!"
fi
echo ""

# Step 6: Check for unused exported metrics
echo "📈 Step 6: Checking metric coverage..."
comm -23 /tmp/validate_dashboard_actual.txt /tmp/validate_dashboard_referenced.txt > /tmp/validate_dashboard_unused.txt
UNUSED_COUNT=$(wc -l < /tmp/validate_dashboard_unused.txt)

if [ "$UNUSED_COUNT" -gt 0 ]; then
    echo "   ⚠️  ${UNUSED_COUNT} exported metrics NOT used in dashboard:"
    head -10 /tmp/validate_dashboard_unused.txt | sed 's/^/      • /'
    if [ "$UNUSED_COUNT" -gt 10 ]; then
        echo "      ... and $((UNUSED_COUNT - 10)) more"
    fi
else
    echo "   ✅ All exported metrics are used in dashboard"
fi
echo ""

# Step 7: JSON validation
echo "🔍 Step 7: Validating JSON structure..."
if jq '.' "$DASHBOARD" >/dev/null 2>&1; then
    echo "   ✅ JSON is valid"
else
    echo "   ❌ JSON is INVALID"
    ERRORS=$((ERRORS + 1))
fi
echo ""

# Step 8: Variable validation
echo "🔧 Step 8: Checking template variables..."
jq -e '.templating.list[] | select(.name == "job")' "$DASHBOARD" >/dev/null 2>&1 && \
    echo "   ✅ Job variable exists"

INST=$(jq -r '.templating.list[] | select(.name == "instance") | .query' "$DASHBOARD" 2>/dev/null)
if echo "$INST" | grep -q 'job="\$job"'; then
    echo "   ✅ Instance depends on job"
else
    echo "   ⚠️  Instance variable may not depend on job"
fi

TOTAL=$(jq -r '.panels[].panels[]?.targets[]?.expr, .panels[].targets[]?.expr' "$DASHBOARD" 2>/dev/null | grep -vc '^null$')
WITH_JOB=$(jq -r '.panels[].panels[]?.targets[]?.expr, .panels[].targets[]?.expr' "$DASHBOARD" 2>/dev/null | grep -c 'job="\$job"' || echo 0)
echo "   ✅ ${WITH_JOB}/${TOTAL} queries use job filter"
echo ""

# Summary
echo "╔════════════════════════════════════════════════════════╗"
if [ "$ERRORS" -gt 0 ]; then
    echo "║  ❌ FAILED - ${ERRORS} validation error(s) found           ║"
    echo "╚════════════════════════════════════════════════════════╝"
    echo ""
    echo "Please fix the invalid metrics in grafana/dashboard.json"
    exit 1
else
    echo "║  ✅ PASSED - Dashboard is valid!                       ║"
    echo "╚════════════════════════════════════════════════════════╝"
    echo ""
    echo "📊 Summary:"
    echo "   • Actual metrics exported: ${ACTUAL_COUNT}"
    echo "   • Metrics used in dashboard: ${DASHBOARD_COUNT}"
    echo "   • Coverage: $((DASHBOARD_COUNT * 100 / ACTUAL_COUNT))%"
    exit 0
fi
