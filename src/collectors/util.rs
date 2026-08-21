//! Shared utilities for collectors:
//! - Global, read-only exclusion list of databases (set once at startup).
//! - Parsed base connection options derived from the DSN to build per-database connections.
//! - Ephemeral per-database connections (opened per query, closed on drop — never cached).

use anyhow::{Result, anyhow};
use arc_swap::ArcSwap;
use once_cell::sync::OnceCell;
use regex::Regex;
use secrecy::{ExposeSecret, SecretString};
use sqlx::Connection;
use sqlx::mysql::{MySqlConnectOptions, MySqlConnection};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{str::FromStr, sync::Arc};
use tracing::{debug, warn};
use url::Url;

/// Global holder for excluded databases, set once at startup via CLI/env.
static EXCLUDED: OnceCell<Arc<[String]>> = OnceCell::new();

/// Parsed base connect options derived from the provided DSN (set once).
static BASE_OPTS: OnceCell<MySqlConnectOptions> = OnceCell::new();

/// Default database name parsed from DSN.
static DEFAULT_DB: OnceCell<String> = OnceCell::new();

/// `MariaDB` version number (e.g., `100_400` for v10.4).
static MARIADB_VERSION: OnceCell<ArcSwap<i32>> = OnceCell::new();

/// Conversion factor: Picoseconds to Seconds
pub const PICO_TO_SECONDS: f64 = 1_000_000_000_000.0;

/// How a failed query should be settled by the calling collector.
///
/// See [`classify_query_error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryFailure {
    /// The table, plugin, schema, column or system variable does not exist here, or the
    /// feature is switched off so the source has nothing to report. The data is known to be
    /// unavailable rather than merely unread, so the collector reports
    /// `Collected::Skipped` and its previous series are removed.
    Absent,
    /// The account may not read it. Worth warning about **once** — it is a configuration
    /// problem, not a version difference — but it is still a known unavailability, so the
    /// collector skips rather than publishing a fabricated zero.
    Denied,
    /// Anything else: a genuine or transient fault (lost connection, timeout, deadlock,
    /// malformed data, unexpected SQL error). It must propagate so the previous snapshot is
    /// preserved rather than cleared.
    Fault,
}

/// `ER_DBACCESS_DENIED_ERROR`: no access to the database.
pub const ER_DBACCESS_DENIED_ERROR: u16 = 1044;
/// `ER_BAD_DB_ERROR`: unknown database (for example `performance_schema` compiled out).
pub const ER_BAD_DB_ERROR: u16 = 1049;
/// `ER_BAD_FIELD_ERROR`: unknown column — the table exists but this build lacks the field.
pub const ER_BAD_FIELD_ERROR: u16 = 1054;
/// `ER_UNKNOWN_TABLE`: unknown table, raised by `information_schema` for absent plugin tables.
pub const ER_UNKNOWN_TABLE: u16 = 1109;
/// `ER_TABLEACCESS_DENIED_ERROR`: the account lacks `SELECT` on the table.
pub const ER_TABLEACCESS_DENIED_ERROR: u16 = 1142;
/// `ER_COLUMNACCESS_DENIED_ERROR`: the account lacks access to a column.
pub const ER_COLUMNACCESS_DENIED_ERROR: u16 = 1143;
/// `ER_NO_SUCH_TABLE`: the table does not exist (absent `performance_schema` table).
pub const ER_NO_SUCH_TABLE: u16 = 1146;
/// `ER_UNKNOWN_SYSTEM_VARIABLE`: `SELECT @@some_var` where the variable does not exist.
pub const ER_UNKNOWN_SYSTEM_VARIABLE: u16 = 1193;
/// `ER_SPECIFIC_ACCESS_DENIED_ERROR`: a required privilege such as `PROCESS`,
/// `BINLOG MONITOR`, `REPLICA MONITOR` or `SUPER` is missing. Raised by
/// `SHOW ENGINE INNODB STATUS`, `SHOW BINARY LOGS` and `SHOW REPLICA STATUS`.
pub const ER_SPECIFIC_ACCESS_DENIED_ERROR: u16 = 1227;
/// `ER_NOT_SUPPORTED_YET`: the statement is not supported by this server.
pub const ER_NOT_SUPPORTED_YET: u16 = 1235;
/// `ER_UNKNOWN_STORAGE_ENGINE`: `SHOW ENGINE <x> STATUS` for an engine that is not built in.
pub const ER_UNKNOWN_STORAGE_ENGINE: u16 = 1286;
/// `ER_PROCACCESS_DENIED_ERROR`: the account lacks access to a stored routine.
pub const ER_PROCACCESS_DENIED_ERROR: u16 = 1370;
/// `ER_NO_BINARY_LOGGING`: "You are not using binary logging" — the feature is switched off,
/// so there is no binlog to describe.
pub const ER_NO_BINARY_LOGGING: u16 = 1381;
/// `ER_PARSE_ERROR`: the server does not understand the statement.
///
/// Deliberately **not** classified as [`QueryFailure::Absent`] — a parse error normally means
/// the exporter emitted broken SQL, which is a fault. It is exported so the one place that
/// probes several spellings of the same statement (`SHOW ALL SLAVES STATUS` and friends) can
/// treat it as "this server does not know this form" without weakening the classifier for
/// everyone else.
pub const ER_PARSE_ERROR: u16 = 1064;

