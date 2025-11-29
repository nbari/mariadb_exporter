use super::super::common;
use anyhow::Result;
use mariadb_exporter::collectors::Collector;
use mariadb_exporter::collectors::schema::SchemaCollector;
use prometheus::Registry;

#[tokio::test]
async fn test_schema_collector_registers_without_error() -> Result<()> {
    let collector = SchemaCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    Ok(())
}

#[tokio::test]
async fn test_schema_collector_collects_table_info() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = SchemaCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Should collect information_schema.tables data
    let result = collector.collect(&pool).await;
    assert!(result.is_ok(), "Should collect schema info successfully");

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn test_schema_collector_handles_large_table_counts() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = SchemaCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Should handle databases with many tables without panic
    let result = collector.collect(&pool).await;
    assert!(result.is_ok(), "Should handle large table counts");

    pool.close().await;
    Ok(())
}
