use crate::{
    collectors::{
        Collector, CollectorType, all_factories,
        config::CollectorConfig,
        exporter::{ScrapeTimer, ScraperCollector},
        system::SystemCollector,
    },
    exporter::GIT_COMMIT_HASH,
};
use futures::{
    FutureExt as _,
    stream::{FuturesUnordered, StreamExt},
};
use prometheus::{Encoder, Gauge, GaugeVec, Opts, Registry, TextEncoder};
use std::{
    collections::HashMap,
    env,
    error::Error,
    fmt,
    future::Future,
    panic::AssertUnwindSafe,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{sync::Semaphore, time::timeout};
use tracing::{Span, debug, debug_span, error, info, info_span, instrument, warn};
use tracing_futures::Instrument as _;

/// Why a `/metrics` scrape produced no exposition.
#[derive(Debug)]
pub enum ScrapeError {
    /// Another scrape holds the single-flight gate.
    Busy,
    /// The scrape exceeded `--scrape.timeout-ms` and was aborted.
    Timeout(Duration),
    /// The scrape task itself panicked or was cancelled.
    TaskFailed(String),
    /// The scrape ran but its result could not be rendered.
    ///
    /// This is *not* "a collector failed": collector failures are reported inside a
    /// successful HTTP 200 exposition as `# Error collecting metrics from '<name>'`
    /// comments, together with the still-honest `mariadb_up` and `mariadb_exporter_*`
    /// families.
    Collect(anyhow::Error),
}

impl fmt::Display for ScrapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("another /metrics scrape is already running"),
            Self::Timeout(duration) => write!(f, "scrape exceeded timeout of {duration:?}"),
            Self::TaskFailed(error) => write!(f, "scrape task failed: {error}"),
            Self::Collect(error) => write!(f, "{error}"),
        }
    }
}

impl Error for ScrapeError {}

impl From<anyhow::Error> for ScrapeError {
    fn from(error: anyhow::Error) -> Self {
        Self::Collect(error)
    }
}

/// A spawned task that is **aborted** when this handle is dropped, rather than detached.
///
/// `tokio::spawn` hands back a `JoinHandle` whose `Drop` detaches: the task keeps running
/// unsupervised, with no deadline of its own and no way to reach it again. Dropping the
/// handle on a scrape timeout is what wedged `/metrics` permanently in the sibling exporter
/// (`nbari/pg_exporter#34`).
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Future for AbortOnDrop<T> {
    type Output = Result<T, tokio::task::JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().0).poll(cx)
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

