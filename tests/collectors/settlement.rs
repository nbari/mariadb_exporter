//! Settlement-contract tests against a live `MariaDB`.
//!
//! The unit tests in `src/collectors/mod.rs` pin the trait mechanics
//! (`Fresh` does not reset, `Skipped` resets, `Err` does not). These tests pin the
//! *observable* consequences against a real server:
//!
//! * a successful but empty snapshot removes the labels that disappeared;
//! * a query error preserves the previous snapshot instead of destroying it;
//! * a source that becomes unreadable removes the series it owned.
//!
//! Every test seeds and drops its own schema, so none of them depends on a lived-in
//! database.

use super::super::common;
use anyhow::Result;
use mariadb_exporter::collectors::{
    Collector, NO_LABELS, schema::SchemaCollector, statements::StatementsCollector,
    tls::TlsCollector,
};
use prometheus::Registry;

/// Number of published samples in a metric family, or `None` when the family is absent.
///
/// Prometheus drops a metric family with no children at `gather()` time, so "absent
/// family" and "family with zero samples" are the same observable state: nothing is
/// exposed for it.
fn sample_count(registry: &Registry, metric: &str) -> Option<usize> {
    registry
        .gather()
        .iter()
        .find(|mf| mf.name() == metric)
        .map(|mf| mf.get_metric().len())
}

fn samples_for_schema(registry: &Registry, metric: &str, schema: &str) -> usize {
    registry
        .gather()
        .iter()
        .filter(|mf| mf.name() == metric)
        .flat_map(prometheus::proto::MetricFamily::get_metric)
        .filter(|m| {
            m.get_label()
                .iter()
                .any(|l| l.name() == "schema" && l.value() == schema)
        })
        .count()
}

/// A schema name no real server will report, used to represent "a series a previous scrape
/// published for an entity that has since disappeared".
const GHOST_SCHEMA: &str = "mariadb_exporter_ghost_schema";

/// A successful snapshot is `Fresh`, not `Skipped`: it must publish the new truth, which
/// means the labels that are no longer in it have to go with it.
#[tokio::test]
async fn successful_snapshot_clears_labels_that_disappeared() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = SchemaCollector::new();
    let registry = Registry::new();
    collector.register_metrics(&registry)?;

    // Seed the state a previous scrape would have left behind for a table that has since
    // been dropped. The source stays perfectly readable, so this is not a skip.
    collector
        .tables()
        .table_size_bytes()
        .with_label_values(&[GHOST_SCHEMA, "gone"])
        .set(4096);
    collector
        .tables()
        .table_rows()
        .with_label_values(&[GHOST_SCHEMA, "gone"])
        .set(7);
    assert_eq!(
        samples_for_schema(
            &registry,
            "mariadb_info_schema_table_size_bytes",
            GHOST_SCHEMA
        ),
        1,
        "the seeded ghost table should start out published"
    );

    collector.collect(&pool).await?;

    assert_eq!(
        samples_for_schema(
            &registry,
            "mariadb_info_schema_table_size_bytes",
            GHOST_SCHEMA
        ),
        0,
        "a fresh snapshot must not keep entities that are no longer in it"
    );
    assert_eq!(
        samples_for_schema(&registry, "mariadb_info_schema_table_rows", GHOST_SCHEMA),
        0,
        "a fresh snapshot must not keep entities that are no longer in it"
    );

    pool.close().await;
    Ok(())
}

/// A query error is *not* an absence: the previous snapshot survives in the collector's own
/// registry so it can resume, while the scrape withholds it (pinned in
/// `collectors::registry::scrape_outcome_tests`).
#[tokio::test]
async fn a_query_error_preserves_the_previous_snapshot() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = SchemaCollector::new();
    let registry = Registry::new();
    collector.register_metrics(&registry)?;

    collector.collect(&pool).await?;
    let before = sample_count(&registry, "mariadb_info_schema_table_size_bytes");

    // Seed a recognisable series so the assertion does not depend on the server's content.
    collector
        .tables()
        .table_size_bytes()
        .with_label_values(&[GHOST_SCHEMA, "kept"])
        .set(4096);

    // Closing the pool turns every subsequent query into a genuine fault.
    pool.close().await;

    let result = collector.collect(&pool).await;
    assert!(
        result.is_err(),
        "a closed pool is a fault, not a graceful absence"
    );

    assert_eq!(
        samples_for_schema(
            &registry,
            "mariadb_info_schema_table_size_bytes",
            GHOST_SCHEMA
        ),
        1,
        "an error must not destroy the last good snapshot"
    );
    assert!(
        sample_count(&registry, "mariadb_info_schema_table_size_bytes") >= before,
        "an error must leave the previous snapshot intact"
    );

    Ok(())
}

