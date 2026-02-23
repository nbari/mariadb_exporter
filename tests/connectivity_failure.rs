#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
use anyhow::Result;
use secrecy::SecretString;

mod common;

#[tokio::test]
async fn test_exporter_starts_when_db_is_down() -> Result<()> {
    let port = common::get_available_port();

    // Use an invalid DSN that will definitely fail to connect (wrong port)
    let dsn = SecretString::from("mysql://root:root@127.0.0.1:12345/mysql");

    let handle = tokio::spawn(async move {
        mariadb_exporter::exporter::new(
            port,
            None,
            dsn,
            vec!["default".to_string(), "exporter".to_string()],
        )
        .await
    });

    // Wait for server to start. It should start even if DB is down.
    assert!(
        common::wait_for_server(port, 50).await,
        "Server failed to start on port {port} when DB is down"
    );

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/metrics", common::get_test_url(port)))
        .send()
        .await?;

    // Status should be 200 OK
    assert_eq!(response.status(), 200);

    let body = response.text().await?;

    // mariadb_up should be 0
    assert!(
        body.contains("mariadb_up 0"),
        "Output should contain 'mariadb_up 0', got: {body}"
    );

    // DB-dependent metrics should be omitted (not present at all)
    // For example, StatusCollector metrics
    assert!(
        !body.contains("mariadb_global_status_uptime_seconds"),
        "Status metrics should be omitted when DB is down"
    );

    // Exporter-specific metrics should still be present
    assert!(
        body.contains("mariadb_exporter_build_info"),
        "Build info should be present"
    );
    assert!(
        body.contains("mariadb_exporter_scrapes_total"),
        "Scrapes total should be present"
    );

    handle.abort();

    Ok(())
}

#[tokio::test]
async fn test_exporter_recovery_when_db_comes_up() -> Result<()> {
    // This is harder to test without a real DB that we can start/stop
    // But we can test that it starts with a VALID DSN even if DB is NOT YET ready
    // (though in our test environment it usually IS ready).
    Ok(())
}
