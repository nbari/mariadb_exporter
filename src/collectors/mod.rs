use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::Registry;
use sqlx::MySqlPool;
use std::collections::HashMap;

#[macro_use]
mod register_macro;

pub trait Collector {
    fn name(&self) -> &'static str;

    /// Register metrics with the prometheus registry
    ///
    /// # Errors
    ///
    /// Returns an error if metric registration fails
    fn register_metrics(&self, registry: &Registry) -> Result<()>;

    // lifetime 'a is needed to tie the future to the lifetime of self and pool
    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>>;

    fn enabled_by_default(&self) -> bool {
        false
    }
}

// Make utils available to all collectors (exclusions, etc.)
pub mod util;

/// Convert i64 to f64 for Prometheus metrics.
///
/// This conversion is safe for `MariaDB` metric values because:
/// - Values are typically small (row counts, connections, etc.)
/// - f64 has 52-bit mantissa precision, accurate up to 2^53 (9 quadrillion)
/// - `MariaDB` metrics will never realistically exceed this threshold
///
/// # Arguments
/// * `value` - The i64 value to convert
///
/// # Returns
/// The f64 representation of the value
#[inline]
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub const fn i64_to_f64(value: i64) -> f64 {
    value as f64
}

// THIS IS THE ONLY PLACE YOU NEED TO ADD NEW COLLECTORS
register_collectors! {
    default => DefaultCollector,
    exporter => ExporterCollector,
    tls => TlsCollector,
    query_response_time => QueryResponseTimeCollector,
    statements => StatementsCollector,
    schema => SchemaCollector,
    replication => ReplicationCollector,
    locks => LocksCollector,
    metadata => MetadataCollector,
    userstat => UserStatCollector,
    innodb => InnodbCollector,
    // Add more collectors here - just follow the same pattern!
}

// Other modules
pub mod config;
pub mod registry;
