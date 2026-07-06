#!/usr/bin/env bash
set -euo pipefail

# Self-contained local soak test for mariadb_exporter.
#
# Drives sustained, phased load against a LOCAL MariaDB with scripts/mariadb_loadtest.py
# while periodically sampling the exporter's own /metrics endpoint (RSS, open FDs, CPU,
# scrape counters, mariadb_up). The goal is a leak/stability check: over many hours RSS
# and open FDs must stay flat/bounded and scrapes must keep succeeding.
#
# Unlike a multi-VM bakeoff, everything runs on this host:
#   MariaDB (just mariadb)  <-- exporter (:9306)  <-- this sampler
#                           <-- mariadb_loadtest.py (load)
#
# Artifacts (samples CSV + summary) are written under bench-artifacts/mariadb-soak/<RUN_ID>/.
#
# Requirements: a running MariaDB (or podman so `just mariadb` can start one), the
# `mariadb` Python connector for the load driver (pip install mariadb; needs
# libmariadb-dev), and `curl`.

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "${SCRIPT_DIR}/../.." && pwd)
LOADTEST="${REPO_ROOT}/scripts/mariadb_loadtest.py"

# --- Defaults (override via flags or env) ------------------------------------
HOURS="${HOURS:-1}"
EXPORTER_URL="${EXPORTER_URL:-http://127.0.0.1:9306}"
DB_HOST="${DB_HOST:-127.0.0.1}"
DB_PORT="${DB_PORT:-3306}"
DB_USER="${DB_USER:-root}"
DB_PASS="${DB_PASS:-root}"
DB_NAME="${DB_NAME:-soaktest}"
WORKERS="${WORKERS:-20}"
SAMPLE_INTERVAL="${SAMPLE_INTERVAL:-10}"
PHASE_SECONDS="${PHASE_SECONDS:-300}"
RUN_ID="${RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
ARTIFACT_ROOT="${ARTIFACT_ROOT:-${REPO_ROOT}/bench-artifacts/mariadb-soak}"
NO_BUILD=false

# Phases cycled for the whole soak duration: "workload:workers".
# Covers steady mixed load, heavier stress, catalog scans, timing, and the broad
# all_metrics sweep so every collector is exercised over time.
PHASES=(
    "mixed:${WORKERS}"
    "stress:${WORKERS}"
    "metadata:${WORKERS}"
    "query_response_time:${WORKERS}"
    "all_metrics:${WORKERS}"
)

usage() {
    cat <<EOF
Usage: $0 [options]

Run a self-contained local soak test that drives load with mariadb_loadtest.py and
samples the exporter's /metrics for leak/stability analysis.

Options:
  --hours N              Soak duration in hours (default: ${HOURS})
  --exporter-url URL     Exporter base URL (default: ${EXPORTER_URL})
  --db-host HOST         MariaDB host (default: ${DB_HOST})
  --db-port PORT         MariaDB port (default: ${DB_PORT})
  --db-user USER         MariaDB user (default: ${DB_USER})
  --db-pass PASS         MariaDB password (default: ${DB_PASS})
  --db-name NAME         Load-test database, created if missing (default: ${DB_NAME})
  --workers N            Load-test workers per phase (default: ${WORKERS})
  --phase-seconds N      Seconds per load phase (default: ${PHASE_SECONDS})
  --sample-interval N    Seconds between exporter samples (default: ${SAMPLE_INTERVAL})
  --run-id ID            Artifact run id (default: UTC timestamp)
  --no-build             Do not build/start the exporter; assume it is already up
  -h, --help             Show this help

Env equivalents: HOURS, EXPORTER_URL, DB_HOST, DB_PORT, DB_USER, DB_PASS, DB_NAME,
WORKERS, PHASE_SECONDS, SAMPLE_INTERVAL, RUN_ID, ARTIFACT_ROOT.
EOF
}

