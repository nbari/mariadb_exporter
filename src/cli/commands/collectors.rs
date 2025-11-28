use crate::collectors::{COLLECTOR_NAMES, Collector, all_factories};
use clap::{Arg, Command};

pub fn add_collectors_args(mut cmd: Command) -> Command {
    let factories = all_factories();

    for &name in COLLECTOR_NAMES {
        let default_enabled = factories.get(name).is_some_and(|factory| {
            let collector = factory();
            collector.enabled_by_default()
        });

        let enable_flag: &'static str = Box::leak(format!("collector.{name}").into_boxed_str());
        let disable_flag: &'static str = Box::leak(format!("no-collector.{name}").into_boxed_str());

        let default_indicator = if default_enabled {
            " [default: enabled]"
        } else {
            " [default: disabled]"
        };
        let enable_help: &'static str =
            Box::leak(format!("Enable the {name} collector{default_indicator}").into_boxed_str());
        let disable_help: &'static str =
            Box::leak(format!("Disable the {name} collector").into_boxed_str());

        cmd = cmd
            .arg(
                Arg::new(enable_flag)
                    .long(enable_flag)
                    .help(enable_help)
                    .action(clap::ArgAction::SetTrue)
                    .default_value(if default_enabled { "true" } else { "false" }),
            )
            .arg(
                Arg::new(disable_flag)
                    .long(disable_flag)
                    .help(disable_help)
                    .action(clap::ArgAction::SetTrue)
                    .overrides_with(enable_flag),
            );
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::commands;
    use crate::cli::dispatch::get_enabled_collectors;

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_all_collector_flags_are_added() {
        let cmd = commands::new();

        for &name in COLLECTOR_NAMES {
            let enable_flag = format!("collector.{name}");
            let disable_flag = format!("no-collector.{name}");

            let matches = cmd
                .clone()
                .try_get_matches_from(vec!["mariadb_exporter"])
                .unwrap();

            assert!(
                matches.contains_id(&enable_flag),
                "Missing enable flag for {name}"
            );
            assert!(
                matches.contains_id(&disable_flag),
                "Missing disable flag for {name}"
            );
        }
    }

    #[test]
    fn test_collector_default_values() {
        let cmd = commands::new();
        let matches = cmd.get_matches_from(vec!["mariadb_exporter"]);

        let factories = all_factories();

        for &name in COLLECTOR_NAMES {
            let enable_flag = format!("collector.{name}");

            if let Some(factory) = factories.get(name) {
                let collector = factory();
                let expected_default = collector.enabled_by_default();
                let actual_value = matches.get_flag(&enable_flag);

                assert_eq!(
                    actual_value, expected_default,
                    "Collector '{name}' default mismatch: expected {expected_default}, got {actual_value}"
                );
            }
        }
    }

    #[test]
    fn test_disable_flag_overrides_enable_flag() {
        let cmd = commands::new();

        let matches = cmd.get_matches_from(vec![
            "mariadb_exporter",
            "--collector.default",
            "--no-collector.default",
        ]);

        assert!(matches.get_flag("no-collector.default"));
    }

    #[test]
    fn test_enable_flag_after_disable_flag() {
        let cmd = commands::new();

        let matches = cmd.get_matches_from(vec![
            "mariadb_exporter",
            "--no-collector.default",
            "--collector.default",
        ]);

        assert!(matches.get_flag("collector.default"));
    }

    #[test]
    fn test_collector_toggle_behavior_in_dispatch() {
        let cmd = commands::new();

        let matches = cmd.clone().get_matches_from(vec![
            "mariadb_exporter",
            "--collector.default",
            "--no-collector.default",
        ]);
        let enabled = get_enabled_collectors(&matches);
        assert!(
            !enabled.contains(&"default".to_string()),
            "default should be disabled when disable flag comes last"
        );

        let matches = cmd.get_matches_from(vec![
            "mariadb_exporter",
            "--no-collector.default",
            "--collector.default",
        ]);
        let enabled = get_enabled_collectors(&matches);
        assert!(enabled.contains(&"default".to_string()));
    }

    #[test]
    fn test_collector_flags_help_text() {
        let mut cmd = commands::new();
        let long_help = cmd.render_long_help().to_string();

        // Verify help text includes default indicators
        assert!(
            long_help.contains("[default: enabled]") || long_help.contains("[default: disabled]"),
            "Help text should indicate default states"
        );

        // Verify specific collectors are mentioned
        for &name in COLLECTOR_NAMES {
            assert!(
                long_help.contains(name),
                "Help text should mention collector '{name}'"
            );
        }
    }

    #[test]
    fn test_multiple_collectors_can_be_disabled() {
        let cmd = commands::new();
        let matches = cmd.get_matches_from(vec![
            "mariadb_exporter",
            "--no-collector.replication",
            "--no-collector.tls",
            "--no-collector.locks",
        ]);

        let enabled = get_enabled_collectors(&matches);

        // These collectors should NOT be in the enabled list
        assert!(!enabled.contains(&"replication".to_string()));
        assert!(!enabled.contains(&"tls".to_string()));
        assert!(!enabled.contains(&"locks".to_string()));

        // But default should still be there (unless also disabled)
        assert!(enabled.contains(&"default".to_string()));
    }

    #[test]
    fn test_enable_disabled_by_default_collector() {
        let cmd = commands::new();
        let factories = all_factories();

        // Find a collector that's disabled by default
        let disabled_collector = COLLECTOR_NAMES.iter().find(|&&name| {
            factories
                .get(name)
                .is_some_and(|f| !f().enabled_by_default())
        });

        if let Some(&name) = disabled_collector {
            let enable_flag = format!("--collector.{name}");
            let matches = cmd.get_matches_from(vec!["mariadb_exporter", &enable_flag]);

            assert!(
                matches.get_flag(&format!("collector.{name}")),
                "Should be able to enable disabled-by-default collector '{name}'"
            );
        }
    }
}
