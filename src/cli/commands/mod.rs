use crate::collectors::system::ProcessMemorySource;
use clap::{
    Arg, ArgAction, ColorChoice, Command,
    builder::styling::{AnsiColor, Effects, Styles},
};

mod collectors;

pub mod built_info {
    #![allow(clippy::doc_markdown)]
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

/// CLI spelling of [`DEFAULT_SCRAPE_TIMEOUT_MS`]. Kept in sync by
/// `scrape_timeout_default_matches_const`.
const SCRAPE_TIMEOUT_MS_DEFAULT: &str = "15000";

/// Flags that shape how a scrape is executed, rather than what it collects.
fn scrape_runtime_args(cmd: Command) -> Command {
    cmd.arg(
        Arg::new("scrape.timeout-ms")
            .long("scrape.timeout-ms")
            .help("Wall-clock budget for one /metrics scrape, in milliseconds")
            .long_help(
                "Wall-clock budget for one /metrics scrape, in milliseconds.\n\n\
                 A scrape that exceeds it is aborted and answered with 504 Gateway Timeout;\n\
                 aborting drops the in-flight queries so their pooled connections are\n\
                 returned instead of being parked server-side.\n\n\
                 /metrics is single-flight: while a scrape is running, another one is\n\
                 refused with 503 Service Unavailable rather than doubling the load on an\n\
                 already-slow server. The gate is released on every exit path, including\n\
                 this timeout and a client disconnect, so it cannot wedge.\n\n\
                 Set it below your Prometheus scrape_timeout so the exporter, not the\n\
                 scraper, decides when to give up. Note the shipped default does not do\n\
                 this: 15000 is above Prometheus's own 10s default, so out of the box the\n\
                 scraper disconnects first and the abort takes the client-disconnect path\n\
                 (the gate is still released, and collectors already in flight are still\n\
                 counted as aborted). Lower it below your scrape_timeout to make the\n\
                 exporter's 504 the decisive signal.",
            )
            .default_value(SCRAPE_TIMEOUT_MS_DEFAULT)
            .env("MARIADB_EXPORTER_SCRAPE_TIMEOUT_MS")
            .value_name("MS")
            .value_parser(clap::value_parser!(u64).range(1..)),
    )
    .arg(
        Arg::new("system.process-memory")
            .long("system.process-memory")
            .help("Source for the system collector's process-group memory gauge [rss|pss]")
            .long_help(
                "Where --collector.system reads MariaDB process-group memory from (Linux).\n\n\
                 - rss (default): /proc/<pid>/statm. One short, already-maintained line per\n\
                   process, so cost is O(processes). MariaDB is thread-per-connection, so a\n\
                   single mariadbd process serves every session and the InnoDB buffer pool is\n\
                   counted exactly once.\n\
                 - pss: /proc/<pid>/smaps_rollup. Divides shared pages proportionally, which\n\
                   only matters when several instances share a host. The kernel must walk\n\
                   every page-table entry of every mapping to produce it, making the cost\n\
                   O(processes x resident pages); measured at ~866x the rss reads on a large\n\
                   sibling deployment. Opt in only when you need the shared-page accounting.\n\n\
                 Ignored on FreeBSD, where only RSS is available.",
            )
            .default_value(ProcessMemorySource::default().as_str())
            .env("MARIADB_EXPORTER_SYSTEM_PROCESS_MEMORY")
            .value_name("SOURCE")
            .value_parser(ProcessMemorySource::parse),
    )
}

#[must_use]
pub fn new() -> Command {
    let styles = Styles::styled()
        .header(AnsiColor::Yellow.on_default() | Effects::BOLD)
        .usage(AnsiColor::Green.on_default() | Effects::BOLD)
        .literal(AnsiColor::Blue.on_default() | Effects::BOLD)
        .placeholder(AnsiColor::Green.on_default());

    let git_hash = built_info::GIT_COMMIT_HASH.unwrap_or("unknown");
    let long_version: &'static str =
        Box::leak(format!("{} - {}", env!("CARGO_PKG_VERSION"), git_hash).into_boxed_str());

    let cmd = Command::new("mariadb_exporter")
        .about("MariaDB metric exporter for Prometheus")
        .version(env!("CARGO_PKG_VERSION"))
        .long_version(long_version)
        .color(ColorChoice::Auto)
        .styles(styles)
        .arg(
            Arg::new("port")
                .short('p')
                .long("port")
                .help("Port to listen on")
                .default_value("9306")
                .env("MARIADB_EXPORTER_PORT")
                .value_parser(clap::value_parser!(u16)),
        )
        .arg(
            Arg::new("listen")
                .short('l')
                .long("listen")
                .help("IP address to bind to (default: [::]:port, accepts both IPv6 and IPv4)")
                .long_help(
                    "IP address to bind to:\n\
                     - Not specified (default): Binds to [::]:port which accepts both IPv6 and IPv4 connections.\n\
                       Falls back to 0.0.0.0:port if IPv6 is not available on the system.\n\
                     - Specific IPv4: e.g., '0.0.0.0', '127.0.0.1', '192.168.1.100'\n\
                     - Specific IPv6: e.g., '::', '::1', 'fe80::1'\n\n\
                     Examples:\n\
                       --listen 0.0.0.0       Bind to all IPv4 interfaces only\n\
                       --listen 127.0.0.1     Bind to localhost IPv4 only\n\
                       --listen ::            Bind to all IPv6 interfaces (typically accepts IPv4 too)\n\
                       --listen ::1           Bind to localhost IPv6 only\n\n\
                     Note: Binding to [::] (IPv6 all interfaces) usually accepts both IPv6 and\n\
                     IPv4 connections through IPv4-mapped IPv6 addresses on dual-stack systems.",
                )
                .env("MARIADB_EXPORTER_LISTEN")
                .value_name("IP"),
        )
        .arg(
            Arg::new("dsn")
                .long("dsn")
                .help("MariaDB connection string (URL format)")
                .long_help(
                    "MariaDB/MySQL connection string in URL format.\n\n\
                     Basic formats:\n\
                     - TCP: mysql://user:password@host:port/database\n\
                     - Unix socket: mysql:///database?socket=/var/run/mysqld/mysqld.sock\n\
                     - Unix socket (short): mysql:///mysql?user=exporter\n\n\
                     SSL/TLS options:\n\
                     - Require SSL: mysql://user@host/db?ssl-mode=REQUIRED\n\
                     - Verify CA: mysql://user@host/db?ssl-mode=VERIFY_CA&ssl-ca=/path/to/ca.pem\n\
                     - Verify identity: mysql://user@host/db?ssl-mode=VERIFY_IDENTITY\n\n\
                     Examples:\n\
                       --dsn mysql://root@localhost:3306/mysql\n\
                       --dsn mysql://monitor:pass@db.example.com/mysql?ssl-mode=REQUIRED\n\
                       --dsn 'mysql:///mysql?user=exporter'\n\
                       --dsn 'mysql:///mysql?socket=/var/run/mysqld/mysqld.sock&user=exporter'\n\n\
                     SSL modes: DISABLED, PREFERRED, REQUIRED, VERIFY_CA, VERIFY_IDENTITY\n\
                     See: https://mariadb.com/kb/en/using-tls-ssl-with-mariadb-connectors/"
                )
                .default_value("mysql://root@localhost:3306/mysql")
                .env("MARIADB_EXPORTER_DSN")
                .value_name("DSN"),
        )
        .arg(
            Arg::new("exclude-databases")
                .long("exclude-databases")
                .help("Comma-separated list of databases to exclude (exact/case-sensitive)")
                .env("MARIADB_EXPORTER_EXCLUDE_DATABASES")
                .value_name("information_schema,performance_schema,...")
                .value_delimiter(',') // split CLI and env values by comma
                .action(ArgAction::Append), // allow repeated flags if desired
        )
        .arg(
            Arg::new("verbose")
                .short('v')
                .long("verbose")
                .help("Increase verbosity, -vv for debug")
                .action(ArgAction::Count),
        );

    collectors::add_collectors_args(scrape_runtime_args(cmd))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clap default is a string; the registry default is a `u64`. If they ever
    /// drift, `--scrape.timeout-ms` silently stops matching the documented default.
    #[test]
    fn scrape_timeout_default_matches_const() {
        use crate::collectors::DEFAULT_SCRAPE_TIMEOUT_MS;

        assert_eq!(
            SCRAPE_TIMEOUT_MS_DEFAULT.parse::<u64>().ok(),
            Some(DEFAULT_SCRAPE_TIMEOUT_MS),
        );
    }

    #[test]
    fn scrape_timeout_parses_and_rejects_zero() {
        let matches = new()
            .try_get_matches_from([
                "mariadb_exporter",
                "--dsn",
                "mysql://x",
                "--scrape.timeout-ms",
                "250",
            ])
            .ok();
        assert_eq!(
            matches
                .as_ref()
                .and_then(|m| m.get_one::<u64>("scrape.timeout-ms"))
                .copied(),
            Some(250)
        );

        assert!(
            new()
                .try_get_matches_from([
                    "mariadb_exporter",
                    "--dsn",
                    "mysql://x",
                    "--scrape.timeout-ms",
                    "0",
                ])
                .is_err(),
            "a zero budget would make every scrape time out"
        );
    }

    #[test]
    fn system_process_memory_defaults_to_rss() {
        let matches = new()
            .try_get_matches_from(["mariadb_exporter", "--dsn", "mysql://x"])
            .ok();

        assert_eq!(
            matches
                .as_ref()
                .and_then(|m| m.get_one::<ProcessMemorySource>("system.process-memory"))
                .copied(),
            Some(ProcessMemorySource::Rss),
            "smaps_rollup is ~866x more expensive than statm; it must stay opt-in"
        );
    }

    #[test]
    fn system_process_memory_accepts_pss_and_rejects_junk() {
        let matches = new()
            .try_get_matches_from([
                "mariadb_exporter",
                "--dsn",
                "mysql://x",
                "--system.process-memory",
                "PSS",
            ])
            .ok();

        assert_eq!(
            matches
                .as_ref()
                .and_then(|m| m.get_one::<ProcessMemorySource>("system.process-memory"))
                .copied(),
            Some(ProcessMemorySource::Pss)
        );

        assert!(
            new()
                .try_get_matches_from([
                    "mariadb_exporter",
                    "--dsn",
                    "mysql://x",
                    "--system.process-memory",
                    "smaps",
                ])
                .is_err(),
            "an unknown source must be rejected, not silently treated as rss"
        );
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_defaults() {
        temp_env::with_var("MARIADB_EXPORTER_DSN", None::<String>, || {
            let command = new();
            let matches = command.get_matches_from(vec!["mariadb_exporter"]);

            assert_eq!(matches.get_one::<u16>("port").copied(), Some(9306));
            assert_eq!(
                matches
                    .get_one::<String>("dsn")
                    .map(std::string::ToString::to_string),
                Some("mysql://root@localhost:3306/mysql".to_string())
            );
        });
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_new() {
        let command = new();

        assert_eq!(command.get_name(), "mariadb_exporter");
        assert_eq!(
            command.get_about().unwrap().to_string(),
            env!("CARGO_PKG_DESCRIPTION")
        );
        assert_eq!(
            command.get_version().unwrap().to_string(),
            env!("CARGO_PKG_VERSION")
        );
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_check_port_and_dsn() {
        let command = new();
        let matches = command.get_matches_from(vec![
            "mariadb_exporter",
            "--port",
            "8080",
            "--dsn",
            "mysql://user:password@localhost:3306/mydb",
            "--exclude-databases",
            "information_schema,performance_schema",
            "--exclude-databases",
            "mysql",
        ]);

        assert_eq!(matches.get_one::<u16>("port").copied(), Some(8080));
        assert_eq!(
            matches
                .get_one::<String>("dsn")
                .map(std::string::ToString::to_string),
            Some("mysql://user:password@localhost:3306/mydb".to_string())
        );

        let excludes: Vec<String> = matches
            .get_many::<String>("exclude-databases")
            .unwrap()
            .map(std::string::ToString::to_string)
            .collect();
        assert_eq!(
            excludes,
            vec!["information_schema", "performance_schema", "mysql"]
        );
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_check_exclude_databases_env() {
        temp_env::with_var(
            "MARIADB_EXPORTER_EXCLUDE_DATABASES",
            Some("db1,db2,db3"),
            || {
                let command = new();
                let matches = command.get_matches_from(vec!["mariadb_exporter"]);

                let excludes: Vec<String> = matches
                    .get_many::<String>("exclude-databases")
                    .unwrap()
                    .map(std::string::ToString::to_string)
                    .collect();
                assert_eq!(excludes, vec!["db1", "db2", "db3"]);
            },
        );
    }

    #[test]
    fn test_verbose_flag_single() {
        let command = new();
        let matches = command.get_matches_from(vec!["mariadb_exporter", "-v"]);
        assert_eq!(matches.get_count("verbose"), 1);
    }

    #[test]
    fn test_verbose_flag_double() {
        let command = new();
        let matches = command.get_matches_from(vec!["mariadb_exporter", "-vv"]);
        assert_eq!(matches.get_count("verbose"), 2);
    }

    #[test]
    fn test_verbose_flag_triple() {
        let command = new();
        let matches = command.get_matches_from(vec!["mariadb_exporter", "-vvv"]);
        assert_eq!(matches.get_count("verbose"), 3);
    }

    #[test]
    fn test_verbose_flag_long_form() {
        let command = new();
        let matches = command.get_matches_from(vec!["mariadb_exporter", "--verbose", "--verbose"]);
        assert_eq!(matches.get_count("verbose"), 2);
    }

    #[test]
    fn test_port_short_flag() {
        let command = new();
        let matches = command.get_matches_from(vec!["mariadb_exporter", "-p", "8080"]);
        assert_eq!(matches.get_one::<u16>("port").copied(), Some(8080));
    }

    #[test]
    fn test_port_validation_min() {
        let command = new();
        let matches = command.get_matches_from(vec!["mariadb_exporter", "--port", "1"]);
        assert_eq!(matches.get_one::<u16>("port").copied(), Some(1));
    }

    #[test]
    fn test_port_validation_max() {
        let command = new();
        let matches = command.get_matches_from(vec!["mariadb_exporter", "--port", "65535"]);
        assert_eq!(matches.get_one::<u16>("port").copied(), Some(65535));
    }

    #[test]
    fn test_port_validation_invalid() {
        let command = new();
        let result = command.try_get_matches_from(vec!["mariadb_exporter", "--port", "99999"]);
        assert!(result.is_err(), "Should reject port > 65535");
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_port_validation_non_numeric() {
        let command = new();
        let result = command.try_get_matches_from(vec!["mariadb_exporter", "--port", "abc"]);
        assert!(result.is_err(), "Should reject non-numeric port");
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_port_from_env() {
        temp_env::with_var("MARIADB_EXPORTER_PORT", Some("7777"), || {
            let command = new();
            let matches = command.get_matches_from(vec!["mariadb_exporter"]);
            assert_eq!(matches.get_one::<u16>("port").copied(), Some(7777));
        });
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_port_cli_overrides_env() {
        temp_env::with_var("MARIADB_EXPORTER_PORT", Some("7777"), || {
            let command = new();
            let matches = command.get_matches_from(vec!["mariadb_exporter", "--port", "8888"]);
            assert_eq!(matches.get_one::<u16>("port").copied(), Some(8888));
        });
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_dsn_with_special_characters() {
        let command = new();
        let matches = command.get_matches_from(vec![
            "mariadb_exporter",
            "--dsn",
            "mysql://user:p@ss%20word@host:3306/db?ssl-mode=REQUIRED",
        ]);

        assert_eq!(
            matches
                .get_one::<String>("dsn")
                .map(std::string::ToString::to_string),
            Some("mysql://user:p@ss%20word@host:3306/db?ssl-mode=REQUIRED".to_string())
        );
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_dsn_from_env() {
        temp_env::with_var(
            "MARIADB_EXPORTER_DSN",
            Some("mysql://custom:3306/mydb"),
            || {
                let command = new();
                let matches = command.get_matches_from(vec!["mariadb_exporter"]);

                assert_eq!(
                    matches
                        .get_one::<String>("dsn")
                        .map(std::string::ToString::to_string),
                    Some("mysql://custom:3306/mydb".to_string())
                );
            },
        );
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_dsn_cli_overrides_env() {
        temp_env::with_var("MARIADB_EXPORTER_DSN", Some("mysql://env:3306/db"), || {
            let command = new();
            let matches =
                command.get_matches_from(vec!["mariadb_exporter", "--dsn", "mysql://cli:3306/db"]);

            assert_eq!(
                matches
                    .get_one::<String>("dsn")
                    .map(std::string::ToString::to_string),
                Some("mysql://cli:3306/db".to_string())
            );
        });
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_exclude_databases_multiple_flags() {
        let command = new();
        let matches = command.get_matches_from(vec![
            "mariadb_exporter",
            "--exclude-databases",
            "db1",
            "--exclude-databases",
            "db2",
            "--exclude-databases",
            "db3",
        ]);

        let excludes: Vec<String> = matches
            .get_many::<String>("exclude-databases")
            .unwrap()
            .map(std::string::ToString::to_string)
            .collect();

        assert_eq!(excludes, vec!["db1", "db2", "db3"]);
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_exclude_databases_comma_separated_single_flag() {
        let command = new();
        let matches = command.get_matches_from(vec![
            "mariadb_exporter",
            "--exclude-databases",
            "db1,db2,db3",
        ]);

        let excludes: Vec<String> = matches
            .get_many::<String>("exclude-databases")
            .unwrap()
            .map(std::string::ToString::to_string)
            .collect();

        assert_eq!(excludes, vec!["db1", "db2", "db3"]);
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_exclude_databases_with_spaces() {
        let command = new();
        let matches = command.get_matches_from(vec![
            "mariadb_exporter",
            "--exclude-databases",
            " db1 , db2 , db3 ",
        ]);

        let excludes: Vec<String> = matches
            .get_many::<String>("exclude-databases")
            .unwrap()
            .map(|s| s.trim().to_string())
            .collect();

        assert_eq!(excludes, vec!["db1", "db2", "db3"]);
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_exclude_databases_mixed_flags_and_commas() {
        let command = new();
        let matches = command.get_matches_from(vec![
            "mariadb_exporter",
            "--exclude-databases",
            "db1,db2",
            "--exclude-databases",
            "db3",
        ]);

        let excludes: Vec<String> = matches
            .get_many::<String>("exclude-databases")
            .unwrap()
            .map(std::string::ToString::to_string)
            .collect();

        assert_eq!(excludes, vec!["db1", "db2", "db3"]);
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_long_version_includes_git_hash() {
        let command = new();
        let long_version = command.get_long_version().unwrap().to_string();

        // Should include version and git hash separated by " - "
        assert!(long_version.contains(env!("CARGO_PKG_VERSION")));
        assert!(long_version.contains(" - "));
    }

    #[test]
    fn test_command_name() {
        let command = new();
        assert_eq!(command.get_name(), "mariadb_exporter");
    }

    #[test]
    fn test_command_has_port_argument() {
        let command = new();
        let port_arg = command.get_arguments().find(|arg| arg.get_id() == "port");
        assert!(port_arg.is_some(), "Command should have 'port' argument");
    }

    #[test]
    fn test_command_has_dsn_argument() {
        let command = new();
        let dsn_arg = command.get_arguments().find(|arg| arg.get_id() == "dsn");
        assert!(dsn_arg.is_some(), "Command should have 'dsn' argument");
    }

    #[test]
    fn test_command_has_verbose_argument() {
        let command = new();
        let verbose_arg = command
            .get_arguments()
            .find(|arg| arg.get_id() == "verbose");
        assert!(
            verbose_arg.is_some(),
            "Command should have 'verbose' argument"
        );
    }

    #[test]
    fn test_command_has_exclude_databases_argument() {
        let command = new();
        let exclude_arg = command
            .get_arguments()
            .find(|arg| arg.get_id() == "exclude-databases");
        assert!(
            exclude_arg.is_some(),
            "Command should have 'exclude-databases' argument"
        );
    }

    #[test]
    fn test_listen_default() {
        temp_env::with_var("MARIADB_EXPORTER_LISTEN", None::<String>, || {
            let command = new();
            let matches = command.get_matches_from(vec!["mariadb_exporter"]);
            assert_eq!(matches.get_one::<String>("listen"), None);
        });
    }

    #[test]
    fn test_listen_ipv4_all() {
        let command = new();
        let matches = command.get_matches_from(vec!["mariadb_exporter", "--listen", "0.0.0.0"]);
        assert_eq!(
            matches
                .get_one::<String>("listen")
                .map(std::string::String::as_str),
            Some("0.0.0.0")
        );
    }

    #[test]
    fn test_listen_ipv4_localhost() {
        let command = new();
        let matches = command.get_matches_from(vec!["mariadb_exporter", "--listen", "127.0.0.1"]);
        assert_eq!(
            matches
                .get_one::<String>("listen")
                .map(std::string::String::as_str),
            Some("127.0.0.1")
        );
    }

    #[test]
    fn test_listen_ipv4_specific() {
        let command = new();
        let matches =
            command.get_matches_from(vec!["mariadb_exporter", "--listen", "192.168.1.100"]);
        assert_eq!(
            matches
                .get_one::<String>("listen")
                .map(std::string::String::as_str),
            Some("192.168.1.100")
        );
    }

    #[test]
    fn test_listen_ipv6_all() {
        let command = new();
        let matches = command.get_matches_from(vec!["mariadb_exporter", "--listen", "::"]);
        assert_eq!(
            matches
                .get_one::<String>("listen")
                .map(std::string::String::as_str),
            Some("::")
        );
    }

    #[test]
    fn test_listen_ipv6_localhost() {
        let command = new();
        let matches = command.get_matches_from(vec!["mariadb_exporter", "--listen", "::1"]);
        assert_eq!(
            matches
                .get_one::<String>("listen")
                .map(std::string::String::as_str),
            Some("::1")
        );
    }

    #[test]
    fn test_listen_from_env() {
        temp_env::with_var("MARIADB_EXPORTER_LISTEN", Some("192.168.1.1"), || {
            let command = new();
            let matches = command.get_matches_from(vec!["mariadb_exporter"]);
            assert_eq!(
                matches
                    .get_one::<String>("listen")
                    .map(std::string::String::as_str),
                Some("192.168.1.1")
            );
        });
    }

    #[test]
    fn test_listen_cli_overrides_env() {
        temp_env::with_var("MARIADB_EXPORTER_LISTEN", Some("::1"), || {
            let command = new();
            let matches =
                command.get_matches_from(vec!["mariadb_exporter", "--listen", "127.0.0.1"]);
            assert_eq!(
                matches
                    .get_one::<String>("listen")
                    .map(std::string::String::as_str),
                Some("127.0.0.1")
            );
        });
    }
}
