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

// THIS IS THE ONLY PLACE YOU NEED TO ADD NEW COLLECTORS
register_collectors! {
    default => DefaultCollector,
    exporter => ExporterCollector,
    tls => TlsCollector,
    query_response_time => QueryResponseTimeCollector,
    audit => AuditCollector,
    statements => StatementsCollector,
    schema => SchemaCollector,
    replication => ReplicationCollector,
    locks => LocksCollector,
    metadata => MetadataCollector,
    userstat => UserStatCollector,
    // Add more collectors here -- just follow the same pattern!
}

pub mod config;
pub mod registry;
