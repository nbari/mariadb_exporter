use crate::collectors::{
    Collected, Collector, NO_LABELS,
    util::{DeniedOnce, QueryFailure, classify_query_error},
};
use anyhow::{Context, Result};
use futures::future::BoxFuture;
use prometheus::{IntGaugeVec, Opts, Registry};
use sqlx::{MySqlPool, Row};
use tracing::{debug, info_span, instrument};
use tracing_futures::Instrument as _;

/// Parser for SHOW ENGINE INNODB STATUS output.
#[derive(Clone)]
pub struct StatusParser {
    // LSN and checkpoint metrics
    lsn_current: IntGaugeVec,
    lsn_flushed: IntGaugeVec,
    lsn_checkpoint: IntGaugeVec,
    checkpoint_age: IntGaugeVec,

    // Transaction metrics
    trx_active_transactions: IntGaugeVec,

    // Semaphore metrics
    semaphore_waits: IntGaugeVec,
    semaphore_wait_time_ms: IntGaugeVec,

    // Adaptive hash index
    adaptive_hash_searches: IntGaugeVec,
    adaptive_hash_searches_btree: IntGaugeVec,

    denied: DeniedOnce,
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
            lsn_current: IntGaugeVec::new(
                Opts::new(
                    "mariadb_innodb_lsn_current",
                    "Current InnoDB log sequence number (LSN)",
                ),
                &NO_LABELS,
            )
            .expect("valid mariadb_innodb_lsn_current metric"),
            lsn_flushed: IntGaugeVec::new(
                Opts::new(
                    "mariadb_innodb_lsn_flushed",
                    "LSN flushed to disk",
                ),
                &NO_LABELS,
            )
            .expect("valid mariadb_innodb_lsn_flushed metric"),
            lsn_checkpoint: IntGaugeVec::new(
                Opts::new(
                    "mariadb_innodb_lsn_checkpoint",
                    "Last checkpoint LSN",
                ),
                &NO_LABELS,
            )
            .expect("valid mariadb_innodb_lsn_checkpoint metric"),
            checkpoint_age: IntGaugeVec::new(
                Opts::new(
                    "mariadb_innodb_checkpoint_age_bytes",
                    "InnoDB checkpoint age in bytes (LSN current - LSN checkpoint)",
                ),
                &NO_LABELS,
            )
            .expect("valid mariadb_innodb_checkpoint_age_bytes metric"),
            trx_active_transactions: IntGaugeVec::new(
                Opts::new(
                    "mariadb_innodb_active_transactions",
                    "Number of active InnoDB transactions",
                ),
                &NO_LABELS,
            )
            .expect("valid mariadb_innodb_active_transactions metric"),
            semaphore_waits: IntGaugeVec::new(
                Opts::new(
                    "mariadb_innodb_semaphore_waits_total",
                    "Total number of semaphore waits",
                ),
                &NO_LABELS,
            )
            .expect("valid mariadb_innodb_semaphore_waits_total metric"),
            semaphore_wait_time_ms: IntGaugeVec::new(
                Opts::new(
                    "mariadb_innodb_semaphore_wait_time_ms_total",
                    "Total semaphore wait time in milliseconds",
                ),
                &NO_LABELS,
            )
            .expect("valid mariadb_innodb_semaphore_wait_time_ms_total metric"),
            adaptive_hash_searches: IntGaugeVec::new(
                Opts::new(
                    "mariadb_innodb_adaptive_hash_searches_total",
                    "Adaptive hash index searches",
                ),
                &NO_LABELS,
            )
            .expect("valid mariadb_innodb_adaptive_hash_searches_total metric"),
            adaptive_hash_searches_btree: IntGaugeVec::new(
                Opts::new(
                    "mariadb_innodb_adaptive_hash_searches_btree_total",
                    "Adaptive hash index searches requiring B-tree lookup",
                ),
                &NO_LABELS,
            )
            .expect("valid mariadb_innodb_adaptive_hash_searches_btree_total metric"),
            denied: DeniedOnce::default(),
        }
    }

    // Getter methods for metrics (used in mod.rs for registration)
    
    /// Get LSN current metric.
    #[must_use]
    pub const fn lsn_current(&self) -> &IntGaugeVec {
        &self.lsn_current
    }

    /// Get LSN flushed metric.
    #[must_use]
    pub const fn lsn_flushed(&self) -> &IntGaugeVec {
        &self.lsn_flushed
    }

    /// Get LSN checkpoint metric.
    #[must_use]
    pub const fn lsn_checkpoint(&self) -> &IntGaugeVec {
        &self.lsn_checkpoint
    }

    /// Get checkpoint age metric.
    #[must_use]
    pub const fn checkpoint_age(&self) -> &IntGaugeVec {
        &self.checkpoint_age
    }

    /// Get active transactions metric.
    #[must_use]
    pub const fn active_transactions(&self) -> &IntGaugeVec {
        &self.trx_active_transactions
    }

    /// Get semaphore waits metric.
    #[must_use]
    pub const fn semaphore_waits(&self) -> &IntGaugeVec {
        &self.semaphore_waits
    }

    /// Get semaphore wait time metric.
    #[must_use]
    pub const fn semaphore_wait_time_ms(&self) -> &IntGaugeVec {
        &self.semaphore_wait_time_ms
    }

    /// Get adaptive hash searches metric.
    #[must_use]
    pub const fn adaptive_hash_searches(&self) -> &IntGaugeVec {
        &self.adaptive_hash_searches
    }

    /// Get adaptive hash B-tree searches metric.
    #[must_use]
    pub const fn adaptive_hash_searches_btree(&self) -> &IntGaugeVec {
        &self.adaptive_hash_searches_btree
    }

    /// Collect `InnoDB` status metrics from database.
    ///
    /// # Errors
    ///
    /// Returns an error if the status query fails for a reason other than the source being
    /// absent or unreadable, or if the `Status` column cannot be read.
    #[instrument(skip(self, pool), level = "debug", fields(sub_collector = "innodb_status"))]
    async fn collect_inner(&self, pool: &MySqlPool) -> Result<Collected> {
        let span = info_span!(
            "db.query",
            db.system = "mysql",
            db.operation = "SHOW",
            db.statement = "SHOW ENGINE INNODB STATUS",
            otel.kind = "client"
        );

        let row = match sqlx::query("SHOW ENGINE INNODB STATUS")
            .fetch_one(pool)
            .instrument(span)
            .await
        {
            Ok(row) => row,
            Err(e) => match classify_query_error(&e) {
                QueryFailure::Absent => {
                    debug!(error = %e, "InnoDB engine status unavailable; skipping");
                    return Ok(Collected::Skipped);
                }
                QueryFailure::Denied => {
                    self.denied.report("SHOW ENGINE INNODB STATUS", &e);
                    return Ok(Collected::Skipped);
                }
                QueryFailure::Fault => {
                    return Err(anyhow::Error::new(e)
                        .context("failed to execute SHOW ENGINE INNODB STATUS"));
                }
            },
        };

        // Get the status text (column index 2: Type, Name, Status)
        // Try by name first, fall back to index if name doesn't match
        let status_text: String = row
            .try_get("Status")
            .or_else(|_| row.try_get(2))
            .context("failed to get Status column from SHOW ENGINE INNODB STATUS")?;

        // A malformed status document is a fault, not an absent source.
        self.parse(&status_text)?;

        Ok(Collected::Fresh)
    }

    /// Publish an optional value, removing the series when the source document did not
    /// report it. Keeping the previous scrape's LSN or checkpoint would be indistinguishable
    /// from a stalled server.
    fn publish_optional(metric: &IntGaugeVec, value: Option<i64>) {
        match value {
            Some(v) => metric.with_label_values(&NO_LABELS).set(v),
            None => {
                let _ = metric.remove_label_values(&NO_LABELS);
            }
        }
    }

    /// Parse SHOW ENGINE INNODB STATUS output.
    ///
    /// # Errors
    ///
    /// Returns an error if parsing fails critically.
    pub fn parse(&self, status: &str) -> Result<()> {
        let parsed = Self::scan(status);
        self.publish(&parsed);
        Ok(())
    }

    /// Extract every field this collector understands from a status document.
    ///
    /// Optional fields stay `None` when their line is absent, which is how a successful
    /// document that no longer contains them (for example with
    /// `innodb_adaptive_hash_index=OFF`) is told apart from one that never had them.
    fn scan(status: &str) -> ParsedStatus {
        let mut lsn_current: Option<i64> = None;
        let mut lsn_flushed: Option<i64> = None;
        let mut lsn_checkpoint: Option<i64> = None;
        let mut adaptive_hash_searches: Option<i64> = None;
        let mut adaptive_hash_searches_btree: Option<i64> = None;
        let mut active_trx = 0;
        let mut semaphore_waits = 0;
        let mut semaphore_wait_time_ms = 0.0;

        for line in status.lines() {
            let line = line.trim();

            // Parse LSN information
            // Example: "Log sequence number          123456789"
            if line.starts_with("Log sequence number")
                && let Some(value) = line.split_whitespace().last()
                && let Ok(lsn) = value.parse::<i64>()
            {
                lsn_current = Some(lsn);
                debug!(lsn_current = lsn, "parsed LSN current");
            }
            // Example: "Log flushed up to           123456000"
            else if line.starts_with("Log flushed up to")
                && let Some(value) = line.split_whitespace().last()
                && let Ok(lsn) = value.parse::<i64>()
            {
                lsn_flushed = Some(lsn);
                debug!(lsn_flushed = lsn, "parsed LSN flushed");
            }
            // Example: "Last checkpoint at          123455000"
            else if line.starts_with("Last checkpoint at")
                && let Some(value) = line.split_whitespace().last()
                && let Ok(lsn) = value.parse::<i64>()
            {
                lsn_checkpoint = Some(lsn);
                debug!(lsn_checkpoint = lsn, "parsed LSN checkpoint");
            }
            // Count active transactions
            // Example: "---TRANSACTION 123456, ACTIVE 5 sec"
            else if line.starts_with("---TRANSACTION") && line.contains("ACTIVE") {
                active_trx += 1;
            }
            // Parse individual semaphore waits/times
            // Example: "--Thread 123 has waited at btr0cur.cc line 123 for 5.00 seconds the semaphore:"
            else if line.contains("has waited at") && line.contains("for") && line.contains("seconds") {
                if let Some(wait_part) = line.split("for").nth(1)
                    && let Some(seconds_str) = wait_part.split_whitespace().next()
                    && let Ok(seconds) = seconds_str.parse::<f64>()
                {
                    semaphore_wait_time_ms += seconds * 1000.0;
                    debug!(wait_seconds = seconds, "parsed individual semaphore wait time");
                }
            }
            // Parse cumulative semaphore waits
            // Example: "Mutex spin waits 12345, rounds 67890, OS waits 123"
            else if line.contains("OS waits")
                && let Some(waits_str) = line.split("OS waits").nth(1)
                && let Some(num_str) = waits_str.split_whitespace().next()
                && let Ok(waits) = num_str.parse::<i64>()
            {
                semaphore_waits += waits;
                debug!(semaphore_waits = waits, "parsed semaphore waits part");
            }
            // Parse adaptive hash index
            // Example: "123456 hash searches/s, 12345 non-hash searches/s"
            else if line.contains("hash searches/s") {
                let parts: Vec<&str> = line.split(',').collect();
                if let Some(hash_part) = parts.first()
                    && let Some(value) = hash_part.split_whitespace().next()
                    && let Ok(searches) = value.parse::<i64>()
                {
                    adaptive_hash_searches = Some(searches);
                    debug!(
                        adaptive_hash_searches = searches,
                        "parsed adaptive hash searches"
                    );
                }
                if let Some(btree_part) = parts.get(1)
                    && let Some(value) = btree_part.split_whitespace().next()
                    && let Ok(searches) = value.parse::<i64>()
                {
                    adaptive_hash_searches_btree = Some(searches);
                    debug!(
                        adaptive_hash_searches_btree = searches,
                        "parsed adaptive hash B-tree searches"
                    );
                }
            }
        }

        ParsedStatus {
            lsn_current,
            lsn_flushed,
            lsn_checkpoint,
            adaptive_hash_searches,
            adaptive_hash_searches_btree,
            active_trx,
            semaphore_waits,
            semaphore_wait_time_ms,
        }
    }

    /// Publish a freshly scanned document, replacing the previous snapshot.
    fn publish(&self, parsed: &ParsedStatus) {
        let ParsedStatus {
            lsn_current,
            lsn_flushed,
            lsn_checkpoint,
            adaptive_hash_searches,
            adaptive_hash_searches_btree,
            active_trx,
            semaphore_waits,
            semaphore_wait_time_ms,
        } = *parsed;

        // Optional lines: absent in a successful document (for example when
        // `innodb_adaptive_hash_index=OFF` removes the AHI section) means the value is
        // unknown now, so the series is removed rather than left at its previous value.
        Self::publish_optional(&self.lsn_current, lsn_current);
        Self::publish_optional(&self.lsn_flushed, lsn_flushed);
        Self::publish_optional(&self.lsn_checkpoint, lsn_checkpoint);
        Self::publish_optional(
            &self.checkpoint_age,
            lsn_current.zip(lsn_checkpoint).map(|(c, p)| c - p),
        );
        Self::publish_optional(&self.adaptive_hash_searches, adaptive_hash_searches);
        Self::publish_optional(
            &self.adaptive_hash_searches_btree,
            adaptive_hash_searches_btree,
        );

        // Always-derivable counts: a document with no matching lines genuinely means zero.
        self.trx_active_transactions
            .with_label_values(&NO_LABELS)
            .set(active_trx);
        debug!(
            active_transactions = active_trx,
            "counted active transactions"
        );

        self.semaphore_waits
            .with_label_values(&NO_LABELS)
            .set(semaphore_waits);
        #[allow(clippy::cast_possible_truncation)]
        self.semaphore_wait_time_ms
            .with_label_values(&NO_LABELS)
            .set(semaphore_wait_time_ms as i64);
        debug!(
            semaphore_waits_total = semaphore_waits,
            semaphore_wait_time_total_ms = semaphore_wait_time_ms,
            "summed semaphore metrics"
        );
    }
}

