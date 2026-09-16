//! Env-var parsing for the CLI.
//!
//! This is its own test binary — hence its own process — so mutating the environment here
//! cannot race tests in other binaries. Everything stays inside a single `#[test]` so the
//! process can still only race itself.
use mariadb_exporter::cli::commands;
use mariadb_exporter::collectors::system::ProcessMemorySource;

/// `--scrape.timeout-ms` and `--system.process-memory` must honor
/// `MARIADB_EXPORTER_SCRAPE_TIMEOUT_MS` / `MARIADB_EXPORTER_SYSTEM_PROCESS_MEMORY`, and an
/// invalid env value must be rejected exactly as an invalid CLI value is.
#[test]
fn cli_reads_scrape_timeout_and_process_memory_from_the_environment() {
    // Unset the other MARIADB_EXPORTER_* inputs so an ambient value cannot break matching.
    temp_env::with_vars(
        [
            ("MARIADB_EXPORTER_SCRAPE_TIMEOUT_MS", Some("2500")),
            ("MARIADB_EXPORTER_SYSTEM_PROCESS_MEMORY", Some("pss")),
            ("MARIADB_EXPORTER_PORT", None),
            ("MARIADB_EXPORTER_LISTEN", None),
            ("MARIADB_EXPORTER_EXCLUDE_DATABASES", None),
        ],
        || {
            let matches =
                commands::new().try_get_matches_from(["mariadb_exporter", "--dsn", "mysql://x"]);

            assert_eq!(
                matches
                    .as_ref()
                    .ok()
                    .and_then(|m| m.get_one::<u64>("scrape.timeout-ms"))
                    .copied(),
                Some(2500),
                "MARIADB_EXPORTER_SCRAPE_TIMEOUT_MS must feed --scrape.timeout-ms"
            );
            assert_eq!(
                matches
                    .as_ref()
                    .ok()
                    .and_then(|m| m.get_one::<ProcessMemorySource>("system.process-memory"))
                    .copied(),
                Some(ProcessMemorySource::Pss),
                "MARIADB_EXPORTER_SYSTEM_PROCESS_MEMORY must feed --system.process-memory"
            );
        },
    );

    // A zero budget would make every scrape time out; the env spelling must be rejected
    // just as `--scrape.timeout-ms 0` is.
    temp_env::with_vars(
        [
            ("MARIADB_EXPORTER_SCRAPE_TIMEOUT_MS", Some("0")),
            ("MARIADB_EXPORTER_SYSTEM_PROCESS_MEMORY", None),
        ],
        || {
            assert!(
                commands::new()
                    .try_get_matches_from(["mariadb_exporter", "--dsn", "mysql://x"])
                    .is_err(),
                "MARIADB_EXPORTER_SCRAPE_TIMEOUT_MS=0 must fail matching"
            );
        },
    );

    // An unknown memory source must be rejected, not silently treated as rss.
    temp_env::with_vars(
        [
            ("MARIADB_EXPORTER_SCRAPE_TIMEOUT_MS", None),
            ("MARIADB_EXPORTER_SYSTEM_PROCESS_MEMORY", Some("junk")),
        ],
        || {
            assert!(
                commands::new()
                    .try_get_matches_from(["mariadb_exporter", "--dsn", "mysql://x"])
                    .is_err(),
                "MARIADB_EXPORTER_SYSTEM_PROCESS_MEMORY=junk must fail matching"
            );
        },
    );
}
