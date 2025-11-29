use super::super::common;
use anyhow::Result;
use mariadb_exporter::collectors::Collector;
use mariadb_exporter::collectors::statements::StatementsCollector;
use prometheus::Registry;

#[tokio::test]
async fn test_statements_collector_registers_without_error() -> Result<()> {
    let collector = StatementsCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    Ok(())
}

#[tokio::test]
async fn test_statements_collector_has_all_metrics() -> Result<()> {
    let pool = common::create_test_pool().await?;

    // Check if performance_schema is available
    let perf_schema_check = common::table_exists(
        &pool,
        "performance_schema",
        "events_statements_summary_by_digest",
    )
    .await?;

    if !perf_schema_check {
        println!(
            "performance_schema.events_statements_summary_by_digest not available, skipping test"
        );
        pool.close().await;
        return Ok(());
    }

    let collector = StatementsCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;

    let metric_families = registry.gather();

    let expected_metrics = vec![
        "mariadb_perf_schema_digest_total",
        "mariadb_perf_schema_digest_errors_total",
        "mariadb_perf_schema_digest_warnings_total",
        "mariadb_perf_schema_digest_rows_examined_total",
        "mariadb_perf_schema_digest_rows_sent_total",
        "mariadb_perf_schema_digest_latency_seconds_total",
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
async fn test_statements_collector_gracefully_handles_missing_performance_schema() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = StatementsCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Should not panic even if performance_schema is missing or disabled
    let result = collector.collect(&pool).await;
    assert!(
        result.is_ok(),
        "Collector should handle missing performance_schema gracefully"
    );

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn test_statements_collector_with_realistic_workload() -> Result<()> {
    let pool = common::create_test_pool().await?;

    // Check if performance_schema is available
    let perf_schema_check = common::table_exists(
        &pool,
        "performance_schema",
        "events_statements_summary_by_digest",
    )
    .await?;

    if !perf_schema_check {
        println!("performance_schema not available, skipping test");
        pool.close().await;
        return Ok(());
    }

    // Generate some queries to populate performance_schema
    let _ = sqlx::query("SELECT 1").fetch_one(&pool).await;
    let _ = sqlx::query("SELECT 2").fetch_one(&pool).await;
    let _ = sqlx::query("SELECT VERSION()").fetch_one(&pool).await;

    let collector = StatementsCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;

    let metric_families = registry.gather();

    // Should have collected digest data
    let digest_total = metric_families
        .iter()
        .find(|m| m.name() == "mariadb_perf_schema_digest_total");

    assert!(digest_total.is_some(), "Digest total metric should exist");
    if let Some(metric) = digest_total {
        let metrics = metric.get_metric();
        assert!(
            !metrics.is_empty(),
            "Digest total should have at least one sample"
        );
    }

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn test_statements_collector_handles_null_values() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let perf_schema_check = common::table_exists(
        &pool,
        "performance_schema",
        "events_statements_summary_by_digest",
    )
    .await?;

    if !perf_schema_check {
        println!("performance_schema not available, skipping test");
        pool.close().await;
        return Ok(());
    }

    let collector = StatementsCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Should handle NULL digest or schema names gracefully
    let result = collector.collect(&pool).await;
    assert!(
        result.is_ok(),
        "Should handle NULL values in performance_schema"
    );

    pool.close().await;
    Ok(())
}
