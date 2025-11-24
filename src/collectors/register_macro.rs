macro_rules! register_collectors {
    (
        $(
            $module:ident => $collector_type:ident
        ),* $(,)?
    ) => {
        $(
            pub mod $module;
            pub use $module::$collector_type;
        )*

        #[derive(Clone)]
        pub enum CollectorType {
            $(
                $collector_type($collector_type),
            )*
        }

        impl Collector for CollectorType {
            fn name(&self) -> &'static str {
                match self {
                    $(
                        CollectorType::$collector_type(c) => c.name(),
                    )*
                }
            }

            fn register_metrics(&self, registry: &Registry) -> Result<()> {
                match self {
                    $(
                        CollectorType::$collector_type(c) => c.register_metrics(registry),
                    )*
                }
            }

            fn collect<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<()>> {
                match self {
                    $(
                        CollectorType::$collector_type(c) => c.collect(pool),
                    )*
                }
            }

            fn enabled_by_default(&self) -> bool {
                match self {
                    $(
                        CollectorType::$collector_type(c) => c.enabled_by_default(),
                    )*
                }
            }
        }

        impl CollectorType {
            pub fn get_scraper(&self) -> Option<std::sync::Arc<crate::collectors::exporter::ScraperCollector>> {
                match self {
                    CollectorType::ExporterCollector(c) => Some(c.get_scraper().clone()),
                    _ => None,
                }
            }
        }

        pub fn all_factories() -> HashMap<&'static str, fn() -> CollectorType> {
            let mut map: HashMap<&'static str, fn() -> CollectorType> = HashMap::new();
            $(
                map.insert(
                    stringify!($module),
                    || CollectorType::$collector_type($collector_type::new()),
                );
            )*
            map
        }

        pub const COLLECTOR_NAMES: &[&'static str] = &[
            $(stringify!($module),)*
        ];
    };
}

#[cfg(test)]
mod tests {
    use crate::collectors::Collector;
    use prometheus::Registry;

    #[test]
    fn test_all_factories_exist() {
        let factories = crate::collectors::all_factories();
        assert!(!factories.is_empty());
    }

    #[test]
    fn test_collector_names_exist() {
        let names = crate::collectors::COLLECTOR_NAMES;
        assert!(!names.is_empty());
        assert!(names.contains(&"default"));
        assert!(names.contains(&"exporter"));
    }

    #[test]
    fn test_factory_creates_valid_collectors() {
        let factories = crate::collectors::all_factories();

        for (name, factory) in &factories {
            let collector = factory();
            assert!(
                !collector.name().is_empty(),
                "Collector {name} has empty name"
            );
        }
    }

    #[test]
    fn test_factories_match_collector_names() {
        let factories = crate::collectors::all_factories();
        let names = crate::collectors::COLLECTOR_NAMES;

        for key in factories.keys() {
            assert!(names.contains(key));
        }

        for name in names {
            assert!(factories.contains_key(name));
        }
    }

    #[test]
    fn test_collector_name_matches_key() {
        let factories = crate::collectors::all_factories();

        for (key, factory) in &factories {
            let collector = factory();
            assert_eq!(collector.name(), *key);
        }
    }

    #[test]
    fn test_default_collector_enabled_by_default() {
        let factories = crate::collectors::all_factories();

        if let Some(factory) = factories.get("default") {
            let collector = factory();
            assert!(collector.enabled_by_default());
        }
    }

    #[test]
    fn test_register_metrics_does_not_panic() {
        let factories = crate::collectors::all_factories();
        let registry = Registry::new();

        for (name, factory) in &factories {
            let collector = factory();
            let result = collector.register_metrics(&registry);
            assert!(
                result.is_ok(),
                "Collector '{name}' failed to register metrics"
            );
        }
    }
}
