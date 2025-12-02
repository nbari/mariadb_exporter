use anyhow::{Context, Result};
use prometheus::IntGauge;
use sqlx::{MySqlPool, Row};
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;

/// Parser for SHOW ENGINE INNODB STATUS output.
#[derive(Clone)]
pub struct StatusParser {
    // LSN and checkpoint metrics
    lsn_current: IntGauge,
    lsn_flushed: IntGauge,
    lsn_checkpoint: IntGauge,
    checkpoint_age: IntGauge,

    // Transaction metrics
    trx_active_transactions: IntGauge,

    // Semaphore metrics
    semaphore_waits: IntGauge,
    semaphore_wait_time_ms: IntGauge,

    // Adaptive hash index
    adaptive_hash_searches: IntGauge,
    adaptive_hash_searches_btree: IntGauge,
}

impl StatusParser {
    #[must_use]
    #[allow(clippy::expect_used)]
    /// Create a new status parser.
    ///
    /// # Panics
    ///
    /// Panics if metric names are invalid (should not occur with static names).
    pub fn new() -> Self {
        Self {
            lsn_current: IntGauge::new(
                "mariadb_innodb_lsn_current",
                "Current InnoDB log sequence number (LSN)",
            )
            .expect("valid mariadb_innodb_lsn_current metric"),
            lsn_flushed: IntGauge::new(
                "mariadb_innodb_lsn_flushed",
                "LSN flushed to disk",
            )
            .expect("valid mariadb_innodb_lsn_flushed metric"),
            lsn_checkpoint: IntGauge::new(
                "mariadb_innodb_lsn_checkpoint",
                "Last checkpoint LSN",
            )
            .expect("valid mariadb_innodb_lsn_checkpoint metric"),
            checkpoint_age: IntGauge::new(
                "mariadb_innodb_checkpoint_age_bytes",
                "InnoDB checkpoint age in bytes (LSN current - LSN checkpoint)",
            )
            .expect("valid mariadb_innodb_checkpoint_age_bytes metric"),
            trx_active_transactions: IntGauge::new(
                "mariadb_innodb_active_transactions",
                "Number of active InnoDB transactions",
            )
            .expect("valid mariadb_innodb_active_transactions metric"),
            semaphore_waits: IntGauge::new(
                "mariadb_innodb_semaphore_waits_total",
                "Total number of semaphore waits",
            )
            .expect("valid mariadb_innodb_semaphore_waits_total metric"),
            semaphore_wait_time_ms: IntGauge::new(
                "mariadb_innodb_semaphore_wait_time_ms_total",
                "Total semaphore wait time in milliseconds",
            )
            .expect("valid mariadb_innodb_semaphore_wait_time_ms_total metric"),
            adaptive_hash_searches: IntGauge::new(
                "mariadb_innodb_adaptive_hash_searches_total",
                "Adaptive hash index searches",
            )
            .expect("valid mariadb_innodb_adaptive_hash_searches_total metric"),
            adaptive_hash_searches_btree: IntGauge::new(
                "mariadb_innodb_adaptive_hash_searches_btree_total",
                "Adaptive hash index searches requiring B-tree lookup",
            )
            .expect("valid mariadb_innodb_adaptive_hash_searches_btree_total metric"),
        }
    }

    // Getter methods for metrics (used in mod.rs for registration)
    
    /// Get LSN current metric.
    #[must_use]
    pub fn lsn_current(&self) -> &IntGauge {
        &self.lsn_current
    }

    /// Get LSN flushed metric.
    #[must_use]
    pub fn lsn_flushed(&self) -> &IntGauge {
        &self.lsn_flushed
    }

    /// Get LSN checkpoint metric.
    #[must_use]
    pub fn lsn_checkpoint(&self) -> &IntGauge {
        &self.lsn_checkpoint
    }

    /// Get checkpoint age metric.
    #[must_use]
    pub fn checkpoint_age(&self) -> &IntGauge {
        &self.checkpoint_age
    }

    /// Get active transactions metric.
    #[must_use]
    pub fn active_transactions(&self) -> &IntGauge {
        &self.trx_active_transactions
    }

    /// Get semaphore waits metric.
    #[must_use]
    pub fn semaphore_waits(&self) -> &IntGauge {
        &self.semaphore_waits
    }

    /// Get semaphore wait time metric.
    #[must_use]
    pub fn semaphore_wait_time_ms(&self) -> &IntGauge {
        &self.semaphore_wait_time_ms
    }

    /// Get adaptive hash searches metric.
    #[must_use]
    pub fn adaptive_hash_searches(&self) -> &IntGauge {
        &self.adaptive_hash_searches
    }

    /// Get adaptive hash B-tree searches metric.
    #[must_use]
    pub fn adaptive_hash_searches_btree(&self) -> &IntGauge {
        &self.adaptive_hash_searches_btree
    }

    /// Collect `InnoDB` status metrics from database.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[instrument(skip(self, pool), level = "debug", fields(sub_collector = "innodb_status"))]
    pub async fn collect(&self, pool: &MySqlPool) -> Result<()> {
        let span = info_span!(
            "db.query",
            db.system = "mysql",
            db.operation = "SHOW",
            db.statement = "SHOW ENGINE INNODB STATUS",
            otel.kind = "client"
        );

        let row = sqlx::query("SHOW ENGINE INNODB STATUS")
            .fetch_one(pool)
            .instrument(span)
            .await
            .context("failed to execute SHOW ENGINE INNODB STATUS")?;

        // Get the status text (column index 2: Type, Name, Status)
        // Try by name first, fall back to index if name doesn't match
        let status_text: String = row
            .try_get("Status")
            .or_else(|_| row.try_get(2))
            .context("failed to get Status column from SHOW ENGINE INNODB STATUS")?;

        // Parse the status text
        self.parse(&status_text)?;

        Ok(())
    }

