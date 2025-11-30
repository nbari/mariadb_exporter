use crate::collectors::Collector;
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{IntGauge, Registry};
use sqlx::MySqlPool;
use tracing::{info_span, instrument};
use tracing_futures::Instrument as _;

/// Plugin status collector (always-on; reports `audit_log` and `userstat` status).
#[derive(Clone)]
pub struct PluginsCollector {
    audit_log_enabled: IntGauge,
    userstat_enabled: IntGauge,
}

impl PluginsCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    /// Create a new plugins collector.
    ///
    /// # Panics
    ///
    /// Panics if metric names are invalid (should not occur with static names).
    pub fn new() -> Self {
        let audit_log_enabled = IntGauge::new(
            "mariadb_audit_log_enabled",
            "Whether the audit_log plugin is active (1=enabled, 0=disabled)",
        )
        .expect("valid mariadb_audit_log_enabled metric");

        let userstat_enabled = IntGauge::new(
            "mariadb_userstat_enabled",
            "Whether user statistics are enabled (1=enabled, 0=disabled)",
        )
        .expect("valid mariadb_userstat_enabled metric");

        Self {
            audit_log_enabled,
            userstat_enabled,
        }
    }
}

impl Default for PluginsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for PluginsCollector {
    fn name(&self) -> &'static str {
        "plugins"
    }

    #[instrument(
        skip(self, registry),
        level = "info",
        err,
        fields(collector = "plugins")
    )]
    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.audit_log_enabled.clone()))?;
        registry.register(Box::new(self.userstat_enabled.clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "plugins", otel.kind = "internal"))]
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // Check audit_log plugin
            let audit_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "SELECT PLUGIN_STATUS FROM information_schema.plugins WHERE PLUGIN_NAME IN ('audit_log', 'SERVER_AUDIT')",
                otel.kind = "client"
            );

            let audit_status: Option<String> = sqlx::query_scalar(
                "SELECT PLUGIN_STATUS FROM information_schema.plugins WHERE PLUGIN_NAME IN ('audit_log', 'SERVER_AUDIT')",
            )
            .fetch_optional(pool)
            .instrument(audit_span)
            .await?;

            let audit_enabled = audit_status
                .as_deref()
                .map_or(0, |s| i64::from(s == "ACTIVE"));

            self.audit_log_enabled.set(audit_enabled);

            // Check userstat
            let userstat_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "SELECT @@userstat",
                otel.kind = "client"
            );

            let userstat: Option<i32> = sqlx::query_scalar("SELECT @@userstat")
                .fetch_optional(pool)
                .instrument(userstat_span)
                .await?;

            self.userstat_enabled.set(i64::from(userstat.unwrap_or(0)));

            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        true
    }
}
