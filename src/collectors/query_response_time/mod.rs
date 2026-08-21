use crate::collectors::{Collected, Collector};
use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::Registry;
use sqlx::MySqlPool;
use tracing::instrument;

pub mod collector;
use collector::QueryResponseTimeCollector as ResponseTimeCollector;

/// Query response time plugin metrics (opt-in; skipped if plugin not installed).
#[derive(Clone)]
pub struct QueryResponseTimeCollector {
    response_time: ResponseTimeCollector,
}

impl QueryResponseTimeCollector {
    #[must_use]
    /// Create a new query response time collector.
    pub fn new() -> Self {
        Self {
            response_time: ResponseTimeCollector::new(),
        }
    }
}

impl Default for QueryResponseTimeCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for QueryResponseTimeCollector {
    fn name(&self) -> &'static str {
        "query_response_time"
    }

    #[instrument(
        skip(self, registry),
        level = "info",
        err,
        fields(collector = "query_response_time")
    )]
    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        self.response_time.register_metrics(registry)?;
        Ok(())
    }

    #[instrument(skip(self, pool), level = "info", err, fields(collector = "query_response_time", otel.kind = "internal"))]
    fn collect_once<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
        Box::pin(async move {
            self.response_time.collect(pool).await?;
            Ok(Collected::Fresh)
        })
    }

    /// Fans out to the sub-collector; this umbrella owns no metrics itself.
    fn reset_metrics(&self) {
        Collector::reset_metrics(&self.response_time);
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_response_time_collector_name() {
        let collector = QueryResponseTimeCollector::new();
        assert_eq!(collector.name(), "query_response_time");
    }

    #[test]
    fn test_query_response_time_collector_not_enabled_by_default() {
        let collector = QueryResponseTimeCollector::new();
        assert!(!collector.enabled_by_default());
    }

    #[test]
    fn test_query_response_time_collector_default() {
        let collector = QueryResponseTimeCollector::default();
        assert_eq!(collector.name(), "query_response_time");
    }
}
