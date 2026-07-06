//! Regression tests for the per-database connection model.
//!
//! `MariaDB` reads every schema from a single shared pool via `information_schema`, so the
//! collectors do not currently fan out per database. When a collector *does* need to run a
//! query in the context of another database, it must use `util::open_db_connection`, which
//! opens the connection **ephemerally** — one connection per query, closed on drop — and
//! must NOT cache a pool/connection per database. Caching reintroduces
//! connection-per-database accumulation that can exhaust `max_connections` on large or
//! connection-constrained servers. These tests lock that invariant so a future change
//! cannot silently regress it.

use super::common;
use mariadb_exporter::collectors::util::{open_db_connection, set_base_connect_options_from_dsn};
use secrecy::SecretString;
use std::time::Duration;

/// Each call must return a **fresh** connection (no cache/reuse), and dropping it must close
/// it. The default DSN database is `mysql`, so `information_schema` is a valid non-default
/// database to connect to.
#[tokio::test]
async fn open_db_connection_is_fresh_and_ephemeral() {
    // Initialise the global base connect options used by `open_db_connection`.
    set_base_connect_options_from_dsn(&SecretString::from(common::get_test_dsn()))
        .expect("set base connect options");

    let Ok(admin) = common::create_test_pool().await else {
        eprintln!("Skipping test: MariaDB not available");
        return;
    };

    let target = "information_schema";

    // Two per-database connections held at once must be two DISTINCT server threads. A
    // per-database pool/connection cache would hand back the same reused connection.
    let mut c1 = open_db_connection(target).await.expect("open conn 1");
    let mut c2 = open_db_connection(target).await.expect("open conn 2");

    let id1: u64 = sqlx::query_scalar("SELECT CONNECTION_ID()")
        .fetch_one(&mut c1)
        .await
        .expect("connection id 1");
    let id2: u64 = sqlx::query_scalar("SELECT CONNECTION_ID()")
        .fetch_one(&mut c2)
        .await
        .expect("connection id 2");

    assert_ne!(
        id1, id2,
        "open_db_connection must open a fresh connection each call (no per-database cache)"
    );

    // Dropping the connections must close them (ephemeral): the server threads must disappear.
    drop(c1);
    drop(c2);

    let mut remaining: i64 = i64::MAX;
    for _ in 0..25 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        remaining = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.processlist WHERE ID IN (?, ?)",
        )
        .bind(id1)
        .bind(id2)
        .fetch_one(&admin)
        .await
        .expect("processlist count");
        if remaining == 0 {
            break;
        }
    }

    assert_eq!(
        remaining, 0,
        "per-database connections must be closed on drop (ephemeral); threads {id1}/{id2} \
         lingered — was a per-database pool/connection cache reintroduced?"
    );

    admin.close().await;
}

/// `open_db_connection` must refuse the default database (that must use the shared pool).
#[tokio::test]
async fn open_db_connection_rejects_default_database() {
    set_base_connect_options_from_dsn(&SecretString::from(common::get_test_dsn()))
        .expect("set base connect options");

    // The default database parsed from the test DSN is `mysql`.
    let result = open_db_connection("mysql").await;
    assert!(
        result.is_err(),
        "open_db_connection must reject the default database and steer callers to the shared pool"
    );
}
