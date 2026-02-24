#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
#![allow(clippy::panic)]

use anyhow::Context;
use mariadb_exporter::collectors::default::status::StatusCollector;
use mariadb_exporter::collectors::replication::ReplicationCollector;
use mariadb_exporter::collectors::util::set_base_connect_options_from_dsn;
use mariadb_exporter::collectors::{
    Collector, config::CollectorConfig, registry::CollectorRegistry,
};
use nix::unistd::geteuid;
use prometheus::{Registry, proto::MetricFamily};
use secrecy::SecretString;
use sqlx::MySqlPool;
use sqlx::Row;
use sqlx::mysql::{MySqlPoolOptions, MySqlRow};
use std::collections::HashSet;
use std::env;
use std::path::Path;
use std::time::Duration;
use testcontainers_modules::mariadb::Mariadb;
use testcontainers_modules::testcontainers::{
    ContainerAsync, ImageExt, core::IntoContainerPort, runners::AsyncRunner,
};
use tokio::time::sleep;
use ulid::Ulid;

const MARIADB_LTS_TAG: &str = "11.8";

const MASTER_CONF: &str = r"[mariadb]
server_id=1
log_bin=mysql-bin
binlog_format=ROW
";

const REPLICA_CONF: &str = r"[mariadb]
server_id=2
relay_log=relay-bin
read_only=ON
";

fn socket_exists(host: &str) -> bool {
    if let Some(path) = host.strip_prefix("unix://") {
        Path::new(path).exists()
    } else {
        true
    }
}

fn testcontainers_runtime_candidates() -> Vec<String> {
    let mut candidates = vec!["unix:///var/run/docker.sock".to_string()];
    if let Ok(runtime_dir) = env::var("XDG_RUNTIME_DIR")
        && !runtime_dir.is_empty()
    {
        candidates.push(format!("unix://{runtime_dir}/.docker/run/docker.sock"));
    }
    if let Ok(home) = env::var("HOME")
        && !home.is_empty()
    {
        candidates.push(format!("unix://{home}/.docker/run/docker.sock"));
        candidates.push(format!("unix://{home}/.docker/desktop/docker.sock"));
    }
    candidates
}

fn find_container_runtime() -> Option<String> {
    // Honor explicit DOCKER_HOST if present and reachable.
    if let Ok(existing) = env::var("DOCKER_HOST")
        && !existing.is_empty()
        && socket_exists(&existing)
    {
        return Some(existing);
    }

    testcontainers_runtime_candidates()
        .into_iter()
        .find(|candidate| socket_exists(candidate))
}

fn detect_podman_socket() -> Option<String> {
    let uid = geteuid().as_raw();
    let candidates = [
        format!("unix:///run/user/{uid}/podman/podman.sock"),
        "unix:///run/podman/podman.sock".to_string(),
        "unix:///var/run/podman/podman.sock".to_string(),
    ];

    candidates
        .into_iter()
        .find(|candidate| socket_exists(candidate))
}

fn should_require_container_runtime() -> bool {
    let in_ci = env::var("CI")
        .ok()
        .is_some_and(|value| value.eq_ignore_ascii_case("true"));
    let force = env::var("MARIADB_EXPORTER_REQUIRE_TESTCONTAINERS")
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE"));

    in_ci || force
}

fn ensure_container_runtime_for_test(test_name: &str) -> anyhow::Result<bool> {
    if find_container_runtime().is_some() {
        return Ok(true);
    }

    let mut message = format!(
        "No container runtime socket found (checked Podman + Docker), cannot run {test_name}"
    );

    if let Some(podman_socket) = detect_podman_socket() {
        message.push_str(". Podman socket detected at ");
        message.push_str(&podman_socket);
        message.push_str("; set DOCKER_HOST to this value so testcontainers can use it");
    }

    if should_require_container_runtime() {
        anyhow::bail!("{message}");
    }

    eprintln!("{message}; skipping");
    Ok(false)
}

