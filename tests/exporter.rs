#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
#![allow(clippy::panic)]
#![allow(clippy::indexing_slicing)]
use anyhow::Result;
use mariadb_exporter::collectors::config::CollectorConfig;
use secrecy::SecretString;

mod common;

fn assert_comment_only_exposition(body: &str, context: &str) {
    assert!(
        !body.is_empty(),
        "{context}: response body must not be empty"
    );
    assert!(
        body.lines()
            .all(|line| line.is_empty() || line.starts_with('#')),
        "{context}: every non-empty line must be a Prometheus comment, got {body:?}"
    );
}

#[tokio::test]
async fn test_exporter_database_connection() -> Result<()> {
    let pool = common::create_test_pool().await?;

    let row: (i32,) = sqlx::query_as("SELECT 1").fetch_one(&pool).await?;

    assert_eq!(row.0, 1);

    pool.close().await;

    Ok(())
}

#[tokio::test]
async fn test_exporter_starts_and_stops() -> Result<()> {
    let port = common::get_available_port();
    let dsn = SecretString::from(common::get_test_dsn());

    let handle = tokio::spawn(async move {
        mariadb_exporter::exporter::new(
            port,
            None,
            dsn,
            CollectorConfig::new().with_enabled(&["default".to_string()]),
        )
        .await
    });

    assert!(
        common::wait_for_server(port, 50).await,
        "Server failed to start on port {port}"
    );

    handle.abort();

    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let result = tokio::net::TcpStream::connect(format!("localhost:{port}")).await;
    assert!(result.is_err(), "Server should be stopped");

    Ok(())
}

#[tokio::test]
async fn test_exporter_metrics_endpoint() -> Result<()> {
    let port = common::get_available_port();
    let dsn = SecretString::from(common::get_test_dsn());

    let handle = tokio::spawn(async move {
        mariadb_exporter::exporter::new(
            port,
            None,
            dsn,
            CollectorConfig::new().with_enabled(&["default".to_string()]),
        )
        .await
    });

    assert!(
        common::wait_for_server(port, 50).await,
        "Server failed to start on port {port}"
    );

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/metrics", common::get_test_url(port)))
        .send()
        .await?;

    assert_eq!(response.status(), 200);

    let body = response.text().await?;
    assert!(!body.is_empty());
    assert!(body.contains("mariadb_"));

    handle.abort();

    Ok(())
}

#[tokio::test]
async fn test_exporter_health_endpoint() -> Result<()> {
    let port = common::get_available_port();
    let dsn = SecretString::from(common::get_test_dsn());

    let handle = tokio::spawn(async move {
        mariadb_exporter::exporter::new(
            port,
            None,
            dsn,
            CollectorConfig::new().with_enabled(&["default".to_string()]),
        )
        .await
    });

    assert!(
        common::wait_for_server(port, 50).await,
        "Server failed to start on port {port}"
    );

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/health", common::get_test_url(port)))
        .send()
        .await?;

    assert_eq!(response.status(), 200);

    handle.abort();

    Ok(())
}

#[tokio::test]
async fn test_exporter_bind_to_ipv4_localhost() -> Result<()> {
    let port = common::get_available_port();
    let dsn = SecretString::from(common::get_test_dsn());

    let handle = tokio::spawn(async move {
        mariadb_exporter::exporter::new(
            port,
            Some("127.0.0.1".to_string()),
            dsn,
            CollectorConfig::new().with_enabled(&["default".to_string()]),
        )
        .await
    });

    assert!(
        common::wait_for_server(port, 50).await,
        "Server failed to start on 127.0.0.1:{port}"
    );

    // Verify it's accessible on IPv4 localhost
    let result = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")).await;
    assert!(result.is_ok(), "Should connect to 127.0.0.1");

    handle.abort();

    Ok(())
}

#[tokio::test]
async fn test_exporter_bind_to_ipv4_all_interfaces() -> Result<()> {
    let port = common::get_available_port();
    let dsn = SecretString::from(common::get_test_dsn());

    let handle = tokio::spawn(async move {
        mariadb_exporter::exporter::new(
            port,
            Some("0.0.0.0".to_string()),
            dsn,
            CollectorConfig::new().with_enabled(&["default".to_string()]),
        )
        .await
    });

    assert!(
        common::wait_for_server(port, 50).await,
        "Server failed to start on 0.0.0.0:{port}"
    );

    // Verify it's accessible
    let result = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")).await;
    assert!(result.is_ok(), "Should connect via 127.0.0.1");

    handle.abort();

    Ok(())
}

