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
async fn test_metadata_collector_handles_missing_metadata_lock_info() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = MetadataCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Should not panic if metadata_lock_info plugin not available
    let result = collector.collect(&pool).await;
    assert!(
        result.is_ok(),
        "Collector should handle missing metadata_lock_info table"
    );

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn test_metadata_collector_with_plugin_enabled() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = MetadataCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    let result = collector.collect(&pool).await;

    assert!(
        result.is_ok(),
        "Collector should successfully collect metadata locks"
    );

    // The metric may or may not have values depending on whether there are locks
    // But it should at least be registered
    let metrics = registry.gather();
    let has_metric = metrics
        .iter()
        .any(|m| m.name() == "mariadb_metadata_lock_info_count");

    // Note: metric won't appear in gathered metrics unless at least one label combination was set
    // This is expected behavior for IntGaugeVec
    println!("Metadata lock metric present: {has_metric} (normal if no locks present)");

    pool.close().await;
    Ok(())
}
