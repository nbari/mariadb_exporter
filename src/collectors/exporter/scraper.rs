use anyhow::Result;
use prometheus::{CounterVec, GaugeVec, HistogramVec, IntGauge, Opts, Registry};
use std::sync::{Arc, RwLock};
use std::time::Instant;

#[derive(Clone)]
pub struct ScraperCollector {
    scrape_duration_seconds: HistogramVec,
    scrape_errors_total: CounterVec,
    last_scrape_timestamp: GaugeVec,
    last_scrape_success: GaugeVec,
    
    metrics_total: IntGauge,
    scrapes_total: IntGauge,
    
    state: Arc<RwLock<ScraperState>>,
}

#[derive(Default)]
struct ScraperState {
    total_scrapes: i64,
    total_metrics: i64,
}

impl Default for ScraperCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ScraperCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    ///
    /// # Panics
    ///
    /// Panics if metric creation fails.
    pub fn new() -> Self {
        let scrape_duration_seconds = HistogramVec::new(
            prometheus::HistogramOpts::new(
                "mariadb_exporter_collector_scrape_duration_seconds",
                "Time spent scraping each collector in seconds",
            )
            .buckets(vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0]),
            &["collector"],
        )
        .expect("mariadb_exporter_collector_scrape_duration_seconds");

        let scrape_errors_total = CounterVec::new(
            Opts::new(
                "mariadb_exporter_collector_scrape_errors_total",
                "Total number of scrape errors per collector",
            ),
            &["collector"],
        )
        .expect("mariadb_exporter_collector_scrape_errors_total");

        let last_scrape_timestamp = GaugeVec::new(
            Opts::new(
                "mariadb_exporter_collector_last_scrape_timestamp_seconds",
                "Unix timestamp of the last scrape attempt per collector",
            ),
            &["collector"],
        )
        .expect("mariadb_exporter_collector_last_scrape_timestamp_seconds");

        let last_scrape_success = GaugeVec::new(
            Opts::new(
                "mariadb_exporter_collector_last_scrape_success",
                "Whether the last scrape was successful (1=success, 0=failure)",
            ),
            &["collector"],
        )
        .expect("mariadb_exporter_collector_last_scrape_success");

        let metrics_total = IntGauge::with_opts(Opts::new(
            "mariadb_exporter_metrics_total",
            "Total number of metrics currently exported (for cardinality monitoring)",
        ))
        .expect("mariadb_exporter_metrics_total");

        let scrapes_total = IntGauge::with_opts(Opts::new(
            "mariadb_exporter_scrapes_total",
            "Total number of scrapes performed since start",
        ))
        .expect("mariadb_exporter_scrapes_total");

        Self {
            scrape_duration_seconds,
            scrape_errors_total,
            last_scrape_timestamp,
            last_scrape_success,
            metrics_total,
            scrapes_total,
            state: Arc::new(RwLock::new(ScraperState::default())),
        }
    }

    #[must_use]
    pub fn start_scrape(&self, collector_name: &str) -> ScrapeTimer {
        ScrapeTimer {
            collector_name: collector_name.to_string(),
            start: Instant::now(),
            scraper: self.clone(),
        }
    }

    pub fn update_metrics_count(&self, count: i64) {
        self.metrics_total.set(count);
        let mut state = match self.state.write() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::warn!("ScraperState write lock was poisoned, recovering");
                poisoned.into_inner()
            }
        };
        state.total_metrics = count;
    }

    pub fn increment_scrapes(&self) {
        let mut state = match self.state.write() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::warn!("ScraperState write lock was poisoned, recovering");
                poisoned.into_inner()
            }
        };
        state.total_scrapes += 1;
        self.scrapes_total.set(state.total_scrapes);
    }

    fn record_success(&self, collector_name: &str, duration: f64) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        self.scrape_duration_seconds
            .with_label_values(&[collector_name])
            .observe(duration);

        self.last_scrape_timestamp
            .with_label_values(&[collector_name])
            .set(timestamp);

        self.last_scrape_success
            .with_label_values(&[collector_name])
            .set(1.0);
    }

    fn record_error(&self, collector_name: &str) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        self.scrape_errors_total
            .with_label_values(&[collector_name])
            .inc();

        self.last_scrape_timestamp
            .with_label_values(&[collector_name])
            .set(timestamp);

        self.last_scrape_success
            .with_label_values(&[collector_name])
            .set(0.0);
    }

    ///
    /// # Errors
    ///
    /// Returns an error if metric registration fails.
    pub fn register(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.scrape_duration_seconds.clone()))?;
        registry.register(Box::new(self.scrape_errors_total.clone()))?;
        registry.register(Box::new(self.last_scrape_timestamp.clone()))?;
        registry.register(Box::new(self.last_scrape_success.clone()))?;
        registry.register(Box::new(self.metrics_total.clone()))?;
        registry.register(Box::new(self.scrapes_total.clone()))?;
        Ok(())
    }
}

