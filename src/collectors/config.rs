use crate::collectors::{DEFAULT_SCRAPE_TIMEOUT_MS, system::ProcessMemorySource};
use std::collections::HashSet;
use std::time::Duration;

/// Settings for the opt-in `system` collector.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemConfig {
    /// Which `/proc` file the process-group memory gauge is read from.
    pub process_memory: ProcessMemorySource,
}

#[derive(Clone, Debug)]
pub struct CollectorConfig {
    pub enabled_collectors: HashSet<String>,
    /// Wall-clock budget for one `/metrics` scrape.
    pub scrape_timeout: Duration,
    pub system: SystemConfig,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            enabled_collectors: HashSet::new(),
            scrape_timeout: Duration::from_millis(DEFAULT_SCRAPE_TIMEOUT_MS),
            system: SystemConfig::default(),
        }
    }
}

impl CollectorConfig {
    /// Create an empty config
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable collectors by name
    #[must_use]
    pub fn with_enabled(mut self, collectors: &[String]) -> Self {
        self.enabled_collectors = collectors.iter().cloned().collect();
        self
    }

    /// Bound one scrape to `scrape_timeout`.
    ///
    /// A zero duration is ignored and the default kept, so a mis-set flag cannot turn
    /// every scrape into an instant timeout.
    #[must_use]
    pub const fn with_scrape_timeout(mut self, scrape_timeout: Duration) -> Self {
        if !scrape_timeout.is_zero() {
            self.scrape_timeout = scrape_timeout;
        }
        self
    }

    /// Select the `system` collector's process-group memory source.
    #[must_use]
    pub const fn with_system_process_memory(mut self, source: ProcessMemorySource) -> Self {
        self.system.process_memory = source;
        self
    }

    /// Check if a collector is enabled
    #[must_use]
    pub fn is_enabled(&self, name: &str) -> bool {
        self.enabled_collectors.contains(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_rss_and_the_documented_scrape_timeout() {
        let config = CollectorConfig::new();

        assert_eq!(config.system.process_memory, ProcessMemorySource::Rss);
        assert_eq!(
            config.scrape_timeout,
            Duration::from_millis(DEFAULT_SCRAPE_TIMEOUT_MS)
        );
    }

    #[test]
    fn a_zero_scrape_timeout_is_refused_in_favour_of_the_default() {
        let config = CollectorConfig::new().with_scrape_timeout(Duration::ZERO);

        assert_eq!(
            config.scrape_timeout,
            Duration::from_millis(DEFAULT_SCRAPE_TIMEOUT_MS),
            "a zero budget would make every scrape time out instantly"
        );
    }

    #[test]
    fn builders_are_honoured() {
        let config = CollectorConfig::new()
            .with_enabled(&["system".to_string()])
            .with_scrape_timeout(Duration::from_millis(2_500))
            .with_system_process_memory(ProcessMemorySource::Pss);

        assert!(config.is_enabled("system"));
        assert!(!config.is_enabled("default"));
        assert_eq!(config.scrape_timeout, Duration::from_millis(2_500));
        assert_eq!(config.system.process_memory, ProcessMemorySource::Pss);
    }
}
