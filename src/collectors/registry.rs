use crate::{
    collectors::{
        Collector, CollectorType, all_factories, config::CollectorConfig,
        exporter::ScraperCollector,
    },
    exporter::GIT_COMMIT_HASH,
};
use futures::stream::{FuturesUnordered, StreamExt};
use prometheus::{Encoder, Gauge, GaugeVec, Opts, Registry, TextEncoder};
use std::{env, sync::Arc};
use tracing::{debug, debug_span, error, info, info_span, instrument, warn};
use tracing_futures::Instrument as _;

#[derive(Clone)]
pub struct CollectorRegistry {
    collectors: Vec<CollectorType>,
    registry: Arc<Registry>,
    mariadb_up_gauge: Gauge,
    scraper: Option<Arc<ScraperCollector>>,
}

impl CollectorRegistry {
    /// Creates a new `CollectorRegistry`
    ///
    /// # Panics
    ///
    /// Panics if core metrics fail to register (should never happen)
    #[allow(clippy::expect_used)]
    pub fn new(config: &CollectorConfig) -> Self {
        let registry = Arc::new(Registry::new());

        // Register mariadb_up gauge
        let mariadb_up_gauge = Gauge::new("mariadb_up", "Whether MariaDB is up (1) or down (0)")
            .expect("Failed to create mariadb_up gauge");

        registry
            .register(Box::new(mariadb_up_gauge.clone()))
            .expect("Failed to register mariadb_up gauge");

        // Register mariadb_exporter_build_info gauge
        let mariadb_exporter_build_info_opts = Opts::new(
            "mariadb_exporter_build_info",
            "Build information for mariadb_exporter",
        );
        let mariadb_exporter_build_info = GaugeVec::new(
            mariadb_exporter_build_info_opts,
            &["version", "commit", "arch"],
        )
        .expect("Failed to create mariadb_exporter_build_info GaugeVec");

        // Add build information as labels
        let version = env!("CARGO_PKG_VERSION");
        let commit_sha = GIT_COMMIT_HASH.unwrap_or("unknown");
        let arch = env::consts::ARCH;

        mariadb_exporter_build_info
            .with_label_values(&[version, commit_sha, arch])
            .set(1.0); // Gauge is always set to 1.0

        registry
            .register(Box::new(mariadb_exporter_build_info))
            .expect("Failed to register mariadb_exporter_build_info GaugeVec");

        info!(
            "Registered mariadb_exporter_build_info: version={} commit={}",
            version, commit_sha
        );

        let factories = all_factories();

        // Extract scraper if exporter collector is enabled
        let mut scraper_opt = None;

        // Build all requested collectors and register their metrics.
        let collectors = config
            .enabled_collectors
            .iter()
            .filter_map(|name| {
                factories.get(name.as_str()).map(|f| {
                    let collector = f();

                    // If this collector provides a scraper, extract it
                    if let Some(scraper) = collector.get_scraper() {
                        scraper_opt = Some(scraper);
                    }

                    // Register metrics per collector under a span so failures surface in traces.
                    let reg_span = debug_span!("collector.register_metrics", collector = %name);
                    let guard = reg_span.enter();
                    if let Err(e) = collector.register_metrics(&registry) {
                        warn!("Failed to register metrics for collector '{}': {}", name, e);
                    }
                    drop(guard);

                    collector
                })
            })
            .collect();

        Self {
            collectors,
            registry,
            mariadb_up_gauge,
            scraper: scraper_opt,
        }
    }