#[tokio::test]
async fn collect_metrics_from_mariadb_container() -> anyhow::Result<()> {
    if !ensure_container_runtime_for_test("collect_metrics_from_mariadb_container")? {
        return Ok(());
    }

    let container = match Mariadb::default()
        .with_tag(MARIADB_LTS_TAG)
        .with_env_var("MARIADB_ROOT_PASSWORD", "root")
        .with_env_var("MARIADB_ROOT_HOST", "%")
        .start()
        .await
    {
        Ok(container) => container,
        Err(e) => {
            eprintln!("Skipping container integration test: {e}");
            return Ok(());
        }
    };

    let port = container.get_host_port_ipv4(3306.tcp()).await?;
    let host = container.get_host().await?.to_string();
    let pool = connect_with_candidates(&host, port, "test").await?;

    let dsn = format!("mysql://root@{host}:{port}/test");
    set_base_connect_options_from_dsn(&SecretString::from(dsn.clone()))?;

    let config =
        CollectorConfig::new().with_enabled(&["default".to_string(), "exporter".to_string()]);
    let registry = CollectorRegistry::new(&config);

    let metrics = registry.collect_all(&pool).await?;

    assert!(
        metrics.contains("mariadb_up"),
        "should include availability gauge"
    );
    assert!(
        metrics.contains("mariadb_version_info"),
        "should include version metric"
    );

    Ok(())
}

async fn start_mariadb_with_conf(
    network: &str,
    name: &str,
    conf: &str,
) -> anyhow::Result<ContainerAsync<Mariadb>> {
    Mariadb::default()
        .with_tag(MARIADB_LTS_TAG)
        .with_env_var("MARIADB_ALLOW_EMPTY_ROOT_PASSWORD", "1")
        .with_env_var("MARIADB_ROOT_HOST", "%")
        .with_copy_to(
            "/etc/mysql/mariadb.conf.d/replication.cnf",
            conf.as_bytes().to_vec(),
        )
        .with_network(network)
        .with_container_name(name)
        .start()
        .await
        .map_err(Into::into)
}

async fn pool_for_container(
    container: &ContainerAsync<Mariadb>,
    db: &str,
) -> anyhow::Result<MySqlPool> {
    let port = container.get_host_port_ipv4(3306.tcp()).await?;
    let host = container.get_host().await?.to_string();
    connect_with_candidates(&host, port, db).await
}

async fn master_log_position(pool: &MySqlPool) -> anyhow::Result<(String, u64)> {
    let master_status = sqlx::query("SHOW MASTER STATUS").fetch_one(pool).await?;
    let binlog_file: String = master_status.try_get("File")?;
    let binlog_pos = if let Ok(value) = master_status.try_get::<u64, _>("Position") {
        value
    } else {
        let signed: i64 = master_status.try_get("Position")?;
        u64::try_from(signed).context("negative binlog position")?
    };

    Ok((binlog_file, binlog_pos))
}

async fn assert_server_version_prefix(
    pool: &MySqlPool,
    expected_prefix: &str,
    role: &str,
) -> anyhow::Result<()> {
    let version: String = sqlx::query_scalar("SELECT VERSION()")
        .fetch_one(pool)
        .await?;
    anyhow::ensure!(
        version.starts_with(expected_prefix),
        "{role} should run MariaDB {expected_prefix}.x, got {version}"
    );
    Ok(())
}

async fn wait_for_master_id(pool: &MySqlPool) -> anyhow::Result<(i64, String)> {
    let mut master_id = 0_i64;
    let mut last_status = String::new();

    for _ in 0..30 {
        if let Some(row) = sqlx::query("SHOW SLAVE STATUS")
            .fetch_optional(pool)
            .await?
        {
            let id = row
                .try_get::<u64, _>("Master_Server_Id")
                .ok()
                .and_then(|v| i64::try_from(v).ok())
                .or_else(|| row.try_get::<i64, _>("Master_Server_Id").ok())
                .unwrap_or(0);
            let io_running: Option<String> = row.try_get("Slave_IO_Running").ok();
            let sql_running: Option<String> = row.try_get("Slave_SQL_Running").ok();
            let last_io_error: Option<String> = row.try_get("Last_IO_Error").ok();
            last_status = format!(
                "Master_Server_Id={id}, Slave_IO_Running={io_running:?}, Slave_SQL_Running={sql_running:?}, Last_IO_Error={last_io_error:?}"
            );

            if id > 0 {
                master_id = id;
                break;
            }
        }

        sleep(Duration::from_secs(1)).await;
    }

    Ok((master_id, last_status))
}

