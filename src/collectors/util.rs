//! Shared utilities for collectors:
//! - Global, read-only exclusion list of databases (set once at startup).
//! - Parsed base connection options derived from the DSN to build per-database connections.
//! - Cached tiny pools per non-default database (reuse across scrapes).

use anyhow::{Result, anyhow};
use once_cell::sync::OnceCell;
use secrecy::{ExposeSecret, SecretString};
use sqlx::MySqlPool;
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
use std::{collections::HashMap, str::FromStr, sync::Arc};
use tokio::sync::RwLock;
use url::Url;

/// Global holder for excluded databases, set once at startup via CLI/env.
static EXCLUDED: OnceCell<Arc<[String]>> = OnceCell::new();

/// Parsed base connect options derived from the provided DSN (set once).
static BASE_OPTS: OnceCell<MySqlConnectOptions> = OnceCell::new();

/// Default database name parsed from DSN.
static DEFAULT_DB: OnceCell<String> = OnceCell::new();

/// Cache of per-database tiny pools (only for non-default DBs).
static POOLS: OnceCell<RwLock<HashMap<String, MySqlPool>>> = OnceCell::new();

/// `MariaDB` version number (e.g., `100_400` for v10.4).
static MARIADB_VERSION: OnceCell<i32> = OnceCell::new();

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
    let _ = MARIADB_VERSION.set(version);
}

/// Get the `MariaDB` version number.
/// Returns 0 if not set (should never happen in production).
#[inline]
pub fn get_mariadb_version() -> i32 {
    MARIADB_VERSION.get().copied().unwrap_or(0)
}

/// Check if `MariaDB` version is at least the specified minimum.
#[inline]
#[must_use]
pub fn is_mariadb_version_at_least(min_version: i32) -> bool {
    get_mariadb_version() >= min_version
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
/// Also records the default database name and initializes the pool cache.
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

    if POOLS.get().is_none() {
        let _ = POOLS.set(RwLock::new(HashMap::new()));
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

/// Get (or create) a tiny pool for the specified database. Only used for non-default DBs.
/// The default DB should reuse the shared pool created at startup.
///
/// # Errors
///
/// Returns an error if pool creation or connection fails
pub async fn get_or_create_pool_for_db(datname: &str) -> Result<MySqlPool> {
    if let Some(def) = get_default_database()
        && def == datname
    {
        return Err(anyhow!(
            "get_or_create_pool_for_db called for default database; use shared pool"
        ));
    }

    let pools = POOLS.get().ok_or_else(|| {
        anyhow!("Pool cache not initialized; call set_base_connect_options_from_dsn()")
    })?;

    {
        let guard = pools.read().await;
        if let Some(pool) = guard.get(datname) {
            return Ok(pool.clone());
        }
    }

    let opts = connect_options_for_db(datname)?;
    let pool = MySqlPoolOptions::new()
        .max_connections(1)
        .min_connections(0)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect_with(opts)
        .await?;

    {
        let mut guard = pools.write().await;
        guard.insert(datname.to_string(), pool.clone());
    }

    Ok(pool)
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
        assert_eq!(get_mariadb_version(), 0);
        assert!(!is_mariadb_version_at_least(100_000));

        set_mariadb_version(100_500);
        assert_eq!(get_mariadb_version(), 100_500);
        assert!(is_mariadb_version_at_least(100_000));
        assert!(!is_mariadb_version_at_least(200_000));
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
