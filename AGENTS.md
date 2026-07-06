# Repository Guidelines

## Project Structure & Module Organization
- Core library in `src/` with `collectors/` housing one folder per Prometheus collector; registration lives in `collectors/mod.rs`.
- CLI entry points in `bin/` and `cli/`; HTTP/exporter server wiring under `exporter/`.
- Integration tests in `tests/`; DB fixtures and configs under `db/`.
- Dashboards and validation scripts live in `grafana/` and `scripts/`; container setups in `Containerfile*` and `contrib/`.

## Build, Test, and Development Commands
- `cargo build` – compile the exporter.
- `cargo test` – run all tests (unit, integration, collectors, exporter, dashboard).
- `cargo test --test collectors_tests` – run comprehensive collector integration tests.
- `cargo test --test exporter` – run HTTP server and binding tests.
- `cargo test --test dashboard_comprehensive` – validate dashboard metrics coverage.
- `just test` – lint (`clippy`), fmt check, start MariaDB via podman if needed, then run all tests against `mysql://root:root@127.0.0.1:3306/mysql`.
- `cargo clippy --all-targets --all-features` – lint; required before commits.
- `cargo fmt --all -- --check` – enforce formatting.
- `just validate-dashboard` – validate Grafana JSON before pushing.
- `scripts/dev-up` – start the compose-based [Dev Container](.devcontainer/README.md) (Rust + MariaDB, optional Prometheus + Grafana); run `just test` inside.
- `just metrics-dev` / `just metrics-dev-stop` – on-demand Prometheus + Grafana for the devcontainer (scrapes `app:9306`, hot-reloads `grafana/dashboard.json`).
- `scripts/benchmark/run-soak.sh` – self-contained local soak/leak test driven by `scripts/mariadb_loadtest.py`; inspect with `scripts/benchmark/check-soak.sh`.
- `scripts/install-mariadb-client.sh` – install the `mariadb`/`mariadb-admin` client used by the helper scripts.

## Coding Style & Naming Conventions
- Rust 2021 defaults: 4-space indentation, `snake_case` modules/functions, `CamelCase` types, `SCREAMING_SNAKE_CASE` consts.
- Clippy denies panics and unchecked access (`unwrap_used`, `expect_used`, `panic`, `indexing_slicing`, `await_holding_lock`). Prefer `?`, `.get()`, and pattern matching for fallible operations.
- Keep collectors modular: one collector per folder with its own `mod.rs` and registration entry.
- Favor small, single-purpose functions; prefer explicit error types over `Box<dyn Error>`.

## Testing Guidelines
- Comprehensive collector tests in `tests/collectors/` mirror `src/collectors/` structure.
- Each collector MUST have: registration test, collection test, feature availability test, edge case test.
- Tests use `cargo test`; set `MARIADB_EXPORTER_DSN` for custom targets (defaults to `mysql://root:root@127.0.0.1:3306/mysql`).
- For container-backed verification, use `just test` (podman required). Clean up with `just stop-containers`.
- See `tests/TESTING.md` for detailed testing philosophy and patterns.
- HTTP server tests in `tests/exporter.rs` validate bind addresses, endpoints, and startup/shutdown.
- Dashboard tests in `tests/dashboard_comprehensive.rs` ensure Grafana dashboards use all collector metrics.
- Coverage: `just coverage` (grcov) if you need an HTML report.

## Commit & Pull Request Guidelines
- Commit messages: short, imperative, and specific (e.g., `Fix TLS collector handshake timeout`). Keep related changes in one commit.
- Before opening a PR: run `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features`, `cargo test`, and `just validate-dashboard` if dashboards changed.
- PRs should describe the change, note collector toggles/defaults touched, include test evidence/commands run, and mention any DSN or container prerequisites.

## Security & Configuration Tips
- Prefer Unix socket DSNs for local testing: `mysql:///mysql?socket=/var/run/mysqld/mysqld.sock&user=exporter`.
- Limit privileges when creating test users; reuse `scripts/setup-exporter-user.sql` when applicable.
- When adding collectors, ensure optional paths fail closed (no panic) when prerequisites are missing.

## Dev Container & Local Tooling
- A compose-based dev container lives in `.devcontainer/` (Rust `app` + `mariadb` service, plus an optional `observability` profile with Prometheus + Grafana). Start it with `scripts/dev-up` (DevPod) and enter with `scripts/dev-ssh`; inside, `just test` runs against the `mariadb` service with no host database. See [`.devcontainer/README.md`](.devcontainer/README.md).
- The `just test` recipe is devcontainer-aware: it uses an already-reachable MariaDB at `MARIADB_HOST:MARIADB_PORT` (default `127.0.0.1:3306`) and only starts a podman container when none is reachable. It honors a pre-set `MARIADB_EXPORTER_DSN`, falling back to the local default when unset.
- Connection model: MariaDB reads every schema from a **single shared `MySqlPool`** via `information_schema`/`performance_schema`. There is no per-database/per-schema connection fan-out. For the rare per-database query, use the ephemeral `util::open_db_connection` (closed on drop, never cached) — do not add a per-database connection/pool cache. This invariant is locked by `tests/collectors/connection.rs`.
- `scripts/benchmark/` holds a self-contained local soak harness (`run-soak.sh` + `check-soak.sh` + `soak-dashboard.json`) driven by `scripts/mariadb_loadtest.py`; it samples the exporter's own `mariadb_exporter_process_*` metrics to catch RSS/FD leaks.
- Contributor workflow, safe-coding rules, and the pre-commit hook are documented in [`CONTRIBUTING.md`](CONTRIBUTING.md) and [`.github/copilot-instructions.md`](.github/copilot-instructions.md).
- The release flow signs tags (`git tag -s`); do not bypass configured commit/tag signing with `--no-gpg-sign`.