log() { printf '\033[0;34m[%s]\033[0m %s\n' "$(date -u +%H:%M:%S)" "$*"; }
err() { printf '\033[0;31m[%s] ERROR:\033[0m %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; }

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
        --hours) HOURS="$2"; shift 2 ;;
        --exporter-url) EXPORTER_URL="$2"; shift 2 ;;
        --db-host) DB_HOST="$2"; shift 2 ;;
        --db-port) DB_PORT="$2"; shift 2 ;;
        --db-user) DB_USER="$2"; shift 2 ;;
        --db-pass) DB_PASS="$2"; shift 2 ;;
        --db-name) DB_NAME="$2"; shift 2 ;;
        --workers) WORKERS="$2"; shift 2 ;;
        --phase-seconds) PHASE_SECONDS="$2"; shift 2 ;;
        --sample-interval) SAMPLE_INTERVAL="$2"; shift 2 ;;
        --run-id) RUN_ID="$2"; shift 2 ;;
        --no-build) NO_BUILD=true; shift ;;
        -h | --help) usage; exit 0 ;;
        *) err "Unknown option: $1"; usage; exit 1 ;;
        esac
    done
    # Recompute PHASES if WORKERS changed via flag.
    PHASES=(
        "mixed:${WORKERS}"
        "stress:${WORKERS}"
        "metadata:${WORKERS}"
        "query_response_time:${WORKERS}"
        "all_metrics:${WORKERS}"
    )
}

# --- State for cleanup -------------------------------------------------------
STARTED_EXPORTER=false
EXPORTER_PID=""
SAMPLER_PID=""
LOADTEST_PID=""
ARTIFACT_DIR=""

cleanup() {
    trap - EXIT INT TERM
    log "Cleaning up..."
    [[ -n "$LOADTEST_PID" ]] && kill "$LOADTEST_PID" 2>/dev/null || true
    [[ -n "$SAMPLER_PID" ]] && kill "$SAMPLER_PID" 2>/dev/null || true
    if [[ "$STARTED_EXPORTER" == true && -n "$EXPORTER_PID" ]]; then
        log "Stopping exporter we started (PID ${EXPORTER_PID})..."
        kill "$EXPORTER_PID" 2>/dev/null || true
        wait "$EXPORTER_PID" 2>/dev/null || true
    fi
    if [[ -n "$ARTIFACT_DIR" && -f "${ARTIFACT_DIR}/samples.csv" ]]; then
        write_summary || true
        if [[ -x "${SCRIPT_DIR}/check-soak.sh" ]]; then
            "${SCRIPT_DIR}/check-soak.sh" --run-dir "$ARTIFACT_DIR" || true
        fi
    fi
}

metrics_text() { curl -fsS --max-time 5 "${EXPORTER_URL}/metrics" 2>/dev/null || true; }

# Extract a single-valued metric line "name value" -> value (empty if absent).
metric_value() {
    local name="$1" text="$2"
    printf '%s\n' "$text" | awk -v n="$name" '$1==n {print $2; exit}'
}

