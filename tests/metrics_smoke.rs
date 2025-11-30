#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
#![allow(clippy::panic)]

use mariadb_exporter::collectors::util::set_base_connect_options_from_dsn;
use mariadb_exporter::collectors::{
    COLLECTOR_NAMES, config::CollectorConfig, registry::CollectorRegistry,
};
use secrecy::SecretString;
use sqlx::mysql::MySqlPoolOptions;
use std::collections::HashSet;
use std::env;
use std::path::Path;
use std::time::Duration;
use testcontainers_modules::mariadb::Mariadb;
use testcontainers_modules::testcontainers::{
    ImageExt, core::IntoContainerPort, runners::AsyncRunner,
};

#[tokio::test]
async fn metrics_smoke_includes_optional_collectors() -> anyhow::Result<()> {
    // If CI provides a DSN (service container), use it; otherwise spin up a testcontainer.
    if let Ok(dsn) = env::var("MARIADB_EXPORTER_DSN") {
        let pool = connect_with_candidates_from_dsn(&dsn).await?;
        set_base_connect_options_from_dsn(&SecretString::from(dsn))?;
        return run_assertions(pool, &[]).await;
    }

    if !Path::new("/var/run/docker.sock").exists() {
        eprintln!("Docker socket not available, skipping metrics smoke test");
        return Ok(());
    }

    let container = match Mariadb::default()
        .with_env_var("MARIADB_ROOT_PASSWORD", "root")
        .with_env_var("MARIADB_ROOT_HOST", "%")
        .start()
        .await
    {
        Ok(container) => container,
        Err(e) => {
            eprintln!("Skipping metrics smoke test: {e}");
            return Ok(());
        }
    };

    let port = container.get_host_port_ipv4(3306.tcp()).await?;
    let host = container.get_host().await?.to_string();
    let pool = connect_with_candidates(&host, port, "mysql").await?;

    set_base_connect_options_from_dsn(&SecretString::from(format!(
        "mysql://root@{host}:{port}/mysql"
    )))?;

    run_assertions(pool, &[]).await
}

async fn connect_with_candidates(
    host: &str,
    port: u16,
    db: &str,
) -> anyhow::Result<sqlx::MySqlPool> {
    let mut tried = Vec::new();
    let mut candidates = HashSet::new();

    if let Ok(pass) = env::var("MARIADB_ROOT_PASSWORD") {
        candidates.insert(pass);
    }
    if let Ok(pass) = env::var("MARIADB_PASSWORD") {
        candidates.insert(pass);
    }
    candidates.insert("root".to_string());
    candidates.insert("test".to_string());
    candidates.insert(String::new());

    let mut last_err = None;
    for pass in candidates {
        let dsn = if pass.is_empty() {
            format!("mysql://root@{host}:{port}/{db}")
        } else {
            format!("mysql://root:{pass}@{host}:{port}/{db}")
        };
        tried.push(dsn.clone());

        match MySqlPoolOptions::new()
            .min_connections(1)
            .max_connections(3)
            .acquire_timeout(Duration::from_secs(20))
            .connect(&dsn)
            .await
        {
            Ok(pool) => return Ok(pool),
            Err(e) => last_err = Some(e),
        }
    }

    Err(anyhow::anyhow!(
        "Failed to connect using candidates: {tried:?}, last error: {last_err:?}"
    ))
}

async fn connect_with_candidates_from_dsn(dsn: &str) -> anyhow::Result<sqlx::MySqlPool> {
    let base = url::Url::parse(dsn)?;
    let mut last_err = None;
    let mut tried = Vec::new();
    let mut candidates = HashSet::new();

    if let Ok(pass) = env::var("MARIADB_ROOT_PASSWORD") {
        candidates.insert(pass);
    }
    if let Ok(pass) = env::var("MARIADB_PASSWORD") {
        candidates.insert(pass);
    }
    candidates.insert("root".to_string());
    candidates.insert("test".to_string());
    candidates.insert(String::new());

    for pass in candidates {
        let mut url = base.clone();
        if pass.is_empty() {
            url.set_password(None).ok();
        } else {
            url.set_password(Some(&pass)).ok();
        }
        let dsn_try = url.to_string();
        tried.push(dsn_try.clone());

        match MySqlPoolOptions::new()
            .min_connections(1)
            .max_connections(3)
            .acquire_timeout(Duration::from_secs(20))
            .connect(&dsn_try)
            .await
        {
            Ok(pool) => return Ok(pool),
            Err(e) => last_err = Some(e),
        }
    }

    Err(anyhow::anyhow!(
        "Failed to connect using candidates: {tried:?}, last error: {last_err:?}"
    ))
}

async fn run_assertions(pool: sqlx::MySqlPool, extra_needles: &[&str]) -> anyhow::Result<()> {
    let config = CollectorConfig::new().with_enabled(
        &COLLECTOR_NAMES
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
    );
    let registry = CollectorRegistry::new(&config);

    // First scrape - establishes the count
    let _first = registry.collect_all(&pool).await?;

    // Second scrape - the count from first scrape is now visible
    let metrics = registry.collect_all(&pool).await?;
    let samples: Vec<&str> = metrics
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .collect();

    let mut needles = vec![
        // Core availability/version
        "mariadb_up",
        "mariadb_version_info",
        // Core status gauges
        "mariadb_global_status_threads_connected",
        "mariadb_global_status_questions",
        "mariadb_innodb_buffer_pool_pages_data",
        // Exporter self-metrics
        "mariadb_exporter_collector_scrape_duration_seconds_bucket",
        "mariadb_exporter_metrics_total",
    ];
    needles.extend_from_slice(extra_needles);

    for needle in needles {
        assert!(
            samples.iter().any(|line| line.starts_with(needle)),
            "metrics output should contain a sample for {needle}"
        );
    }

    // Verify mariadb_exporter_metrics_total matches actual count
    // Note: due to eventual consistency, the reported count is from the PREVIOUS scrape
    let actual_count = samples.len();
    let reported_count = samples
        .iter()
        .find(|line| line.starts_with("mariadb_exporter_metrics_total"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|s| s.parse::<usize>().ok())
        .expect("mariadb_exporter_metrics_total should have a valid count");

    assert_eq!(
        actual_count, reported_count,
        "mariadb_exporter_metrics_total ({reported_count}) should match actual sample count ({actual_count})"
    );

    Ok(())
}
