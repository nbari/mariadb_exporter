use anyhow::Result;
use prometheus::{Counter, CounterVec, GaugeVec, HistogramVec, IntGauge, Opts, Registry};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

#[derive(Clone)]
pub struct ScraperCollector {
    scrape_duration_seconds: HistogramVec,
    scrape_errors_total: CounterVec,
    scrape_aborted_total: CounterVec,
    scrape_aborted: Counter,
    last_scrape_timestamp: GaugeVec,
    last_scrape_success: GaugeVec,

    metrics_total: IntGauge,
    scrapes_total: IntGauge,

    state: Arc<RwLock<ScraperState>>,
}

#[derive(Default)]
struct ScraperState {
    total_scrapes: i64,
    total_metrics: i64,
    /// Scrape generation (`total_scrapes` at timer start) of the newest outcome recorded
    /// per collector. Guards `last_scrape_success`/`last_scrape_timestamp` against a
    /// *stale abort*: the gate is released as soon as a scrape times out, so the next
    /// scrape can complete a collector before the runtime reaps the aborted task and
    /// drops its timer — that late `Drop` must not clobber the newer outcome.
    outcome_epochs: HashMap<String, i64>,
}

impl Default for ScraperCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ScraperCollector {
    #[must_use]
    #[allow(clippy::expect_used)]
    ///
    /// # Panics
    ///
    /// Panics if metric creation fails.
    pub fn new() -> Self {
        let scrape_duration_seconds = HistogramVec::new(
            prometheus::HistogramOpts::new(
                "mariadb_exporter_collector_scrape_duration_seconds",
                "Time spent scraping each collector in seconds",
            )
            .buckets(vec![
                0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
            ]),
            &["collector"],
        )
        .expect("mariadb_exporter_collector_scrape_duration_seconds");

        let scrape_errors_total = CounterVec::new(
            Opts::new(
                "mariadb_exporter_collector_scrape_errors_total",
                "Total number of scrape errors per collector",
            ),
            &["collector"],
        )
        .expect("mariadb_exporter_collector_scrape_errors_total");

        let scrape_aborted_total = CounterVec::new(
            Opts::new(
                "mariadb_exporter_collector_scrape_aborted_total",
                "Total scrape attempts aborted mid-flight per collector (scrape timeout or \
                 client disconnect); the duration sample records time-until-abort",
            ),
            &["collector"],
        )
        .expect("mariadb_exporter_collector_scrape_aborted_total");

        let scrape_aborted = Counter::with_opts(Opts::new(
            "mariadb_exporter_scrape_aborted_total",
            "Total scrapes abandoned before any collector finished, counted once per scrape \
             rather than per collector. Incremented when a scrape exceeds --scrape.timeout-ms, \
             including while the connectivity check is still in flight, which is the window the \
             per-collector counter cannot see. A client that disconnects mid-scrape drops the \
             request future and so runs no code here: those aborts appear only in the \
             per-collector counter, and only for collectors already in flight",
        ))
        .expect("mariadb_exporter_scrape_aborted_total");

        let last_scrape_timestamp = GaugeVec::new(
            Opts::new(
                "mariadb_exporter_collector_last_scrape_timestamp_seconds",
                "Unix timestamp of the last scrape attempt per collector",
            ),
            &["collector"],
        )
        .expect("mariadb_exporter_collector_last_scrape_timestamp_seconds");

        let last_scrape_success = GaugeVec::new(
            Opts::new(
                "mariadb_exporter_collector_last_scrape_success",
                "Whether the last scrape was successful (1=success, 0=failure)",
            ),
            &["collector"],
        )
        .expect("mariadb_exporter_collector_last_scrape_success");

        let metrics_total = IntGauge::with_opts(Opts::new(
            "mariadb_exporter_metrics_total",
            "Total number of metrics currently exported (for cardinality monitoring)",
        ))
        .expect("mariadb_exporter_metrics_total");

        let scrapes_total = IntGauge::with_opts(Opts::new(
            "mariadb_exporter_scrapes_total",
            "Total number of scrapes performed since start",
        ))
        .expect("mariadb_exporter_scrapes_total");

        Self {
            scrape_duration_seconds,
            scrape_errors_total,
            scrape_aborted_total,
            scrape_aborted,
            last_scrape_timestamp,
            last_scrape_success,
            metrics_total,
            scrapes_total,
            state: Arc::new(RwLock::new(ScraperState::default())),
        }
    }

