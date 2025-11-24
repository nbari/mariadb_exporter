use anyhow::Result;
use futures::future::BoxFuture;
use prometheus::Registry;
use sqlx::MySqlPool;
use std::collections::HashMap;

#[macro_use]
mod register_macro;

pub trait Collector {
    fn name(&self) -> &'static str;

    /// Register metrics with the prometheus registry.
    ///
    /// # Errors
    ///
    /// Returns an error if any metric fails to register.
    fn register_metrics(&self, registry: &Registry) -> Result<()>;

    fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>>;

    fn enabled_by_default(&self) -> bool {
        false
    }
}

pub mod util;

register_collectors! {
    default => DefaultCollector,
    exporter => ExporterCollector,
}

pub mod config;
pub mod registry;
