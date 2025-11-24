use crate::collectors::Collector;
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{IntGauge, Registry};
use sqlx::MySqlPool;
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;

/// Audit plugin presence (opt-in; reports 1 if enabled).
#[derive(Clone)]
pub struct AuditCollector {
    audit_log_enabled: IntGauge,
}

impl AuditCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    /// Create a new audit collector.
    ///
    /// # Panics
    ///
    /// Panics if metric names are invalid (should not occur with static names).
    pub fn new() -> Self {
        let audit_log_enabled = IntGauge::new(
            "mariadb_audit_log_enabled",
            "Whether the audit_log plugin is active (1/0)",
        )
        .expect("valid mariadb_audit_log_enabled metric");

        Self { audit_log_enabled }
    }
}

impl Default for AuditCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for AuditCollector {
    fn name(&self) -> &'static str {
        "audit"
    }

    #[instrument(
        skip(self, registry),
        level = "info",
        err,
        fields(collector = "audit")
    )]
    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.audit_log_enabled.clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "audit", otel.kind = "internal"))]
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "SELECT PLUGIN_STATUS FROM information_schema.plugins WHERE PLUGIN_NAME='audit_log'",
                otel.kind = "client"
            );

            let status: Option<String> = sqlx::query_scalar(
                "SELECT PLUGIN_STATUS FROM information_schema.plugins WHERE PLUGIN_NAME='audit_log'",
            )
            .fetch_optional(pool)
            .instrument(span)
            .await
            .unwrap_or(None);

            let enabled = matches!(status.as_deref(), Some("ACTIVE"));
            self.audit_log_enabled.set(i64::from(enabled));
            if !enabled {
                debug!("audit_log plugin not active");
            }
            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
