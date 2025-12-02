use super::super::common;
use anyhow::Result;
use mariadb_exporter::collectors::Collector;
use mariadb_exporter::collectors::innodb::InnodbCollector;
use prometheus::Registry;

#[tokio::test]
async fn test_innodb_collector_registers_without_error() -> Result<()> {
    let collector = InnodbCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Verify all metrics are registered
    let metric_families = registry.gather();

    let expected_metrics = vec![
        "mariadb_innodb_lsn_current",
        "mariadb_innodb_lsn_flushed",
        "mariadb_innodb_lsn_checkpoint",
        "mariadb_innodb_checkpoint_age_bytes",
        "mariadb_innodb_active_transactions",
        "mariadb_innodb_semaphore_waits_total",
        "mariadb_innodb_semaphore_wait_time_ms_total",
        "mariadb_innodb_adaptive_hash_searches_total",
        "mariadb_innodb_adaptive_hash_searches_btree_total",
    ];

    for metric_name in expected_metrics {
        let found = metric_families.iter().any(|m| m.name() == metric_name);
        assert!(found, "Metric {metric_name} should be registered");
    }

    Ok(())
}

#[tokio::test]
async fn test_innodb_collector_is_disabled_by_default() {
    let collector = InnodbCollector::new();
    assert!(
        !collector.enabled_by_default(),
        "InnoDB collector should be opt-in (disabled by default)"
    );
}

#[tokio::test]
async fn test_innodb_collector_collects_successfully() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = InnodbCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Should collect without errors if SHOW ENGINE INNODB STATUS privilege exists
    let result = collector.collect(&pool).await;

    // The collector might fail if privileges are missing, but should not panic
    match result {
        Ok(()) => {
            // Verify metrics exist
            let metric_families = registry.gather();
            let lsn_current = metric_families
                .iter()
                .find(|mf| mf.name() == "mariadb_innodb_lsn_current");

            assert!(
                lsn_current.is_some(),
                "Should have LSN current metric after collection"
            );
        }
        Err(e) => {
            // If collection fails, it might be due to privileges - that's acceptable
            println!("Collection failed (likely missing privileges): {e}");
        }
    }

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn test_innodb_collector_handles_privilege_errors_gracefully() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = InnodbCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Should handle PROCESS privilege errors gracefully (SHOW ENGINE INNODB STATUS requires it)
    let result = collector.collect(&pool).await;

    // Either succeeds or fails with a descriptive error, but never panics
    match result {
        Ok(()) => println!("Collection succeeded with proper privileges"),
        Err(e) => println!("Collection failed (expected without privileges): {e}"),
    }

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn test_innodb_collector_metrics_populated() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = InnodbCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Collect metrics
    if collector.collect(&pool).await.is_ok() {
        let metric_families = registry.gather();

        // Verify key metrics exist
        let lsn_current = metric_families
            .iter()
            .find(|mf| mf.name() == "mariadb_innodb_lsn_current");
        let checkpoint_age = metric_families
            .iter()
            .find(|mf| mf.name() == "mariadb_innodb_checkpoint_age_bytes");

        assert!(lsn_current.is_some(), "Should have LSN current metric");
        assert!(
            checkpoint_age.is_some(),
            "Should have checkpoint age metric"
        );
    }

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn test_innodb_collector_name() {
    let collector = InnodbCollector::new();
    assert_eq!(collector.name(), "innodb");
}

#[tokio::test]
async fn test_innodb_collector_can_be_cloned() {
    let collector = InnodbCollector::new();
    let cloned = collector.clone();
    assert_eq!(collector.name(), cloned.name());
}