#[tokio::test]
async fn test_exporter_bind_to_ipv6_localhost() -> Result<()> {
    let port = common::get_available_port();
    let dsn = SecretString::from(common::get_test_dsn());

    let handle = tokio::spawn(async move {
        mariadb_exporter::exporter::new(
            port,
            Some("::1".to_string()),
            dsn,
            CollectorConfig::new().with_enabled(&["default".to_string()]),
        )
        .await
    });

    // Give it time to start (or fail if IPv6 not available)
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Try to connect via IPv6 localhost
    let result = tokio::net::TcpStream::connect(format!("[::1]:{port}")).await;

    if result.is_ok() {
        println!("✓ IPv6 localhost binding works");
    } else {
        println!("ℹ IPv6 localhost not available (expected on some systems)");
    }

    handle.abort();

    Ok(())
}

#[tokio::test]
async fn test_exporter_default_bind_auto_detect() -> Result<()> {
    let port = common::get_available_port();
    let dsn = SecretString::from(common::get_test_dsn());

    // None = auto-detect (try IPv6, fallback to IPv4)
    let handle = tokio::spawn(async move {
        mariadb_exporter::exporter::new(
            port,
            None,
            dsn,
            CollectorConfig::new().with_enabled(&["default".to_string()]),
        )
        .await
    });

    assert!(
        common::wait_for_server(port, 50).await,
        "Server failed to start with auto-detect on port {port}"
    );

    // Should be accessible regardless of IPv4 or IPv6
    let result = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}")).await;
    assert!(result.is_ok(), "Should connect via IPv4 localhost");

    handle.abort();

    Ok(())
}

/// End-to-end proof that the scrape budget reaches the HTTP layer.
///
/// A scrape cannot complete in a single nanosecond, so `/metrics` must answer
/// `504 Gateway Timeout` rather than blocking, returning a half-built exposition, or —
/// as in `nbari/pg_exporter#34` — wedging the gate.
#[tokio::test]
async fn metrics_answers_504_when_the_scrape_budget_is_exhausted() -> Result<()> {
    let port = common::get_available_port();
    let dsn = SecretString::from(common::get_test_dsn());

    let handle = tokio::spawn(async move {
        mariadb_exporter::exporter::new(
            port,
            None,
            dsn,
            CollectorConfig::new()
                .with_enabled(&["default".to_string()])
                .with_scrape_timeout(std::time::Duration::from_nanos(1)),
        )
        .await
    });

    assert!(
        common::wait_for_server(port, 50).await,
        "Server failed to start on port {port}"
    );

    // Twice: the second request proves the gate reopened after the timeout instead of
    // staying closed behind an abandoned scrape.
    for attempt in 1..=2 {
        let response = reqwest::get(format!("{}/metrics", common::get_test_url(port))).await?;
        assert_eq!(
            response.status().as_u16(),
            504,
            "attempt {attempt}: an exhausted scrape budget must be reported as 504"
        );

        // A timed-out scrape produces no exposition, so the body is a comment naming the
        // cause — matching the handler's `# Error collecting metrics: {error}` format.
        let body = response.text().await?;
        assert!(
            body.starts_with("# Error collecting metrics"),
            "attempt {attempt}: a 504 body must stay a valid comment-only exposition, got {body:?}"
        );
        assert_comment_only_exposition(&body, &format!("attempt {attempt}: 504 response"));
        assert!(
            body.contains("scrape exceeded timeout of 1ns"),
            "attempt {attempt}: the body must name the exhausted budget, got {body:?}"
        );
    }

    handle.abort();

    Ok(())
}

/// The scrape budget must not leak into the liveness probe.
///
/// `/health` does reach the database — it acquires a connection and pings, and answers 503
/// when the server is down. What it does not do is go through the single-flight scrape gate
/// or `--scrape.timeout-ms`, so an exhausted scrape budget cannot take liveness down with
/// it. That independence is what this pins.
#[tokio::test]
async fn health_stays_available_while_scrapes_time_out() -> Result<()> {
    let port = common::get_available_port();
    let dsn = SecretString::from(common::get_test_dsn());

    let handle = tokio::spawn(async move {
        mariadb_exporter::exporter::new(
            port,
            None,
            dsn,
            CollectorConfig::new()
                .with_enabled(&["default".to_string()])
                .with_scrape_timeout(std::time::Duration::from_nanos(1)),
        )
        .await
    });

    assert!(
        common::wait_for_server(port, 50).await,
        "Server failed to start on port {port}"
    );

    let response = reqwest::get(format!("{}/health", common::get_test_url(port))).await?;
    assert!(response.status().is_success());

    handle.abort();

    Ok(())
}

