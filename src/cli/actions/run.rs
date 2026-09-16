use crate::cli::actions::Action;
use crate::exporter::new;
use anyhow::Result;

/// Handle the run action
///
/// # Errors
///
/// Returns an error if the exporter fails to start
pub async fn handle(action: Action) -> Result<()> {
    match action {
        Action::Run {
            port,
            listen,
            dsn,
            config,
        } => {
            new(port, listen, dsn, config).await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::{config::CollectorConfig, system::ProcessMemorySource};
    use secrecy::SecretString;
    use std::time::Duration;

    #[tokio::test]
    async fn test_handle_action_signature() {
        let action = Action::Run {
            port: 9999,
            listen: None,
            dsn: SecretString::new("invalid-dsn".into()),
            config: CollectorConfig::new().with_enabled(&["default".to_string()]),
        };

        let result = handle(action).await;

        assert!(result.is_err(), "Should fail with an invalid DSN format");
    }

    #[test]
    fn test_action_creation() {
        let action = Action::Run {
            port: 9306,
            listen: Some("127.0.0.1".to_string()),
            dsn: SecretString::new("mysql://root@localhost:3306/mysql".into()),
            config: CollectorConfig::new()
                .with_enabled(&["default".to_string(), "exporter".to_string()])
                .with_scrape_timeout(Duration::from_millis(1_234))
                .with_system_process_memory(ProcessMemorySource::Pss),
        };

        match action {
            Action::Run {
                port,
                listen,
                dsn: _,
                config,
            } => {
                assert_eq!(port, 9306);
                assert_eq!(listen, Some("127.0.0.1".to_string()));
                assert_eq!(config.enabled_collectors.len(), 2);
                assert!(config.enabled_collectors.contains("default"));
                assert!(config.enabled_collectors.contains("exporter"));
                assert_eq!(config.scrape_timeout, Duration::from_millis(1_234));
                assert_eq!(config.system.process_memory, ProcessMemorySource::Pss);
            }
        }
    }

    #[test]
    fn test_action_with_empty_collectors() {
        // Test that Action can be created with empty collectors list
        let action = Action::Run {
            port: 8080,
            listen: None,
            dsn: SecretString::new("mysql://localhost:3306/mysql".into()),
            config: CollectorConfig::new().with_enabled(&[]),
        };

        match action {
            Action::Run { config, .. } => {
                assert_eq!(
                    config.enabled_collectors.len(),
                    0,
                    "Should allow empty collectors list"
                );
            }
        }
    }
}
