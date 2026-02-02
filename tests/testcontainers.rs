#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
#![allow(clippy::panic)]

use anyhow::Context;
use mariadb_exporter::collectors::replication::ReplicationCollector;
use mariadb_exporter::collectors::util::set_base_connect_options_from_dsn;
use mariadb_exporter::collectors::{
    Collector, config::CollectorConfig, registry::CollectorRegistry,
};
use nix::unistd::geteuid;
use prometheus::Registry;
use secrecy::SecretString;
use sqlx::MySqlPool;
use sqlx::Row;
use sqlx::mysql::MySqlPoolOptions;
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

fn find_container_runtime() -> Option<String> {
    // Honor explicit DOCKER_HOST if present and reachable.
    if let Ok(existing) = env::var("DOCKER_HOST")
        && !existing.is_empty()
        && socket_exists(&existing)
    {
        return Some(existing);
    }

    // Prefer Podman sockets first, fall back to Docker socket.
    let uid = geteuid().as_raw();
    let candidates = [
        format!("unix:///run/user/{uid}/podman/podman.sock"),
        "unix:///run/podman/podman.sock".to_string(),
        "unix:///var/run/podman/podman.sock".to_string(),
        "unix:///var/run/docker.sock".to_string(),
    ];

    candidates.into_iter().find(|c| socket_exists(c))
}

#[tokio::test]
async fn collect_metrics_from_mariadb_container() -> anyhow::Result<()> {
    let Some(docker_host) = find_container_runtime() else {
        eprintln!(
            "No container runtime socket found (checked Podman + Docker), skipping container integration test"
        );
        return Ok(());
    };

    // Safe because we control the variable name/value and keep it ASCII for the child processes.
    unsafe { env::set_var("DOCKER_HOST", &docker_host) };

    let container = match Mariadb::default()
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

#[tokio::test]
async fn replication_master_server_id_from_container_pair() -> anyhow::Result<()> {
    let Some(docker_host) = find_container_runtime() else {
        eprintln!(
            "No container runtime socket found (checked Podman + Docker), skipping replication test"
        );
        return Ok(());
    };

    // Safe because we control the variable name/value and keep it ASCII for the child processes.
    unsafe { env::set_var("DOCKER_HOST", &docker_host) };

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

    sqlx::query("CREATE USER IF NOT EXISTS 'repl'@'%' IDENTIFIED BY 'repl'")
        .execute(&master_pool)
        .await?;
    sqlx::query("GRANT REPLICATION SLAVE ON *.* TO 'repl'@'%'")
        .execute(&master_pool)
        .await?;
    sqlx::query("FLUSH PRIVILEGES")
        .execute(&master_pool)
        .await?;

    let (binlog_file, binlog_pos) = master_log_position(&master_pool).await?;

    let replica_pool = pool_for_container(&replica, "mysql").await?;

    let _ = sqlx::query("STOP SLAVE").execute(&replica_pool).await;
    let change_master = format!(
        "CHANGE MASTER TO \
        MASTER_HOST = '{master_name}', \
        MASTER_USER = 'repl', \
        MASTER_PASSWORD = 'repl', \
        MASTER_PORT = 3306, \
        MASTER_LOG_FILE = '{binlog_file}', \
        MASTER_LOG_POS = {binlog_pos}"
    );
    sqlx::query(&change_master).execute(&replica_pool).await?;
    sqlx::query("START SLAVE").execute(&replica_pool).await?;

    let (master_id, last_status) = wait_for_master_id(&replica_pool).await?;

    assert_eq!(
        master_id, 1,
        "Expected Master_Server_Id=1 after replication starts; {last_status}"
    );

    let collector = ReplicationCollector::new();
    let registry = Registry::new();
    collector.register_metrics(&registry)?;
    collector.collect(&replica_pool).await?;

    let metric_families = registry.gather();
    let master_metric = metric_families
        .iter()
        .find(|m| m.name() == "mariadb_replica_master_server_id")
        .expect("mariadb_replica_master_server_id metric missing");
    let master_value = master_metric
        .get_metric()
        .first()
        .and_then(|m| m.get_gauge().value)
        .unwrap_or(0.0);

    assert!(
        (master_value - 1.0).abs() < f64::EPSILON,
        "collector should expose master server id from SHOW SLAVE STATUS (got {master_value})"
    );

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