    #[must_use]
    pub fn start_scrape(&self, collector_name: &str) -> ScrapeTimer {
        // `increment_scrapes` runs once per scrape before any timer starts, so
        // `total_scrapes` identifies the scrape generation this timer belongs to.
        let epoch = match self.state.read() {
            Ok(guard) => guard.total_scrapes,
            Err(poisoned) => {
                tracing::warn!("ScraperState read lock was poisoned, recovering");
                poisoned.into_inner().total_scrapes
            }
        };
        ScrapeTimer {
            collector_name: collector_name.to_string(),
            start: Instant::now(),
            scraper: self.clone(),
            recorded: false,
            epoch,
        }
    }

    pub fn update_metrics_count(&self, count: i64) {
        self.metrics_total.set(count);
        let mut state = match self.state.write() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::warn!("ScraperState write lock was poisoned, recovering");
                poisoned.into_inner()
            }
        };
        state.total_metrics = count;
    }

    pub fn increment_scrapes(&self) {
        let mut state = match self.state.write() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::warn!("ScraperState write lock was poisoned, recovering");
                poisoned.into_inner()
            }
        };
        state.total_scrapes += 1;
        self.scrapes_total.set(state.total_scrapes);
    }

    fn record_success(&self, collector_name: &str, duration: f64, epoch: i64) {
        self.scrape_duration_seconds
            .with_label_values(&[collector_name])
            .observe(duration);
        self.record_outcome(collector_name, epoch, true);
    }

    fn record_error(&self, collector_name: &str, epoch: i64) {
        self.scrape_errors_total
            .with_label_values(&[collector_name])
            .inc();
        self.record_outcome(collector_name, epoch, false);
    }

    /// Publishes one collector outcome unless a newer scrape already won.
    ///
    /// The epoch comparison, epoch update, timestamp, and success gauge are one critical
    /// section. Keeping the Prometheus writes under the same lock is load-bearing: if the
    /// lock were released first, a stale abort could pass its check, a newer success could
    /// publish, and then the stale abort could overwrite that success after the newer epoch
    /// was already stored.
    fn record_outcome(&self, collector_name: &str, epoch: i64, success: bool) {
        self.publish_outcome_if_current(collector_name, epoch, || {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            self.last_scrape_timestamp
                .with_label_values(&[collector_name])
                .set(timestamp);
            self.last_scrape_success
                .with_label_values(&[collector_name])
                .set(if success { 1.0 } else { 0.0 });
        });
    }

    fn publish_outcome_if_current(
        &self,
        collector_name: &str,
        epoch: i64,
        publish: impl FnOnce(),
    ) {
        let mut state = match self.state.write() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::warn!("ScraperState write lock was poisoned, recovering");
                poisoned.into_inner()
            }
        };
        if state.outcome_epochs.get(collector_name) > Some(&epoch) {
            return;
        }
        state
            .outcome_epochs
            .insert(collector_name.to_string(), epoch);
        publish();
    }

    /// Records a scrape that was aborted mid-flight, so no outcome is known.
    ///
    /// The abort is always accounted for — counter increment and duration observation —
    /// because the attempt really happened. But the `last_scrape_success` /
    /// `last_scrape_timestamp` write is skipped when a *newer* scrape already recorded an
    /// outcome for this collector: a timed-out scrape releases the gate before the runtime
    /// reaps the aborted task, so this late `Drop` can land after the next scrape's
    /// success and must not overwrite it.
    fn record_aborted(&self, collector_name: &str, duration: f64, epoch: i64) {
        self.scrape_duration_seconds
            .with_label_values(&[collector_name])
            .observe(duration);

        self.scrape_aborted_total
            .with_label_values(&[collector_name])
            .inc();

        self.record_outcome(collector_name, epoch, false);
    }

    /// Count a scrape abandoned as a whole, independently of any collector.
    ///
    /// The per-collector counter only moves for collectors that had already started, so a
    /// scrape that times out during the connectivity check leaves no trace there.
    pub fn record_scrape_aborted(&self) {
        self.scrape_aborted.inc();
    }

    ///
    /// # Errors
    ///
    /// Returns an error if metric registration fails.
    pub fn register(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.scrape_duration_seconds.clone()))?;
        registry.register(Box::new(self.scrape_errors_total.clone()))?;
        registry.register(Box::new(self.scrape_aborted_total.clone()))?;
        registry.register(Box::new(self.scrape_aborted.clone()))?;
        registry.register(Box::new(self.last_scrape_timestamp.clone()))?;
        registry.register(Box::new(self.last_scrape_success.clone()))?;
        registry.register(Box::new(self.metrics_total.clone()))?;
        registry.register(Box::new(self.scrapes_total.clone()))?;
        Ok(())
    }
}

