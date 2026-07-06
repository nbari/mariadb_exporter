#!/usr/bin/env bash
set -euo pipefail

# Inspect a local mariadb_exporter soak run (leak/stability snapshot).
#
# Two modes:
#   --run-dir DIR      Analyze bench-artifacts/mariadb-soak/<RUN_ID>/samples.csv
#                      (RSS trend, open-FD ceiling, scrape errors, mariadb_up gaps).
#   --exporter-url URL Take a live one-shot snapshot from the running exporter.
#
# With no arguments it analyzes the most recent run under bench-artifacts/mariadb-soak.

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "${SCRIPT_DIR}/../.." && pwd)
ARTIFACT_ROOT="${ARTIFACT_ROOT:-${REPO_ROOT}/bench-artifacts/mariadb-soak}"

RUN_DIR=""
EXPORTER_URL=""

usage() {
    cat <<EOF
Usage: $0 [--run-dir DIR | --exporter-url URL]

  --run-dir DIR       Analyze a soak run's samples.csv (default: most recent run)
  --exporter-url URL  Live one-shot snapshot from a running exporter
  -h, --help          Show this help
EOF
}

err() { printf '\033[0;31mERROR:\033[0m %s\n' "$*" >&2; }

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
        --run-dir) RUN_DIR="$2"; shift 2 ;;
        --exporter-url) EXPORTER_URL="$2"; shift 2 ;;
        -h | --help) usage; exit 0 ;;
        *) err "Unknown option: $1"; usage; exit 1 ;;
        esac
    done
}

metric_value() {
    local name="$1" text="$2"
    printf '%s\n' "$text" | awk -v n="$name" '$1==n {print $2; exit}'
}

live_snapshot() {
    local url="$1" text
    text="$(curl -fsS --max-time 5 "${url}/metrics" 2>/dev/null || true)"
    if [[ -z "$text" ]]; then
        err "no metrics from ${url}/metrics"
        exit 1
    fi
    local rss fds cpu scrapes up start now uptime errsum
    rss="$(metric_value mariadb_exporter_process_resident_memory_bytes "$text")"
    fds="$(metric_value mariadb_exporter_process_open_fds "$text")"
    cpu="$(metric_value mariadb_exporter_process_cpu_percent "$text")"
    scrapes="$(metric_value mariadb_exporter_scrapes_total "$text")"
    up="$(metric_value mariadb_up "$text")"
    start="$(metric_value mariadb_exporter_process_start_time_seconds "$text")"
    errsum="$(printf '%s\n' "$text" | awk '/^mariadb_exporter_collector_scrape_errors_total/ {s+=$2} END {printf "%d", s+0}')"
    now="$(date -u +%s)"
    uptime="n/a"
    [[ -n "$start" ]] && uptime="$(awk -v n="$now" -v s="$start" 'BEGIN {printf "%d", n - s}')s"

    echo "== live snapshot (${url}) =="
    printf "  mariadb_up:            %s\n" "${up:-?}"
    printf "  RSS:                   %.1f MB\n" "$(awk -v r="${rss:-0}" 'BEGIN{print r/1048576}')"
    printf "  open_fds:              %s\n" "${fds:-?}"
    printf "  cpu_percent:           %s\n" "${cpu:-?}"
    printf "  scrapes_total:         %s\n" "${scrapes:-?}"
    printf "  scrape_errors_total:   %s\n" "${errsum:-?}"
    printf "  uptime:                %s\n" "$uptime"
}

analyze_run() {
    local dir="$1"
    local csv="${dir}/samples.csv"
    [[ -f "$csv" ]] || { err "samples.csv not found in ${dir}"; exit 1; }

    echo "== soak analysis: ${dir} =="
    awk -F, '
    NR>1 && $3!="" {
        n++
        rss=$3
        if (rss_min==""||rss<rss_min) rss_min=rss
        if (rss>rss_max) rss_max=rss
        rss_sum+=rss
        if (n==1) rss_first=rss
        rss_last=rss
        if ($5>fds_max) fds_max=$5
        err_last=$9+0
        if ($10=="0") down++
        uptime_last=$11
    }
    END {
        if (n==0) { print "  no samples yet"; exit }
        drift = rss_last - rss_first
        printf "  samples:               %d\n", n
        printf "  RSS min/avg/max:       %.1f / %.1f / %.1f MB\n", rss_min/1048576, (rss_sum/n)/1048576, rss_max/1048576
        printf "  RSS first->last:       %.1f -> %.1f MB (drift %+.1f MB)\n", rss_first/1048576, rss_last/1048576, drift/1048576
        printf "  open_fds max:          %d\n", fds_max
        printf "  scrape_errors (final): %d\n", err_last
        printf "  mariadb_up==0 samples: %d\n", down+0
        printf "  exporter uptime (s):   %s\n", uptime_last
        print  ""
        # Heuristic verdict: flag sustained RSS growth or fd growth.
        verdict="OK"
        if (drift > 0.25*rss_first && drift/1048576 > 20) verdict="WARN: RSS grew notably (possible leak)"
        if (err_last > 0) verdict=verdict "  | scrape errors present"
        if (down > 0) verdict=verdict "  | mariadb_up dropped during run"
        printf "  verdict: %s\n", verdict
    }' "$csv"
}

main() {
    parse_args "$@"

    if [[ -n "$EXPORTER_URL" ]]; then
        live_snapshot "$EXPORTER_URL"
        exit 0
    fi

    if [[ -z "$RUN_DIR" ]]; then
        RUN_DIR="$(ls -1dt "${ARTIFACT_ROOT}"/*/ 2>/dev/null | head -1 || true)"
        [[ -n "$RUN_DIR" ]] || { err "no runs under ${ARTIFACT_ROOT}; pass --run-dir or --exporter-url"; exit 1; }
    fi
    analyze_run "${RUN_DIR%/}"
}

main "$@"