preflight() {
    command -v curl >/dev/null 2>&1 || { err "curl is required"; exit 1; }
    [[ -f "$LOADTEST" ]] || { err "load driver not found: $LOADTEST"; exit 1; }

    if ! command -v python3 >/dev/null 2>&1; then
        err "python3 is required for mariadb_loadtest.py"
        exit 1
    fi
    if ! python3 -c "import mariadb" >/dev/null 2>&1; then
        err "Python 'mariadb' connector not installed (required by mariadb_loadtest.py)."
        err "Install it, e.g.:  sudo apt-get install -y libmariadb-dev && pip install mariadb"
        exit 1
    fi

    # MariaDB must be reachable; try to start it via just if a container runtime exists.
    if ! mariadb-admin ping -h "$DB_HOST" -P "$DB_PORT" -u"$DB_USER" -p"$DB_PASS" --silent >/dev/null 2>&1; then
        if command -v just >/dev/null 2>&1 && command -v podman >/dev/null 2>&1; then
            log "MariaDB not reachable; starting it with 'just mariadb'..."
            (cd "$REPO_ROOT" && just mariadb) || true
            timeout 40 bash -c "until mariadb-admin ping -h '$DB_HOST' -P '$DB_PORT' -u'$DB_USER' -p'$DB_PASS' --silent >/dev/null 2>&1; do sleep 1; done" ||
                { err "MariaDB did not become reachable on ${DB_HOST}:${DB_PORT}"; exit 1; }
        else
            err "MariaDB not reachable on ${DB_HOST}:${DB_PORT} and cannot auto-start (need just + podman)."
            exit 1
        fi
    fi
    log "MariaDB reachable on ${DB_HOST}:${DB_PORT}."

    # Ensure the load-test database exists.
    mariadb -h "$DB_HOST" -P "$DB_PORT" -u"$DB_USER" -p"$DB_PASS" \
        -e "CREATE DATABASE IF NOT EXISTS \`${DB_NAME}\`" >/dev/null 2>&1 ||
        { err "could not create database ${DB_NAME}"; exit 1; }
    log "Load-test database '${DB_NAME}' ready."
}

ensure_exporter() {
    if [[ -n "$(metrics_text)" ]]; then
        log "Exporter already responding at ${EXPORTER_URL}."
        return
    fi
    if [[ "$NO_BUILD" == true ]]; then
        err "Exporter not responding at ${EXPORTER_URL} and --no-build was set."
        exit 1
    fi
    log "Exporter not responding; building release binary..."
    (cd "$REPO_ROOT" && cargo build --release --quiet)
    local bin="${REPO_ROOT}/target/release/mariadb_exporter"
    [[ -x "$bin" ]] || { err "built binary not found: $bin"; exit 1; }

    local port
    port="$(printf '%s\n' "$EXPORTER_URL" | sed -E 's#.*:([0-9]+).*#\1#')"
    log "Starting exporter on port ${port} (broad collector set)..."
    MARIADB_EXPORTER_DSN="mysql://${DB_USER}:${DB_PASS}@${DB_HOST}:${DB_PORT}/mysql" \
        "$bin" --port "$port" \
        --collector.default --collector.exporter --collector.innodb \
        --collector.replication --collector.locks --collector.metadata \
        --collector.schema --collector.userstat --collector.query_response_time \
        --collector.statements --collector.tls \
        >"${ARTIFACT_DIR}/exporter.log" 2>&1 &
    EXPORTER_PID=$!
    STARTED_EXPORTER=true
    timeout 30 bash -c "until curl -fsS --max-time 3 '${EXPORTER_URL}/metrics' >/dev/null 2>&1; do sleep 1; done" ||
        { err "exporter did not start responding; see ${ARTIFACT_DIR}/exporter.log"; exit 1; }
    log "Exporter up (PID ${EXPORTER_PID})."
}

start_sampler() {
    local csv="${ARTIFACT_DIR}/samples.csv"
    echo "unix,iso,rss_bytes,vsz_bytes,open_fds,cpu_percent,scrapes_total,metrics_total,scrape_errors_total,mariadb_up,uptime_seconds" >"$csv"
    (
        while true; do
            local text now iso rss vsz fds cpu scrapes mtot up start_t errsum uptime
            text="$(metrics_text)"
            now="$(date -u +%s)"
            iso="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
            rss="$(metric_value mariadb_exporter_process_resident_memory_bytes "$text")"
            vsz="$(metric_value mariadb_exporter_process_virtual_memory_bytes "$text")"
            fds="$(metric_value mariadb_exporter_process_open_fds "$text")"
            cpu="$(metric_value mariadb_exporter_process_cpu_percent "$text")"
            scrapes="$(metric_value mariadb_exporter_scrapes_total "$text")"
            mtot="$(metric_value mariadb_exporter_metrics_total "$text")"
            up="$(metric_value mariadb_up "$text")"
            start_t="$(metric_value mariadb_exporter_process_start_time_seconds "$text")"
            # Sum scrape errors across all collectors.
            errsum="$(printf '%s\n' "$text" | awk '/^mariadb_exporter_collector_scrape_errors_total/ {s+=$2} END {printf "%d", s+0}')"
            uptime=""
            if [[ -n "$start_t" ]]; then
                uptime="$(awk -v n="$now" -v s="$start_t" 'BEGIN {printf "%d", n - s}')"
            fi
            echo "${now},${iso},${rss:-},${vsz:-},${fds:-},${cpu:-},${scrapes:-},${mtot:-},${errsum:-},${up:-},${uptime:-}" >>"$csv"
            sleep "$SAMPLE_INTERVAL"
        done
    ) &
    SAMPLER_PID=$!
    log "Sampler started (PID ${SAMPLER_PID}) -> ${csv}"
}