async fn wait_for_replica_threads_state(pool: &MySqlPool, expected: &str) -> anyhow::Result<()> {
    let mut last_status = String::new();

    for _ in 0..30 {
        if let Some(row) = sqlx::query("SHOW SLAVE STATUS")
            .fetch_optional(pool)
            .await?
        {
            let io_running: Option<String> = row.try_get("Slave_IO_Running").ok();
            let sql_running: Option<String> = row.try_get("Slave_SQL_Running").ok();
            last_status =
                format!("Slave_IO_Running={io_running:?}, Slave_SQL_Running={sql_running:?}");

            if io_running.as_deref() == Some(expected) && sql_running.as_deref() == Some(expected) {
                return Ok(());
            }
        }

        sleep(Duration::from_secs(1)).await;
    }

    anyhow::bail!(
        "replica threads did not reach expected state '{expected}'; last status: {last_status}"
    );
}

async fn wait_for_non_null_lag(pool: &MySqlPool) -> anyhow::Result<()> {
    let mut last_status = String::new();

    for _ in 0..30 {
        if let Some(row) = sqlx::query("SHOW SLAVE STATUS")
            .fetch_optional(pool)
            .await?
        {
            let lag_unsigned = row
                .try_get::<Option<u64>, _>("Seconds_Behind_Master")
                .ok()
                .flatten();
            let lag_signed = row
                .try_get::<Option<i64>, _>("Seconds_Behind_Master")
                .ok()
                .flatten();
            last_status = format!(
                "Seconds_Behind_Master(unsigned)={lag_unsigned:?}, (signed)={lag_signed:?}"
            );

            if lag_unsigned.is_some() || lag_signed.is_some() {
                return Ok(());
            }
        }

        sleep(Duration::from_secs(1)).await;
    }

    anyhow::bail!("replica lag stayed NULL; last status: {last_status}");
}

async fn wait_for_replicated_marker(
    master_pool: &MySqlPool,
    replica_pool: &MySqlPool,
) -> anyhow::Result<()> {
    let marker = Ulid::new().to_string();

    sqlx::query("CREATE DATABASE IF NOT EXISTS exporter_test")
        .execute(master_pool)
        .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS exporter_test.replication_probe (marker VARCHAR(64) PRIMARY KEY)",
    )
    .execute(master_pool)
    .await?;
    sqlx::query("INSERT INTO exporter_test.replication_probe (marker) VALUES (?)")
        .bind(&marker)
        .execute(master_pool)
        .await?;

    for _ in 0..30 {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM exporter_test.replication_probe WHERE marker = ?",
        )
        .bind(&marker)
        .fetch_one(replica_pool)
        .await
        .unwrap_or(0);
        if count > 0 {
            return Ok(());
        }
        sleep(Duration::from_secs(1)).await;
    }

    anyhow::bail!("marker row did not replicate to replica");
}

async fn configure_replica_from_master(
    master_pool: &MySqlPool,
    replica_pool: &MySqlPool,
    master_name: &str,
) -> anyhow::Result<()> {
    sqlx::query("CREATE USER IF NOT EXISTS 'repl'@'%' IDENTIFIED BY 'repl'")
        .execute(master_pool)
        .await?;
    sqlx::query("GRANT REPLICATION SLAVE ON *.* TO 'repl'@'%'")
        .execute(master_pool)
        .await?;
    sqlx::query("FLUSH PRIVILEGES").execute(master_pool).await?;

    let (binlog_file, binlog_pos) = master_log_position(master_pool).await?;

    let _ = sqlx::query("STOP SLAVE").execute(replica_pool).await;
    let change_master = format!(
        "CHANGE MASTER TO \
        MASTER_HOST = '{master_name}', \
        MASTER_USER = 'repl', \
        MASTER_PASSWORD = 'repl', \
        MASTER_PORT = 3306, \
        MASTER_LOG_FILE = '{binlog_file}', \
        MASTER_LOG_POS = {binlog_pos}"
    );
    sqlx::query(&change_master).execute(replica_pool).await?;
    sqlx::query("START SLAVE").execute(replica_pool).await?;

    let (master_id, last_status) = wait_for_master_id(replica_pool).await?;
    if master_id != 1 {
        anyhow::bail!("Expected Master_Server_Id=1 after replication starts; {last_status}");
    }

    Ok(())
}