impl crate::collectors::Collector for ScraperCollector {
    fn name(&self) -> &'static str {
        "scraper"
    }

    fn register_metrics(&self, registry: &Registry) -> Result<()> {
        self.register(registry)
    }

    fn collect_once<'a>(
        &'a self,
        _pool: &'a sqlx::MySqlPool,
    ) -> futures::future::BoxFuture<'a, Result<crate::collectors::Collected>> {
        Box::pin(async move { Ok(crate::collectors::Collected::Fresh) })
    }

    /// Deliberately a no-op: the scraper observes the exporter itself, so it has no source
    /// that can become unavailable and no skip path. Clearing it would also destroy the
    /// per-collector error counters that make a skip or failure alertable.
    fn reset_metrics(&self) {}

    fn enabled_by_default(&self) -> bool {
        false
    }
}

/// Times one collector's scrape and records its outcome when it is consumed.
///
/// `success()` and `error()` consume the timer and record explicitly. If the timer is
/// instead *dropped* without either call, the scrape task was aborted while this collector
/// was still in flight (`--scrape.timeout-ms` elapsed, or the client disconnected). That is
/// recorded in `mariadb_exporter_collector_scrape_aborted_total` with
/// `last_scrape_success = 0` — never as a success.
pub struct ScrapeTimer {
    collector_name: String,
    start: Instant,
    scraper: ScraperCollector,
    recorded: bool,
    /// Scrape generation this timer belongs to (`ScraperState::total_scrapes` when the
    /// timer started); lets a late abort detect that a newer scrape already recorded an
    /// outcome for this collector.
    epoch: i64,
}

impl ScrapeTimer {
    pub fn success(mut self) {
        self.recorded = true;
        let duration = self.start.elapsed().as_secs_f64();
        self.scraper
            .record_success(&self.collector_name, duration, self.epoch);
    }

    pub fn error(mut self) {
        self.recorded = true;
        self.scraper.record_error(&self.collector_name, self.epoch);
    }
}

impl Drop for ScrapeTimer {
    fn drop(&mut self) {
        if self.recorded {
            return;
        }

        // Neither success() nor error() was called: the scrape future was dropped while
        // this collector was still in flight, which happens when the scrape task is
        // aborted at `--scrape.timeout-ms` or because the client disconnected.
        //
        // Recording a success here — the previous behaviour — made every collector caught
        // mid-flight by a timed-out scrape report `last_scrape_success = 1` and a
        // timeout-sized "success" duration, so the exporter's own health metrics read
        // healthy during exactly the incident they exist to diagnose.
        //
        // This drop can also land *late*: the gate is released the moment a scrape times
        // out, so the next scrape may complete this same collector before the runtime
        // reaps the aborted task. The abort is still counted (the attempt really
        // happened), but `record_aborted` skips the `last_scrape_success` /
        // `last_scrape_timestamp` write when `outcome_epochs` shows a newer scrape
        // already recorded an outcome — otherwise a stale abort would report failure for
        // a collector whose most recent completed scrape succeeded.
        let duration = self.start.elapsed().as_secs_f64();
        self.scraper
            .record_aborted(&self.collector_name, duration, self.epoch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prometheus::{Encoder, TextEncoder};
    use std::thread;
    use std::time::{Duration, Instant};

    /// `nbari/pg_exporter#34` follow-up: when the scrape task is aborted mid-flight
    /// (`--scrape.timeout-ms` or a client disconnect) each in-flight collector's timer is
    /// dropped without `success()`/`error()`. That must be visible as an abort — never as
    /// a success, which is what the exporter used to report during exactly the incident
    /// these metrics exist to diagnose.
    #[test]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn an_unobserved_timer_drop_is_recorded_as_an_abort() {
        let scraper = ScraperCollector::new();
        let registry = Registry::new();
        scraper.register(&registry).unwrap();

        {
            let timer = scraper.start_scrape("test_collector");
            thread::sleep(Duration::from_millis(10));
            drop(timer);
        }

        let metrics = registry.gather();
        let sample = |name: &str| {
            metrics
                .iter()
                .find(|m| m.name() == name)
                .and_then(|m| m.get_metric().first().cloned())
        };

        let aborted = sample("mariadb_exporter_collector_scrape_aborted_total")
            .expect("aborted counter should exist");
        assert!(
            (aborted.get_counter().value() - 1.0).abs() < f64::EPSILON,
            "an unobserved timer drop must count exactly one abort"
        );

        let success = sample("mariadb_exporter_collector_last_scrape_success")
            .expect("success gauge should exist");
        assert!(
            success.get_gauge().value().abs() < f64::EPSILON,
            "an aborted collector must not report last_scrape_success = 1"
        );

        let duration = sample("mariadb_exporter_collector_scrape_duration_seconds")
            .expect("duration histogram should exist");
        assert_eq!(
            duration.get_histogram().get_sample_count(),
            1,
            "time-until-abort is still observed so a stalled collector shows up in _sum/_count"
        );

        assert!(
            sample("mariadb_exporter_collector_scrape_errors_total").is_none(),
            "an abort is not a collector error: the collector may be blameless"
        );
    }

    /// An explicit outcome must not also be counted as an abort when the timer drops.
    #[test]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn an_observed_outcome_is_never_counted_as_an_abort() {
        let scraper = ScraperCollector::new();
        let registry = Registry::new();
        scraper.register(&registry).unwrap();

        scraper.start_scrape("ok_collector").success();
        scraper.start_scrape("bad_collector").error();

        let metrics = registry.gather();
        assert!(
            metrics
                .iter()
                .all(|m| m.name() != "mariadb_exporter_collector_scrape_aborted_total"),
            "success()/error() must not leave an abort behind"
        );
    }

