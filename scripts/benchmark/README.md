# mariadb_exporter — local soak / benchmark harness

A **self-contained** soak test for leak and stability verification. Everything runs
on one host: a local MariaDB, the exporter, a load driver
([`../mariadb_loadtest.py`](../mariadb_loadtest.py)), and a sampler that records the
exporter's own `/metrics` over time.

The point is a long-running check that the exporter's footprint stays flat under
sustained, varied load:

- **RSS** (`mariadb_exporter_process_resident_memory_bytes`) stays flat → no memory leak.
- **Open FDs** (`mariadb_exporter_process_open_fds`) stays bounded → no fd/connection leak.
- **Scrapes keep succeeding** (`mariadb_up`, `mariadb_exporter_collector_scrape_errors_total`).

## Requirements

- A running MariaDB, or `podman` + `just` so the harness can start one (`just mariadb`).
- `curl`, `python3`, and the Python **`mariadb`** connector for the load driver:
  ```bash
  sudo apt-get install -y libmariadb-dev
  pip install mariadb
  ```

## Run

```bash
# 1 hour soak against a local MariaDB, auto-building/starting the exporter on :9306
scripts/benchmark/run-soak.sh --hours 1

# Point at an already-running exporter and DB, longer run
scripts/benchmark/run-soak.sh --hours 12 \
  --exporter-url http://127.0.0.1:9306 \
  --db-host 127.0.0.1 --db-port 3306 --db-user root --db-pass root \
  --workers 40 --no-build
```

The harness:

1. Ensures MariaDB is reachable (starts it via `just mariadb` if needed) and creates
   the load-test database.
2. Ensures the exporter responds at `--exporter-url` (builds + starts it if not, and
   stops it again at the end).
3. Starts a background sampler that appends a row to `samples.csv` every
   `--sample-interval` seconds.
4. Cycles load **phases** (`mixed → stress → metadata → query_response_time →
   all_metrics`) via `mariadb_loadtest.py` until the duration elapses, so every
   collector is exercised over time.
5. On exit writes `summary.txt` and prints a `check-soak.sh` verdict.

Artifacts land under `bench-artifacts/mariadb-soak/<RUN_ID>/`:

| File | Contents |
| --- | --- |
| `samples.csv` | Per-sample RSS, VSZ, open FDs, CPU, scrape/metric counters, `mariadb_up`, uptime. |
| `summary.txt` | Min/avg/max RSS, max FDs, final scrape errors, `mariadb_up` gaps. |
| `exporter.log` | Exporter stdout/stderr (only when the harness started it). |
| `loadtest.log` | Load driver output across all phases. |

## Inspect

```bash
# Analyze the most recent run (RSS drift, FD ceiling, errors, verdict)
scripts/benchmark/check-soak.sh

# Analyze a specific run
scripts/benchmark/check-soak.sh --run-dir bench-artifacts/mariadb-soak/<RUN_ID>

# Live one-shot snapshot from a running exporter
scripts/benchmark/check-soak.sh --exporter-url http://127.0.0.1:9306
```

## Dashboard

`soak-dashboard.json` is a Grafana dashboard for the soak self-metrics (RSS, open
FDs, CPU, scrape rate, scrape duration and errors by collector). Import it into
Grafana (it prompts for a Prometheus datasource), or drop it into the devcontainer
observability stack (`just metrics-dev`) alongside the main dashboard.
