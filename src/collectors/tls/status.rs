use super::certificate::parse_ssl_timestamp;
use crate::collectors::{
    Collected, Collector, NO_LABELS,
    util::{DeniedOnce, QueryFailure, classify_query_error},
};
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{GaugeVec, IntGaugeVec, Opts, Registry};
use sqlx::MySqlPool;
use tracing::{debug, info_span, instrument, warn};
use tracing_futures::Instrument as _;

/// Collector for SSL/TLS status metrics.
#[derive(Clone)]
pub struct SslStatusCollector {
    server_configured: IntGaugeVec,
    version_info: IntGaugeVec,
    cert_not_before_seconds: GaugeVec,
    cert_not_after_seconds: GaugeVec,
    denied: DeniedOnce,
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
        // Zero-label vectors: `0` is a factual claim ("TLS is off") that must never be
        // published when the exporter simply could not read the server's TLS state.
        let server_configured = IntGaugeVec::new(
            Opts::new(
                "mariadb_ssl_server_configured",
                "Whether the MariaDB server has SSL/TLS configured (1) or not (0)",
            ),
            &NO_LABELS,
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

        let cert_not_before_seconds = GaugeVec::new(
            Opts::new(
                "mariadb_ssl_cert_not_before_seconds",
                "Unix timestamp of the SSL certificate's not-before date",
            ),
            &NO_LABELS,
        )
        .expect("valid mariadb_ssl_cert_not_before_seconds metric");

        let cert_not_after_seconds = GaugeVec::new(
            Opts::new(
                "mariadb_ssl_cert_not_after_seconds",
                "Unix timestamp of the SSL certificate's not-after (expiration) date",
            ),
            &NO_LABELS,
        )
        .expect("valid mariadb_ssl_cert_not_after_seconds metric");

        Self {
            server_configured,
            version_info,
            cert_not_before_seconds,
            cert_not_after_seconds,
            denied: DeniedOnce::default(),
        }
    }

    /// Get server configured metric.
    #[must_use]
    pub const fn server_configured(&self) -> &IntGaugeVec {
        &self.server_configured
    }

    /// Get version info metric.
    #[must_use]
    pub const fn version_info(&self) -> &IntGaugeVec {
        &self.version_info
    }

    /// Get certificate not before metric.
    #[must_use]
    pub const fn cert_not_before_seconds(&self) -> &GaugeVec {
        &self.cert_not_before_seconds
    }

    /// Get certificate not after metric.
    #[must_use]
    pub const fn cert_not_after_seconds(&self) -> &GaugeVec {
        &self.cert_not_after_seconds
    }

    /// Publish the timestamp of an optional certificate field.
    ///
    /// A field that is absent or unparseable in an otherwise successful read removes only
    /// that series — keeping the previous scrape's expiry date would silently hide a
    /// certificate rotation.
    fn set_cert_timestamp(
        metric: &GaugeVec,
        raw: Option<&String>,
        variable: &'static str,
    ) {
        match raw {
            Some(value) => match parse_ssl_timestamp(value) {
                Ok(timestamp) => {
                    metric.with_label_values(&NO_LABELS).set(timestamp);
                }
                Err(e) => {
                    warn!(error = %e, value = %value, variable, "Failed to parse SSL certificate timestamp");
                    let _ = metric.remove_label_values(&NO_LABELS);
                }
            },
            None => {
                let _ = metric.remove_label_values(&NO_LABELS);
            }
        }
    }

    /// Collect SSL status metrics from `SHOW STATUS`.
    ///
    /// # Errors
    ///
    /// Returns an error if the status query fails for a reason other than the source being
    /// absent or unreadable.
    #[instrument(skip(self, pool), level = "debug", fields(sub_collector = "ssl_status"))]
    async fn collect_inner(&self, pool: &MySqlPool) -> Result<Collected> {
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

        let rows = match sqlx::query_as::<_, (String, String)>(query)
            .fetch_all(pool)
            .instrument(span)
            .await
        {
            Ok(rows) => rows,
            Err(e) => match classify_query_error(&e) {
                QueryFailure::Absent => {
                    debug!(error = %e, "SSL status variables unavailable; skipping");
                    return Ok(Collected::Skipped);
                }
                QueryFailure::Denied => {
                    self.denied.report("SHOW STATUS (Ssl_*)", &e);
                    return Ok(Collected::Skipped);
                }
                // An unreadable TLS state is not evidence that TLS is off.
                QueryFailure::Fault => return Err(e.into()),
            },
        };

        let ssl_data: std::collections::HashMap<String, String> = rows.into_iter().collect();

        // Reset only after the successful read, immediately before publishing.
        self.version_info.reset();

        // TLS genuinely off is a fresh, factual `0`; the certificate series are removed
        // because no certificate exists to describe.
        let Some(version) = ssl_data.get("Ssl_version").filter(|v| !v.is_empty()) else {
            self.server_configured.with_label_values(&NO_LABELS).set(0);
            let _ = self.cert_not_before_seconds.remove_label_values(&NO_LABELS);
            let _ = self.cert_not_after_seconds.remove_label_values(&NO_LABELS);
            return Ok(Collected::Fresh);
        };

        self.server_configured.with_label_values(&NO_LABELS).set(1);

        if let Some(cipher) = ssl_data.get("Ssl_cipher") {
            self.version_info
                .with_label_values(&[version, cipher])
                .set(1);
        }

        Self::set_cert_timestamp(
            &self.cert_not_before_seconds,
            ssl_data.get("Ssl_server_not_before"),
            "Ssl_server_not_before",
        );
        Self::set_cert_timestamp(
            &self.cert_not_after_seconds,
            ssl_data.get("Ssl_server_not_after"),
            "Ssl_server_not_after",
        );

        Ok(Collected::Fresh)
    }
}

impl Collector for SslStatusCollector {
    fn name(&self) -> &'static str {
        "ssl_status"
    }

    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.server_configured.clone()))?;
        registry.register(Box::new(self.version_info.clone()))?;
        registry.register(Box::new(self.cert_not_before_seconds.clone()))?;
        registry.register(Box::new(self.cert_not_after_seconds.clone()))?;
        Ok(())
    }

    fn collect_once<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
        Box::pin(async move { self.collect_inner(pool).await })
    }

    fn reset_metrics(&self) {
        self.server_configured.reset();
        self.version_info.reset();
        self.cert_not_before_seconds.reset();
        self.cert_not_after_seconds.reset();
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}

impl Default for SslStatusCollector {
    fn default() -> Self {
        Self::new()
    }
}
