use crate::{
    cli::telemetry::shutdown_tracer,
    collectors::{
        config::CollectorConfig,
        registry::CollectorRegistry,
        util::{
            get_excluded_databases, parse_mariadb_version, set_base_connect_options_from_dsn,
            set_mariadb_version,
        },
    },
};
use anyhow::{Context, Result, anyhow};
use axum::{
    Extension, Router,
    body::Body,
    http::{HeaderName, HeaderValue, Request},
    routing::get,
};
use secrecy::{ExposeSecret, SecretString};
use sqlx::mysql::MySqlPoolOptions;
use std::time::Duration;
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_http::{
    request_id::PropagateRequestIdLayer, set_header::SetRequestHeaderLayer, trace::TraceLayer,
};
use tracing::{Span, error, info, info_span, warn};
use ulid::Ulid;

mod handlers;
mod shutdown;
#[cfg(feature = "telemetry")]
mod telemetry;

pub mod built_info {
    #![allow(clippy::doc_markdown)]
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

pub const GIT_COMMIT_HASH: Option<&str> = built_info::GIT_COMMIT_HASH;

/// Starts the `MariaDB` metrics exporter
///
/// # Errors
///
/// Returns an error if database connection, HTTP server setup, or shutdown handling fails
pub async fn new(
    port: u16,
    listen: Option<String>,
    dsn: SecretString,
    config: CollectorConfig,
) -> Result<()> {
    let pool = connect_pool(&dsn)?;

    if let Err(e) = initialize_version(&pool).await {
        warn!(
            "Failed to initialize MariaDB version at startup: {}. Will retry during collection.",
            e
        );
    }

    let _ = set_base_connect_options_from_dsn(&dsn);

    let mut collectors: Vec<String> = config.enabled_collectors.iter().cloned().collect();
    collectors.sort();

    let registry = CollectorRegistry::new(&config);

    let app = build_router(pool.clone(), registry);

    let (listener, bind_addr) = bind_listener(port, listen).await?;

    let excluded = get_excluded_databases();

    print_startup(&bind_addr, &collectors, excluded);

    run_server(listener, app).await;

    info!("shutting down");

    shutdown_tracer();

    Ok(())
}

fn connect_pool(dsn: &SecretString) -> Result<sqlx::MySqlPool> {
    let db_dsn = dsn.expose_secret().to_string();

    let pool = MySqlPoolOptions::new()
        .min_connections(0)
        .max_connections(3)
        .max_lifetime(Duration::from_mins(2))
        .acquire_timeout(Duration::from_secs(2))
        .test_before_acquire(true)
        .connect_lazy(&db_dsn)?;

    info!("Database pool initialized (lazy)");

    Ok(pool)
}

async fn initialize_version(pool: &sqlx::MySqlPool) -> Result<()> {
    let version_string: String = sqlx::query_scalar("SELECT VERSION()")
        .fetch_one(pool)
        .await
        .context("Failed to get MariaDB version")?;

    let version_num = parse_mariadb_version(&version_string);

    set_mariadb_version(version_num);
    info!(version = version_num, "MariaDB version detected");
    Ok(())
}

/// Builds the HTTP router without binding a socket.
///
/// The server path calls this with the shared pool and registry; integration tests call it
/// directly to drive the handlers in-process — e.g. proving `/health` never goes through
/// the registry's single-flight scrape gate while a scrape holds it.
pub fn build_router(pool: sqlx::MySqlPool, registry: CollectorRegistry) -> Router {
    let trace_layer = TraceLayer::new_for_http()
        .make_span_with(make_span)
        .on_response(on_response);

    let middleware = ServiceBuilder::new()
        .layer(SetRequestHeaderLayer::if_not_present(
            HeaderName::from_static("x-request-id"),
            |_req: &_| HeaderValue::from_str(Ulid::generate().to_string().as_str()).ok(),
        ))
        .layer(PropagateRequestIdLayer::new(HeaderName::from_static(
            "x-request-id",
        )))
        .layer(trace_layer);
    #[cfg(feature = "telemetry")]
    let middleware = middleware.layer(axum::middleware::from_fn(telemetry::add_trace_headers));

    Router::new()
        .route("/metrics", get(handlers::metrics))
        .route("/health", get(handlers::health).options(handlers::health))
        .layer(middleware.layer(Extension(pool)).layer(Extension(registry)))
}

async fn bind_listener(port: u16, listen: Option<String>) -> Result<(TcpListener, String)> {
    if let Some(addr) = listen {
        let ip = addr.parse::<std::net::IpAddr>().map_err(|_| {
            anyhow!(
                "Invalid IP address: '{addr}'. Expected IPv4 (e.g., 0.0.0.0, 127.0.0.1) or IPv6 (e.g., ::, ::1)"
            )
        })?;
        let bind_addr = format!("{ip}:{port}");
        let listener = TcpListener::bind(&bind_addr)
            .await
            .with_context(|| format!("Failed to bind to {bind_addr}"))?;
        let display = if ip.is_ipv6() {
            format!("[{ip}]:{port}")
        } else {
            bind_addr.clone()
        };
        Ok((listener, display))
    } else {
        if let Ok(listener) = TcpListener::bind(format!("::0:{port}")).await {
            return Ok((listener, format!("[::]:{port}")));
        }
        let listener = TcpListener::bind(format!("0.0.0.0:{port}")).await?;
        Ok((listener, format!("0.0.0.0:{port}")))
    }
}

fn print_startup(bind_addr: &str, collectors: &[String], excluded: &[String]) {
    println!(
        "{} {} - Listening on {bind_addr}\n\nEnabled collectors:\n{}",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        format_list(collectors),
    );

    if !excluded.is_empty() {
        println!("\nExcluded databases:\n{}", format_list(excluded));
    }
}

async fn run_server(listener: TcpListener, app: Router) {
    if let Err(e) = axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown::shutdown_signal_handler())
        .await
    {
        error!(error=%e, "server error");
    }
}

