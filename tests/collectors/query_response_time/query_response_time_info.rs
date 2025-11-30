use super::super::common;
use anyhow::Result;
use mariadb_exporter::collectors::Collector;
use mariadb_exporter::collectors::query_response_time::QueryResponseTimeCollector;
use prometheus::Registry;

#[tokio::test]
async fn test_query_response_time_collector_registers_without_error() -> Result<()> {
    let collector = QueryResponseTimeCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    Ok(())
}

#[tokio::test]
async fn test_query_response_time_collector_collects_metrics() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = QueryResponseTimeCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Should collect query response time data (may be empty if not enabled)
    let result = collector.collect(&pool).await;
    assert!(
        result.is_ok(),
        "Should collect query response time info successfully"
    );

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn test_query_response_time_collector_handles_missing_table() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = QueryResponseTimeCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Should gracefully handle if QUERY_RESPONSE_TIME table doesn't exist or is empty
    let result = collector.collect(&pool).await;
    assert!(
        result.is_ok(),
        "Should handle missing/empty response time data"
    );

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn test_query_response_time_enabled_by_default() {
    let collector = QueryResponseTimeCollector::new();
    assert!(
        !collector.enabled_by_default(),
        "Query response time collector should be opt-in"
    );
}
