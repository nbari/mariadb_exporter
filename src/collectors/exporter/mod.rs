mod process;
mod scraper;

pub use process::ProcessCollector;
pub use scraper::{ScrapeTimer, ScraperCollector};

use crate::collectors::Collector;
use anyhow::Result;
use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};
use prometheus::Registry;
use sqlx::MySqlPool;
use std::sync::Arc;
use tracing::{debug, info_span, instrument, warn};
use tracing_futures::Instrument as _;

/// Exporter self-monitoring
#[derive(Clone)]
pub struct ExporterCollector {
    subs: Vec<Arc<dyn Collector + Send + Sync>>,
    scraper: Arc<ScraperCollector>,
}

impl Default for ExporterCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ExporterCollector {
    #[must_use]
    pub fn new() -> Self {
        let scraper = Arc::new(ScraperCollector::new());
        Self {
            subs: vec![
                Arc::new(ProcessCollector::new()),
                Arc::clone(&scraper) as Arc<dyn Collector + Send + Sync>,
            ],
            scraper,
        }
    }

    #[must_use]
    pub const fn get_scraper(&self) -> &Arc<ScraperCollector> {
        &self.scraper
    }
}

impl Collector for ExporterCollector {
    fn name(&self) -> &'static str {
        "exporter"
    }

    #[instrument(
        skip(self, registry),
        level = "info",
        err,
        fields(collector = "exporter")
    )]
    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        for sub in &self.subs {
            let span = info_span!("collector.register_metrics", sub_collector = %sub.name());

            let res = sub.register_metrics(registry);

            match res {
                Ok(()) => debug!(collector = sub.name(), "registered exporter metrics"),
                Err(ref e) => {
                    warn!(collector = sub.name(), error = %e, "failed to register exporter metrics");
                }
            }

            res?;

            drop(span);
        }
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "exporter", otel.kind = "internal"))]
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut tasks = FuturesUnordered::new();

            for sub in &self.subs {
                let span = info_span!("collector.collect", sub_collector = %sub.name(), otel.kind = "internal");
                tasks.push(sub.collect(pool).instrument(span));
            }

            while let Some(res) = tasks.next().await {
                res?;
            }

            Ok(())
        })
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_exporter_collector_new() {
        let collector = ExporterCollector::new();
        assert_eq!(collector.subs.len(), 2);
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_exporter_collector_name() {
        let collector = ExporterCollector::new();
        assert_eq!(collector.name(), "exporter");
    }
}
