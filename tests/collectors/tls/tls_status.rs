use super::super::common;
use anyhow::Result;
use mariadb_exporter::collectors::Collector;
use mariadb_exporter::collectors::tls::TlsCollector;
use prometheus::Registry;

#[tokio::test]
async fn test_tls_collector_registers_without_error() -> Result<()> {
    let collector = TlsCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    Ok(())
}

#[tokio::test]
async fn test_tls_collector_collects_metrics() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = TlsCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Collect metrics
    let result = collector.collect(&pool).await;
    assert!(result.is_ok(), "Collector should succeed");

    // Get metrics
    let metrics = registry.gather();
    let metric_names: Vec<String> = metrics.iter().map(|m| m.name().to_string()).collect();

    // Verify the base metric is always present (indicates SSL configured or not)
    assert!(
        metric_names.contains(&"mariadb_ssl_server_configured".to_string()),
        "Should have ssl_server_configured metric"
    );

    // Note: Other SSL metrics (version_info, cert timestamps) are only present when SSL is actually configured
    // The test passes as long as collection succeeds and the base metric exists

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn test_tls_collector_handles_no_tls() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = TlsCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Should not panic on servers without TLS configured
    let result = collector.collect(&pool).await;
    assert!(
        result.is_ok(),
        "Collector should handle servers without TLS gracefully"
    );

    pool.close().await;
    Ok(())
}
