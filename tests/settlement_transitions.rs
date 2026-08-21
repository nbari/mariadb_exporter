//! Feature-transition tests: a source that goes away must take its series with it.
//!
//! These tests mutate global server state (plugins, `@@userstat`), so each one runs against
//! its **own** isolated container and seeds everything it needs. They never touch a
//! lived-in database and never share a server with the rest of the suite.
//!
//! When no container runtime is reachable the tests skip with an explicit message, unless
//! `CI=true` or `MARIADB_EXPORTER_REQUIRE_TESTCONTAINERS=1` forces them to be required.

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use mariadb_exporter::collectors::{
    Collector, query_response_time::QueryResponseTimeCollector, userstat::UserStatCollector,
};
use nix::unistd::geteuid;
use prometheus::Registry;
use sqlx::MySqlPool;
use sqlx::mysql::MySqlPoolOptions;
use std::env;
use std::path::Path;
use std::time::Duration;
use testcontainers_modules::mariadb::Mariadb;
use testcontainers_modules::testcontainers::{
    ContainerAsync, ImageExt, core::IntoContainerPort, runners::AsyncRunner,
};

const MARIADB_LTS_TAG: &str = "11.8";

fn socket_exists(host: &str) -> bool {
    host.strip_prefix("unix://")
        .is_none_or(|path| Path::new(path).exists())
}

fn find_container_runtime() -> Option<String> {
    if let Ok(existing) = env::var("DOCKER_HOST")
        && !existing.is_empty()
        && socket_exists(&existing)
    {
        return Some(existing);
    }

    let uid = geteuid().as_raw();
    let candidates = [
        "unix:///var/run/docker.sock".to_string(),
        format!("unix:///run/user/{uid}/podman/podman.sock"),
        "unix:///run/podman/podman.sock".to_string(),
        "unix:///var/run/podman/podman.sock".to_string(),
    ];
    candidates.into_iter().find(|c| socket_exists(c))
}

fn container_runtime_required() -> bool {
    env::var("CI").is_ok_and(|v| v.eq_ignore_ascii_case("true"))
        || env::var("MARIADB_EXPORTER_REQUIRE_TESTCONTAINERS")
            .is_ok_and(|v| matches!(v.as_str(), "1" | "true" | "TRUE"))
}

/// Starts a dedicated `MariaDB` container and returns it with a pool bound to it.
///
/// Returns `Ok(None)` when the environment has no usable container runtime, so the caller
/// can report an explicit, environment-driven skip.
async fn isolated_server(
    test_name: &str,
) -> anyhow::Result<Option<(ContainerAsync<Mariadb>, MySqlPool)>> {
    if find_container_runtime().is_none() {
        let message =
            format!("no container runtime socket found (Podman or Docker), cannot run {test_name}");
        if container_runtime_required() {
            anyhow::bail!("{message}");
        }
        eprintln!("{message}; skipping");
        return Ok(None);
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
            if container_runtime_required() {
                anyhow::bail!("failed to start MariaDB for {test_name}: {e}");
            }
            eprintln!("failed to start MariaDB for {test_name} ({e}); skipping");
            return Ok(None);
        }
    };

    let port = container.get_host_port_ipv4(3306.tcp()).await?;
    let host = container.get_host().await?.to_string();

    let pool = MySqlPoolOptions::new()
        .min_connections(1)
        .max_connections(3)
        .acquire_timeout(Duration::from_secs(30))
        .connect(&format!("mysql://root:root@{host}:{port}/mysql"))
        .await?;

    Ok(Some((container, pool)))
}

fn published(registry: &Registry, metric: &str) -> usize {
    registry
        .gather()
        .iter()
        .filter(|mf| mf.name() == metric)
        .map(|mf| mf.get_metric().len())
        .sum()
}

/// Plugin installed -> removed.
///
/// While `query_response_time` is installed the histogram is real data. Once the plugin is
/// uninstalled the source is gone, so the bucket, the count *and* the sum must all
/// disappear together — a histogram that keeps its `_count` after losing its buckets is
/// worse than no histogram at all.
#[tokio::test]
async fn plugin_installed_to_removed_clears_the_whole_histogram() -> anyhow::Result<()> {
    let Some((_container, pool)) =
        isolated_server("plugin_installed_to_removed_clears_the_whole_histogram").await?
    else {
        return Ok(());
    };

    if sqlx::query("INSTALL SONAME 'query_response_time'")
        .execute(&pool)
        .await
        .is_err()
    {
        eprintln!("query_response_time plugin not available in this image; skipping");
        return Ok(());
    }
    sqlx::query("SET GLOBAL query_response_time_stats = ON")
        .execute(&pool)
        .await?;
    sqlx::query("SELECT SLEEP(0.01)").execute(&pool).await?;

    let collector = QueryResponseTimeCollector::new();
    let registry = Registry::new();
    collector.register_metrics(&registry)?;

    collector.collect(&pool).await?;
    assert!(
        published(
            &registry,
            "mariadb_info_schema_query_response_time_seconds_bucket"
        ) > 0,
        "the installed plugin should publish buckets"
    );
    assert_eq!(
        published(
            &registry,
            "mariadb_info_schema_query_response_time_seconds_count"
        ),
        1,
        "the installed plugin should publish a count"
    );

    sqlx::query("UNINSTALL SONAME 'query_response_time'")
        .execute(&pool)
        .await?;

    collector.collect(&pool).await?;

    assert_eq!(
        published(
            &registry,
            "mariadb_info_schema_query_response_time_seconds_bucket"
        ),
        0,
        "an uninstalled plugin must remove its buckets"
    );
    assert_eq!(
        published(
            &registry,
            "mariadb_info_schema_query_response_time_seconds_count"
        ),
        0,
        "an uninstalled plugin must remove its count, not freeze the last one"
    );
    assert_eq!(
        published(
            &registry,
            "mariadb_info_schema_query_response_time_seconds_sum"
        ),
        0,
        "an uninstalled plugin must remove its sum, not freeze the last one"
    );

    pool.close().await;
    Ok(())
}

/// `userstat` enabled -> disabled.
///
/// With `@@userstat = 0` the server stops maintaining `information_schema.USER_STATISTICS`
/// entirely. Keeping the last per-user counters would misreport a server that is no longer
/// measuring anything, so the whole surface has to go.
#[tokio::test]
async fn userstat_enabled_to_disabled_clears_per_user_series() -> anyhow::Result<()> {
    let Some((_container, pool)) =
        isolated_server("userstat_enabled_to_disabled_clears_per_user_series").await?
    else {
        return Ok(());
    };

    sqlx::query("SET GLOBAL userstat = 1")
        .execute(&pool)
        .await?;
    sqlx::query("SELECT 1").execute(&pool).await?;

    let collector = UserStatCollector::new();
    let registry = Registry::new();
    collector.register_metrics(&registry)?;

    collector.collect(&pool).await?;
    let before = published(&registry, "mariadb_info_schema_userstats_connections_total");
    assert!(
        before > 0,
        "enabled userstat should publish per-user counters"
    );

    sqlx::query("SET GLOBAL userstat = 0")
        .execute(&pool)
        .await?;

    collector.collect(&pool).await?;

    assert_eq!(
        published(&registry, "mariadb_info_schema_userstats_connections_total"),
        0,
        "disabled userstat must remove the per-user counters it owned"
    );
    assert_eq!(
        published(&registry, "mariadb_info_schema_userstats_rows_read_total"),
        0,
        "disabled userstat must remove every series it owned"
    );

    pool.close().await;
    Ok(())
}
