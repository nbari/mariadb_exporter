use crate::collectors::Collector;
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::{IntGauge, IntGaugeVec, Opts, Registry};
use sqlx::MySqlPool;
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;

/// User statistics collector (opt-in; requires userstat=1).
#[derive(Clone)]
pub struct UserStatCollector {
    userstat_enabled: IntGauge,
    connections_total: IntGaugeVec,
    bytes_received_total: IntGaugeVec,
    bytes_sent_total: IntGaugeVec,
    rows_read_total: IntGaugeVec,
    rows_sent_total: IntGaugeVec,
    rows_deleted_total: IntGaugeVec,
    rows_inserted_total: IntGaugeVec,
    rows_updated_total: IntGaugeVec,
}

impl UserStatCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    /// Create a new userstat collector.
    ///
    /// # Panics
    ///
    /// Panics if metric names are invalid (should not occur with static names).
    pub fn new() -> Self {
        let gvec = |name: &str, help: &str| {
            IntGaugeVec::new(Opts::new(name, help), &["user"])
                .expect("valid userstat metric")
        };

        Self {
            userstat_enabled: IntGauge::new(
                "mariadb_userstat_enabled",
                "Whether user statistics (userstat) are enabled (1/0)",
            )
            .expect("valid mariadb_userstat_enabled metric"),
            connections_total: gvec(
                "mariadb_info_schema_userstats_connections_total",
                "Total connections per user (user_statistics)",
            ),
            bytes_received_total: gvec(
                "mariadb_info_schema_userstats_bytes_received_total",
                "Bytes received per user",
            ),
            bytes_sent_total: gvec(
                "mariadb_info_schema_userstats_bytes_sent_total",
                "Bytes sent per user",
            ),
            rows_read_total: gvec(
                "mariadb_info_schema_userstats_rows_read_total",
                "Rows read per user",
            ),
            rows_sent_total: gvec(
                "mariadb_info_schema_userstats_rows_sent_total",
                "Rows sent per user",
            ),
            rows_deleted_total: gvec(
                "mariadb_info_schema_userstats_rows_deleted_total",
                "Rows deleted per user",
            ),
            rows_inserted_total: gvec(
                "mariadb_info_schema_userstats_rows_inserted_total",
                "Rows inserted per user",
            ),
            rows_updated_total: gvec(
                "mariadb_info_schema_userstats_rows_updated_total",
                "Rows updated per user",
            ),
        }
    }
}

impl Default for UserStatCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for UserStatCollector {
    fn name(&self) -> &'static str {
        "userstat"
    }

    #[instrument(
        skip(self, registry),
        level = "info",
        err,
        fields(collector = "userstat")
    )]
    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.userstat_enabled.clone()))?;
        registry.register(Box::new(self.connections_total.clone()))?;
        registry.register(Box::new(self.bytes_received_total.clone()))?;
        registry.register(Box::new(self.bytes_sent_total.clone()))?;
        registry.register(Box::new(self.rows_read_total.clone()))?;
        registry.register(Box::new(self.rows_sent_total.clone()))?;
        registry.register(Box::new(self.rows_deleted_total.clone()))?;
        registry.register(Box::new(self.rows_inserted_total.clone()))?;
        registry.register(Box::new(self.rows_updated_total.clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "userstat", otel.kind = "internal"))]
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // Check userstat status.
            let status_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "SELECT @@userstat",
                otel.kind = "client"
            );
            let enabled: i64 = sqlx::query_scalar("SELECT @@userstat")
                .fetch_one(pool)
                .instrument(status_span)
                .await
                .unwrap_or(0);
            self.userstat_enabled.set(enabled);

            if enabled == 0 {
                return Ok(());
            }

            // Confirm table exists.
            let exists_span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "check USER_STATISTICS table",
                otel.kind = "client"
            );

            let has_table = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema='information_schema' AND table_name='USER_STATISTICS'",
            )
            .fetch_one(pool)
            .instrument(exists_span)
            .await
            .unwrap_or(0)
                > 0;

            if !has_table {
                debug!("USER_STATISTICS not available even though userstat=1; skipping metrics");
                return Ok(());
            }

            let span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "SELECT * FROM information_schema.USER_STATISTICS",
                otel.kind = "client"
            );

            let rows = sqlx::query_as::<_, (String, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64)>(
                "SELECT USER, TOTAL_CONNECTIONS, BYTES_RECEIVED, BYTES_SENT,
                        ROWS_READ, ROWS_SENT, ROWS_DELETED, ROWS_INSERTED, ROWS_UPDATED,
                        0 as rows_tmp1, 0 as rows_tmp2, 0 as rows_tmp3
                 FROM information_schema.USER_STATISTICS",
            )
            .fetch_all(pool)
            .instrument(span)
            .await
            .unwrap_or_default();

            for (user, total_conn, bytes_recv, bytes_sent, rows_read, rows_sent, rows_del, rows_ins, rows_upd, _, _, _) in rows {
                let u = user.as_str();
                self.connections_total.with_label_values(&[u]).set(total_conn);
                self.bytes_received_total.with_label_values(&[u]).set(bytes_recv);
                self.bytes_sent_total.with_label_values(&[u]).set(bytes_sent);
                self.rows_read_total.with_label_values(&[u]).set(rows_read);
                self.rows_sent_total.with_label_values(&[u]).set(rows_sent);
                self.rows_deleted_total.with_label_values(&[u]).set(rows_del);
                self.rows_inserted_total.with_label_values(&[u]).set(rows_ins);
                self.rows_updated_total.with_label_values(&[u]).set(rows_upd);
            }

            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}