/// A scrape abandoned on the budget must leave a counted trace.
///
/// Per-collector abort counters are started *after* the connectivity check, so a scrape that
/// times out while `SELECT 1` is still in flight moves none of them: without a scrape-level
/// counter that abort is invisible. This drives the real `collect_all` path — gate, budget,
/// timeout arm — and then reads the counter back out of a rendered exposition.
///
/// The render needs a second, healthy pool because a timed-out scrape returns no exposition
/// at all (`/metrics` answers 504 with a comment). `collect_all` takes the pool per call, so
/// one registry can be driven with a stalled pool and then with a working one.
#[tokio::test]
async fn an_abandoned_scrape_is_counted_in_the_exposition() -> Result<()> {
    use mariadb_exporter::collectors::registry::{CollectorRegistry, ScrapeError};
    use sqlx::MySqlPool;
    use tokio::net::TcpListener;

    // A server that accepts TCP and then never speaks MySQL: the handshake blocks until the
    // budget expires, with no dependence on network conditions or DNS.
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let stalled_addr = listener.local_addr()?;
    let stalled_server = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });

    let registry = CollectorRegistry::new(
        &CollectorConfig::new()
            .with_enabled(&["exporter".to_string()])
            .with_scrape_timeout(std::time::Duration::from_millis(500)),
    );

    let stalled = MySqlPool::connect_lazy(&format!("mysql://root@{stalled_addr}/mysql"))?;
    let outcome = registry.collect_all(&stalled).await;
    assert!(
        matches!(outcome, Err(ScrapeError::Timeout(_))),
        "a scrape against a server that never completes the handshake must hit the budget, \
         got {outcome:?}"
    );

    let healthy = common::create_test_pool().await?;

    // The healthy verification scrape shares the 500ms budget, so on a starved runner even
    // it can time out — which a single attempt then misreports as a wedged gate. A timeout
    // returns Err and the gate reopens, so retrying is safe; but every timed-out retry is
    // itself counted as an abort, so track them instead of asserting a fixed total.
    let mut expected_aborts = 1_u64;
    let mut body = None;
    for _ in 0..20 {
        match registry.collect_all(&healthy).await {
            Ok(rendered) => {
                body = Some(rendered);
                break;
            }
            Err(ScrapeError::Timeout(_)) => expected_aborts += 1,
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "the gate must reopen after a timeout: {error}"
                ));
            }
        }
    }
    let body = body.ok_or_else(|| {
        anyhow::anyhow!("a healthy scrape must fit the budget within 20 attempts")
    })?;

    let expected_line = format!("mariadb_exporter_scrape_aborted_total {expected_aborts}");
    assert!(
        body.lines().any(|line| line == expected_line),
        "the abandoned scrape must be counted once the exposition is available: \
         expected `{expected_line}` in:\n{body}"
    );

    stalled_server.abort();
    healthy.close().await;

    Ok(())
}

