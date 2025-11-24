#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
#![allow(clippy::panic)]

use mariadb_exporter::collectors::util::set_base_connect_options_from_dsn;
use mariadb_exporter::collectors::{config::CollectorConfig, registry::CollectorRegistry};
use nix::unistd::geteuid;
use secrecy::SecretString;
use sqlx::mysql::MySqlPoolOptions;
use std::env;
use std::path::Path;
use std::time::Duration;
use testcontainers_modules::mariadb::Mariadb;
use testcontainers_modules::testcontainers::{core::IntoContainerPort, runners::AsyncRunner};

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

    let container = match Mariadb::default().start().await {
        Ok(container) => container,
        Err(e) => {
            eprintln!("Skipping container integration test: {e}");
            return Ok(());
        }
    };

    let port = container.get_host_port_ipv4(3306.tcp()).await?;
    let host = container.get_host().await?;
    let dsn = format!("mysql://root@{host}:{port}/test");

    let pool = MySqlPoolOptions::new()
        .min_connections(1)
        .max_connections(3)
        .acquire_timeout(Duration::from_secs(20))
        .connect(&dsn)
        .await?;

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
