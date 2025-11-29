use super::super::common;
use anyhow::Result;
use mariadb_exporter::collectors::Collector;
use mariadb_exporter::collectors::default::status::StatusCollector;
use prometheus::Registry;

#[tokio::test]
async fn test_global_status_collector_registers_without_error() -> Result<()> {
    let collector = StatusCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    Ok(())
}

#[tokio::test]
async fn test_global_status_collector_collects_metrics() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = StatusCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;

    let metric_families = registry.gather();

    // Core metrics that should always exist
    let expected_metrics = vec![
        "mariadb_global_status_threads_connected",
        "mariadb_global_status_threads_running",
        "mariadb_global_status_connections",
        "mariadb_global_status_queries_total",
        "mariadb_global_status_questions_total",
        "mariadb_global_status_uptime_seconds",
        "mariadb_global_status_aborted_connects",
    ];

    for metric_name in expected_metrics {
        let found = metric_families.iter().any(|m| m.name() == metric_name);
        assert!(
            found,
            "Metric {} should exist. Found: {:?}",
            metric_name,
            metric_families
                .iter()
                .map(prometheus::proto::MetricFamily::name)
                .collect::<Vec<_>>()
        );
    }

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn test_global_status_collector_handles_missing_status_vars() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = StatusCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Should not panic even if some status vars are missing
    let result = collector.collect(&pool).await;
    assert!(
        result.is_ok(),
        "Collector should handle missing status vars gracefully"
    );

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn test_global_status_collector_numeric_values() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = StatusCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;

    let metric_families = registry.gather();

    // Check that uptime metric exists and has samples
    let uptime_metric = metric_families
        .iter()
        .find(|m| m.name() == "mariadb_global_status_uptime_seconds");

    assert!(uptime_metric.is_some(), "Uptime metric should exist");
    if let Some(metric) = uptime_metric {
        let metrics = metric.get_metric();
        assert!(
            !metrics.is_empty(),
            "Uptime should have at least one sample"
        );
    }

    pool.close().await;
    Ok(())
}