/// `/metrics` is single-flight: while one scrape holds the gate, the next one must be
/// refused with 503 — never queued behind a stalled server, doubling its load.
///
/// The exporter's DSN points at a server that accepts TCP and then never speaks MySQL, so
/// scrape A parks in the handshake and holds the gate (the 30s budget and the pool's
/// acquire timeout are both far longer than request B's arrival). The fake server signals
/// when A's connection is accepted; because the connectivity check starts after gate
/// acquisition, B is sent only once the test has proof that A holds the permit. Attempts
/// remain bounded so a startup failure cannot hang CI or pass vacuously.
#[tokio::test]
async fn concurrent_scrape_answers_503() -> Result<()> {
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    // A server that accepts TCP and then never speaks MySQL: the handshake blocks until the
    // budget expires, with no dependence on network conditions or DNS.
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let stalled_addr = listener.local_addr()?;
    let (accepted_tx, mut accepted_rx) = mpsc::unbounded_channel();
    let stalled_server = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
            let _ = accepted_tx.send(());
        }
    });

    let (port, server) = common::start_exporter_with_retry(
        SecretString::from(format!("mysql://root@{stalled_addr}/mysql")),
        CollectorConfig::new()
            .with_enabled(&["default".to_string()])
            .with_scrape_timeout(std::time::Duration::from_secs(30)),
    )
    .await?;

    let url = format!("{}/metrics", common::get_test_url(port));
    // Request A gets a client without a timeout: it is meant to park on the stalled server
    // until aborted. Request B must not wait out the 30s budget when it loses the race for
    // the gate, so it gets a short client timeout.
    let client_a = reqwest::Client::new();
    let client_b = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?;

    let mut refused_body = None;
    for _attempt in 1..=5 {
        while accepted_rx.try_recv().is_ok() {}

        // Request A: occupies the gate once its scrape starts. Never awaited — it is
        // aborted below; dropping its future closes the connection, which releases the
        // server-side gate for the next attempt.
        let a = tokio::spawn({
            let client = client_a.clone();
            let url = url.clone();
            async move { client.get(url).send().await }
        });

        // The connection is opened only after A acquired the scrape gate and reached the
        // connectivity check. Waiting for the fake server to accept it proves the permit is
        // held; a fixed sleep would merely guess at that ordering on a loaded runner.
        let reached_stalled_server =
            tokio::time::timeout(std::time::Duration::from_secs(5), accepted_rx.recv())
                .await
                .is_ok_and(|accepted| accepted.is_some());
        if !reached_stalled_server {
            a.abort();
            continue;
        }

        let outcome_b = client_b.get(&url).send().await;
        match outcome_b {
            Ok(response) if response.status().as_u16() == 503 => {
                let body = response.text().await?;
                assert!(
                    body.starts_with("# Error collecting metrics"),
                    "a 503 must still be a comment-only exposition, got {body:?}"
                );
                assert_comment_only_exposition(&body, "503 response");
                refused_body = Some(body);
                a.abort();
                break;
            }
            _ => {
                // A lost the race (B now holds the gate until its client timeout drops it)
                // or has not reached the gate yet; settle, then start over with a fresh A.
                a.abort();
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
        }
    }

    server.abort();
    stalled_server.abort();

    assert!(
        refused_body.is_some(),
        "request B never observed 503 after request A provably reached the stalled server: \
         the single-flight gate is broken"
    );

    Ok(())
}

/// The scrape gate must not gate the liveness probe.
///
/// Router-level (no socket): `build_router` serves from a healthy pool while a spawned
/// `collect_all` — driven against a server that never completes the MySQL handshake —
/// holds the gate for its 30s budget. Cloning the registry shares the gate
/// (`Arc<Semaphore>`), so the router and the spawned scrape contend on the same permit.
/// This is the stronger form of the property: `/health` answers **200 from its own pool**
/// while a scrape is parked, not merely "answers with some status" against a stalled one.
#[tokio::test]
async fn health_is_answered_while_the_scrape_gate_is_held() -> Result<()> {
    use axum::{body::Body, http::Request};
    use mariadb_exporter::{collectors::registry::CollectorRegistry, exporter::build_router};
    use sqlx::MySqlPool;
    use tokio::net::TcpListener;
    use tower::ServiceExt as _;

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let stalled_addr = listener.local_addr()?;
    let stalled_server = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });

    let registry = CollectorRegistry::new(
        &CollectorConfig::new()
            .with_enabled(&["exporter".to_string()])
            .with_scrape_timeout(std::time::Duration::from_secs(30)),
    );

    let stalled = MySqlPool::connect_lazy(&format!("mysql://root@{stalled_addr}/mysql"))?;

    // Hold the gate: the handshake never completes, so this scrape pends until aborted.
    // The permit is taken synchronously on first poll, before the scrape awaits anything.
    let holder = tokio::spawn({
        let registry = registry.clone();
        async move { registry.collect_all(&stalled).await }
    });

    let app = build_router(common::create_test_pool().await?, registry);
    let probe = |path: &str| {
        app.clone().oneshot(
            Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("a static request always builds"),
        )
    };

    // Confirm the gate is actually held — `/metrics` refused with 503 — before trusting
    // the /health assertion below; bounded retries cover runner scheduling, and failing
    // here keeps the test from proving nothing.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let mut gate_held = false;
    for _ in 0..20 {
        if probe("/metrics").await?.status().as_u16() == 503 {
            gate_held = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        gate_held,
        "the stalled scrape never took the gate; a /health answer would prove nothing"
    );

    let health = probe("/health").await?;
    assert_eq!(
        health.status().as_u16(),
        200,
        "/health must be answered from its own pool while the scrape gate is held"
    );

    // The gate must still be held afterwards: answering /health must not have touched it.
    assert_eq!(
        probe("/metrics").await?.status().as_u16(),
        503,
        "/health must not acquire or release the scrape gate"
    );

    holder.abort();
    stalled_server.abort();

    Ok(())
}