fn gauge_value(metric_families: &[MetricFamily], metric_name: &str) -> anyhow::Result<f64> {
    let family = metric_families
        .iter()
        .find(|metric| metric.name() == metric_name)
        .ok_or_else(|| anyhow::anyhow!("metric '{metric_name}' not found"))?;

    family
        .get_metric()
        .first()
        .and_then(|metric| metric.get_gauge().value)
        .ok_or_else(|| anyhow::anyhow!("metric '{metric_name}' has no gauge value"))
}

fn assert_gauge_eq(
    metric_families: &[MetricFamily],
    metric_name: &str,
    expected_value: f64,
    context: &str,
) -> anyhow::Result<()> {
    let actual = gauge_value(metric_families, metric_name)?;
    anyhow::ensure!(
        (actual - expected_value).abs() < f64::EPSILON,
        "{context}: expected {expected_value}, got {actual}"
    );
    Ok(())
}

fn seconds_behind_master_from_row(row: &MySqlRow) -> Option<u64> {
    row.try_get::<Option<u64>, _>("Seconds_Behind_Master")
        .ok()
        .flatten()
        .or_else(|| {
            row.try_get::<Option<i64>, _>("Seconds_Behind_Master")
                .ok()
                .flatten()
                .and_then(|value| u64::try_from(value).ok())
        })
}

async fn gather_replication_collector_metrics(
    pool: &MySqlPool,
) -> anyhow::Result<Vec<MetricFamily>> {
    let collector = ReplicationCollector::new();
    let registry = Registry::new();
    collector.register_metrics(&registry)?;
    collector.collect(pool).await?;
    Ok(registry.gather())
}

async fn gather_default_status_metrics(pool: &MySqlPool) -> anyhow::Result<Vec<MetricFamily>> {
    let collector = StatusCollector::new();
    let registry = Registry::new();
    collector.register_metrics(&registry)?;
    collector.collect(pool).await?;
    Ok(registry.gather())
}

async fn assert_running_replica_collectors(replica_pool: &MySqlPool) -> anyhow::Result<()> {
    let replication_metrics = gather_replication_collector_metrics(replica_pool).await?;
    assert_gauge_eq(
        &replication_metrics,
        "mariadb_replica_configured",
        1.0,
        "replication collector should mark running replica as configured",
    )?;
    assert_gauge_eq(
        &replication_metrics,
        "mariadb_replica_io_running",
        1.0,
        "replication collector should report IO thread running on replica",
    )?;
    assert_gauge_eq(
        &replication_metrics,
        "mariadb_replica_sql_running",
        1.0,
        "replication collector should report SQL thread running on replica",
    )?;
    assert_gauge_eq(
        &replication_metrics,
        "mariadb_replica_master_server_id",
        1.0,
        "replication collector should expose master server id on replica",
    )?;
    assert!(
        gauge_value(
            &replication_metrics,
            "mariadb_replica_seconds_behind_master_seconds"
        )? >= 0.0,
        "replication collector should report non-negative lag when replica is running"
    );
    let lag_by_channel = replication_metrics
        .iter()
        .find(|metric| metric.name() == "mariadb_replica_seconds_behind_master_seconds_by_channel")
        .ok_or_else(|| anyhow::anyhow!("per-channel lag metric should be registered"))?;
    let first_channel = lag_by_channel
        .get_metric()
        .first()
        .ok_or_else(|| anyhow::anyhow!("running replica should expose at least one channel"))?;
    let labels = first_channel.get_label();
    anyhow::ensure!(
        labels.iter().any(|label| label.name() == "channel_name"),
        "per-channel lag metric should include channel_name label"
    );
    anyhow::ensure!(
        labels.iter().any(|label| label.name() == "connection_name"),
        "per-channel lag metric should include connection_name label"
    );
    anyhow::ensure!(
        first_channel.get_gauge().value.unwrap_or(-2.0) >= 0.0,
        "per-channel lag should be non-negative for running replica"
    );

    let default_metrics = gather_default_status_metrics(replica_pool).await?;
    assert_gauge_eq(
        &default_metrics,
        "mariadb_slave_status_io_running",
        1.0,
        "default collector should report IO running on replica",
    )?;
    assert_gauge_eq(
        &default_metrics,
        "mariadb_slave_status_sql_running",
        1.0,
        "default collector should report SQL running on replica",
    )?;
    assert!(
        gauge_value(
            &default_metrics,
            "mariadb_slave_status_seconds_behind_master"
        )? >= 0.0,
        "default collector should report non-negative lag on running replica"
    );

    Ok(())
}