/// Returns the MariaDB/MySQL error number of a failed query, if it is a database error.
#[must_use]
pub fn mysql_error_number(error: &sqlx::Error) -> Option<u16> {
    error
        .as_database_error()?
        .try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()
        .map(sqlx::mysql::MySqlDatabaseError::number)
}

/// Classifies a failed query so a collector can tell "not available here" from "broke".
///
/// Deliberately keyed on the `MariaDB` **error number** rather than the error message:
/// matching a table name in the message text reads a permission error as an absent table,
/// because the name appears in both. `SQLSTATE` alone is not enough either — `MariaDB` reuses
/// `42000` for both `ER_TABLEACCESS_DENIED_ERROR` and a plain syntax error, and reports
/// `ER_UNKNOWN_SYSTEM_VARIABLE` and `ER_NO_BINARY_LOGGING` under the catch-all `HY000`.
///
/// `ER_ACCESS_DENIED_ERROR` (1045) is intentionally **not** treated as `Denied`: it is a
/// connection-time authentication failure, not a per-source privilege problem, and turning
/// it into a skip would silently erase every collector's metrics when credentials break.
///
/// Non-database `sqlx` errors (I/O, pool timeout, TLS, protocol, decode) are always
/// [`QueryFailure::Fault`].
#[must_use]
pub fn classify_query_error(error: &sqlx::Error) -> QueryFailure {
    let Some(number) = mysql_error_number(error) else {
        return QueryFailure::Fault;
    };

    match number {
        ER_BAD_DB_ERROR
        | ER_BAD_FIELD_ERROR
        | ER_UNKNOWN_TABLE
        | ER_NO_SUCH_TABLE
        | ER_UNKNOWN_SYSTEM_VARIABLE
        | ER_NOT_SUPPORTED_YET
        | ER_UNKNOWN_STORAGE_ENGINE
        | ER_NO_BINARY_LOGGING => QueryFailure::Absent,
        ER_DBACCESS_DENIED_ERROR
        | ER_TABLEACCESS_DENIED_ERROR
        | ER_COLUMNACCESS_DENIED_ERROR
        | ER_SPECIFIC_ACCESS_DENIED_ERROR
        | ER_PROCACCESS_DENIED_ERROR => QueryFailure::Denied,
        _ => QueryFailure::Fault,
    }
}

/// A once-per-process warning latch for an optional source the account may not read.
///
/// A revoked privilege is a configuration problem worth surfacing, but it does not heal on
/// its own, so warning on every scrape would turn a static misconfiguration into log noise.
/// The latch is shared across clones so a collector cloned into `CollectorType` does not
/// reset it.
#[derive(Debug, Clone, Default)]
pub struct DeniedOnce(Arc<AtomicBool>);

impl DeniedOnce {
    /// Report a denied optional source: `warn!` the first time, `debug!` afterwards.
    pub fn report(&self, source: &str, error: &sqlx::Error) {
        if self.0.swap(true, Ordering::Relaxed) {
            debug!(source, error = %error, "optional source still not readable; skipping");
        } else {
            warn!(
                source,
                error = %error,
                "permission denied reading optional source; its metrics will be absent until the privilege is granted"
            );
        }
    }
}

/// Conversion factor: Picoseconds to Milliseconds
pub const PICO_TO_MILLIS: f64 = 1_000_000_000.0;

/// List of internal/system schemas to exclude from general collection
pub const SYSTEM_SCHEMAS: &[&str] = &["mysql", "information_schema", "performance_schema", "sys"];

