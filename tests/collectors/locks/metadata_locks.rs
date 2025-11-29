use super::super::common;
use anyhow::Result;
use mariadb_exporter::collectors::Collector;
use mariadb_exporter::collectors::locks::LocksCollector;
use prometheus::Registry;

#[tokio::test]
async fn test_locks_collector_registers_without_error() -> Result<()> {
    let collector = LocksCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    Ok(())
}

#[tokio::test]
async fn test_locks_collector_handles_missing_metadata_lock_info() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = LocksCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Should not panic if metadata_lock_info plugin not installed
    let result = collector.collect(&pool).await;
    assert!(
        result.is_ok(),
        "Collector should handle missing metadata_lock_info plugin"
    );

    pool.close().await;
    Ok(())
}