    /// The scrape gate is released as soon as a scrape times out, but the per-collector
    /// abort accounting lands only when the runtime reaps the aborted task and drops its
    /// timer. A newer scrape can complete the same collector in between; the late drop
    /// must still count the abort yet must not overwrite the newer success.
    #[test]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn a_stale_abort_does_not_overwrite_a_newer_outcome() {
        let scraper = ScraperCollector::new();
        let registry = Registry::new();
        scraper.register(&registry).unwrap();

        // Timer from scrape generation 0: the in-flight future of a scrape that times
        // out. The gate frees immediately; this timer's drop is deferred until the
        // runtime reaps the aborted task.
        let stale = scraper.start_scrape("c");

        // The next scrape acquires the freed gate and completes the same collector.
        scraper.increment_scrapes();
        scraper.start_scrape("c").success();

        // Only now is the aborted task reaped: the stale timer drops late.
        drop(stale);

        let metrics = registry.gather();
        let sample = |name: &str| {
            metrics
                .iter()
                .find(|m| m.name() == name)
                .and_then(|m| m.get_metric().first().cloned())
        };

        let aborted = sample("mariadb_exporter_collector_scrape_aborted_total")
            .expect("aborted counter should exist");
        assert!(
            (aborted.get_counter().value() - 1.0).abs() < f64::EPSILON,
            "the aborted attempt is still counted"
        );

