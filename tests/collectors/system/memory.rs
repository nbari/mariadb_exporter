//! Integration tests for the `system.memory` sub-collector.

use super::super::common;
use anyhow::Result;
use mariadb_exporter::collectors::Collector;
use mariadb_exporter::collectors::system::memory::MemoryCollector;
use prometheus::Registry;

fn gauge(registry: &Registry, name: &str) -> Option<f64> {
    registry
        .gather()
        .iter()
        .find(|f| f.name() == name)
        .and_then(|f| f.get_metric().first())
        .map(|m| m.get_gauge().value())
}

/// Registration test.
#[tokio::test]
async fn test_memory_collector_registers_without_error() -> Result<()> {
    let collector = MemoryCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    for name in [
        "mariadb_system_memory_total_bytes",
        "mariadb_system_memory_used_bytes",
        "mariadb_system_memory_free_bytes",
        "mariadb_system_memory_available_bytes",
        "mariadb_system_swap_total_bytes",
        "mariadb_system_swap_used_bytes",
        "mariadb_system_swap_free_bytes",
    ] {
        assert!(
            registry.gather().iter().any(|f| f.name() == name),
            "{name} should be registered"
        );
    }

    Ok(())
}

/// Collection test: real host memory is reported and is internally consistent.
#[tokio::test]
async fn test_memory_collector_collects_successfully() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = MemoryCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;

    let total = gauge(&registry, "mariadb_system_memory_total_bytes").unwrap_or_default();
    let used = gauge(&registry, "mariadb_system_memory_used_bytes").unwrap_or_default();
    let free = gauge(&registry, "mariadb_system_memory_free_bytes").unwrap_or_default();

    assert!(total > 0.0, "host must report some physical memory");
    assert!(
        used >= 0.0 && used <= total,
        "used memory must fit in total"
    );
    assert!(
        free >= 0.0 && free <= total,
        "free memory must fit in total"
    );

    Ok(())
}

/// Feature-availability test: host memory is always readable through `sysinfo`,
/// so the collector never fails a scrape.
#[tokio::test]
async fn test_memory_collector_is_infallible() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = MemoryCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    for _ in 0..3 {
        assert!(collector.collect(&pool).await.is_ok());
    }

    Ok(())
}

/// Settlement test: memory is never `Skipped`, so `reset_metrics` is a
/// documented no-op and the gauges stay published.
#[tokio::test]
async fn test_memory_reset_metrics_is_a_no_op() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = MemoryCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;

    Collector::reset_metrics(&collector);

    assert!(
        registry
            .gather()
            .iter()
            .any(|f| f.name() == "mariadb_system_memory_total_bytes"),
        "memory gauges are always Fresh and must survive a reset"
    );

    Ok(())
}

/// Edge-case test: repeated scrapes stay stable and non-negative.
#[tokio::test]
async fn test_memory_repeated_collection_is_stable() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = MemoryCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    collector.collect(&pool).await?;
    let first = gauge(&registry, "mariadb_system_memory_total_bytes").unwrap_or_default();
    collector.collect(&pool).await?;
    let second = gauge(&registry, "mariadb_system_memory_total_bytes").unwrap_or_default();

    assert!(
        (first - second).abs() < f64::EPSILON,
        "total physical memory should not change between scrapes ({first} vs {second})"
    );

    Ok(())
}

/// Swap may legitimately be absent (containers, swapless hosts): zero is a
/// factual reading, not a skip.
#[tokio::test]
async fn test_memory_swap_zero_is_reported_as_a_value() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = MemoryCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;

    let swap_total = gauge(&registry, "mariadb_system_swap_total_bytes");
    assert!(
        swap_total.is_some_and(|v| v >= 0.0),
        "swap total must always be published, even when it is zero"
    );

    Ok(())
}

#[tokio::test]
async fn test_memory_collector_is_disabled_by_default() {
    assert!(!MemoryCollector::new().enabled_by_default());
}