/// Set the excluded databases from CLI/env. Call this once during startup.
pub fn set_excluded_databases(list: Vec<String>) {
    let mut cleaned: Vec<String> = list
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    cleaned.dedup();
    let _ = EXCLUDED.set(Arc::from(cleaned));
}

/// Get the excluded databases as a static slice.
#[inline]
pub fn get_excluded_databases() -> &'static [String] {
    match EXCLUDED.get() {
        Some(arc) => &arc[..],
        None => &[],
    }
}

/// Convenience check: is a given database name excluded?
#[inline]
#[must_use]
pub fn is_database_excluded(datname: &str) -> bool {
    get_excluded_databases().iter().any(|d| d == datname)
}

/// Set the `MariaDB` version. Call this once during startup after connecting.
pub fn set_mariadb_version(version: i32) {
    let cell = MARIADB_VERSION.get_or_init(|| ArcSwap::from_pointee(0));
    cell.store(Arc::new(version));
}

/// Get the `MariaDB` version number.
/// Returns 0 if not set (should never happen in production).
#[inline]
pub fn get_mariadb_version() -> i32 {
    MARIADB_VERSION.get().map_or(0, |v| **v.load())
}

/// Check if `MariaDB` version is at least the specified minimum.
#[inline]
#[must_use]
pub fn is_mariadb_version_at_least(min_version: i32) -> bool {
    get_mariadb_version() >= min_version
}

/// Parse `MariaDB` version string into an integer (e.g., "10.5.8-MariaDB" -> 100508).
/// Returns 0 if parsing fails.
#[must_use]
pub fn parse_mariadb_version(version_string: &str) -> i32 {
    let (_, num) = normalize_mariadb_version(version_string);
    #[allow(clippy::cast_possible_truncation)]
    let res = num as i32;
    res
}

/// Parse and normalize `MariaDB` version string.
/// Returns a tuple of (`normalized_string`, `version_number`).
/// e.g. "10.5.8-MariaDB" -> ("10.5.8", 100508)
///      "11.4" -> ("11.4.0", 110400)
///
/// # Panics
///
/// Panics if the regex cannot be compiled (should never happen).
#[must_use]
pub fn normalize_mariadb_version(version_string: &str) -> (String, i64) {
    // Regex to capture major, optional minor, optional patch
    // Matches start of string or after whitespace/common separators if needed,
    // but usually version strings from SELECT VERSION() start with the number.
    // We use a slightly more permissive regex than before.
    static RE: OnceCell<Regex> = OnceCell::new();
    let re = RE.get_or_init(|| {
        #[allow(clippy::expect_used)]
        Regex::new(r"^(\d+)(?:\.(\d+))?(?:\.(\d+))?").expect("Invalid regex")
    });

    if let Some(caps) = re.captures(version_string) {
        let major = caps
            .get(1)
            .map_or(0, |m| m.as_str().parse::<i64>().unwrap_or(0));
        let minor = caps
            .get(2)
            .map_or(0, |m| m.as_str().parse::<i64>().unwrap_or(0));
        let patch = caps
            .get(3)
            .map_or(0, |m| m.as_str().parse::<i64>().unwrap_or(0));

        let normalized = format!("{major}.{minor}.{patch}");
        let num = major * 10000 + minor * 100 + patch;

        (normalized, num)
    } else {
        ("0.0.0".to_string(), 0)
    }
}

fn parse_database_from_dsn(dsn: &SecretString) -> Option<String> {
    Url::parse(dsn.expose_secret()).ok().and_then(|url| {
        let db = url.path().trim_start_matches('/');
        if db.is_empty() {
            None
        } else {
            Some(db.to_string())
        }
    })
}

/// Initialize (idempotent) the base connect options from the provided DSN (`SecretString`).
/// Also records the default database name.
///
/// # Errors
///
/// Returns an error if DSN parsing fails
pub fn set_base_connect_options_from_dsn(dsn: &SecretString) -> Result<()> {
    if BASE_OPTS.get().is_none() {
        let opts = MySqlConnectOptions::from_str(dsn.expose_secret())?;
        let _ = BASE_OPTS.set(opts.clone());

        let dbname = parse_database_from_dsn(dsn).unwrap_or_else(|| "mysql".to_string());
        let _ = DEFAULT_DB.set(dbname);
    }

    Ok(())
}