impl crate::collectors::Collector for ScraperCollector {
    fn name(&self) -> &'static str {
        "scraper"
    }

    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        self.register(registry)
    }

    fn collect<'a>(&'a self, _pool: &'a sqlx::MySqlPool) -> futures::future::BoxFuture<'a, Result<()>> {
        Box::pin(async move { Ok(()) })
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}

pub struct ScrapeTimer {
    collector_name: String,
    start: Instant,
    scraper: ScraperCollector,
}

impl ScrapeTimer {
    pub fn success(self) {
        let duration = self.start.elapsed().as_secs_f64();
        self.scraper.record_success(&self.collector_name, duration);
    }

    pub fn error(self) {
        self.scraper.record_error(&self.collector_name);
    }
}

impl Drop for ScrapeTimer {
    fn drop(&mut self) {
        let duration = self.start.elapsed().as_secs_f64();
        self.scraper.record_success(&self.collector_name, duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_scraper_collector_new() {
        let scraper = ScraperCollector::new();
        assert_eq!(scraper.metrics_total.get(), 0);
        assert_eq!(scraper.scrapes_total.get(), 0);
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_scraper_collector_registers_without_error() {
        let scraper = ScraperCollector::new();
        let registry = Registry::new();
        assert!(scraper.register(&registry).is_ok());
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    #[allow(clippy::expect_used)]
    fn test_scrape_timer_records_duration() {
        let scraper = ScraperCollector::new();
        let registry = Registry::new();
        scraper.register(&registry).unwrap();

        {
            let timer = scraper.start_scrape("test_collector");
            thread::sleep(Duration::from_millis(10));
            timer.success();
        }

        let metrics = registry.gather();
        let duration_metric = metrics
            .iter()
            .find(|m| m.name() == "mariadb_exporter_collector_scrape_duration_seconds")
            .expect("duration metric should exist");

        assert!(!duration_metric.get_metric().is_empty());
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    #[allow(clippy::expect_used)]
    fn test_scrape_timer_records_error() {
        let scraper = ScraperCollector::new();
        let registry = Registry::new();
        scraper.register(&registry).unwrap();

        {
            let timer = scraper.start_scrape("test_collector");
            timer.error();
        }

        let metrics = registry.gather();
        let error_metric = metrics
            .iter()
            .find(|m| m.name() == "mariadb_exporter_collector_scrape_errors_total")
            .expect("error metric should exist");

        assert!(!error_metric.get_metric().is_empty());
    }

    #[test]
    fn test_update_metrics_count() {
        let scraper = ScraperCollector::new();
        scraper.update_metrics_count(42);
        assert_eq!(scraper.metrics_total.get(), 42);
    }

    #[test]
    fn test_increment_scrapes() {
        let scraper = ScraperCollector::new();
        scraper.increment_scrapes();
        assert_eq!(scraper.scrapes_total.get(), 1);
        scraper.increment_scrapes();
        assert_eq!(scraper.scrapes_total.get(), 2);
    }
}
