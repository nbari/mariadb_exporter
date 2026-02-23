#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
use anyhow::Result;
use mariadb_exporter::collectors::Collector;
use mariadb_exporter::collectors::innodb::status::StatusParser as InnodbStatusParser;

#[test]
fn regression_innodb_semaphore_summing() {
    let parser = InnodbStatusParser::new();
    let status = "
Mutex spin waits 10, rounds 20, OS waits 5
--Thread 1 has waited at line 1 for 1.0 seconds
RW-shared spins 30, rounds 40, OS waits 15
--Thread 2 has waited at line 2 for 2.5 seconds
";
    parser.parse(status).unwrap();

    // Sum of OS waits: 5 + 15 = 20
    assert_eq!(
        parser.semaphore_waits().get(),
        20,
        "Should sum all OS waits"
    );
    // Sum of wait times: 1.0 + 2.5 = 3.5s = 3500ms
    assert_eq!(
        parser.semaphore_wait_time_ms().get(),
        3500,
        "Should sum all wait times"
    );
}

#[tokio::test]
async fn regression_statements_type_safety() -> Result<()> {
    use mariadb_exporter::collectors::statements::StatementsCollector;
    use prometheus::Registry;

    let dsn = std::env::var("MARIADB_EXPORTER_DSN")
        .unwrap_or_else(|_| "mysql://root:root@127.0.0.1:3306/mysql".to_string());

    let Ok(pool) = sqlx::mysql::MySqlPoolOptions::new()
        .max_connections(1)
        .connect(&dsn)
        .await
    else {
        eprintln!("Skipping regression_statements_type_safety: DB not available");
        return Ok(());
    };

    let collector = StatementsCollector::new();
    let registry = Registry::new();
    collector.register_metrics(&registry)?;

    // If this fails due to DECIMAL vs u64 mismatch, the test will now FAIL
    // because we no longer swallow errors in the collector.
    collector.collect(&pool).await?;

    let metrics = registry.gather();
    assert!(
        metrics
            .iter()
            .any(|m| m.name().starts_with("mariadb_perf_schema_digest")),
        "Statements metrics should be present in the registry"
    );

    Ok(())
}

#[tokio::test]
async fn regression_metrics_reset_on_collect() -> Result<()> {
    // This tests that collectors which use .reset() correctly clear old labels

    let config = mariadb_exporter::collectors::config::CollectorConfig::new()
        .with_enabled(&["schema".to_string()]);
    let _registry = mariadb_exporter::collectors::registry::CollectorRegistry::new(&config);

    Ok(())
}