run_phases() {
    local end_ts=$(( $(date -u +%s) + HOURS * 3600 ))
    local i=0
    while [[ "$(date -u +%s)" -lt "$end_ts" ]]; do
        local phase="${PHASES[$(( i % ${#PHASES[@]} ))]}"
        local workload="${phase%%:*}"
        local workers="${phase##*:}"
        local remaining=$(( end_ts - $(date -u +%s) ))
        local dur="$PHASE_SECONDS"
        [[ "$remaining" -lt "$dur" ]] && dur="$remaining"
        [[ "$dur" -lt 5 ]] && break

        log "Phase $((i + 1)): workload=${workload} workers=${workers} duration=${dur}s (soak ends $(date -u -d "@${end_ts}" +%H:%M:%SZ 2>/dev/null || echo "@${end_ts}"))"
        python3 "$LOADTEST" \
            --host "$DB_HOST" --port "$DB_PORT" --user "$DB_USER" --password "$DB_PASS" \
            --database "$DB_NAME" --workload "$workload" --workers "$workers" \
            --duration "$dur" >>"${ARTIFACT_DIR}/loadtest.log" 2>&1 &
        LOADTEST_PID=$!
        wait "$LOADTEST_PID" 2>/dev/null || log "load phase exited non-zero (continuing)"
        LOADTEST_PID=""
        i=$((i + 1))
    done
    log "All soak phases complete (${i} phases over ~${HOURS}h)."
}

write_summary() {
    local csv="${ARTIFACT_DIR}/samples.csv"
    local summary="${ARTIFACT_DIR}/summary.txt"
    [[ -f "$csv" ]] || return 0
    awk -F, 'NR>1 && $3!="" {
        n++
        rss=$3; if (rss_min==""||rss<rss_min) rss_min=rss; if (rss>rss_max) rss_max=rss; rss_sum+=rss
        f=$5; if (f>fds_max) fds_max=f
        e=$9+0; err_last=e
        up=$10; if (up=="0") down++
        last_uptime=$11
    } END {
        if (n==0) { print "no samples"; exit }
        printf "samples: %d\n", n
        printf "rss_bytes: min=%d max=%d avg=%d (max=%.1f MB)\n", rss_min, rss_max, rss_sum/n, rss_max/1048576
        printf "open_fds: max=%d\n", fds_max
        printf "scrape_errors_total (final): %d\n", err_last
        printf "mariadb_up==0 samples: %d\n", down+0
        printf "exporter uptime (final, s): %s\n", last_uptime
    }' "$csv" | tee "$summary"
}

main() {
    parse_args "$@"
    ARTIFACT_DIR="${ARTIFACT_ROOT}/${RUN_ID}"
    mkdir -p "$ARTIFACT_DIR"
    trap cleanup EXIT INT TERM

    log "mariadb_exporter local soak — RUN_ID=${RUN_ID}, ${HOURS}h"
    log "Artifacts: ${ARTIFACT_DIR}"
    preflight
    ensure_exporter
    start_sampler
    run_phases
    # Give the sampler one more tick, then cleanup (which writes the summary).
    sleep "$SAMPLE_INTERVAL"
}

main "$@"