// Helper to format a list of items with a leading dash and indentation for the
// start up message
fn format_list<T: std::fmt::Display>(items: &[T]) -> String {
    items
        .iter()
        .map(|i| format!("  - {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn make_span(request: &Request<Body>) -> Span {
    let method = request.method().as_str();

    let path = request.uri().path();

    let target = request.uri().to_string();

    let scheme = request.uri().scheme_str().unwrap_or("http");

    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("none");

    let user_agent = request
        .headers()
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    let span = info_span!(
        "http.server.request",
        otel.kind = "server",
        otel.status_code = tracing::field::Empty,
        http.method = method,
        http.route = path,
        http.target = target,
        http.scheme = scheme,
        http.user_agent = user_agent,
        request_id = request_id,
    );

    #[cfg(feature = "telemetry")]
    telemetry::set_parent(request, &span);

    span
}

fn on_response<B>(response: &axum::http::Response<B>, latency: Duration, span: &Span) {
    if response.status().is_server_error() {
        span.record("otel.status_code", "ERROR");
    } else {
        span.record("otel.status_code", "OK");
    }

    #[allow(clippy::cast_possible_truncation)]
    let elapsed_ms = latency.as_millis() as u64;

    #[cfg(feature = "telemetry")]
    if let Some(trace_id) = telemetry::trace_id(span) {
        info!(
            parent: span,
            status = response.status().as_u16(),
            elapsed_ms,
            trace_id = %trace_id,
            "request completed"
        );
        return;
    }
    info!(
        parent: span,
        status = response.status().as_u16(),
        elapsed_ms,
        "request completed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_records_status_on_request_span() -> Result<()> {
        use crate::trace_capture;
        use anyhow::{Context as _, ensure};

        let (subscriber, observations) = trace_capture::subscriber();
        tracing::subscriber::with_default(subscriber, || -> Result<()> {
            for (status, expected) in [(200, "OK"), (400, "OK"), (500, "ERROR"), (503, "ERROR")] {
                let request = Request::builder()
                    .uri("/metrics")
                    .header("x-request-id", "status-probe")
                    .body(Body::empty())?;
                let span = make_span(&request);
                let id = span.id().context("request span disabled")?;
                let response = axum::http::Response::builder().status(status).body(())?;
                on_response(&response, Duration::from_millis(1), &span);
                let event = observations
                    .try_iter()
                    .find(|event| {
                        event
                            .fields
                            .get("message")
                            .is_some_and(|message| message == "request completed")
                    })
                    .context("completion log missing")?;
                trace_capture::assert_request(&event, &id, "status-probe")?;
                ensure!(
                    event
                        .scope
                        .first()
                        .and_then(|root| root.fields.get("otel.status_code"))
                        .map(String::as_str)
                        == Some(expected),
                    "status {status} was not recorded: {event:?}"
                );
            }
            Ok(())
        })
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_git_commit_hash_is_valid_if_present() {
        // GIT_COMMIT_HASH is an Option - either Some(hash) or None
        if let Some(hash) = GIT_COMMIT_HASH {
            // If present, should be a valid git hash (hex string)
            assert!(
                hash.len() >= 7,
                "Git commit hash should be at least 7 chars, got: {hash}"
            );
            assert!(
                hash.chars().all(|c| c.is_ascii_hexdigit()),
                "Git commit hash should be hex digits, got: {hash}"
            );
        } else {
            // None is valid when not built from git (e.g., cargo install from crates.io)
            println!("No git commit hash available (normal for crates.io installs)");
        }
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_format_list_empty() {
        let items: Vec<String> = vec![];
        let result = format_list(&items);
        assert_eq!(result, "");
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_format_list_single_item() {
        let items = vec!["item1"];
        let result = format_list(&items);
        assert_eq!(result, "  - item1");
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_format_list_multiple_items() {
        let items = vec!["item1", "item2", "item3"];
        let result = format_list(&items);
        assert_eq!(result, "  - item1\n  - item2\n  - item3");
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_format_list_with_numbers() {
        let items = vec![1, 2, 3];
        let result = format_list(&items);
        assert_eq!(result, "  - 1\n  - 2\n  - 3");
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_format_list_formatting() {
        let items = vec!["collector1", "collector2"];
        let result = format_list(&items);

        // Should start with two spaces and a dash
        assert!(result.starts_with("  - "));

        // Should contain both items
        assert!(result.contains("collector1"));
        assert!(result.contains("collector2"));

        // Should have newline between items
        assert!(result.contains('\n'));
    }

    // Test the on_response function behavior
    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_on_response_status_codes() {
        use axum::http::{Response, StatusCode};
        use std::time::Duration;
        use tracing::info_span;

        let span = info_span!("test");

        // Test with 200 OK
        let response_ok = Response::builder().status(StatusCode::OK).body(()).unwrap();

        let latency = Duration::from_millis(100);

        // This should not panic
        on_response(&response_ok, latency, &span);

        // Test with 500 error
        let response_err = Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(())
            .unwrap();

        on_response(&response_err, latency, &span);
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_make_span_creates_span() {
        use axum::body::Body;
        use axum::http::Request;

        let request = Request::builder()
            .method("GET")
            .uri("/metrics")
            .header("user-agent", "test-client")
            .body(Body::empty())
            .unwrap();

        let span = make_span(&request);

        // Verify span was created with correct metadata
        assert_eq!(
            span.metadata().map(tracing::Metadata::name),
            Some("http.server.request")
        );
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_make_span_with_request_id() {
        use axum::body::Body;
        use axum::http::Request;

        let request = Request::builder()
            .method("POST")
            .uri("/health")
            .header("x-request-id", "test-id-12345")
            .header("user-agent", "Mozilla/5.0")
            .body(Body::empty())
            .unwrap();

        let span = make_span(&request);

        // Just verify it doesn't panic and the span has the correct name
        assert_eq!(
            span.metadata().map(tracing::Metadata::name),
            Some("http.server.request")
        );
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_make_span_without_optional_headers() {
        use axum::body::Body;
        use axum::http::Request;

        let request = Request::builder()
            .method("GET")
            .uri("/")
            .body(Body::empty())
            .unwrap();

        let span = make_span(&request);

        // Should still create a valid span even without optional headers
        assert_eq!(
            span.metadata().map(tracing::Metadata::name),
            Some("http.server.request")
        );
    }
}
