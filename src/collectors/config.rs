use std::collections::HashSet;

#[derive(Clone, Debug, Default)]
pub struct CollectorConfig {
    pub enabled_collectors: HashSet<String>,
}

impl CollectorConfig {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_enabled(mut self, collectors: &[String]) -> Self {
        self.enabled_collectors = collectors.iter().cloned().collect();
        self
    }

    #[must_use]
    pub fn is_enabled(&self, name: &str) -> bool {
        self.enabled_collectors.contains(name)
    }
}
