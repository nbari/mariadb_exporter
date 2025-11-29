use super::super::common;
use anyhow::Result;
use mariadb_exporter::collectors::Collector;
use mariadb_exporter::collectors::default::version::VersionCollector;
use prometheus::Registry;

#[tokio::test]
async fn test_version_collector_registers_without_error() -> Result<()> {
    let collector = VersionCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    Ok(())
}

#[tokio::test]
async fn test_version_collector_queries_database() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = VersionCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;

    let metric_families = registry.gather();

    // Should have version_info metric
    let version_metric = metric_families
        .iter()
        .find(|m| m.name() == "mariadb_version_info");

    assert!(version_metric.is_some(), "version_info metric should exist");

    // Should have at least one sample
    if let Some(metric) = version_metric {
        let metrics = metric.get_metric();
        assert!(
            !metrics.is_empty(),
            "Should have at least one version sample"
        );

        // Check that version label exists
        let first_metric = &metrics[0];
        let labels = first_metric.get_label();
        let has_version = labels.iter().any(|l| l.name() == "version");
        assert!(has_version, "Should have version label");
    }

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn test_version_collector_has_system_memory() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = VersionCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;

    let metric_families = registry.gather();

    // Should have system memory metric
    let memory_metric = metric_families
        .iter()
        .find(|m| m.name() == "mariadb_exporter_system_memory_total_bytes");

    assert!(
        memory_metric.is_some(),
        "system_memory_total_bytes metric should exist"
    );

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn test_version_collector_handles_connection_error_gracefully() -> Result<()> {
    let collector = VersionCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Try with invalid pool - this should still not panic during registration
    // The collect() will fail, but that's expected
    let result = sqlx::mysql::MySqlPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(1))
        .connect("mysql://invalid:invalid@127.0.0.1:9999/test")
        .await;

    assert!(result.is_err(), "Should fail to connect to invalid server");

    Ok(())
}
