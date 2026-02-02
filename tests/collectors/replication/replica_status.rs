use super::super::common;
use anyhow::Result;
use mariadb_exporter::collectors::Collector;
use mariadb_exporter::collectors::replication::ReplicationCollector;
use prometheus::Registry;

#[tokio::test]
async fn test_replication_collector_registers_without_error() -> Result<()> {
    let collector = ReplicationCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    Ok(())
}

#[tokio::test]
async fn test_replication_collector_handles_no_replication() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = ReplicationCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Should not panic on servers without replication configured
    let result = collector.collect(&pool).await;
    assert!(
        result.is_ok(),
        "Collector should handle servers without replication gracefully"
    );

    let metric_families = registry.gather();
    let configured_metric = metric_families
        .iter()
        .find(|m| m.name() == "mariadb_replica_configured");
    assert!(
        configured_metric.is_some(),
        "mariadb_replica_configured should be registered"
    );

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn test_replication_collector_handles_privilege_errors() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = ReplicationCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Should handle REPLICATION CLIENT privilege errors gracefully
    let result = collector.collect(&pool).await;
    assert!(result.is_ok(), "Should handle privilege errors gracefully");

    pool.close().await;
    Ok(())
}
