use crate::collectors::Collector;
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::Registry;
use sqlx::MySqlPool;
use tracing::instrument;

pub mod certificate;
pub mod status;

use status::SslStatusCollector;

/// TLS collector (opt-in). Collects SSL/TLS status from `MariaDB`.
#[derive(Clone)]
pub struct TlsCollector {
    ssl_status: SslStatusCollector,
}

impl TlsCollector {
    #[must_use]
    /// Create a new TLS collector.
    pub fn new() -> Self {
        Self {
            ssl_status: SslStatusCollector::new(),
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
        registry.register(Box::new(self.ssl_status.server_configured().clone()))?;
        registry.register(Box::new(self.ssl_status.version_info().clone()))?;
        registry.register(Box::new(self.ssl_status.cert_not_before_seconds().clone()))?;
        registry.register(Box::new(self.ssl_status.cert_not_after_seconds().clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "tls", otel.kind = "internal"))]
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.ssl_status.collect(pool).await?;
            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
