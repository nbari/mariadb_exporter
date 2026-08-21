//! Integration tests for the `system` umbrella collector.

use super::super::common;
use anyhow::Result;
use mariadb_exporter::collectors::system::SystemCollector;
use mariadb_exporter::collectors::{Collector, all_factories};
use prometheus::Registry;

/// The umbrella is registered under the exact module name so
/// `--collector.system` / `--no-collector.system` resolve.
#[tokio::test]
async fn test_system_collector_is_registered_in_the_factory() {
    let factories = all_factories();
    assert!(
        factories.iter().any(|(name, _)| *name == "system"),
        "the system collector must be reachable through all_factories()"
    );
    assert_eq!(SystemCollector::new().name(), "system");
}

/// Registration test: every child registers through the umbrella.
#[tokio::test]
async fn test_system_collector_registers_without_error() -> Result<()> {
    let collector = SystemCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    Ok(())
}

/// Collection test: one umbrella scrape publishes metrics from every child.
#[tokio::test]
async fn test_system_collector_collects_every_child() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = SystemCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;

    let families = registry.gather();

    // cpu child
    assert!(
        families
            .iter()
            .any(|f| f.name() == "mariadb_system_cpu_cores"),
        "the cpu child should have published"
    );
    // memory child
    assert!(
        families
            .iter()
            .any(|f| f.name() == "mariadb_system_memory_total_bytes"),
        "the memory child should have published"
    );

    Ok(())
}

/// Settlement test: the umbrella never reports `Skipped`, so a child that is
/// unavailable on this host cannot clear its fresh siblings.
#[tokio::test]
async fn test_skipped_child_does_not_clear_fresh_siblings() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = SystemCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;

    // Settle only the process child by hand, exactly as a skip would.
    let process = mariadb_exporter::collectors::system::process::ProcessGroupCollector::new();
    Collector::reset_metrics(&process);

    let families = registry.gather();
    assert!(
        families
            .iter()
            .any(|f| f.name() == "mariadb_system_memory_total_bytes"),
        "memory metrics must survive a sibling's skip"
    );
    assert!(
        families
            .iter()
            .any(|f| f.name() == "mariadb_system_cpu_cores"),
        "cpu metrics must survive a sibling's skip"
    );

    Ok(())
}

/// The umbrella `reset_metrics` fans out to every child.
#[tokio::test]
async fn test_system_reset_metrics_fans_out() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = SystemCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;

    Collector::reset_metrics(&collector);

    let families = registry.gather();
    assert!(
        !families
            .iter()
            .any(|f| f.name() == "mariadb_system_cpu_seconds_total"),
        "the cpu child should have been reset through the umbrella"
    );
    assert!(
        !families
            .iter()
            .any(|f| f.name() == "mariadb_system_process_group_count"),
        "the process child should have been reset through the umbrella"
    );

    Ok(())
}

/// Feature-availability test: the umbrella never fails a scrape, so enabling it
/// on an unsupported host cannot withhold the database-dependent families.
#[tokio::test]
async fn test_system_collector_never_fails_a_scrape() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = SystemCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    for _ in 0..3 {
        assert!(collector.collect(&pool).await.is_ok());
    }

    Ok(())
}

/// The collector must stay opt-in: it describes the exporter's host, which is
/// only the database host when the two are co-located.
#[tokio::test]
async fn test_system_collector_is_disabled_by_default() {
    assert!(!SystemCollector::new().enabled_by_default());
}