    /// Parse SHOW ENGINE INNODB STATUS output.
    ///
    /// # Errors
    ///
    /// Returns an error if parsing fails critically.
    pub fn parse(&self, status: &str) -> Result<()> {
        let mut lsn_current: Option<i64> = None;
        let mut lsn_checkpoint: Option<i64> = None;
        let mut active_trx = 0;

        for line in status.lines() {
            let line = line.trim();

            // Parse LSN information
            // Example: "Log sequence number          123456789"
            if line.starts_with("Log sequence number")
                && let Some(value) = line.split_whitespace().last()
                && let Ok(lsn) = value.parse::<i64>()
            {
                lsn_current = Some(lsn);
                self.lsn_current.set(lsn);
                debug!(lsn_current = lsn, "parsed LSN current");
            }
            // Example: "Log flushed up to           123456000"
            else if line.starts_with("Log flushed up to")
                && let Some(value) = line.split_whitespace().last()
                && let Ok(lsn) = value.parse::<i64>()
            {
                self.lsn_flushed.set(lsn);
                debug!(lsn_flushed = lsn, "parsed LSN flushed");
            }
            // Example: "Last checkpoint at          123455000"
            else if line.starts_with("Last checkpoint at")
                && let Some(value) = line.split_whitespace().last()
                && let Ok(lsn) = value.parse::<i64>()
            {
                lsn_checkpoint = Some(lsn);
                self.lsn_checkpoint.set(lsn);
                debug!(lsn_checkpoint = lsn, "parsed LSN checkpoint");
            }
            // Count active transactions
            // Example: "---TRANSACTION 123456, ACTIVE 5 sec"
            else if line.starts_with("---TRANSACTION") && line.contains("ACTIVE") {
                active_trx += 1;
            }
            // Parse semaphore waits
            // Example: "Mutex spin waits 12345, rounds 67890, OS waits 123"
            else if line.contains("OS waits")
                && let Some(waits_str) = line.split("OS waits").nth(1)
                && let Some(num_str) = waits_str.split_whitespace().next()
                && let Ok(waits) = num_str.parse::<i64>()
            {
                self.semaphore_waits.set(waits);
                debug!(semaphore_waits = waits, "parsed semaphore waits");
            }
            // Parse adaptive hash index
            // Example: "123456 hash searches/s, 12345 non-hash searches/s"
            else if line.contains("hash searches/s") {
                let parts: Vec<&str> = line.split(',').collect();
                if let Some(hash_part) = parts.first()
                    && let Some(value) = hash_part.split_whitespace().next()
                    && let Ok(searches) = value.parse::<i64>()
                {
                    self.adaptive_hash_searches.set(searches);
                    debug!(
                        adaptive_hash_searches = searches,
                        "parsed adaptive hash searches"
                    );
                }
                if let Some(btree_part) = parts.get(1)
                    && let Some(value) = btree_part.split_whitespace().next()
                    && let Ok(searches) = value.parse::<i64>()
                {
                    self.adaptive_hash_searches_btree.set(searches);
                    debug!(
                        adaptive_hash_searches_btree = searches,
                        "parsed adaptive hash B-tree searches"
                    );
                }
            }
        }

        // Calculate checkpoint age
        if let (Some(current), Some(checkpoint)) = (lsn_current, lsn_checkpoint) {
            let age = current - checkpoint;
            self.checkpoint_age.set(age);
            debug!(checkpoint_age = age, "calculated checkpoint age");
        }

        // Set active transactions
        self.trx_active_transactions.set(active_trx);
        debug!(
            active_transactions = active_trx,
            "counted active transactions"
        );

        Ok(())
    }
}

impl Default for StatusParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_parse_lsn_metrics() {
        let parser = StatusParser::new();
        let status = "
=====================================
2024-12-02 06:30:00 0x7f8b8c000700 INNODB MONITOR OUTPUT
=====================================
Log sequence number          123456789
Log flushed up to            123456000
Pages flushed up to          123455000
Last checkpoint at           123450000
        ";

        parser.parse(status).unwrap();

        assert_eq!(parser.lsn_current.get(), 123_456_789);
        assert_eq!(parser.lsn_flushed.get(), 123_456_000);
        assert_eq!(parser.lsn_checkpoint.get(), 123_450_000);
        assert_eq!(parser.checkpoint_age.get(), 123_456_789 - 123_450_000);
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_parse_active_transactions() {
        let parser = StatusParser::new();
        let status = "
---TRANSACTION 421234567890, ACTIVE 5 sec starting index read
---TRANSACTION 421234567891, ACTIVE 10 sec
---TRANSACTION 421234567892, ACTIVE 2 sec inserting
        ";

        parser.parse(status).unwrap();

        assert_eq!(parser.trx_active_transactions.get(), 3);
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_parse_semaphore_waits() {
        let parser = StatusParser::new();
        let status = "
Mutex spin waits 12345, rounds 67890, OS waits 123
RW-shared spins 54321, rounds 98765, OS waits 456
        ";

        parser.parse(status).unwrap();

        // Should capture the last OS waits value
        assert_eq!(parser.semaphore_waits.get(), 456);
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_parse_adaptive_hash() {
        let parser = StatusParser::new();
        let status = "
123456 hash searches/s, 12345 non-hash searches/s
        ";

        parser.parse(status).unwrap();

        assert_eq!(parser.adaptive_hash_searches.get(), 123_456);
        assert_eq!(parser.adaptive_hash_searches_btree.get(), 12_345);
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_parse_empty_status() {
        let parser = StatusParser::new();
        let status = "";

        // Should not panic on empty input
        parser.parse(status).unwrap();
    }
}
