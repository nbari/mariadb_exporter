use crate::collectors::{
    Collected, Collector, NO_LABELS,
    util::{DeniedOnce, QueryFailure, classify_query_error},
};
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{IntGaugeVec, Opts, Registry};
use sqlx::MySqlPool;
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;

/// Plugin status collector (always-on; reports `audit_log` and `userstat` status).
///
/// Both metrics are factual claims — `0` means "the plugin is installed but inactive" and
/// "user statistics are switched off". Neither may be published when the exporter could not
/// read the state, so both are zero-label vectors that disappear instead of reading `0`.
#[derive(Clone)]
pub struct PluginsCollector {
    audit_log_enabled: IntGaugeVec,
    userstat_enabled: IntGaugeVec,
    audit_denied: DeniedOnce,
    userstat_denied: DeniedOnce,
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
        let audit_log_enabled = IntGaugeVec::new(
            Opts::new(
                "mariadb_audit_log_enabled",
                "Whether the audit_log plugin is active (1=enabled, 0=disabled)",
            ),
            &NO_LABELS,
        )
        .expect("valid mariadb_audit_log_enabled metric");

        let userstat_enabled = IntGaugeVec::new(
            Opts::new(
                "mariadb_userstat_enabled",
                "Whether user statistics are enabled (1=enabled, 0=disabled)",
            ),
            &NO_LABELS,
        )
        .expect("valid mariadb_userstat_enabled metric");

        Self {
            audit_log_enabled,
            userstat_enabled,
            audit_denied: DeniedOnce::default(),
            userstat_denied: DeniedOnce::default(),
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
    fn collect_once<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
        Box::pin(async move {
            // The two signals come from independent sources, so they settle independently:
            // an unreadable `@@userstat` must not erase a freshly read audit-plugin state.
            let audit_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "SELECT PLUGIN_STATUS FROM information_schema.plugins WHERE PLUGIN_NAME IN ('audit_log', 'SERVER_AUDIT')",
                otel.kind = "client"
            );

            let audit_result: Result<Option<String>, sqlx::Error> = sqlx::query_scalar(
                "SELECT PLUGIN_STATUS FROM information_schema.plugins WHERE PLUGIN_NAME IN ('audit_log', 'SERVER_AUDIT')",
            )
            .fetch_optional(pool)
            .instrument(audit_span)
            .await;

            let audit_published = match audit_result {
                Ok(audit_status) => {
                    let audit_enabled = audit_status
                        .as_deref()
                        .map_or(0, |s| i64::from(s == "ACTIVE"));
                    self.audit_log_enabled
                        .with_label_values(&NO_LABELS)
                        .set(audit_enabled);
                    true
                }
                Err(e) => match classify_query_error(&e) {
                    QueryFailure::Absent => {
                        debug!(error = %e, "information_schema.plugins unavailable; audit_log state absent");
                        let _ = self.audit_log_enabled.remove_label_values(&NO_LABELS);
                        false
                    }
                    QueryFailure::Denied => {
                        self.audit_denied.report("information_schema.plugins", &e);
                        let _ = self.audit_log_enabled.remove_label_values(&NO_LABELS);
                        false
                    }
                    QueryFailure::Fault => return Err(e.into()),
                },
            };

            let userstat_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "SELECT @@userstat",
                otel.kind = "client"
            );

            let userstat_result: Result<Option<i32>, sqlx::Error> =
                sqlx::query_scalar("SELECT @@userstat")
                    .fetch_optional(pool)
                    .instrument(userstat_span)
                    .await;

            let userstat_published = match userstat_result {
                Ok(userstat) => {
                    self.userstat_enabled
                        .with_label_values(&NO_LABELS)
                        .set(i64::from(userstat.unwrap_or(0)));
                    true
                }
                Err(e) => match classify_query_error(&e) {
                    QueryFailure::Absent => {
                        debug!(error = %e, "@@userstat not supported here; userstat state absent");
                        let _ = self.userstat_enabled.remove_label_values(&NO_LABELS);
                        false
                    }
                    QueryFailure::Denied => {
                        self.userstat_denied.report("@@userstat", &e);
                        let _ = self.userstat_enabled.remove_label_values(&NO_LABELS);
                        false
                    }
                    QueryFailure::Fault => return Err(e.into()),
                },
            };

            if audit_published || userstat_published {
                Ok(Collected::Fresh)
            } else {
                Ok(Collected::Skipped)
            }
        })
    }

    fn reset_metrics(&self) {
        self.audit_log_enabled.reset();
        self.userstat_enabled.reset();
    }

    fn enabled_by_default(&self) -> bool {
        true
    }
}
