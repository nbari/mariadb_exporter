use super::super::common;
use anyhow::Result;
use mariadb_exporter::collectors::Collector;
use mariadb_exporter::collectors::userstat::UserStatCollector;
use prometheus::Registry;

#[tokio::test]
async fn test_userstat_collector_registers_without_error() -> Result<()> {
    let collector = UserStatCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    Ok(())
}

#[tokio::test]
async fn test_userstat_collector_handles_disabled_userstat() -> Result<()> {
    let pool = common::create_test_pool().await?;

    // Check if userstat is enabled
    let userstat_enabled = common::variable_enabled(&pool, "userstat")
        .await
        .unwrap_or(false);

    let collector = UserStatCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Should not panic whether userstat is enabled or not
    let result = collector.collect(&pool).await;
    assert!(
        result.is_ok(),
        "Collector should handle disabled userstat gracefully"
    );

    if !userstat_enabled {
        println!("userstat is disabled, collector should handle gracefully");
    }

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn test_userstat_collector_handles_missing_tables() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = UserStatCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Should handle missing information_schema.user_statistics table
    let result = collector.collect(&pool).await;
    assert!(result.is_ok(), "Should handle missing userstat tables");

    pool.close().await;
    Ok(())
}