async fn assert_replica_collectors_positive_lag(replica_pool: &MySqlPool) -> anyhow::Result<()> {
    let mut last_status = String::new();
    let mut last_replication_lag = f64::NAN;
    let mut last_default_lag = f64::NAN;

    for _ in 0..45 {
        if let Some(row) = sqlx::query("SHOW SLAVE STATUS")
            .fetch_optional(replica_pool)
            .await?
        {
            let io_running: Option<String> = row.try_get("Slave_IO_Running").ok();
            let sql_running: Option<String> = row.try_get("Slave_SQL_Running").ok();
            let lag = seconds_behind_master_from_row(&row);
            last_status = format!(
                "Slave_IO_Running={io_running:?}, Slave_SQL_Running={sql_running:?}, Seconds_Behind_Master={lag:?}"
            );

            if io_running.as_deref() != Some("Yes") || sql_running.as_deref() != Some("Yes") {
                sleep(Duration::from_millis(200)).await;
                continue;
            }
        }

        let replication_metrics = gather_replication_collector_metrics(replica_pool).await?;
        let replication_lag = gauge_value(
            &replication_metrics,
            "mariadb_replica_seconds_behind_master_seconds",
        )?;
        last_replication_lag = replication_lag;

        let default_metrics = gather_default_status_metrics(replica_pool).await?;
        let default_lag = gauge_value(
            &default_metrics,
            "mariadb_slave_status_seconds_behind_master",
        )?;
        last_default_lag = default_lag;

        if replication_lag >= 1.0 && default_lag >= 1.0 {
            return Ok(());
        }

        sleep(Duration::from_millis(200)).await;
    }

    anyhow::bail!(
        "collectors never exposed positive lag during replay backlog; last status: {last_status}; last replication lag={last_replication_lag}, last default lag={last_default_lag}"
    );
}

async fn assert_replica_collectors_zero_lag(replica_pool: &MySqlPool) -> anyhow::Result<()> {
    let mut last_replication_lag = f64::NAN;
    let mut last_default_lag = f64::NAN;

    for _ in 0..20 {
        let replication_metrics = gather_replication_collector_metrics(replica_pool).await?;
        let replication_lag = gauge_value(
            &replication_metrics,
            "mariadb_replica_seconds_behind_master_seconds",
        )?;
        last_replication_lag = replication_lag;

        let default_metrics = gather_default_status_metrics(replica_pool).await?;
        let default_lag = gauge_value(
            &default_metrics,
            "mariadb_slave_status_seconds_behind_master",
        )?;
        last_default_lag = default_lag;

        if (replication_lag - 0.0).abs() < f64::EPSILON && (default_lag - 0.0).abs() < f64::EPSILON
        {
            return Ok(());
        }

        sleep(Duration::from_millis(500)).await;
    }

    anyhow::bail!(
        "collectors never converged to zero lag after catch-up; last replication lag={last_replication_lag}, last default lag={last_default_lag}"
    );
}

async fn assert_primary_collectors(primary_pool: &MySqlPool) -> anyhow::Result<()> {
    let replication_metrics = gather_replication_collector_metrics(primary_pool).await?;
    assert_gauge_eq(
        &replication_metrics,
        "mariadb_replica_configured",
        0.0,
        "primary should not be marked as configured replica",
    )?;
    assert_gauge_eq(
        &replication_metrics,
        "mariadb_replica_seconds_behind_master_seconds",
        -1.0,
        "primary should expose lag as -1 (unknown/not replica)",
    )?;
    assert!(
        gauge_value(&replication_metrics, "mariadb_primary_binlog_files")? >= 1.0,
        "primary should expose at least one binlog file when log_bin is enabled"
    );

    let default_metrics = gather_default_status_metrics(primary_pool).await?;
    assert_gauge_eq(
        &default_metrics,
        "mariadb_slave_status_io_running",
        0.0,
        "default collector should report IO not running on primary",
    )?;
    assert_gauge_eq(
        &default_metrics,
        "mariadb_slave_status_sql_running",
        0.0,
        "default collector should report SQL not running on primary",
    )?;
    assert_gauge_eq(
        &default_metrics,
        "mariadb_slave_status_seconds_behind_master",
        -1.0,
        "default collector should report lag as -1 on primary",
    )?;

    Ok(())
}