        let success = sample("mariadb_exporter_collector_last_scrape_success")
            .expect("success gauge should exist");
        assert!(
            (success.get_gauge().value() - 1.0).abs() < f64::EPSILON,
            "the newer scrape's success must survive the late abort"
        );
    }

    /// The epoch and gauges must be one atomic publication. This forces both an older abort
    /// and a newer success to queue at the epoch lock after their always-recorded histogram
    /// work has completed. Whichever waiter acquires the lock first, the newer epoch must be
    /// the final gauge value; the stale abort may never write after it.
    #[test]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn a_stale_abort_cannot_write_after_a_newer_outcome_during_lock_contention() {
        let scraper = ScraperCollector::new();
        let registry = Registry::new();
        scraper.register(&registry).unwrap();

        scraper.increment_scrapes();
        let stale = scraper.start_scrape("race");
        scraper.increment_scrapes();
        let newer = scraper.start_scrape("race");

        let held = scraper.state.write().unwrap();
        let stale_thread = thread::spawn(move || drop(stale));

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let abort_recorded = registry
                .gather()
                .iter()
                .find(|m| m.name() == "mariadb_exporter_collector_scrape_aborted_total")
                .and_then(|m| m.get_metric().first())
                .is_some_and(|m| m.get_counter().value() >= 1.0);
            if abort_recorded {
                break;
            }
            assert!(Instant::now() < deadline, "stale abort never reached the epoch lock");
            thread::yield_now();
        }

        let newer_thread = thread::spawn(move || newer.success());
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let both_durations_recorded = registry
                .gather()
                .iter()
                .find(|m| m.name() == "mariadb_exporter_collector_scrape_duration_seconds")
                .and_then(|m| m.get_metric().first())
                .is_some_and(|m| m.get_histogram().get_sample_count() >= 2);
            if both_durations_recorded {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "newer success never reached the epoch lock"
            );
            thread::yield_now();
        }

        drop(held);
        stale_thread.join().unwrap();
        newer_thread.join().unwrap();

        let metrics = registry.gather();
        let success = metrics
            .iter()
            .find(|m| m.name() == "mariadb_exporter_collector_last_scrape_success")
            .and_then(|m| m.get_metric().first())
            .expect("success gauge should exist");
        assert!((success.get_gauge().value() - 1.0).abs() < f64::EPSILON);
        assert_eq!(
            scraper
                .state
                .read()
                .unwrap()
                .outcome_epochs
                .get("race"),
            Some(&2)
        );
    }

    #[test]
    fn outcome_publication_holds_the_epoch_lock() {
        let scraper = ScraperCollector::new();
        scraper.publish_outcome_if_current("atomic", 1, || {
            assert!(
                scraper.state.try_write().is_err(),
                "publishing the gauges after releasing the epoch lock reopens the stale-writer race"
            );
        });
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_scraper_collector_new() {
        let scraper = ScraperCollector::new();
        assert_eq!(scraper.metrics_total.get(), 0);
        assert_eq!(scraper.scrapes_total.get(), 0);
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_scraper_collector_registers_without_error() {
        let scraper = ScraperCollector::new();
        let registry = Registry::new();
        assert!(scraper.register(&registry).is_ok());
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    #[allow(clippy::expect_used)]
    fn test_scrape_timer_records_duration() {
        let scraper = ScraperCollector::new();
        let registry = Registry::new();
        scraper.register(&registry).unwrap();

        {
            let timer = scraper.start_scrape("test_collector");
            thread::sleep(Duration::from_millis(10));
            timer.success();
        }

        let metrics = registry.gather();
        let duration_metric = metrics
            .iter()
            .find(|m| m.name() == "mariadb_exporter_collector_scrape_duration_seconds")
            .expect("duration metric should exist");

        assert!(!duration_metric.get_metric().is_empty());
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    #[allow(clippy::expect_used)]
    fn test_scrape_timer_records_error() {
        let scraper = ScraperCollector::new();
        let registry = Registry::new();
        scraper.register(&registry).unwrap();

        {
            let timer = scraper.start_scrape("test_collector");
            timer.error();
        }

        let metrics = registry.gather();
        let error_metric = metrics
            .iter()
            .find(|m| m.name() == "mariadb_exporter_collector_scrape_errors_total")
            .expect("error metric should exist");

        assert!(!error_metric.get_metric().is_empty());
    }

    #[test]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn conditional_counter_help_matches_the_golden_fixture_as_rendered() {
        let scraper = ScraperCollector::new();
        let registry = Registry::new();
        scraper.register(&registry).unwrap();

        scraper.increment_scrapes();
        scraper.start_scrape("error").error();
        let aborted = scraper.start_scrape("aborted");
        drop(aborted);

        let mut encoded = Vec::new();
        TextEncoder::new()
            .encode(&registry.gather(), &mut encoded)
            .unwrap();
        let exposition = String::from_utf8(encoded).expect("text encoder must emit UTF-8");
        let fixture = include_str!("../../../tests/fixtures/metric_metadata.tsv");

        for name in [
            "mariadb_exporter_collector_scrape_aborted_total",
            "mariadb_exporter_collector_scrape_errors_total",
        ] {
            let help = exposition
                .lines()
                .find_map(|line| line.strip_prefix(&format!("# HELP {name} ")))
                .expect("materialised counter must render HELP");
            let rendered_row = format!("{name}\tcounter\t{help}");
            assert!(
                fixture.lines().any(|line| line == rendered_row),
                "rendered metadata row does not match the fixture byte-for-byte: {rendered_row:?}"
            );
        }
    }

    #[test]
    fn test_update_metrics_count() {
        let scraper = ScraperCollector::new();
        scraper.update_metrics_count(42);
        assert_eq!(scraper.metrics_total.get(), 42);
    }

    #[test]
    fn test_increment_scrapes() {
        let scraper = ScraperCollector::new();
        scraper.increment_scrapes();
        assert_eq!(scraper.scrapes_total.get(), 1);
        scraper.increment_scrapes();
        assert_eq!(scraper.scrapes_total.get(), 2);
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    #[allow(clippy::expect_used)]
    fn test_double_recording_bug() {
        let scraper = ScraperCollector::new();
        let registry = Registry::new();
        scraper.register(&registry).unwrap();

        {
            let timer = scraper.start_scrape("test_double");
            timer.success();
            // timer is dropped here
        }

        let metrics = registry.gather();

        let duration_metric = metrics
            .iter()
            .find(|m| m.name() == "mariadb_exporter_collector_scrape_duration_seconds")
            .expect("duration metric should exist");

        let count = duration_metric
            .get_metric()
            .first()
            .expect("metric should have at least one sample")
            .get_histogram()
            .get_sample_count();

        // If bug exists, count will be 2
        assert_eq!(
            count, 1,
            "Should record exactly one observation, but got {count}"
        );
    }
}