/// Runs one collector future, converting a panic into an ordinary collector error.
///
/// Without the boundary a panicking collector unwinds the whole scrape task, which the gate
/// reports as `ScrapeError::TaskFailed` — so one bad collector answers `/metrics` with 503
/// and takes every healthy collector's metrics with it. Worse, the panic drops every
/// in-flight `ScrapeTimer` unobserved, so the incident is recorded as a scrape *abort*
/// rather than as the collector error it is.
///
/// Catching it here keeps the documented contract: a collector failure is an HTTP 200
/// exposition carrying `# Error collecting metrics from '<name>'`, honest `mariadb_up`, and
/// fresh `mariadb_exporter_*` self-observation metrics.
async fn collect_with_outcome<F>(
    name: &'static str,
    timer: Option<ScrapeTimer>,
    future: F,
    span: Span,
) -> (&'static str, anyhow::Result<()>)
where
    F: Future<Output = anyhow::Result<()>>,
{
    debug!("collector '{}' start", name);

    let result = match AssertUnwindSafe(future.instrument(span))
        .catch_unwind()
        .await
    {
        Ok(result) => result,
        Err(payload) => Err(anyhow::anyhow!(
            "collector panicked: {}",
            panic_payload_message(payload.as_ref())
        )),
    };

    match &result {
        Ok(()) => {
            debug!("collector '{}' done: ok", name);
            if let Some(timer) = timer {
                timer.success();
            }
        }
        Err(error) => {
            error!("collector '{}' done: error: {}", name, error);
            if let Some(timer) = timer {
                timer.error();
            }
        }
    }

    (name, result)
}

/// Runs one scrape behind the single-flight gate, bounded by `scrape_timeout`.
///
/// # Why the permit is held here and not inside the task
///
/// The permit is owned by **this** future, so it is released the moment this future returns
/// *or is dropped* — on success, on timeout, on a collector panic, and when the HTTP client
/// disconnects mid-scrape. Nothing about the gate depends on the spawned task making
/// progress.
///
/// The shape this deliberately avoids is `nbari/pg_exporter#34`: moving the permit *into*
/// the spawned task and dropping the `JoinHandle` on timeout, which detaches rather than
/// aborts. Releasing the gate then depends entirely on the detached task unwinding on its
/// own, and nothing guarantees that it ever does — the inner collector loop drives a
/// `FuturesUnordered` with no deadline of its own, so one future that never resolves holds
/// the only permit of a `Semaphore::new(1)` for the rest of the process lifetime and every
/// later scrape fails with [`ScrapeError::Busy`]. Observed in production as 66+ minutes of
/// continuous 503 cleared only by a manual restart.
///
/// Aborting is not sufficient on its own either: a collector that blocks inside synchronous
/// code contains no await point, and a Tokio task can only be cancelled at an await point,
/// so an abort would not land until it yields. (That is the other half of why every OS read
/// now goes through [`crate::collectors::blocking::offload_coalesced`] — it gives the scrape
/// real await points.) The gate therefore must not, and does not, depend on the task's
/// cancellation at all.
///
/// Aborting is still the right thing to do alongside it: dropping the in-flight `sqlx`
/// futures releases the pooled connections the timed-out scrape had checked out, instead of
/// leaving them parked server-side waiting for a client that will never speak.
async fn run_gated_scrape<F>(
    gate: &Arc<Semaphore>,
    scrape_timeout: Duration,
    scraper: Option<&ScraperCollector>,
    scrape: F,
) -> Result<String, ScrapeError>
where
    F: Future<Output = Result<String, ScrapeError>> + Send + 'static,
{
    // The permit stays owned by *this* future — the request — and is released when this
    // function returns or is dropped (client disconnect). It is deliberately NOT moved into
    // the spawned task: on timeout the task's handle is dropped, and a dropped `JoinHandle`
    // detaches rather than aborts, so a permit living inside the task would never come back
    // and the gate would wedge at 503 forever (`nbari/pg_exporter#34`). `_permit` (not `_`)
    // matters: `let _ = ...` would drop the permit immediately and disable the gate.
    let _permit = Arc::clone(gate)
        .try_acquire_owned()
        .map_err(|_| ScrapeError::Busy)?;

    let scrape = scrape.instrument(Span::current());

    let outcome = timeout(scrape_timeout, AbortOnDrop(tokio::spawn(scrape))).await;

    match outcome {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(ScrapeError::TaskFailed(error.to_string())),
        Err(_) => {
            // Per-collector abort counters only move for collectors that had already
            // started, so a scrape that times out during the connectivity check would
            // otherwise leave no trace at all.
            if let Some(scraper) = scraper {
                scraper.record_scrape_aborted();
            }
            warn!(
                timeout = ?scrape_timeout,
                "scrape exceeded --scrape.timeout-ms; aborted it and released the scrape gate so \
                 the next scrape can run"
            );
            Err(ScrapeError::Timeout(scrape_timeout))
        }
    }
}

/// Builds one collector by name, passing configuration to those that take it.
fn build_collector(
    name: &str,
    config: &CollectorConfig,
    factories: &HashMap<&'static str, fn() -> CollectorType>,
) -> Option<CollectorType> {
    match name {
        "system" => Some(CollectorType::SystemCollector(
            SystemCollector::with_config(config.system.process_memory),
        )),
        _ => factories.get(name).map(|factory| factory()),
    }
}

#[derive(Clone)]
pub struct CollectorRegistry {
    collectors: Vec<CollectorType>,
    registry: Arc<Registry>,
    mariadb_up_gauge: Gauge,
    scraper: Option<Arc<ScraperCollector>>,
    /// Single-flight gate: one scrape at a time, so a slow server cannot be piled on by
    /// several scrapers at once. The permit is owned by the request future, never by the
    /// scrape task — see [`run_gated_scrape`].
    scrape_gate: Arc<Semaphore>,
    scrape_timeout: Duration,
}

impl CollectorRegistry {
    /// Creates a new `CollectorRegistry`
    ///
    /// # Panics
    ///
    /// Panics if core metrics fail to register (should never happen)
    #[allow(clippy::expect_used)]
    pub fn new(config: &CollectorConfig) -> Self {
        let registry = Arc::new(Registry::new());

        // Register mariadb_up gauge
        let mariadb_up_gauge = Gauge::new("mariadb_up", "Whether MariaDB is up (1) or down (0)")
            .expect("Failed to create mariadb_up gauge");

        registry
            .register(Box::new(mariadb_up_gauge.clone()))
            .expect("Failed to register mariadb_up gauge");

        // Register mariadb_exporter_build_info gauge
        let mariadb_exporter_build_info_opts = Opts::new(
            "mariadb_exporter_build_info",
            "Build information for mariadb_exporter",
        );
        let mariadb_exporter_build_info = GaugeVec::new(
            mariadb_exporter_build_info_opts,
            &["version", "commit", "arch"],
        )
        .expect("Failed to create mariadb_exporter_build_info GaugeVec");

        // Add build information as labels
        let version = env!("CARGO_PKG_VERSION");
        let commit_sha = GIT_COMMIT_HASH.unwrap_or("unknown");
        let arch = env::consts::ARCH;

        mariadb_exporter_build_info
            .with_label_values(&[version, commit_sha, arch])
            .set(1.0); // Gauge is always set to 1.0

        registry
            .register(Box::new(mariadb_exporter_build_info))
            .expect("Failed to register mariadb_exporter_build_info GaugeVec");

        info!(
            "Registered mariadb_exporter_build_info: version={} commit={}",
            version, commit_sha
        );

        let factories = all_factories();

        // Extract scraper if exporter collector is enabled
        let mut scraper_opt = None;

        // Build all requested collectors and register their metrics.
        let collectors = config
            .enabled_collectors
            .iter()
            .filter_map(|name| {
                build_collector(name, config, &factories).inspect(|collector| {
                    // If this collector provides a scraper, extract it
                    if let Some(scraper) = collector.get_scraper() {
                        scraper_opt = Some(scraper);
                    }

                    // Register metrics per collector under a span so failures surface in traces.
                    let reg_span = debug_span!("collector.register_metrics", collector = %name);
                    let guard = reg_span.enter();
                    if let Err(e) = collector.register_metrics(&registry) {
                        warn!("Failed to register metrics for collector '{}': {}", name, e);
                    }
                    drop(guard);
                })
            })
            .collect();

        Self {
            collectors,
            registry,
            mariadb_up_gauge,
            scraper: scraper_opt,
            scrape_gate: Arc::new(Semaphore::new(1)),
            scrape_timeout: config.scrape_timeout,
        }
    }

    /// Collect from all enabled collectors, behind the single-flight scrape gate.
    ///
    /// # Errors
    ///
    /// Returns [`ScrapeError::Busy`] when another scrape holds the gate,
    /// [`ScrapeError::Timeout`] when this one exceeded `--scrape.timeout-ms`, and
    /// [`ScrapeError::Collect`] when the exposition could not be rendered. A *collector*
    /// failure is not an error here: it is reported inside a successful exposition.
    #[instrument(skip(self, pool), level = "info", fields(otel.kind = "internal"))]
    pub async fn collect_all(&self, pool: &sqlx::MySqlPool) -> Result<String, ScrapeError> {
        let registry = self.clone();
        let pool = pool.clone();

        run_gated_scrape(
            &self.scrape_gate,
            self.scrape_timeout,
            self.scraper.as_deref(),
            async move { registry.collect_all_inner(&pool).await.map_err(Into::into) },
        )
        .await
    }

    /// Collect from all enabled collectors.
    ///
    /// # Errors
    ///
    /// Returns an error if metric collection or encoding fails
    #[instrument(skip(self, pool), level = "info", err, fields(otel.kind = "internal"))]
    async fn collect_all_inner(&self, pool: &sqlx::MySqlPool) -> anyhow::Result<String> {
        // Increment scrape counter if scraper is available
        if let Some(ref scraper) = self.scraper {
            scraper.increment_scrapes();
        }

        // Quick connectivity check (does not guarantee every collector will succeed).
        let connect_span = info_span!(
            "db.connectivity_check",
            otel.kind = "client",
            db.system = "mysql",
            db.operation = "SELECT",
            db.statement = "SELECT 1"
        );

        let db_up = match sqlx::query("SELECT 1")
            .fetch_one(pool)
            .instrument(connect_span)
            .await
        {
            Ok(_) => {
                self.mariadb_up_gauge.set(1.0);

                // Initialize version if not already set (e.g. failed at startup)
                if crate::collectors::util::get_mariadb_version() == 0 {
                    let version_span = info_span!("db.version_init", otel.kind = "client");
                    if let Ok(version_string) = sqlx::query_scalar::<_, String>("SELECT VERSION()")
                        .fetch_one(pool)
                        .instrument(version_span)
                        .await
                    {
                        let version_num =
                            crate::collectors::util::parse_mariadb_version(&version_string);
                        crate::collectors::util::set_mariadb_version(version_num);
                        info!(
                            version = version_num,
                            "MariaDB version detected during collection"
                        );
                    }
                }
                true
            }

            Err(e) => {
                error!("Failed to connect to MariaDB: {}", e);
                self.mariadb_up_gauge.set(0.0);
                false
            }
        };

        // If DB is down, skip collectors except exporter self-monitoring
        let mut tasks = FuturesUnordered::new();

        for collector in &self.collectors {
            let name = collector.name();

            // Skip DB-dependent collectors if DB is down
            if !db_up && name != "exporter" {
                debug!("Skipping collector '{}' because database is down", name);
                continue;
            }

            // Create a span per collector execution to visualize overlap in traces.
            let span = info_span!("collector.collect", collector = %name, otel.kind = "internal");

            // Start timing this collector if scraper is available
            let timer = self.scraper.as_ref().map(|s| s.start_scrape(name));

            // Defer even construction of the collector future until it is inside the panic
            // boundary. Trait implementations normally just box an async block, but a panic
            // before returning that box must still be an error, not an unobserved timer drop.
            let collector_pool = pool.clone();
            let fut = async move { collector.collect(&collector_pool).await };

            // Push an instrumented future that logs start/finish.
            tasks.push(collect_with_outcome(name, timer, fut, span));
        }

        // Drain *every* launched task before deciding what to expose, so a failure in one
        // collector cannot leave siblings half-finished or their timers unrecorded.
        let mut failures: Vec<(&'static str, String)> = Vec::new();
        while let Some((name, res)) = tasks.next().await {
            match res {
                Ok(()) => {
                    debug!("Collected metrics from '{}'", name);
                }

                Err(e) => {
                    error!("Collector '{}' failed: {}", name, e);
                    failures.push((name, e.to_string()));
                }
            }
        }

        self.render_exposition(db_up, &failures)
    }

    /// Renders the current registry into the Prometheus exposition format for one scrape.
    ///
    /// Split out of [`Self::collect_all`] so the withholding rules can be exercised
    /// directly, without having to provoke a real collector failure against a live server.
    ///
    /// # Errors
    ///
    /// Returns an error if encoding the metric families fails.
    fn render_exposition(
        &self,
        db_up: bool,
        failures: &[(&'static str, String)],
    ) -> anyhow::Result<String> {
        let encode_span = debug_span!("prometheus.encode");
        let guard = encode_span.enter();

        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();

        // Keep only what the exporter can vouch for when:
        //   * the database is unreachable — `mariadb_up 0` and nothing else, or
        //   * a collector returned `Err`, so part of the registry is a preserved older
        //     snapshot that must not be timestamped as current.
        // The filter is an allow-list of `mariadb_up` plus the exporter's own
        // `mariadb_exporter_*` self-observation, so it withholds more than the
        // database-dependent families: host metrics (`mariadb_system_*`) go too, even though
        // they were sampled from the OS and are unaffected by the database. That is
        // deliberately conservative — one scrape's worth of host metrics is a cheaper loss
        // than publishing a half-built exposition — but it is a wider cut than "database
        // families only".
        // A `Collected::Skipped` collector is *not* a failure: it is a successful scrape in
        // which only that collector's unavailable series disappeared.
        let withhold_db_families = !db_up || !failures.is_empty();

        let families_to_encode = if withhold_db_families {
            metric_families
                .into_iter()
                .filter(|mf| {
                    let name = mf.name();
                    // `mariadb_up` is exporter-owned self-observation rather than a collector
                    // snapshot, and it is honest in both modes: `0` when the connectivity
                    // check failed, `1` when it succeeded but a collector then failed. It is
                    // never fabricated to `0` for a collector error.
                    name == "mariadb_up" || name.starts_with("mariadb_exporter_")
                })
                .collect()
        } else {
            metric_families
        };

        let mut buffer = Vec::new();

        // Surface the failures as exposition comments. HTTP stays 200 (see the module docs
        // on the intentional divergence from pg_exporter's 503), and the machine-readable
        // signal remains `mariadb_exporter_collector_last_scrape_success` /
        // `mariadb_exporter_collector_scrape_errors_total`, which are still exported.
        for (name, message) in failures {
            let sanitized = message.replace(['\n', '\r'], " ");
            buffer.extend_from_slice(
                format!("# Error collecting metrics from '{name}': {sanitized}\n").as_bytes(),
            );
        }

        encoder.encode(&families_to_encode, &mut buffer)?;

        // Update metrics count for next scrape
        // Count actual time series lines (non-comment, non-empty lines)
        // This matches: curl -s 0:9306/metrics | grep -vEc '^(#|\s*$)'
        // Note: This count will be visible in the NEXT scrape (eventual consistency)
        if let Some(ref scraper) = self.scraper {
            // Prefer zero-copy UTF-8, fall back to lossy for robustness
            let output = match std::str::from_utf8(&buffer) {
                Ok(s) => std::borrow::Cow::Borrowed(s),
                Err(_) => std::borrow::Cow::Owned(String::from_utf8_lossy(&buffer).into_owned()),
            };

            let count = output
                .lines()
                // Ignore comment lines (Prometheus-spec: '#' at column 0)
                .filter(|line| !line.starts_with('#'))
                // Ignore whitespace-only lines
                .filter(|line| !line.trim().is_empty())
                .count();

            let sample_count = i64::try_from(count).unwrap_or(0);

            scraper.update_metrics_count(sample_count);
        }

        drop(guard);

        Ok(String::from_utf8(buffer)?)
    }

    #[must_use]
    pub const fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    #[must_use]
    pub fn collector_names(&self) -> Vec<&'static str> {
        self.collectors.iter().map(super::Collector::name).collect()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.collectors.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::collectors::config::CollectorConfig;
    use sqlx::mysql::MySqlPoolOptions;
    use std::time::Duration;

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_registry_new() {
        let config = CollectorConfig::new().with_enabled(&["default".to_string()]);
        let registry = CollectorRegistry::new(&config);

        assert!(!registry.is_empty());
        assert!(registry.collector_names().contains(&"default"));

        // Verify core metrics are registered
        let metrics = registry.registry().gather();
        assert!(metrics.iter().any(|m| m.name() == "mariadb_up"));
        assert!(
            metrics
                .iter()
                .any(|m| m.name() == "mariadb_exporter_build_info")
        );
    }

    #[test]
    fn test_registry_empty() {
        let config = CollectorConfig::new();
        let registry = CollectorRegistry::new(&config);

        assert!(registry.is_empty());
        assert_eq!(registry.collector_names().len(), 0);
    }

    #[tokio::test]
    async fn test_collect_all_db_down() {
        let config = CollectorConfig::new().with_enabled(&["default".to_string()]);
        let registry = CollectorRegistry::new(&config);

        // Use a pool that will definitely fail to connect
        let pool = MySqlPoolOptions::new()
            .acquire_timeout(Duration::from_millis(10))
            .connect_lazy("mysql://invalid:invalid@127.0.0.1:1/invalid")
            .unwrap();

        let result = registry.collect_all(&pool).await;

        assert!(result.is_ok());
        let output = result.unwrap();

        // Should contain mariadb_up 0
        assert!(output.contains("mariadb_up 0"));

        // DB metrics should be omitted
        assert!(!output.contains("mariadb_global_status_uptime_seconds"));
    }

    #[test]
    fn test_registry_collector_names() {
        let config =
            CollectorConfig::new().with_enabled(&["default".to_string(), "exporter".to_string()]);
        let registry = CollectorRegistry::new(&config);

        let names = registry.collector_names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"default"));
        assert!(names.contains(&"exporter"));
    }

    #[tokio::test]
    async fn test_collect_all_increments_scrapes() {
        let config = CollectorConfig::new().with_enabled(&["exporter".to_string()]);
        let registry = CollectorRegistry::new(&config);

        let pool = MySqlPoolOptions::new()
            .acquire_timeout(Duration::from_millis(100))
            .connect_lazy("mysql://root:root@127.0.0.1:3306/mysql")
            .unwrap();

        let output = registry.collect_all(&pool).await.unwrap();

        // Should contain mariadb_exporter_scrapes_total 1
        assert!(output.contains("mariadb_exporter_scrapes_total 1"));
    }

    #[tokio::test]
    async fn test_collect_all_reports_metrics_count() {
        let config = CollectorConfig::new().with_enabled(&["exporter".to_string()]);
        let registry = CollectorRegistry::new(&config);

        let pool = MySqlPoolOptions::new()
            .acquire_timeout(Duration::from_millis(100))
            .connect_lazy("mysql://root:root@127.0.0.1:3306/mysql")
            .unwrap();

        // First scrape to trigger count update for NEXT scrape
        let _ = registry.collect_all(&pool).await.unwrap();
        // Second scrape to see the count from the first one
        let output = registry.collect_all(&pool).await.unwrap();

        // Should contain mariadb_exporter_metrics_total
        assert!(output.contains("mariadb_exporter_metrics_total"));

        // Extract the value and check it's > 0
        let count = output
            .lines()
            .find(|l| l.starts_with("mariadb_exporter_metrics_total"))
            .and_then(|l| l.split_whitespace().last())
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);

        assert!(count > 0.0);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod scrape_outcome_tests {
    use super::*;
    use crate::collectors::config::CollectorConfig;

    /// Builds a registry holding the `exporter` collector and publishes one recognisable
    /// database-owned sample so the withholding rules have something to withhold.
    fn registry_with_a_published_db_sample() -> CollectorRegistry {
        let config = CollectorConfig::new().with_enabled(&["exporter".to_string()]);
        let registry = CollectorRegistry::new(&config);

        // Simulate a previous successful scrape: a database-owned family carries a value.
        let uptime = prometheus::IntGaugeVec::new(
            Opts::new("mariadb_global_status_uptime_seconds", "test uptime"),
            &[],
        )
        .unwrap();
        uptime
            .with_label_values(&crate::collectors::NO_LABELS)
            .set(7);
        registry.registry().register(Box::new(uptime)).unwrap();

        registry.mariadb_up_gauge.set(1.0);
        registry
    }

    #[test]
    fn successful_scrape_exposes_database_families() {
        let registry = registry_with_a_published_db_sample();

        let body = registry.render_exposition(true, &[]).unwrap();

        assert!(
            body.contains("mariadb_global_status_uptime_seconds 7"),
            "a scrape with no failures must expose database metrics, got:\n{body}"
        );
        assert!(body.contains("mariadb_up 1"));
        assert!(
            !body.contains("# Error collecting metrics"),
            "no failures means no error comments"
        );
    }

    /// A `Collected::Skipped` collector never reaches the failure list, so its siblings stay
    /// visible; only the series it cleared are gone. This pins that a skip is a *successful*
    /// scrape rather than an error.
    #[test]
    fn a_skip_is_not_a_failure_and_does_not_withhold_siblings() {
        let registry = registry_with_a_published_db_sample();

        // `render_exposition` is reached with an empty failure list for a skipped collector.
        let body = registry.render_exposition(true, &[]).unwrap();

        assert!(body.contains("mariadb_global_status_uptime_seconds 7"));
    }

    #[test]
    fn collector_failure_keeps_http_200_shape_with_mariadb_up_1_and_no_db_samples() {
        let registry = registry_with_a_published_db_sample();

        let body = registry
            .render_exposition(
                true,
                &[("statements", "connection reset by peer".to_string())],
            )
            .unwrap();

        // Connectivity succeeded, so `mariadb_up` must stay 1 — never fabricated to 0.
        assert!(
            body.contains("mariadb_up 1"),
            "collector failure must not fabricate mariadb_up 0, got:\n{body}"
        );
        // The preserved older snapshot must not be timestamped as current.
        assert!(
            !body.contains("mariadb_global_status_uptime_seconds 7"),
            "a preserved snapshot must not be exposed after a collector error, got:\n{body}"
        );
        // The failure is visible to a human...
        assert!(
            body.contains("# Error collecting metrics from 'statements': connection reset by peer"),
            "the failure must be surfaced as an exposition comment, got:\n{body}"
        );
        // ...and the exporter self-observation metrics stay available for alerting.
        assert!(body.contains("mariadb_exporter_build_info"));
        assert!(body.contains("mariadb_exporter_scrapes_total"));
    }

    #[test]
    fn error_comments_never_break_the_exposition_format() {
        let registry = registry_with_a_published_db_sample();

        let body = registry
            .render_exposition(
                true,
                &[("schema", "line one\nline two\rline three".to_string())],
            )
            .unwrap();

        let comment_lines: Vec<&str> = body
            .lines()
            .filter(|l| l.starts_with("# Error collecting metrics"))
            .collect();
        assert_eq!(
            comment_lines.len(),
            1,
            "a multi-line error must stay on a single comment line, got:\n{body}"
        );
        assert!(
            comment_lines
                .first()
                .is_some_and(|line| line.contains("line one line two line three")),
            "the sanitized message must survive, got:\n{body}"
        );
    }

    #[test]
    fn database_down_withholds_database_families_and_reports_mariadb_up_0() {
        let registry = registry_with_a_published_db_sample();
        registry.mariadb_up_gauge.set(0.0);

        let body = registry.render_exposition(false, &[]).unwrap();

        assert!(body.contains("mariadb_up 0"));
        assert!(!body.contains("mariadb_global_status_uptime_seconds"));
        assert!(body.contains("mariadb_exporter_build_info"));
    }
}

#[cfg(test)]
// `clippy::panic` is allowed here for one test only: proving that a panicking scrape
// still releases the gate requires a scrape that actually panics.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod gate_tests {
    //! Regression tests for the single-flight scrape gate.
    //!
    //! The bug these lock down (`nbari/pg_exporter#34`) was not "the gate is too
    //! strict", it was "the gate never reopens": the permit had been moved *into*
    //! the spawned scrape task, and a timeout drops the `JoinHandle`, which
    //! **detaches** the task rather than cancelling it. The detached task kept the
    //! permit for as long as it kept running, so `/metrics` answered 503 forever.
    //!
    //! Every test below therefore checks the same property from a different exit
    //! path: after the request future is done, the next scrape can start.

    use super::{ScrapeError, run_gated_scrape};
    use std::{
        future::pending,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use tokio::sync::Semaphore;

    fn gate() -> Arc<Semaphore> {
        Arc::new(Semaphore::new(1))
    }

    /// A panicking collector must stay a *collector* failure.
    ///
    /// Without the `catch_unwind` boundary the panic unwinds the whole spawned scrape
    /// task: the gate reports `TaskFailed` (HTTP 503, every healthy collector's metrics
    /// withheld) and each in-flight `ScrapeTimer` is dropped unobserved, so the incident is
    /// filed as a scrape *abort* instead of naming the collector that broke.
    #[tokio::test]
    #[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    async fn a_panicking_collector_is_recorded_as_an_error_not_an_abort() {
        use super::{ScraperCollector, Span, collect_with_outcome};
        use prometheus::Registry;

        let scraper = ScraperCollector::new();
        let prometheus_registry = Registry::new();
        scraper.register(&prometheus_registry).unwrap();
        let timer = scraper.start_scrape("panicking_collector");

        let (name, result) = collect_with_outcome(
            "panicking_collector",
            Some(timer),
            async { panic!("collector exploded") },
            Span::none(),
        )
        .await;

        assert_eq!(name, "panicking_collector");
        let error = result.expect_err("a collector panic must become an error");
        assert!(
            error.to_string().contains("collector exploded"),
            "the panic message must survive so the culprit is nameable: {error}"
        );

        let metrics = prometheus_registry.gather();
        let errors = metrics
            .iter()
            .find(|metric| metric.name() == "mariadb_exporter_collector_scrape_errors_total")
            .expect("a caught panic must be published as a collector scrape error");
        let error_value = errors
            .get_metric()
            .first()
            .expect("scrape error metric has no sample")
            .get_counter()
            .value();
        assert!((error_value - 1.0).abs() < f64::EPSILON);

        assert!(
            metrics
                .iter()
                .find(|metric| metric.name() == "mariadb_exporter_collector_scrape_aborted_total")
                .is_none_or(|metric| metric.get_metric().is_empty()),
            "a caught collector panic must not be classified as a scrape abort"
        );
    }

    #[tokio::test]
    async fn a_successful_scrape_releases_the_gate() {
        let gate = gate();

        for _ in 0..3 {
            let body = run_gated_scrape(&gate, Duration::from_secs(5), None, async {
                Ok("mariadb_up 1\n".to_string())
            })
            .await
            .expect("gate should be free");

            assert_eq!(body, "mariadb_up 1\n");
        }

        assert_eq!(gate.available_permits(), 1);
    }

    #[tokio::test]
    async fn a_failing_scrape_releases_the_gate() {
        let gate = gate();

        let error = run_gated_scrape(&gate, Duration::from_secs(5), None, async {
            Err(ScrapeError::Collect(anyhow::anyhow!("encode failed")))
        })
        .await
        .expect_err("the scrape reported a failure");

        assert!(matches!(error, ScrapeError::Collect(_)));
        assert_eq!(gate.available_permits(), 1);
    }

    #[tokio::test]
    async fn a_panicking_scrape_releases_the_gate_and_is_reported() {
        let gate = gate();

        let error = run_gated_scrape(&gate, Duration::from_secs(5), None, async {
            panic!("collector exploded");
        })
        .await
        .expect_err("a panic must not be reported as a successful scrape");

        assert!(matches!(error, ScrapeError::TaskFailed(_)));
        assert_eq!(
            gate.available_permits(),
            1,
            "a panicking scrape must not wedge the gate"
        );
    }

    #[tokio::test]
    async fn a_concurrent_scrape_is_refused_instead_of_queued() {
        let gate = gate();
        let release = Arc::new(tokio::sync::Notify::new());

        let held = {
            let gate = Arc::clone(&gate);
            let release = Arc::clone(&release);
            tokio::spawn(async move {
                run_gated_scrape(&gate, Duration::from_secs(30), None, async move {
                    release.notified().await;
                    Ok("first\n".to_string())
                })
                .await
            })
        };

        while gate.available_permits() == 1 {
            tokio::task::yield_now().await;
        }

        let refused = run_gated_scrape(&gate, Duration::from_secs(30), None, async {
            Ok("second\n".to_string())
        })
        .await;

        assert!(
            matches!(refused, Err(ScrapeError::Busy)),
            "a second scrape must be refused, not piled on top of a slow server"
        );

        release.notify_one();
        assert_eq!(held.await.unwrap().unwrap(), "first\n");
        assert_eq!(gate.available_permits(), 1);
    }

    // `flavor = "current_thread"` is load-bearing (it is the default — this pins it):
    // the permit assertion runs synchronously after the request future ends, with no
    // yield between the abort and the check. On a multi-thread runtime the assertion
    // would race the aborted task's teardown and silently lose its power over the
    // permit-in-task regression this test exists to catch.
    #[tokio::test(flavor = "current_thread")]
    async fn the_gate_reopens_after_a_scrape_that_never_finishes() {
        let gate = gate();
        let timeout = Duration::from_millis(50);

        let error = run_gated_scrape(
            &gate,
            timeout,
            None,
            pending::<Result<String, ScrapeError>>(),
        )
        .await
        .expect_err("a scrape that never completes must time out");

        assert!(matches!(error, ScrapeError::Timeout(t) if t == timeout));
        assert_eq!(
            gate.available_permits(),
            1,
            "the permit must be released by the request future, not by the scrape task"
        );

        let body = run_gated_scrape(&gate, Duration::from_secs(5), None, async {
            Ok("recovered\n".to_string())
        })
        .await
        .expect("the gate must reopen after a timeout");

        assert_eq!(body, "recovered\n");
    }

    #[tokio::test]
    async fn a_timed_out_scrape_is_aborted_not_detached() {
        let gate = gate();
        let steps = Arc::new(AtomicUsize::new(0));

        let counter = Arc::clone(&steps);
        let error = run_gated_scrape(&gate, Duration::from_millis(50), None, async move {
            loop {
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect_err("the scrape must time out");

        assert!(matches!(error, ScrapeError::Timeout(_)));

        let observed = steps.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert_eq!(
            steps.load(Ordering::SeqCst),
            observed,
            "an abandoned scrape must be aborted so its queries are dropped and their \
             pooled connections returned; detaching it leaks work and connections"
        );
    }

    // Same `current_thread` requirement as the gate-reopen test above: the permit check
    // must observe the state the dropped request future left behind, not whatever a
    // multi-thread runtime happens to have scheduled since.
    #[tokio::test(flavor = "current_thread")]
    async fn dropping_the_request_releases_the_gate() {
        let gate = gate();

        {
            let scrape = run_gated_scrape(
                &gate,
                Duration::from_secs(30),
                None,
                pending::<Result<String, ScrapeError>>(),
            );
            tokio::pin!(scrape);

            let poll = tokio::time::timeout(Duration::from_millis(50), &mut scrape).await;
            assert!(poll.is_err(), "the scrape should still be in flight");
        }

        assert_eq!(
            gate.available_permits(),
            1,
            "a client that disconnects mid-scrape must not wedge the gate"
        );
    }

    /// Recorded `(span name, parent span name)` pairs, in creation order.
    type SpanRecords = Arc<std::sync::Mutex<Vec<(String, Option<String>)>>>;

    #[derive(Clone, Default)]
    struct SpanLog(SpanRecords);

    impl<S> tracing_subscriber::Layer<S> for SpanLog
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        fn on_new_span(
            &self,
            _attrs: &tracing::span::Attributes<'_>,
            id: &tracing::span::Id,
            ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if let Some(span) = ctx.span(id) {
                let parent = span.parent().map(|p| p.name().to_string());
                self.0
                    .lock()
                    .expect("span log lock")
                    .push((span.name().to_string(), parent));
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_spawned_scrape_inherits_the_request_span() {
        use tracing_futures::Instrument as _;
        use tracing_subscriber::prelude::*;

        let log = SpanLog::default();
        let subscriber = tracing_subscriber::registry().with(log.clone());
        let _guard = tracing::subscriber::set_default(subscriber);

        let gate = gate();
        let request = tracing::info_span!("http.server.request", http_route = "/metrics");

        async {
            run_gated_scrape(&gate, Duration::from_secs(5), None, async {
                tracing::info_span!("probe.scrape_work").in_scope(|| ());
                Ok::<_, ScrapeError>("ok\n".to_string())
            })
            .await
            .expect("scrape should succeed");
        }
        .instrument(request)
        .await;

        let spans = log.0.lock().expect("span log lock");
        let work = spans
            .iter()
            .find(|(name, _)| name == "probe.scrape_work")
            .expect("the span created inside the scrape was recorded");
        assert_eq!(
            work.1.as_deref(),
            Some("http.server.request"),
            "the scrape's spans must parent to the request span, not float as roots: {spans:?}"
        );
    }
}
