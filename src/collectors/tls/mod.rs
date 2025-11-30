use crate::collectors::Collector;
use anyhow::Result;
use chrono::{DateTime, NaiveDateTime, Utc};
use futures::future::BoxFuture;
use prometheus::{Gauge, IntGauge, IntGaugeVec, Opts, Registry};
use sqlx::MySqlPool;
use tracing::{debug, info_span, instrument, warn};
use tracing_futures::Instrument as _;

/// TLS collector (opt-in). Collects SSL/TLS status from `MariaDB`.
#[derive(Clone)]
#[allow(clippy::struct_field_names)]
pub struct TlsCollector {
    ssl_server_configured: IntGauge,
    ssl_version_info: IntGaugeVec,
    ssl_cert_not_before_seconds: Gauge,
    ssl_cert_not_after_seconds: Gauge,
}

impl TlsCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    /// Create a new TLS collector.
    ///
    /// # Panics
    ///
    /// Panics if metric names are invalid (should not occur with static names).
    pub fn new() -> Self {
        let ssl_server_configured = IntGauge::new(
            "mariadb_ssl_server_configured",
            "Whether the MariaDB server has SSL/TLS configured (1) or not (0)",
        )
        .expect("valid mariadb_ssl_server_configured metric");

        let ssl_version_info = IntGaugeVec::new(
            Opts::new(
                "mariadb_ssl_version_info",
                "TLS version and cipher configured on the server",
            ),
            &["version", "cipher"],
        )
        .expect("valid mariadb_ssl_version_info metric");

        let ssl_cert_not_before_seconds = Gauge::new(
            "mariadb_ssl_cert_not_before_seconds",
            "Unix timestamp of the SSL certificate's not-before date",
        )
        .expect("valid mariadb_ssl_cert_not_before_seconds metric");

        let ssl_cert_not_after_seconds = Gauge::new(
            "mariadb_ssl_cert_not_after_seconds",
            "Unix timestamp of the SSL certificate's not-after (expiration) date",
        )
        .expect("valid mariadb_ssl_cert_not_after_seconds metric");

        Self {
            ssl_server_configured,
            ssl_version_info,
            ssl_cert_not_before_seconds,
            ssl_cert_not_after_seconds,
        }
    }
}

impl Default for TlsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for TlsCollector {
    fn name(&self) -> &'static str {
        "tls"
    }

    #[instrument(
        skip(self, registry),
        level = "info",
        err,
        fields(collector = "tls")
    )]
    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.ssl_server_configured.clone()))?;
        registry.register(Box::new(self.ssl_version_info.clone()))?;
        registry.register(Box::new(self.ssl_cert_not_before_seconds.clone()))?;
        registry.register(Box::new(self.ssl_cert_not_after_seconds.clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "tls", otel.kind = "internal"))]
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SHOW STATUS",
                db.statement = "SHOW STATUS WHERE Variable_name IN (...)",
                otel.kind = "client"
            );

            // Query SSL status variables
            // These are server status variables, not session variables
            let query = "
                SHOW STATUS WHERE Variable_name IN (
                    'Ssl_version',
                    'Ssl_cipher',
                    'Ssl_server_not_before',
                    'Ssl_server_not_after'
                )
            ";

            match sqlx::query_as::<_, (String, String)>(query)
                .fetch_all(pool)
                .instrument(span)
                .await
            {
                Ok(rows) => {
                    // Parse rows into a map for easier access
                    let mut ssl_data = std::collections::HashMap::new();
                    for (var_name, value) in rows {
                        ssl_data.insert(var_name, value);
                    }

                    // Check if SSL is configured by presence of Ssl_version
                    if let Some(version) = ssl_data.get("Ssl_version") {
                        if version.is_empty() {
                            // SSL not configured
                            self.ssl_server_configured.set(0);
                        } else {
                            self.ssl_server_configured.set(1);

                            // Set version and cipher info
                            if let Some(cipher) = ssl_data.get("Ssl_cipher") {
                                self.ssl_version_info
                                    .with_label_values(&[version, cipher])
                                    .set(1);
                            }

                            // Parse certificate timestamps
                            if let Some(not_before) = ssl_data.get("Ssl_server_not_before") {
                                match parse_ssl_timestamp(not_before) {
                                    Ok(timestamp) => {
                                        self.ssl_cert_not_before_seconds.set(timestamp);
                                    }
                                    Err(e) => {
                                        warn!(
                                            error = %e,
                                            value = %not_before,
                                            "Failed to parse Ssl_server_not_before"
                                        );
                                    }
                                }
                            }

                            if let Some(not_after) = ssl_data.get("Ssl_server_not_after") {
                                match parse_ssl_timestamp(not_after) {
                                    Ok(timestamp) => {
                                        self.ssl_cert_not_after_seconds.set(timestamp);
                                    }
                                    Err(e) => {
                                        warn!(
                                            error = %e,
                                            value = %not_after,
                                            "Failed to parse Ssl_server_not_after"
                                        );
                                    }
                                }
                            }
                        }
                    } else {
                        // No Ssl_version variable found - SSL not available
                        self.ssl_server_configured.set(0);
                    }
                }
                Err(e) => {
                    debug!(error = %e, "Failed to query SSL status; setting not configured");
                    self.ssl_server_configured.set(0);
                }
            }
            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}

/// Parse SSL certificate timestamp from `MariaDB` format.
///
/// `MariaDB` returns timestamps in format: `"Nov 28 05:59:29 2035 GMT"`
/// or `"May 24 11:46:23 2020 GMT"`
fn parse_ssl_timestamp(timestamp_str: &str) -> Result<f64> {
    // Parse the timestamp string
    // Format: "Nov 28 05:59:29 2035 GMT"
    let dt = NaiveDateTime::parse_from_str(timestamp_str, "%b %d %H:%M:%S %Y GMT")
        .or_else(|_| {
            // Try alternative format without GMT suffix
            NaiveDateTime::parse_from_str(
                timestamp_str.trim_end_matches(" GMT"),
                "%b %d %H:%M:%S %Y",
            )
        })
        .map_err(|e| anyhow::anyhow!("Failed to parse timestamp '{timestamp_str}': {e}"))?;

    // Convert to UTC DateTime
    let utc_dt: DateTime<Utc> = DateTime::from_naive_utc_and_offset(dt, Utc);

    // Return Unix timestamp as f64
    // Note: i64 to f64 conversion is safe for timestamps in the valid range
    // (years ~1677-2262), precision loss only matters for nanoseconds
    #[allow(clippy::cast_precision_loss)]
    Ok(utc_dt.timestamp() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_parse_ssl_timestamp() {
        // Test valid timestamp
        let ts = parse_ssl_timestamp("Nov 28 05:59:29 2035 GMT").unwrap();
        assert!(ts > 0.0);

        // Test another format
        let ts2 = parse_ssl_timestamp("May 24 11:46:23 2020 GMT").unwrap();
        assert!(ts2 > 0.0);
        assert!(ts > ts2); // 2035 should be after 2020

        // Test invalid timestamp
        assert!(parse_ssl_timestamp("invalid").is_err());
    }

    #[test]
    #[allow(clippy::unwrap_used, clippy::float_cmp)]
    fn test_ssl_timestamp_conversion() {
        // Known timestamp for verification
        let ts = parse_ssl_timestamp("Jan 01 00:00:00 2020 GMT").unwrap();
        // 2020-01-01 00:00:00 UTC = 1577836800
        assert_eq!(ts, 1_577_836_800.0);
    }
}