    /// Collect from all enabled collectors.
    ///
    /// # Errors
    ///
    /// Returns an error if metric collection or encoding fails
    #[allow(clippy::too_many_lines)]
    #[instrument(skip(self, pool), level = "info", err, fields(otel.kind = "internal"))]
    pub async fn collect_all(&self, pool: &sqlx::MySqlPool) -> anyhow::Result<String> {
        // Increment scrape counter if scraper is available
        if let Some(ref scraper) = self.scraper {
            scraper.increment_scrapes();
        }

        // Quick connectivity check (does not guarantee every collector will succeed).
        let connect_span = info_span!(
            "db.connectivity_check",
            otel.kind = "client",
            db.system = "mysql",
            db.operation = "SELECT",
            db.statement = "SELECT 1"
        );

        let db_up = match sqlx::query("SELECT 1")
            .fetch_one(pool)
            .instrument(connect_span)
            .await
        {
            Ok(_) => {
                self.mariadb_up_gauge.set(1.0);

                // Initialize version if not already set (e.g. failed at startup)
                if crate::collectors::util::get_mariadb_version() == 0 {
                    let version_span = info_span!("db.version_init", otel.kind = "client");
                    if let Ok(version_string) = sqlx::query_scalar::<_, String>("SELECT VERSION()")
                        .fetch_one(pool)
                        .instrument(version_span)
                        .await
                    {
                        let version_num =
                            crate::collectors::util::parse_mariadb_version(&version_string);
                        crate::collectors::util::set_mariadb_version(version_num);
                        info!(
                            version = version_num,
                            "MariaDB version detected during collection"
                        );
                    }
                }
                true
            }

            Err(e) => {
                error!("Failed to connect to MariaDB: {}", e);
                self.mariadb_up_gauge.set(0.0);
                false
            }
        };

        // If DB is down, skip collectors except exporter self-monitoring
        let mut tasks = FuturesUnordered::new();

        for collector in &self.collectors {
            let name = collector.name();

            // Skip DB-dependent collectors if DB is down
            if !db_up && name != "exporter" {
                debug!("Skipping collector '{}' because database is down", name);
                continue;
            }

            // Create a span per collector execution to visualize overlap in traces.
            let span = info_span!("collector.collect", collector = %name, otel.kind = "internal");

            // Start timing this collector if scraper is available
            let timer = self.scraper.as_ref().map(|s| s.start_scrape(name));

            // Prepare the future now (do not await here).
            let fut = collector.collect(pool);

            // Push an instrumented future that logs start/finish.
            tasks.push(async move {
                debug!("collector '{}' start", name);

                let res = fut.instrument(span).await;

                match &res {
                    Ok(()) => {
                        debug!("collector '{}' done: ok", name);
                        if let Some(t) = timer {
                            t.success();
                        }
                    }
                    Err(e) => {
                        error!("collector '{}' done: error: {}", name, e);
                        if let Some(t) = timer {
                            t.error();
                        }
                    }
                }

                (name, res)
            });
        }

        // Drain completions as they finish (unordered).
        while let Some((name, res)) = tasks.next().await {
            match res {
                Ok(()) => {
                    debug!("Collected metrics from '{}'", name);
                }

                Err(e) => {
                    error!("Collector '{}' failed: {}", name, e);
                }
            }
        }

        // Encode current registry into Prometheus exposition format.
        let encode_span = debug_span!("prometheus.encode");
        let guard = encode_span.enter();

        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();

        // If DB is down, filter out DB-dependent metrics to avoid stale/zero data
        let families_to_encode = if db_up {
            metric_families
        } else {
            metric_families
                .into_iter()
                .filter(|mf| {
                    let name = mf.name();
                    name == "mariadb_up" || name.starts_with("mariadb_exporter_")
                })
                .collect()
        };

        let mut buffer = Vec::new();
        encoder.encode(&families_to_encode, &mut buffer)?;

        // Update metrics count for next scrape
        // Count actual time series lines (non-comment, non-empty lines)
        // This matches: curl -s 0:9306/metrics | grep -vEc '^(#|\s*$)'
        // Note: This count will be visible in the NEXT scrape (eventual consistency)
        if let Some(ref scraper) = self.scraper {
            // Prefer zero-copy UTF-8, fall back to lossy for robustness
            let output = match std::str::from_utf8(&buffer) {
                Ok(s) => std::borrow::Cow::Borrowed(s),
                Err(_) => std::borrow::Cow::Owned(String::from_utf8_lossy(&buffer).into_owned()),
            };

            let count = output
                .lines()
                // Ignore comment lines (Prometheus-spec: '#' at column 0)
                .filter(|line| !line.starts_with('#'))
                // Ignore whitespace-only lines
                .filter(|line| !line.trim().is_empty())
                .count();

            let sample_count = i64::try_from(count).unwrap_or(0);

            scraper.update_metrics_count(sample_count);
        }

        drop(guard);

        Ok(String::from_utf8(buffer)?)
    }

