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
            collectors,
        } => {
            new(port, listen, dsn, collectors).await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;

    #[tokio::test]
    async fn test_handle_action_signature() {
        let action = Action::Run {
            port: 9999,
            listen: None,
            dsn: SecretString::new("mysql://root:password@localhost:3306/mysql".into()),
            collectors: vec!["default".to_string()],
        };

        let result = handle(action).await;

        assert!(
            result.is_err(),
            "Should fail without a real database connection"
        );
    }

    #[test]
    fn test_action_creation() {
        let action = Action::Run {
            port: 9104,
            listen: Some("127.0.0.1".to_string()),
            dsn: SecretString::new("mysql://root@localhost:3306/mysql".into()),
            collectors: vec!["default".to_string(), "exporter".to_string()],
        };

        match action {
            Action::Run {
                port,
                listen,
                dsn: _,
                collectors,
            } => {
                assert_eq!(port, 9104);
                assert_eq!(listen, Some("127.0.0.1".to_string()));
                assert!(collectors.contains(&"default".to_string()));
                assert!(collectors.contains(&"exporter".to_string()));
            }
        }
    }
}
