use super::super::common;
use anyhow::Result;
use mariadb_exporter::collectors::Collector;
use mariadb_exporter::collectors::tls::TlsCollector;
use prometheus::Registry;

#[tokio::test]
async fn test_tls_collector_registers_without_error() -> Result<()> {
    let collector = TlsCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    Ok(())
}

#[tokio::test]
async fn test_tls_collector_handles_no_tls() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let collector = TlsCollector::new();
    let registry = Registry::new();

    collector.register_metrics(&registry)?;

    // Should not panic on servers without TLS configured
    let result = collector.collect(&pool).await;
    assert!(
        result.is_ok(),
        "Collector should handle servers without TLS gracefully"
    );

    pool.close().await;
    Ok(())
}
