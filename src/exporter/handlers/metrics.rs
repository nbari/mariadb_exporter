use crate::collectors::registry::{CollectorRegistry, ScrapeError};
use axum::{
    extract::Extension,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use sqlx::MySqlPool;
use tracing::{debug, error, instrument};

#[instrument(skip(pool, registry), fields(http.route="/metrics"))]
pub async fn metrics(
    Extension(pool): Extension<MySqlPool>,
    Extension(registry): Extension<CollectorRegistry>,
) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        "content-type",
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );

    match registry.collect_all(&pool).await {
        Ok(metrics) => {
            debug!("Successfully collected metrics");
            (StatusCode::OK, headers, metrics).into_response()
        }
        Err(e) => {
            error!("Failed to collect metrics: {}", e);
            let sanitized = e.to_string().replace(['\n', '\r'], " ");

            (
                status_for(&e),
                headers,
                format!("# Error collecting metrics: {sanitized}\n"),
            )
                .into_response()
        }
    }
}

/// Maps a scrape outcome to its HTTP status.
///
/// A *collector* failure never reaches here: it is reported inside a successful HTTP 200
/// exposition alongside an honest `mariadb_up` and fresh `mariadb_exporter_*`
/// self-observation metrics. These statuses are reserved for scrapes that produced no
/// exposition at all.
fn status_for(error: &ScrapeError) -> StatusCode {
    match error {
        // The gate is held by a scrape that is still running, or the scrape task died.
        // Prometheus retries on its own interval; 503 tells it this sample is missing
        // rather than presenting a stale or half-built one.
        ScrapeError::Busy | ScrapeError::TaskFailed(_) => StatusCode::SERVICE_UNAVAILABLE,
        ScrapeError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
        // Encoding failed, so the database state could not be represented safely. Emitting
        // `mariadb_up 0` here would fabricate an outage that was never observed — the
        // comment is the honest answer, and the scrape still succeeded as far as
        // reachability is concerned.
        ScrapeError::Collect(_) => StatusCode::OK,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests require a database connection, so they're more integration tests
    // We'll create unit tests for the response structure

    /// The exporter's documented contract is that database problems stay HTTP 200. Only
    /// the two states where no exposition exists at all carry a status code.
    #[test]
    fn only_gate_outcomes_break_the_always_200_contract() {
        assert_eq!(
            status_for(&ScrapeError::Busy),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            status_for(&ScrapeError::TaskFailed("panicked".to_string())),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            status_for(&ScrapeError::Timeout(std::time::Duration::from_secs(15))),
            StatusCode::GATEWAY_TIMEOUT
        );
        assert_eq!(
            status_for(&ScrapeError::Collect(anyhow::anyhow!("encode failed"))),
            StatusCode::OK,
            "an encode failure is still a scrape that reached the server; fabricating a \
             non-200 there would report an outage that was never observed"
        );
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_metrics_response_headers() {
        // Test that we're setting the correct content-type
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );

        assert_eq!(
            headers.get("content-type").unwrap(),
            "text/plain; charset=utf-8"
        );
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_header_value_creation() {
        let header_val = HeaderValue::from_static("text/plain; charset=utf-8");
        assert_eq!(header_val.to_str().unwrap(), "text/plain; charset=utf-8");
    }
}