async fn assert_stopped_replica_collectors(replica_pool: &MySqlPool) -> anyhow::Result<()> {
    let replication_metrics = gather_replication_collector_metrics(replica_pool).await?;
    assert_gauge_eq(
        &replication_metrics,
        "mariadb_replica_configured",
        1.0,
        "stopped replica should remain configured",
    )?;
    assert_gauge_eq(
        &replication_metrics,
        "mariadb_replica_io_running",
        0.0,
        "replication collector should report IO thread down after STOP SLAVE",
    )?;
    assert_gauge_eq(
        &replication_metrics,
        "mariadb_replica_sql_running",
        0.0,
        "replication collector should report SQL thread down after STOP SLAVE",
    )?;
    assert_gauge_eq(
        &replication_metrics,
        "mariadb_replica_seconds_behind_master_seconds",
        -1.0,
        "replication collector should report lag=-1 for stopped replication",
    )?;

    let default_metrics = gather_default_status_metrics(replica_pool).await?;
    assert_gauge_eq(
        &default_metrics,
        "mariadb_slave_status_io_running",
        0.0,
        "default collector should report IO down for stopped replica",
    )?;
    assert_gauge_eq(
        &default_metrics,
        "mariadb_slave_status_sql_running",
        0.0,
        "default collector should report SQL down for stopped replica",
    )?;
    assert_gauge_eq(
        &default_metrics,
        "mariadb_slave_status_seconds_behind_master",
        -1.0,
        "default collector should report lag=-1 for stopped replica",
    )?;

    Ok(())
}

async fn set_replica_master_delay(
    replica_pool: &MySqlPool,
    delay_seconds: u64,
) -> anyhow::Result<()> {
    sqlx::query("STOP SLAVE").execute(replica_pool).await?;
    let change_master = format!("CHANGE MASTER TO MASTER_DELAY = {delay_seconds}");
    sqlx::query(&change_master).execute(replica_pool).await?;
    sqlx::query("START SLAVE").execute(replica_pool).await?;
    wait_for_replica_threads_state(replica_pool, "Yes").await
}

async fn generate_replication_backlog(master_pool: &MySqlPool) -> anyhow::Result<()> {
    sqlx::query("CREATE DATABASE IF NOT EXISTS exporter_test")
        .execute(master_pool)
        .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS exporter_test.replication_backlog (id BIGINT PRIMARY KEY AUTO_INCREMENT, marker VARCHAR(64) NOT NULL)",
    )
    .execute(master_pool)
    .await?;

    for _ in 0..32 {
        let marker = Ulid::new().to_string();
        sqlx::query("INSERT INTO exporter_test.replication_backlog (marker) VALUES (?)")
            .bind(marker)
            .execute(master_pool)
            .await?;
        sleep(Duration::from_millis(125)).await;
    }

    Ok(())
}

async fn wait_for_lag_at_most(pool: &MySqlPool, maximum_seconds: u64) -> anyhow::Result<u64> {
    let mut last_status = String::new();

    for _ in 0..45 {
        if let Some(row) = sqlx::query("SHOW SLAVE STATUS")
            .fetch_optional(pool)
            .await?
        {
            let io_running: Option<String> = row.try_get("Slave_IO_Running").ok();
            let sql_running: Option<String> = row.try_get("Slave_SQL_Running").ok();
            let lag = seconds_behind_master_from_row(&row);
            last_status = format!(
                "Slave_IO_Running={io_running:?}, Slave_SQL_Running={sql_running:?}, Seconds_Behind_Master={lag:?}"
            );

            if let Some(value) = lag
                && value <= maximum_seconds
            {
                return Ok(value);
            }
        }
        sleep(Duration::from_secs(1)).await;
    }

    anyhow::bail!(
        "replica lag never recovered to <= {maximum_seconds}; last status: {last_status}"
    );
}

