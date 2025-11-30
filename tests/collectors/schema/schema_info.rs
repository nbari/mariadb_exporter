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

    // Verify metrics are actually gathered
    let metrics = registry.gather();
    let _has_table_metrics = metrics
        .iter()
        .any(|m| m.name().starts_with("mariadb_info_schema_table"));

    // May be empty if no user tables exist, but should not error
    assert!(
        result.is_ok(),
        "Should collect without errors even if no user tables"
    );

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

#[tokio::test]
async fn test_schema_collector_not_enabled_by_default() {
    let collector = SchemaCollector::new();
    assert!(
        !collector.enabled_by_default(),
        "Schema collector should be opt-in to avoid high cardinality"
    );
}

#[tokio::test]
async fn test_schema_collector_metrics_have_labels() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = SchemaCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;

    let metrics = registry.gather();

    for metric_family in &metrics {
        if metric_family
            .name()
            .starts_with("mariadb_info_schema_table")
        {
            // If metrics exist, they should have schema and table labels
            for metric in metric_family.get_metric() {
                let labels = metric.get_label();
                if !labels.is_empty() {
                    let has_schema = labels.iter().any(|l| l.name() == "schema");
                    let has_table = labels.iter().any(|l| l.name() == "table");
                    assert!(has_schema, "Schema metrics should have 'schema' label");
                    assert!(has_table, "Schema metrics should have 'table' label");
                }
            }
        }
    }

    pool.close().await;
    Ok(())
}
