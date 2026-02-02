use crate::collectors::{util::normalize_mariadb_version, Collector};
use anyhow::{Result, anyhow};
use futures::future::BoxFuture;
use prometheus::{IntGauge, IntGaugeVec, Opts, Registry};
use sqlx::MySqlPool;
use sysinfo::System;
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;

/// Handles `MariaDB` version metrics
#[derive(Clone)]
pub struct VersionCollector {
    mariadb_version_info: IntGaugeVec,
    mariadb_version_num: IntGaugeVec,
    system_memory_total_bytes: IntGauge,
}

impl Default for VersionCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl VersionCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    ///
    /// # Panics
    ///
    /// Panics if metric creation fails.
    pub fn new() -> Self {
        let mariadb_version_info = IntGaugeVec::new(
            Opts::new(
                "mariadb_version_info",
                "MariaDB version information with labels for version details.",
            ),
            &["version", "short_version"],
        )
        .expect("valid mariadb_version_info metric opts");

        let mariadb_version_num = IntGaugeVec::new(
            Opts::new(
                "mariadb_version_num",
                "MariaDB version number formatted as major*10000 + minor*100 + patch",
            ),
            &["server"],
        )
        .expect("valid mariadb_version_num metric opts");

        let system_memory_total_bytes = IntGauge::with_opts(Opts::new(
            "mariadb_exporter_system_memory_total_bytes",
            "Total system memory in bytes",
        ))
        .expect("mariadb_exporter_system_memory_total_bytes");

        // Initialize system memory (static value)
        let system = System::new_all();
        let total_memory = system.total_memory();
        system_memory_total_bytes.set(i64::try_from(total_memory).unwrap_or(0));

        Self {
            mariadb_version_info,
            mariadb_version_num,
            system_memory_total_bytes,
        }
    }

    #[instrument(skip(self, pool), level = "info", err, fields(db.system = "mysql", otel.kind = "client"))]
    async fn get_server_info(&self, pool: &MySqlPool) -> Result<String> {
        if let Ok(server_label) = std::env::var("MARIADB_EXPORTER_SERVER_LABEL") {
            return Ok(server_label);
        }

        let span = info_span!(
            "db.query",
            db.operation = "SELECT",
            db.statement = "SELECT @@hostname, @@port, DATABASE()"
        );
        let server_info =
            sqlx::query_as::<_, (Option<String>, Option<u16>, Option<String>)>(
                "SELECT @@hostname as host, @@port as port, DATABASE() as db",
            )
            .fetch_one(pool)
            .instrument(span)
            .await;

        match server_info {
            Ok((host, port, database)) => {
                let host = host.unwrap_or_else(|| "localhost".to_string());
                let port = port.unwrap_or(3306);
                let db = database.unwrap_or_else(|| "mysql".to_string());
                Ok(format!("{host}:{port}:{db}"))
            }
            Err(e) => {
                debug!(error = %e, "failed to fetch server info; using fallback label");
                Ok("unknown".to_string())
            }
        }
    }

    fn normalize_version(version: &str) -> Result<(String, i64)> {
        let (normalized, num) = normalize_mariadb_version(version);
        if num == 0 && normalized == "0.0.0" {
            return Err(anyhow!("could not parse version from server response: {version}"));
        }
        Ok((normalized, num))
    }

    fn update_version_metrics(
        &self,
        full_version: &str,
        short_version: &str,
        server_label: &str,
        version_num: i64,
    ) {
        // Avoid stale labels if MariaDB is upgraded while exporter stays running.
        self.mariadb_version_info.reset();
        self.mariadb_version_num.reset();

        self.mariadb_version_info
            .with_label_values(&[full_version, short_version])
            .set(1);
        self.mariadb_version_num
            .with_label_values(&[server_label])
            .set(version_num);
    }
}

impl Collector for VersionCollector {
    fn name(&self) -> &'static str {
        "version"
    }

    #[instrument(
        skip(self, registry),
        level = "info",
        err,
        fields(collector = "version")
    )]
    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.mariadb_version_info.clone()))?;
        registry.register(Box::new(self.mariadb_version_num.clone()))?;
        registry.register(Box::new(self.system_memory_total_bytes.clone()))?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "version", otel.kind = "internal"))]
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let span = info_span!(
                "db.query",
                db.system = "mysql",
                db.operation = "SELECT",
                db.statement = "SELECT VERSION()",
                otel.kind = "client"
            );
            let full_version = sqlx::query_scalar::<_, String>("SELECT VERSION()")
                .fetch_one(pool)
                .instrument(span)
                .await?;

            let (short_version, version_num) = Self::normalize_version(&full_version)?;
            let server_label = self.get_server_info(pool).await?;

            self.update_version_metrics(
                &full_version,
                &short_version,
                &server_label,
                version_num,
            );

            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_version() {
        assert!(matches!(
            VersionCollector::normalize_version("10.5.12-MariaDB"),
            Ok((ref normalized, num))
                if normalized == "10.5.12" && num == 10 * 10000 + 5 * 100 + 12
        ));
    }

    #[test]
    fn test_normalize_version_short() {
        assert!(matches!(
            VersionCollector::normalize_version("11.4"),
            Ok((ref normalized, num))
                if normalized == "11.4.0" && num == 11 * 10000 + 4 * 100
        ));
    }

    #[test]
    fn test_normalize_version_single_component() {
        assert!(matches!(
            VersionCollector::normalize_version("12"),
            Ok((ref normalized, num))
                if normalized == "12.0.0" && num == 12 * 10000
        ));
    }

    #[test]
    fn test_collectors_name() {
        let collector = VersionCollector::new();
        assert_eq!(collector.name(), "version");
    }

    #[test]
    fn test_version_labels_reset_on_update() -> Result<()> {
        let collector = VersionCollector::new();
        let registry = Registry::new();

        collector.register_metrics(&registry)?;

        collector.update_version_metrics(
            "10.5.12-MariaDB",
            "10.5.12",
            "localhost:3306:mysql",
            100_512,
        );
        collector.update_version_metrics(
            "10.6.1-MariaDB",
            "10.6.1",
            "localhost:3306:mysql",
            100_601,
        );

        let metric_families = registry.gather();
        let version_info = metric_families
            .iter()
            .find(|m| m.name() == "mariadb_version_info")
            .ok_or_else(|| anyhow!("mariadb_version_info should exist"))?;
        assert_eq!(version_info.get_metric().len(), 1);

        let version_num = metric_families
            .iter()
            .find(|m| m.name() == "mariadb_version_num")
            .ok_or_else(|| anyhow!("mariadb_version_num should exist"))?;
        assert_eq!(version_num.get_metric().len(), 1);

        Ok(())
    }
}
