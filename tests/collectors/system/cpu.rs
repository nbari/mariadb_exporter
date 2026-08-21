//! Integration tests for the `system.cpu` sub-collector.
//!
//! These exercise the collector against the real host operating system, since
//! `system` reads `/proc` (or FreeBSD sysctls) rather than `MariaDB`. A pool is
//! still passed through `collect()` to prove the collector never touches it.

use super::super::common;
use anyhow::Result;
use mariadb_exporter::collectors::Collector;
use mariadb_exporter::collectors::system::cpu::CpuCollector;
use prometheus::Registry;

/// Registration test: every CPU metric family is declared without error.
#[tokio::test]
async fn test_cpu_collector_registers_without_error() -> Result<()> {
    let collector = CpuCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // The scalar-style metrics are zero-label vectors so a skip can remove them,
    // which means nothing is gathered until a value is published.
    let pool = common::create_test_pool().await?;
    collector.collect(&pool).await?;

    let families = registry.gather();
    for name in [
        "mariadb_system_cpu_cores",
        "mariadb_system_load1",
        "mariadb_system_load5",
        "mariadb_system_load15",
    ] {
        assert!(
            families.iter().any(|f| f.name() == name),
            "{name} should be published"
        );
    }

    Ok(())
}

/// Collection test: a real scrape populates plausible values.
#[tokio::test]
async fn test_cpu_collector_collects_successfully() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = CpuCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;

    let families = registry.gather();

    let cores = families
        .iter()
        .find(|f| f.name() == "mariadb_system_cpu_cores")
        .and_then(|f| f.get_metric().first())
        .map(|m| m.get_gauge().value())
        .unwrap_or_default();
    assert!(cores >= 1.0, "host should report at least one logical core");

    let load1 = families
        .iter()
        .find(|f| f.name() == "mariadb_system_load1")
        .and_then(|f| f.get_metric().first())
        .map_or(-1.0, |m| m.get_gauge().value());
    assert!(load1 >= 0.0, "load average must not be negative");

    Ok(())
}

/// Feature-availability test: the collector never returns `Err` for an
/// unreadable host source, because an optional host collector must not withhold
/// the database-dependent registry families for the whole scrape.
#[tokio::test]
async fn test_cpu_collector_is_infallible() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = CpuCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    for _ in 0..3 {
        assert!(
            collector.collect(&pool).await.is_ok(),
            "system.cpu must degrade gracefully instead of failing the scrape"
        );
    }

    Ok(())
}

/// Settlement test: `reset_metrics` removes every per-core series so a host that
/// stops reporting per-CPU data does not serve a frozen snapshot.
#[tokio::test]
async fn test_cpu_reset_metrics_removes_per_core_series() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = CpuCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;

    Collector::reset_metrics(&collector);

    let families = registry.gather();
    assert!(
        !families
            .iter()
            .any(|f| f.name() == "mariadb_system_cpu_seconds_total"),
        "per-core CPU series must disappear after a reset"
    );

    Ok(())
}

/// Edge-case test: counters are monotonic across repeated scrapes.
#[tokio::test]
async fn test_cpu_counters_never_decrease() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = CpuCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    let read_total = || {
        registry
            .gather()
            .iter()
            .find(|f| f.name() == "mariadb_system_cpu_seconds_total")
            .map(|f| {
                f.get_metric()
                    .iter()
                    .map(|m| m.get_counter().value())
                    .sum::<f64>()
            })
            .unwrap_or_default()
    };

    collector.collect(&pool).await?;
    let first = read_total();
    collector.collect(&pool).await?;
    let second = read_total();

    assert!(
        second >= first,
        "CPU counters must be monotonic ({second} < {first})"
    );

    Ok(())
}

/// Type-compatibility test: the CPU time series is a counter with the documented
/// `cpu` and `mode` labels.
#[tokio::test]
async fn test_cpu_seconds_wire_format() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = CpuCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;
    collector.collect(&pool).await?;

    let families = registry.gather();
    let Some(family) = families
        .iter()
        .find(|f| f.name() == "mariadb_system_cpu_seconds_total")
    else {
        // Platforms without per-core data legitimately publish nothing.
        return Ok(());
    };

    assert_eq!(
        family.get_field_type(),
        prometheus::proto::MetricType::COUNTER
    );

    let Some(metric) = family.get_metric().first() else {
        return Ok(());
    };
    let labels: Vec<&str> = metric
        .get_label()
        .iter()
        .map(prometheus::proto::LabelPair::name)
        .collect();
    assert_eq!(labels, vec!["cpu", "mode"]);

    Ok(())
}

/// The collector is opt-in; enabling it by default would mislead users running
/// the exporter away from the database host.
#[tokio::test]
async fn test_cpu_collector_is_disabled_by_default() {
    assert!(!CpuCollector::new().enabled_by_default());
}
