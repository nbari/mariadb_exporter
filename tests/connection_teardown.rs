//! Normal shutdown must send `COM_QUIT`, including over TLS, before the runtime exits.
#![cfg(unix)]

mod common;

use anyhow::{Context, Result, ensure};
use mariadb_exporter::collectors::COLLECTOR_NAMES;
use sqlx::{Connection, MySqlConnection};
use std::{process::Stdio, time::Duration};
use testcontainers_modules::testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};
use tokio::{
    process::{Child, Command},
    time::{sleep, timeout},
};

async fn start_mariadb() -> Result<ContainerAsync<GenericImage>> {
    let startup = r"
        openssl req -new -x509 -nodes -newkey rsa:2048 -days 1 \
            -subj /CN=localhost -keyout /tmp/server.key -out /tmp/server.crt
        chown mysql:mysql /tmp/server.key /tmp/server.crt
        chmod 600 /tmp/server.key
        exec docker-entrypoint.sh mariadbd \
            --ssl-cert=/tmp/server.crt --ssl-key=/tmp/server.key \
            --tls-version=TLSv1.3 --log-warnings=2
    ";
    Ok(GenericImage::new("mariadb", "11.4")
        .with_entrypoint("sh")
        .with_exposed_port(3306.tcp())
        .with_wait_for(WaitFor::message_on_stderr("port: 3306"))
        .with_env_var("MARIADB_ROOT_PASSWORD", "root")
        .with_env_var("MARIADB_ROOT_HOST", "%")
        .with_cmd(["-ec", startup])
        .start()
        .await?)
}

async fn aborted_clients(admin: &mut MySqlConnection) -> Result<u64> {
    let (_, value): (String, String) = sqlx::query_as("SHOW GLOBAL STATUS LIKE 'Aborted_clients'")
        .fetch_one(admin)
        .await?;
    Ok(value.parse()?)
}

async fn exporter_sessions(admin: &mut MySqlConnection) -> Result<Vec<u64>> {
    Ok(sqlx::query_scalar(
        "SELECT CAST(ID AS UNSIGNED) FROM information_schema.PROCESSLIST WHERE USER = 'teardown'",
    )
    .fetch_all(admin)
    .await?)
}

async fn scrape_and_stop(child: &mut Child, url: &str, admin: &mut MySqlConnection) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    timeout(Duration::from_secs(20), async {
        loop {
            if client.get(format!("{url}/health")).send().await.is_ok() {
                return Ok::<_, anyhow::Error>(());
            }
            ensure!(
                child.try_wait()?.is_none(),
                "exporter exited before startup"
            );
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("exporter did not start")??;

    let before = aborted_clients(admin).await?;
    for _ in 0..5 {
        let response = client.get(format!("{url}/metrics")).send().await?;
        ensure!(response.status().is_success(), "scrape failed");
        let body = response.text().await?;
        ensure!(
            body.lines().any(|line| line == "mariadb_up 1"),
            "database is down: {body}"
        );
        ensure!(
            body.contains("mariadb_info_schema_table_size_bytes{"),
            "schema collector did not run: {body}"
        );
    }
    let sessions = exporter_sessions(admin).await?;
    ensure!(
        !sessions.is_empty(),
        "fixture has no pooled sessions to close"
    );
    ensure!(
        aborted_clients(admin).await? == before,
        "normal scrapes aborted connections"
    );

    let pid = child.id().context("exporter exited before SIGTERM")?;
    let signal = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .await?;
    ensure!(signal.success(), "failed to send SIGTERM");
    let status = timeout(Duration::from_secs(10), child.wait())
        .await
        .context("exporter exceeded shutdown budget")??;
    ensure!(status.success(), "exporter failed on SIGTERM: {status}");

    // An empty processlist alone can race the server's disconnect accounting. COM_QUIT
    // appears in the general log only after the server handles a graceful disconnect.
    timeout(Duration::from_secs(10), async {
        loop {
            ensure!(
                aborted_clients(admin).await? == before,
                "SIGTERM aborted database connections"
            );
            let mut quit_count = 0;
            for id in &sessions {
                let count: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM mysql.general_log WHERE thread_id = ? AND command_type = 'Quit'",
                )
                .bind(id)
                .fetch_one(&mut *admin)
                .await?;
                if count > 0 {
                    quit_count += 1;
                }
            }
            if exporter_sessions(admin).await?.is_empty() && quit_count == sessions.len() {
                return Ok::<_, anyhow::Error>(());
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("exporter sessions remained after shutdown")??;
    ensure!(
        aborted_clients(admin).await? == before,
        "SIGTERM aborted database connections"
    );
    Ok(())
}

async fn run_exporter(dsn: &str, admin: &mut MySqlConnection) -> Result<()> {
    let port = common::get_available_port();
    let mut command = Command::new(env!("CARGO_BIN_EXE_mariadb_exporter"));
    for (key, _) in std::env::vars_os() {
        if key
            .to_str()
            .is_some_and(|key| key.starts_with("MARIADB_EXPORTER_") || key.starts_with("OTEL_"))
        {
            command.env_remove(key);
        }
    }
    command.args([
        "--dsn",
        dsn,
        "--listen",
        "127.0.0.1",
        "--port",
        &port.to_string(),
    ]);
    for name in COLLECTOR_NAMES {
        command.arg(format!("--no-collector.{name}"));
    }
    for name in ["default", "schema", "exporter"] {
        command.arg(format!("--collector.{name}"));
    }
    let mut child = command
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let result = scrape_and_stop(&mut child, &format!("http://127.0.0.1:{port}"), admin).await;
    if child.id().is_some() {
        child.kill().await?;
    }
    child.wait().await?;
    result
}

#[tokio::test]
async fn sigterm_closes_plaintext_and_tls_sessions_cleanly() -> Result<()> {
    if !common::containers::ensure_container_runtime_for_test(
        "sigterm_closes_plaintext_and_tls_sessions_cleanly",
    )? {
        return Ok(());
    }
    let container = start_mariadb().await?;
    let host = container.get_host().await?;
    let port = container.get_host_port_ipv4(3306.tcp()).await?;
    let mut admin = MySqlConnection::connect(&format!(
        "mysql://root:root@{host}:{port}/mysql?ssl-mode=DISABLED",
    ))
    .await?;
    let result = async {
        sqlx::query("CREATE DATABASE teardown_data")
            .execute(&mut admin)
            .await?;
        sqlx::query("CREATE TABLE teardown_data.items (id INT PRIMARY KEY)")
            .execute(&mut admin)
            .await?;
        sqlx::query("CREATE USER 'teardown'@'%' IDENTIFIED BY 'teardown'")
            .execute(&mut admin)
            .await?;
        sqlx::query("GRANT SELECT, PROCESS, REPLICATION CLIENT ON *.* TO 'teardown'@'%'")
            .execute(&mut admin)
            .await?;
        sqlx::query("SET GLOBAL log_output = 'TABLE'")
            .execute(&mut admin)
            .await?;
        sqlx::query("SET GLOBAL general_log = ON")
            .execute(&mut admin)
            .await?;
        for mode in ["DISABLED", "REQUIRED"] {
            run_exporter(
                &format!("mysql://teardown:teardown@{host}:{port}/mysql?ssl-mode={mode}"),
                &mut admin,
            )
            .await
            .with_context(|| format!("connection mode {mode}"))?;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let closed = admin.close().await;
    if let Err(error) = result {
        let logs = String::from_utf8_lossy(&container.stderr_to_vec().await?).into_owned();
        return Err(error.context(format!("MariaDB logs:\n{logs}")));
    }
    closed?;
    Ok(())
}
