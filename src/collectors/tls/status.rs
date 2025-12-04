use super::certificate::parse_ssl_timestamp;
use anyhow::Result;
use prometheus::{Gauge, IntGauge, IntGaugeVec, Opts};
use sqlx::MySqlPool;
use tracing::{debug, info_span, instrument, warn};
use tracing_futures::Instrument as _;

/// Collector for SSL/TLS status metrics.
#[derive(Clone)]
pub struct SslStatusCollector {
    server_configured: IntGauge,
    version_info: IntGaugeVec,
    cert_not_before_seconds: Gauge,
    cert_not_after_seconds: Gauge,
}

impl SslStatusCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    /// Create a new SSL status collector.
    ///
    /// # Panics
    ///
    /// Panics if metric names are invalid (should not occur with static names).
    pub fn new() -> Self {
        let server_configured = IntGauge::new(
            "mariadb_ssl_server_configured",
            "Whether the MariaDB server has SSL/TLS configured (1) or not (0)",
        )
        .expect("valid mariadb_ssl_server_configured metric");

        let version_info = IntGaugeVec::new(
            Opts::new(
                "mariadb_ssl_version_info",
                "TLS version and cipher configured on the server",
            ),
            &["version", "cipher"],
        )
        .expect("valid mariadb_ssl_version_info metric");

        let cert_not_before_seconds = Gauge::new(
            "mariadb_ssl_cert_not_before_seconds",
            "Unix timestamp of the SSL certificate's not-before date",
        )
        .expect("valid mariadb_ssl_cert_not_before_seconds metric");

        let cert_not_after_seconds = Gauge::new(
            "mariadb_ssl_cert_not_after_seconds",
            "Unix timestamp of the SSL certificate's not-after (expiration) date",
        )
        .expect("valid mariadb_ssl_cert_not_after_seconds metric");

        Self {
            server_configured,
            version_info,
            cert_not_before_seconds,
            cert_not_after_seconds,
        }
    }

    /// Get server configured metric.
    #[must_use]
    pub const fn server_configured(&self) -> &IntGauge {
        &self.server_configured
    }

    /// Get version info metric.
    #[must_use]
    pub const fn version_info(&self) -> &IntGaugeVec {
        &self.version_info
    }

    /// Get certificate not before metric.
    #[must_use]
    pub const fn cert_not_before_seconds(&self) -> &Gauge {
        &self.cert_not_before_seconds
    }

    /// Get certificate not after metric.
    #[must_use]
    pub const fn cert_not_after_seconds(&self) -> &Gauge {
        &self.cert_not_after_seconds
    }

    /// Collect SSL status metrics from SHOW STATUS.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails (though queries are best-effort).
    #[instrument(skip(self, pool), level = "debug", fields(sub_collector = "ssl_status"))]
    pub async fn collect(&self, pool: &MySqlPool) -> Result<()> {
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
                        self.server_configured.set(0);
                    } else {
                        self.server_configured.set(1);

                        // Set version and cipher info
                        if let Some(cipher) = ssl_data.get("Ssl_cipher") {
                            self.version_info
                                .with_label_values(&[version, cipher])
                                .set(1);
                        }

                        // Parse certificate timestamps
                        if let Some(not_before) = ssl_data.get("Ssl_server_not_before") {
                            match parse_ssl_timestamp(not_before) {
                                Ok(timestamp) => {
                                    self.cert_not_before_seconds.set(timestamp);
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
                                    self.cert_not_after_seconds.set(timestamp);
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
                    self.server_configured.set(0);
                }
            }
            Err(e) => {
                debug!(error = %e, "Failed to query SSL status; setting not configured");
                self.server_configured.set(0);
            }
        }
        Ok(())
    }
}

impl Default for SslStatusCollector {
    fn default() -> Self {
        Self::new()
    }
}
