use anyhow::Result;
use sqlx::mysql::{MySqlPool, MySqlPoolOptions};
use std::env;
use std::net::TcpListener;
use std::time::Duration;
use tokio::time::sleep;

/// Get DSN from environment or use default
pub fn get_test_dsn() -> String {
    env::var("MARIADB_EXPORTER_DSN")
        .unwrap_or_else(|_| "mysql://root:root@127.0.0.1:3306/mysql".to_string())
}

/// Create a test database pool
pub async fn create_test_pool() -> Result<MySqlPool> {
    let dsn = get_test_dsn();

    let pool = MySqlPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&dsn)
        .await?;

    Ok(pool)
}

/// Get an available port for testing
#[allow(dead_code)]
pub fn get_available_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("Failed to bind to ephemeral port")
        .local_addr()
        .expect("Failed to get local address")
        .port()
}

/// Build test URL for HTTP requests
#[allow(dead_code)]
pub fn get_test_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// Wait for server to be ready
#[allow(dead_code)]
pub async fn wait_for_server(port: u16, max_attempts: u32) -> bool {
    for _ in 0..max_attempts {
        if tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .is_ok()
        {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
}

/// Check if a specific table exists
#[allow(dead_code)]
pub async fn table_exists(pool: &MySqlPool, schema: &str, table: &str) -> Result<bool> {
    let result: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM information_schema.tables 
         WHERE table_schema = ? AND table_name = ? LIMIT 1",
    )
    .bind(schema)
    .bind(table)
    .fetch_optional(pool)
    .await?;

    Ok(result.is_some())
}

/// Check if a plugin is installed
#[allow(dead_code)]
pub async fn plugin_installed(pool: &MySqlPool, plugin_name: &str) -> Result<bool> {
    let result: Option<(String,)> = sqlx::query_as(
        "SELECT plugin_name FROM information_schema.plugins WHERE plugin_name = ? AND plugin_status = 'ACTIVE'"
    )
    .bind(plugin_name)
    .fetch_optional(pool)
    .await?;

    Ok(result.is_some())
}

/// Check if a variable is enabled
#[allow(dead_code)]
pub async fn variable_enabled(pool: &MySqlPool, variable_name: &str) -> Result<bool> {
    let result: Option<(String,)> = sqlx::query_as(
        "SELECT @@GLOBAL.{} AS value"
            .replace("{}", variable_name)
            .as_str(),
    )
    .fetch_optional(pool)
    .await?;

    if let Some((value,)) = result {
        Ok(value.eq_ignore_ascii_case("ON") || value == "1")
    } else {
        Ok(false)
    }
}

/// Execute a query and ignore errors (useful for cleanup)
#[allow(dead_code)]
pub async fn execute_ignore_error(pool: &MySqlPool, query: &str) {
    let _ = sqlx::query(query).execute(pool).await;
}

/// Get `MariaDB` version
#[allow(dead_code)]
pub async fn get_mariadb_version(pool: &MySqlPool) -> Result<String> {
    let row: (String,) = sqlx::query_as("SELECT VERSION()").fetch_one(pool).await?;
    Ok(row.0)
}
