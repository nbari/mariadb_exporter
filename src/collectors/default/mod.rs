use crate::collectors::Collector;
use anyhow::Result;
use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};
use prometheus::Registry;
use sqlx::MySqlPool;
use std::sync::Arc;
use tracing::{debug, info_span, instrument, warn};
use tracing_futures::Instrument as _;

pub mod version;
use version::VersionCollector;

pub mod status;
use status::StatusCollector;

pub mod plugins;
use plugins::PluginsCollector;

/// `DefaultCollector` bundles lightweight always-on signals.
#[derive(Clone, Default)]
pub struct DefaultCollector {
    subs: Vec<Arc<dyn Collector + Send + Sync>>,
}

impl DefaultCollector {
    #[must_use]
    pub fn new() -> Self {
        Self {
            subs: vec![
                Arc::new(VersionCollector::new()),
                Arc::new(StatusCollector::new()),
                Arc::new(PluginsCollector::new()),
            ],
        }
    }
}

impl Collector for DefaultCollector {
    fn name(&self) -> &'static str {
        "default"
    }

    #[instrument(
        skip(self, registry),
        level = "info",
        err,
        fields(collector = "default")
    )]
    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        for sub in &self.subs {
            let span = info_span!("collector.register_metrics", sub_collector = %sub.name());
            let res = sub.register_metrics(registry);
            match res {
                Ok(()) => debug!(collector = sub.name(), "registered metrics"),
                Err(ref e) => {
                    warn!(collector = sub.name(), error = %e, "failed to register metrics");
                }
            }
            res?;
            drop(span);
        }
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "default", otel.kind = "internal"))]
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
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_collector_name() {
        let collector = DefaultCollector::new();
        assert_eq!(collector.name(), "default");
    }

    #[test]
    fn test_default_collector_enabled_by_default() {
        let collector = DefaultCollector::new();
        assert!(collector.enabled_by_default());
    }
}