/// TLS certificate fields present -> absent.
///
/// `Ssl_version` in `SHOW STATUS` describes the *current connection*. Scraping over an
/// unencrypted connection therefore reports TLS as not in use, which is a factual
/// `configured=0`; because no certificate is being presented, any previously published
/// expiry timestamp must be removed rather than carried forward — a stale `not_after`
/// would hide a rotation.
#[tokio::test]
async fn tls_certificate_fields_absent_in_a_fresh_read_are_removed() -> Result<()> {
    let plain = match plaintext_pool().await {
        Ok(pool) => pool,
        Err(e) => {
            eprintln!("cannot open an unencrypted connection ({e}); skipping");
            return Ok(());
        }
    };

    let collector = TlsCollector::new();
    let registry = Registry::new();
    collector.register_metrics(&registry)?;

    // Seed the state a previous scrape against a TLS-enabled connection would have left.
    collector
        .ssl_status()
        .cert_not_after_seconds()
        .with_label_values(&NO_LABELS)
        .set(1_700_000_000.0);
    collector
        .ssl_status()
        .cert_not_before_seconds()
        .with_label_values(&NO_LABELS)
        .set(1_600_000_000.0);
    assert_eq!(
        sample_count(&registry, "mariadb_ssl_cert_not_after_seconds"),
        Some(1),
        "seeded certificate expiry should start out published"
    );

    collector.collect(&plain).await?;

    assert_eq!(
        sample_count(&registry, "mariadb_ssl_cert_not_after_seconds"),
        None,
        "a read with no certificate must not keep an old expiry timestamp"
    );
    assert_eq!(
        sample_count(&registry, "mariadb_ssl_cert_not_before_seconds"),
        None,
        "a read with no certificate must not keep an old start timestamp"
    );
    // The honest, factual part of the same fresh read stays published.
    assert_eq!(
        sample_count(&registry, "mariadb_ssl_server_configured"),
        Some(1),
        "TLS genuinely not in use is a fresh 0, not an absence"
    );

    plain.close().await;
    Ok(())
}

/// Opens a pool that explicitly disables TLS, so `SHOW STATUS` reports no `Ssl_version`.
async fn plaintext_pool() -> Result<sqlx::MySqlPool> {
    let dsn = common::get_test_dsn();
    let separator = if dsn.contains('?') { "&" } else { "?" };
    let plaintext = format!("{dsn}{separator}ssl-mode=DISABLED");

    sqlx::mysql::MySqlPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&plaintext)
        .await
        .map_err(Into::into)
}