/// Returns the default database name derived from the DSN, if available.
#[inline]
pub fn get_default_database() -> Option<&'static str> {
    DEFAULT_DB.get().map(std::string::String::as_str)
}

/// Build connect options for a specific database name based on the base DSN.
///
/// # Errors
///
/// Returns an error if base options are not initialized
pub fn connect_options_for_db(datname: &str) -> Result<MySqlConnectOptions> {
    let base = BASE_OPTS.get().cloned().ok_or_else(|| {
        anyhow!("BASE_OPTS not set; call set_base_connect_options_from_dsn() at startup")
    })?;
    Ok(base.database(datname))
}

/// Open a fresh connection to the specified non-default database.
///
/// Connections are intentionally **not** pooled or cached: the caller runs a single scrape
/// query and drops the connection, which closes it. This keeps the exporter's per-database
/// connection footprint bounded and independent of the number of databases, instead of
/// pinning one persistent connection per database (which would exhaust `max_connections` on
/// large or connection-constrained clusters). The default database must use the shared pool
/// created at startup.
///
/// `MariaDB` collectors generally read every schema from the shared connection via
/// `information_schema`, so this helper exists for the rare collector that must run a query
/// *in the context of* another database. If you add such a collector, use this (ephemeral)
/// helper — never reintroduce a per-database pool/connection cache.
///
/// # Errors
///
/// Returns an error if called for the default database, or if the connection fails.
pub async fn open_db_connection(datname: &str) -> Result<MySqlConnection> {
    if let Some(def) = get_default_database()
        && def == datname
    {
        return Err(anyhow!(
            "open_db_connection called for default database; use shared pool"
        ));
    }

    let opts = connect_options_for_db(datname)?;
    let conn = MySqlConnection::connect_with(&opts).await?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_and_get_exclusions() {
        set_excluded_databases(vec![
            "mysql".into(),
            "information_schema".into(),
            "information_schema".into(),
            " ".into(),
        ]);

        let got = get_excluded_databases();
        assert_eq!(
            got,
            &["mysql".to_string(), "information_schema".to_string()]
        );
        assert!(is_database_excluded("mysql"));
        assert!(!is_database_excluded("not_there"));
    }

    #[test]
    fn test_mariadb_version_utilities() {
        // Reset global state for test isolation
        if let Some(cell) = MARIADB_VERSION.get() {
            cell.store(Arc::new(0));
        }

        assert_eq!(get_mariadb_version(), 0);
        assert!(!is_mariadb_version_at_least(100_000));

        set_mariadb_version(100_500);
        assert_eq!(get_mariadb_version(), 100_500);
        assert!(is_mariadb_version_at_least(100_000));
        assert!(!is_mariadb_version_at_least(200_000));
    }

    #[test]
    fn test_parse_mariadb_version() {
        assert_eq!(parse_mariadb_version("10.5.8-MariaDB"), 100_508);
        assert_eq!(parse_mariadb_version("10.11.2"), 101_102);
        assert_eq!(parse_mariadb_version("5.7.33"), 50_733);
        assert_eq!(parse_mariadb_version("11.4"), 110_400); // New case
        assert_eq!(parse_mariadb_version("12"), 120_000); // New case
        assert_eq!(parse_mariadb_version("invalid"), 0);
        assert_eq!(parse_mariadb_version(""), 0);
    }

    #[test]
    fn test_normalize_mariadb_version() {
        assert_eq!(
            normalize_mariadb_version("10.5.8-MariaDB"),
            ("10.5.8".to_string(), 100_508)
        );
        assert_eq!(
            normalize_mariadb_version("11.4"),
            ("11.4.0".to_string(), 110_400)
        );
        assert_eq!(
            normalize_mariadb_version("12"),
            ("12.0.0".to_string(), 120_000)
        );
        assert_eq!(
            normalize_mariadb_version("invalid"),
            ("0.0.0".to_string(), 0)
        );
    }

    #[test]
    fn test_parse_database_from_dsn() {
        let dsn = SecretString::new("mysql://root:pass@localhost:3306/mydb".into());
        assert_eq!(parse_database_from_dsn(&dsn), Some("mydb".to_string()));

        let socket_dsn = SecretString::new("mysql:///mysql?socket=/var/run/mysqld.sock".into());
        assert_eq!(
            parse_database_from_dsn(&socket_dsn),
            Some("mysql".to_string())
        );
    }
}