async fn verify_replica_role_and_lag_semantics(
    master_pool: &MySqlPool,
    replica_pool: &MySqlPool,
) -> anyhow::Result<()> {
    wait_for_replica_threads_state(replica_pool, "Yes").await?;
    wait_for_replicated_marker(master_pool, replica_pool).await?;
    wait_for_non_null_lag(replica_pool).await?;
    assert_running_replica_collectors(replica_pool).await?;
    assert_primary_collectors(master_pool).await?;

    set_replica_master_delay(replica_pool, 5).await?;
    generate_replication_backlog(master_pool).await?;
    assert_replica_collectors_positive_lag(replica_pool).await?;
    set_replica_master_delay(replica_pool, 0).await?;
    wait_for_replicated_marker(master_pool, replica_pool).await?;
    wait_for_lag_at_most(replica_pool, 0).await?;
    assert_replica_collectors_zero_lag(replica_pool).await?;
    assert_running_replica_collectors(replica_pool).await?;

    sqlx::query("STOP SLAVE").execute(replica_pool).await?;
    wait_for_replica_threads_state(replica_pool, "No").await?;
    assert_stopped_replica_collectors(replica_pool).await?;

    Ok(())
}

#[tokio::test]
async fn replication_lag_from_mariadb_11_8_primary_replica_pair() -> anyhow::Result<()> {
    if !ensure_container_runtime_for_test("replication_lag_from_mariadb_11_8_primary_replica_pair")?
    {
        return Ok(());
    }

    let suffix = Ulid::new().to_string().to_lowercase();
    let network = format!("mariadb-repl-{suffix}");
    let master_name = format!("mariadb-master-{suffix}");
    let replica_name = format!("mariadb-replica-{suffix}");

    let master = match start_mariadb_with_conf(&network, &master_name, MASTER_CONF).await {
        Ok(container) => container,
        Err(e) => {
            eprintln!("Skipping replication test: {e}");
            return Ok(());
        }
    };

    let replica = match start_mariadb_with_conf(&network, &replica_name, REPLICA_CONF).await {
        Ok(container) => container,
        Err(e) => {
            eprintln!("Skipping replication test: {e}");
            return Ok(());
        }
    };

    let master_pool = pool_for_container(&master, "mysql").await?;
    let replica_pool = pool_for_container(&replica, "mysql").await?;

    assert_server_version_prefix(&master_pool, MARIADB_LTS_TAG, "primary").await?;
    assert_server_version_prefix(&replica_pool, MARIADB_LTS_TAG, "replica").await?;

    configure_replica_from_master(&master_pool, &replica_pool, &master_name).await?;
    verify_replica_role_and_lag_semantics(&master_pool, &replica_pool).await?;

    Ok(())
}

async fn connect_with_candidates(
    host: &str,
    port: u16,
    db: &str,
) -> anyhow::Result<sqlx::MySqlPool> {
    let mut tried = Vec::new();
    let mut candidates = HashSet::new();

    if let Ok(pass) = env::var("MARIADB_ROOT_PASSWORD") {
        candidates.insert(pass);
    }
    if let Ok(pass) = env::var("MARIADB_PASSWORD") {
        candidates.insert(pass);
    }
    candidates.insert("root".to_string());
    candidates.insert("test".to_string());
    candidates.insert(String::new());

    let mut last_err = None;
    for pass in candidates {
        let dsn = if pass.is_empty() {
            format!("mysql://root@{host}:{port}/{db}")
        } else {
            format!("mysql://root:{pass}@{host}:{port}/{db}")
        };
        tried.push(dsn.clone());

        match MySqlPoolOptions::new()
            .min_connections(1)
            .max_connections(3)
            .acquire_timeout(Duration::from_secs(20))
            .connect(&dsn)
            .await
        {
            Ok(pool) => return Ok(pool),
            Err(e) => last_err = Some(e),
        }
    }

    Err(anyhow::anyhow!(
        "Failed to connect using candidates: {tried:?}, last error: {last_err:?}"
    ))
}