/// Everything a single `SHOW ENGINE INNODB STATUS` document yielded.
///
/// `Option` fields correspond to lines that a healthy server may legitimately omit.
struct ParsedStatus {
    lsn_current: Option<i64>,
    lsn_flushed: Option<i64>,
    lsn_checkpoint: Option<i64>,
    adaptive_hash_searches: Option<i64>,
    adaptive_hash_searches_btree: Option<i64>,
    active_trx: i64,
    semaphore_waits: i64,
    semaphore_wait_time_ms: f64,
}

impl Collector for StatusParser {
    fn name(&self) -> &'static str {
        "innodb_status"
    }

    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.lsn_current.clone()))?;
        registry.register(Box::new(self.lsn_flushed.clone()))?;
        registry.register(Box::new(self.lsn_checkpoint.clone()))?;
        registry.register(Box::new(self.checkpoint_age.clone()))?;
        registry.register(Box::new(self.trx_active_transactions.clone()))?;
        registry.register(Box::new(self.semaphore_waits.clone()))?;
        registry.register(Box::new(self.semaphore_wait_time_ms.clone()))?;
        registry.register(Box::new(self.adaptive_hash_searches.clone()))?;
        registry.register(Box::new(self.adaptive_hash_searches_btree.clone()))?;
        Ok(())
    }

    fn collect_once<'a>(&'a self, pool: &'a MySqlPool) -> BoxFuture<'a, Result<Collected>> {
        Box::pin(async move { self.collect_inner(pool).await })
    }

    fn reset_metrics(&self) {
        self.lsn_current.reset();
        self.lsn_flushed.reset();
        self.lsn_checkpoint.reset();
        self.checkpoint_age.reset();
        self.trx_active_transactions.reset();
        self.semaphore_waits.reset();
        self.semaphore_wait_time_ms.reset();
        self.adaptive_hash_searches.reset();
        self.adaptive_hash_searches_btree.reset();
    }

    fn enabled_by_default(&self) -> bool {
        false
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

        assert_eq!(parser.lsn_current.with_label_values(&NO_LABELS).get(), 123_456_789);
        assert_eq!(parser.lsn_flushed.with_label_values(&NO_LABELS).get(), 123_456_000);
        assert_eq!(parser.lsn_checkpoint.with_label_values(&NO_LABELS).get(), 123_450_000);
        assert_eq!(
            parser.checkpoint_age.with_label_values(&NO_LABELS).get(),
            123_456_789 - 123_450_000
        );
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

        assert_eq!(
            parser
                .trx_active_transactions
                .with_label_values(&NO_LABELS)
                .get(),
            3
        );
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_parse_semaphore_waits() {
        let parser = StatusParser::new();
        let status = "
Mutex spin waits 12345, rounds 67890, OS waits 123
RW-shared spins 54321, rounds 98765, OS waits 456
--Thread 123 has waited at btr0cur.cc line 123 for 5.00 seconds the semaphore:
--Thread 456 has waited at ha_innodb.cc line 456 for 1.25 seconds the semaphore:
        ";

        parser.parse(status).unwrap();

        // Should capture the sum of all OS waits values (123 + 456 = 579)
        assert_eq!(parser.semaphore_waits.with_label_values(&NO_LABELS).get(), 579);
        // Should capture the sum of all wait times (5.00 + 1.25 = 6.25 seconds = 6250 ms)
        assert_eq!(
            parser
                .semaphore_wait_time_ms
                .with_label_values(&NO_LABELS)
                .get(),
            6250
        );
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_parse_adaptive_hash() {
        let parser = StatusParser::new();
        let status = "
123456 hash searches/s, 12345 non-hash searches/s
        ";

        parser.parse(status).unwrap();

        assert_eq!(
            parser
                .adaptive_hash_searches
                .with_label_values(&NO_LABELS)
                .get(),
            123_456
        );
        assert_eq!(
            parser
                .adaptive_hash_searches_btree
                .with_label_values(&NO_LABELS)
                .get(),
            12_345
        );
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
