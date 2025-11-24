use crate::collectors::Collector;
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{IntGauge, IntGaugeVec, Opts, Registry};
use sqlx::MySqlPool;
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;

/// TLS collector (opt-in). Works even if SSL is disabled by reporting zeros.
#[derive(Clone)]
pub struct TlsCollector {
    tls_session_active: IntGauge,
    tls_version_info: IntGaugeVec,
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
        let tls_session_active = IntGauge::new(
            "mariadb_tls_session_active",
            "Whether the current connection uses TLS (1/0)",
        )
        .expect("valid mariadb_tls_session_active metric");

        let tls_version_info = IntGaugeVec::new(
            Opts::new(
                "mariadb_tls_version_info",
                "TLS version and cipher in use for the session",
            ),
            &["version", "cipher"],
        )
        .expect("valid mariadb_tls_version_info metric");

        Self {
            tls_session_active,
            tls_version_info,
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
        registry.register(Box::new(self.tls_session_active.clone()))?;
        registry.register(Box::new(self.tls_version_info.clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "tls", otel.kind = "internal"))]
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "SELECT @@ssl_version, @@ssl_cipher",
                otel.kind = "client"
            );

            match sqlx::query_as::<_, (Option<String>, Option<String>)>(
                "SELECT @@ssl_version as v, @@ssl_cipher as c",
            )
            .fetch_one(pool)
            .instrument(span)
            .await
            {
                Ok((version, cipher)) => {
                    if let (Some(v), Some(c)) = (version, cipher) {
                        self.tls_session_active.set(1);
                        self.tls_version_info
                            .with_label_values(&[v.as_str(), c.as_str()])
                            .set(1);
                    } else {
                        self.tls_session_active.set(0);
                    }
                }
                Err(e) => {
                    debug!(error = %e, "TLS metadata unavailable; setting inactive");
                    self.tls_session_active.set(0);
                }
            }
            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
