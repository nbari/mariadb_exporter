//! Integration tests for the `system.process` sub-collector.
//!
//! The collector aggregates the `mariadbd`/`mysqld` process group on the host
//! running the exporter. In CI the database usually runs in a container, so the
//! group is frequently empty — that is a *fresh* zero, not a skip, and the tests
//! assert the shape of the published series rather than a specific value.

use super::super::common;
use anyhow::Result;
use mariadb_exporter::collectors::Collector;
use mariadb_exporter::collectors::system::process::ProcessGroupCollector;
use prometheus::Registry;

const SUPPORTED: bool = cfg!(any(target_os = "linux", target_os = "freebsd"));

/// Registration test.
#[tokio::test]
async fn test_process_collector_registers_without_error() -> Result<()> {
    let collector = ProcessGroupCollector::new();
    let registry = Registry::new();

    assert!(collector.register_metrics(&registry).is_ok());

    Ok(())
}

/// Collection test: on a supported platform the group series are published with
/// the documented `group` label.
#[tokio::test]
async fn test_process_collector_collects_successfully() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = ProcessGroupCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;

    let families = registry.gather();

    if !SUPPORTED {
        assert!(
            families.is_empty(),
            "unsupported platforms must publish nothing rather than a fake zero"
        );
        return Ok(());
    }

    for name in [
        "mariadb_system_process_group_cpu_seconds_total",
        "mariadb_system_process_group_memory_bytes",
        "mariadb_system_process_group_count",
    ] {
        let Some(family) = families.iter().find(|f| f.name() == name) else {
            panic!("{name} should be published on a supported platform");
        };
        let Some(metric) = family.get_metric().first() else {
            panic!("{name} should carry at least one series");
        };
        let labels: Vec<(&str, &str)> = metric
            .get_label()
            .iter()
            .map(|l| (l.name(), l.value()))
            .collect();
        assert_eq!(labels, vec![("group", "mariadb")]);
    }

    Ok(())
}

/// Feature-availability test: an unreadable `/proc` entry (a process that exits
/// mid-scan, a hardened container) must never fail the scrape.
#[tokio::test]
async fn test_process_collector_is_infallible() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = ProcessGroupCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    for _ in 0..3 {
        assert!(collector.collect(&pool).await.is_ok());
    }

    Ok(())
}

/// Settlement test: `reset_metrics` removes the group series entirely, so a host
/// where the process group can no longer be observed stops serving stale values.
#[tokio::test]
async fn test_process_reset_metrics_removes_group_series() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = ProcessGroupCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;

    Collector::reset_metrics(&collector);

    let families = registry.gather();
    for name in [
        "mariadb_system_process_group_cpu_seconds_total",
        "mariadb_system_process_group_memory_bytes",
        "mariadb_system_process_group_count",
    ] {
        assert!(
            !families.iter().any(|f| f.name() == name),
            "{name} must disappear after a skip is settled"
        );
    }

    Ok(())
}

/// Edge-case test: a scrape matching zero processes is a *fresh* zero, not a
/// removal — the exporter can honestly say "no `MariaDB` process here".
#[tokio::test]
async fn test_process_empty_group_is_a_fresh_zero() -> Result<()> {
    if !SUPPORTED {
        return Ok(());
    }

    let pool = common::create_test_pool().await?;
    let collector = ProcessGroupCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;

    let count = registry
        .gather()
        .iter()
        .find(|f| f.name() == "mariadb_system_process_group_count")
        .and_then(|f| f.get_metric().first())
        .map(|m| m.get_gauge().value());

    assert!(
        count.is_some_and(|v| v >= 0.0),
        "the group count must always be published on a supported platform"
    );

    Ok(())
}

/// Edge-case test: the CPU counter is monotonic across scrapes.
#[tokio::test]
async fn test_process_cpu_counter_never_decreases() -> Result<()> {
    if !SUPPORTED {
        return Ok(());
    }

    let pool = common::create_test_pool().await?;
    let collector = ProcessGroupCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    let read = || {
        registry
            .gather()
            .iter()
            .find(|f| f.name() == "mariadb_system_process_group_cpu_seconds_total")
            .and_then(|f| f.get_metric().first())
            .map(|m| m.get_counter().value())
            .unwrap_or_default()
    };

    collector.collect(&pool).await?;
    let first = read();
    collector.collect(&pool).await?;
    let second = read();

    assert!(second >= first, "process CPU counter must be monotonic");

    Ok(())
}

/// Type-compatibility test: the label set stays bounded to exactly one series
/// regardless of how many processes are in the group.
#[tokio::test]
async fn test_process_label_set_stays_bounded() -> Result<()> {
    if !SUPPORTED {
        return Ok(());
    }

    let pool = common::create_test_pool().await?;
    let collector = ProcessGroupCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;
    collector.collect(&pool).await?;

    let series = registry
        .gather()
        .iter()
        .find(|f| f.name() == "mariadb_system_process_group_count")
        .map_or(0, |f| f.get_metric().len());

    assert_eq!(
        series, 1,
        "the process group must be a single aggregate series"
    );

    Ok(())
}

#[tokio::test]
async fn test_process_collector_is_disabled_by_default() {
    assert!(!ProcessGroupCollector::new().enabled_by_default());
}