    #[must_use]
    pub const fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    #[must_use]
    pub fn collector_names(&self) -> Vec<&'static str> {
        self.collectors.iter().map(super::Collector::name).collect()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.collectors.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::collectors::config::CollectorConfig;
    use sqlx::mysql::MySqlPoolOptions;
    use std::time::Duration;

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_registry_new() {
        let config = CollectorConfig::new().with_enabled(&["default".to_string()]);
        let registry = CollectorRegistry::new(&config);

        assert!(!registry.is_empty());
        assert!(registry.collector_names().contains(&"default"));

        // Verify core metrics are registered
        let metrics = registry.registry().gather();
        assert!(metrics.iter().any(|m| m.name() == "mariadb_up"));
        assert!(
            metrics
                .iter()
                .any(|m| m.name() == "mariadb_exporter_build_info")
        );
    }

    #[test]
    fn test_registry_empty() {
        let config = CollectorConfig::new();
        let registry = CollectorRegistry::new(&config);

        assert!(registry.is_empty());
        assert_eq!(registry.collector_names().len(), 0);
    }

    #[tokio::test]
    async fn test_collect_all_db_down() {
        let config = CollectorConfig::new().with_enabled(&["default".to_string()]);
        let registry = CollectorRegistry::new(&config);

        // Use a pool that will definitely fail to connect
        let pool = MySqlPoolOptions::new()
            .acquire_timeout(Duration::from_millis(10))
            .connect_lazy("mysql://invalid:invalid@127.0.0.1:1/invalid")
            .unwrap();

        let result = registry.collect_all(&pool).await;

        assert!(result.is_ok());
        let output = result.unwrap();

        // Should contain mariadb_up 0
        assert!(output.contains("mariadb_up 0"));

        // DB metrics should be omitted
        assert!(!output.contains("mariadb_global_status_uptime_seconds"));
    }

    #[test]
    fn test_registry_collector_names() {
        let config =
            CollectorConfig::new().with_enabled(&["default".to_string(), "exporter".to_string()]);
        let registry = CollectorRegistry::new(&config);

        let names = registry.collector_names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"default"));
        assert!(names.contains(&"exporter"));
    }

    #[tokio::test]
    async fn test_collect_all_increments_scrapes() {
        let config = CollectorConfig::new().with_enabled(&["exporter".to_string()]);
        let registry = CollectorRegistry::new(&config);

        let pool = MySqlPoolOptions::new()
            .acquire_timeout(Duration::from_millis(100))
            .connect_lazy("mysql://root:root@127.0.0.1:3306/mysql")
            .unwrap();

        let output = registry.collect_all(&pool).await.unwrap();

        // Should contain mariadb_exporter_scrapes_total 1
        assert!(output.contains("mariadb_exporter_scrapes_total 1"));
    }

    #[tokio::test]
    async fn test_collect_all_reports_metrics_count() {
        let config = CollectorConfig::new().with_enabled(&["exporter".to_string()]);
        let registry = CollectorRegistry::new(&config);

        let pool = MySqlPoolOptions::new()
            .acquire_timeout(Duration::from_millis(100))
            .connect_lazy("mysql://root:root@127.0.0.1:3306/mysql")
            .unwrap();

        // First scrape to trigger count update for NEXT scrape
        let _ = registry.collect_all(&pool).await.unwrap();
        // Second scrape to see the count from the first one
        let output = registry.collect_all(&pool).await.unwrap();

        // Should contain mariadb_exporter_metrics_total
        assert!(output.contains("mariadb_exporter_metrics_total"));

        // Extract the value and check it's > 0
        let count = output
            .lines()
            .find(|l| l.starts_with("mariadb_exporter_metrics_total"))
            .and_then(|l| l.split_whitespace().last())
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);

        assert!(count > 0.0);
    }
}
