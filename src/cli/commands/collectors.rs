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
}