/// A source that is present but unreadable for this connection (revoked privilege) is a
/// skip, and a skip must remove every series the collector owned.
#[tokio::test]
async fn a_denied_source_clears_the_series_it_owned() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let can_manage_users = sqlx::query("SELECT 1 FROM mysql.user LIMIT 1")
        .fetch_optional(&pool)
        .await
        .is_ok();
    if !can_manage_users {
        eprintln!("insufficient privileges to create a restricted user; skipping");
        pool.close().await;
        return Ok(());
    }

    let user = "mdbexp_settle";
    let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP USER IF EXISTS '{user}'@'%'"
    )))
    .execute(&pool)
    .await;
    if sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE USER '{user}'@'%' IDENTIFIED BY 'settle_pw'"
    )))
    .execute(&pool)
    .await
    .is_err()
    {
        eprintln!("cannot create a restricted user here; skipping");
        pool.close().await;
        return Ok(());
    }
    // Deliberately no PROCESS privilege: `SHOW ENGINE INNODB STATUS` is denied.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "GRANT SELECT ON `mysql`.* TO '{user}'@'%'"
    )))
    .execute(&pool)
    .await?;

    let collector = mariadb_exporter::collectors::innodb::InnodbCollector::new();
    let registry = Registry::new();
    collector.register_metrics(&registry)?;

    collector.collect(&pool).await?;
    let privileged = sample_count(&registry, "mariadb_innodb_active_transactions");
    if privileged.is_none() {
        eprintln!("InnoDB status unavailable even for the test user; skipping");
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP USER IF EXISTS '{user}'@'%'"
        )))
        .execute(&pool)
        .await;
        pool.close().await;
        return Ok(());
    }

    let dsn = common::get_test_dsn();
    let restricted_dsn = restrict_dsn(&dsn, user, "settle_pw");
    let restricted = sqlx::mysql::MySqlPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&restricted_dsn)
        .await;

    match restricted {
        Ok(restricted) => {
            collector.collect(&restricted).await?;
            assert_eq!(
                sample_count(&registry, "mariadb_innodb_active_transactions"),
                None,
                "a denied source must remove the series it owned, not serve stale values"
            );
            restricted.close().await;
        }
        Err(e) => eprintln!("cannot connect as the restricted user ({e}); skipping"),
    }

    let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP USER IF EXISTS '{user}'@'%'"
    )))
    .execute(&pool)
    .await;
    pool.close().await;
    Ok(())
}

/// Rewrites the credentials of a `mysql://user:pass@host:port/db` DSN.
fn restrict_dsn(dsn: &str, user: &str, password: &str) -> String {
    let rest = dsn.strip_prefix("mysql://").unwrap_or(dsn);
    let tail = rest.rsplit_once('@').map_or(rest, |(_, tail)| tail);
    format!("mysql://{user}:{password}@{tail}")
}

/// performance-schema source available -> unavailable.
///
/// Losing access to `performance_schema` (the tables are invisible to an unprivileged
/// connection, and selecting from them is denied) is a skip, so every statement series the
/// collector owned has to disappear instead of being served as current.
#[tokio::test]
async fn performance_schema_becoming_unavailable_clears_statement_series() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = StatementsCollector::new();
    let registry = Registry::new();
    collector.register_metrics(&registry)?;

    collector.collect(&pool).await?;
    if sample_count(&registry, "mariadb_perf_schema_digest_total").is_none() {
        eprintln!("performance_schema statement digests unavailable here; skipping");
        pool.close().await;
        return Ok(());
    }

    let Some(restricted) = restricted_pool(&pool, "mdbexp_ps").await? else {
        pool.close().await;
        return Ok(());
    };

    collector.collect(&restricted).await?;

    assert_eq!(
        sample_count(&registry, "mariadb_perf_schema_digest_total"),
        None,
        "an unavailable performance_schema must remove the statement totals"
    );
    assert_eq!(
        sample_count(&registry, "mariadb_perf_schema_digest_latency_seconds"),
        None,
        "an unavailable performance_schema must remove the per-digest latencies"
    );

    restricted.close().await;
    drop_user(&pool, "mdbexp_ps").await;
    pool.close().await;
    Ok(())
}

/// Creates a connection-capable user with no interesting privileges and returns a pool for
/// it, or `None` when the environment does not allow user management.
async fn restricted_pool(pool: &sqlx::MySqlPool, user: &str) -> Result<Option<sqlx::MySqlPool>> {
    drop_user(pool, user).await;

    if sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE USER '{user}'@'%' IDENTIFIED BY 'settle_pw'"
    )))
    .execute(pool)
    .await
    .is_err()
    {
        eprintln!("cannot create a restricted user here; skipping");
        return Ok(None);
    }
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "GRANT SELECT ON `mysql`.* TO '{user}'@'%'"
    )))
    .execute(pool)
    .await?;

    let restricted_dsn = restrict_dsn(&common::get_test_dsn(), user, "settle_pw");
    match sqlx::mysql::MySqlPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&restricted_dsn)
        .await
    {
        Ok(restricted) => Ok(Some(restricted)),
        Err(e) => {
            eprintln!("cannot connect as the restricted user ({e}); skipping");
            drop_user(pool, user).await;
            Ok(None)
        }
    }
}

async fn drop_user(pool: &sqlx::MySqlPool, user: &str) {
    let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP USER IF EXISTS '{user}'@'%'"
    )))
    .execute(pool)
    .await;
}
