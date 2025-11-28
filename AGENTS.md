# Repository Guidelines

## Project Structure & Module Organization
- Core library in `src/` with `collectors/` housing one folder per Prometheus collector; registration lives in `collectors/mod.rs`.
- CLI entry points in `bin/` and `cli/`; HTTP/exporter server wiring under `exporter/`.
- Integration tests in `tests/`; DB fixtures and configs under `db/`.
- Dashboards and validation scripts live in `grafana/` and `scripts/`; container setups in `Containerfile*` and `contrib/`.

## Build, Test, and Development Commands
- `cargo build` – compile the exporter.
- `cargo test` – run unit/integration tests (uses `MARIADB_EXPORTER_DSN` if set).
- `just test` – lint (`clippy`), fmt check, start MariaDB via podman if needed, then run tests against `mysql://root:root@127.0.0.1:3306/mysql`.
- `cargo clippy --all-targets --all-features` – lint; required before commits.
- `cargo fmt --all -- --check` – enforce formatting.
- `just validate-dashboard` – validate Grafana JSON before pushing.

## Coding Style & Naming Conventions
- Rust 2021 defaults: 4-space indentation, `snake_case` modules/functions, `CamelCase` types, `SCREAMING_SNAKE_CASE` consts.
- Clippy denies panics and unchecked access (`unwrap_used`, `expect_used`, `panic`, `indexing_slicing`, `await_holding_lock`). Prefer `?`, `.get()`, and pattern matching for fallible operations.
- Keep collectors modular: one collector per folder with its own `mod.rs` and registration entry.
- Favor small, single-purpose functions; prefer explicit error types over `Box<dyn Error>`.

## Testing Guidelines
- Unit/integration tests use `cargo test`; set `MARIADB_EXPORTER_DSN` for custom targets.
- For container-backed verification, use `just test` (podman required). Clean up with `just stop-containers`.
- Name test files descriptively in `tests/`; use `#[tokio::test]` for async paths.
- Coverage: `just coverage` (grcov) if you need an HTML report.

## Commit & Pull Request Guidelines
- Commit messages: short, imperative, and specific (e.g., `Fix TLS collector handshake timeout`). Keep related changes in one commit.
- Before opening a PR: run `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features`, `cargo test`, and `just validate-dashboard` if dashboards changed.
- PRs should describe the change, note collector toggles/defaults touched, include test evidence/commands run, and mention any DSN or container prerequisites.

## Security & Configuration Tips
- Prefer Unix socket DSNs for local testing: `mysql:///mysql?socket=/var/run/mysqld/mysqld.sock&user=exporter`.
- Limit privileges when creating test users; reuse `scripts/setup-exporter-user.sql` when applicable.
- When adding collectors, ensure optional paths fail closed (no panic) when prerequisites are missing.
