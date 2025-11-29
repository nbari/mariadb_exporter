use super::super::common;
use anyhow::Result;
use mariadb_exporter::collectors::Collector;
use mariadb_exporter::collectors::metadata::MetadataCollector;
use prometheus::Registry;

#[tokio::test]
async fn test_metadata_collector_registers_without_error() -> Result<()> {
    let collector = MetadataCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    Ok(())
}

#[tokio::test]
async fn test_metadata_collector_handles_missing_performance_schema() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = MetadataCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Should not panic if performance_schema.metadata_locks not available
    let result = collector.collect(&pool).await;
    assert!(
        result.is_ok(),
        "Collector should handle missing performance_schema tables"
    );

    pool.close().await;
    Ok(())
}
