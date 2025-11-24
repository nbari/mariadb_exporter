use crate::collectors::Collector;
use anyhow::{Result, anyhow};
use futures::future::BoxFuture;
use prometheus::{IntGaugeVec, Opts, Registry};
use regex::Regex;
use sqlx::MySqlPool;
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;

/// Handles `MariaDB` version metrics
#[derive(Clone)]
pub struct VersionCollector {
    mariadb_version_info: IntGaugeVec,
    mariadb_version_num: IntGaugeVec,
    version_regex: Regex,
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

        let version_regex =
            Regex::new(r"((\d+)(\.\d+)?(\.\d+)?)").expect("valid version regex");

        Self {
            mariadb_version_info,
            mariadb_version_num,
            version_regex,
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

    fn normalize_version(&self, version: &str) -> Result<(String, i64)> {
        if let Some(captures) = self.version_regex.captures(version)
            && let Some(version_match) = captures.get(1)
        {
            let parts: Vec<&str> = version_match.as_str().split('.').collect();
            let major = parts.first().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            let minor = parts.get(1).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
            let patch = parts.get(2).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);

            let normalized = match parts.len() {
                1 => format!("{major}.0.0"),
                2 => format!("{major}.{minor}.0"),
                _ => version_match.as_str().to_string(),
            };

            let version_num = major * 10000 + minor * 100 + patch;

            return Ok((normalized, version_num));
        }

        Err(anyhow!("could not parse version from server response: {version}"))
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

            let (short_version, version_num) = self.normalize_version(&full_version)?;
            let server_label = self.get_server_info(pool).await?;

            self.mariadb_version_info
                .with_label_values(&[&full_version, &short_version])
                .set(1);
            self.mariadb_version_num
                .with_label_values(&[&server_label])
                .set(version_num);

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
        let collector = VersionCollector::new();
        assert!(matches!(
            collector.normalize_version("10.5.12-MariaDB"),
            Ok((ref normalized, num))
                if normalized == "10.5.12" && num == 10 * 10000 + 5 * 100 + 12
        ));
    }

    #[test]
    fn test_normalize_version_short() {
        let collector = VersionCollector::new();
        assert!(matches!(
            collector.normalize_version("11.4"),
            Ok((ref normalized, num))
                if normalized == "11.4.0" && num == 11 * 10000 + 4 * 100
        ));
    }

    #[test]
    fn test_normalize_version_single_component() {
        let collector = VersionCollector::new();
        assert!(matches!(
            collector.normalize_version("12"),
            Ok((ref normalized, num))
                if normalized == "12.0.0" && num == 12 * 10000
        ));
    }

    #[test]
    fn test_collectors_name() {
        let collector = VersionCollector::new();
        assert_eq!(collector.name(), "version");
    }
}
