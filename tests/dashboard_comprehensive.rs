#![allow(clippy::panic, clippy::map_unwrap_or)]
use serde_json::Value;
use std::collections::HashSet;
use std::fs;

fn load_dashboard() -> Value {
    let dashboard_content = fs::read_to_string("grafana/dashboard.json")
        .unwrap_or_else(|e| panic!("Failed to read dashboard.json: {e}"));

    serde_json::from_str(&dashboard_content)
        .unwrap_or_else(|e| panic!("Failed to parse dashboard.json: {e}"))
}

#[test]
fn dashboard_uses_comprehensive_metrics() {
    let dashboard = load_dashboard();

    // Extract all metrics used in dashboard
    let mut dashboard_metrics = HashSet::new();
    if let Some(panels) = dashboard.get("panels").and_then(Value::as_array) {
        extract_metrics_from_panels(panels, &mut dashboard_metrics);
    }

    println!("Dashboard uses {} unique metrics", dashboard_metrics.len());

    // Core metrics that should always be present
    let required_metrics = vec![
        "mariadb_version_info",
        "mariadb_exporter_system_memory_total_bytes", // version collector (default)
        // Exporter self-monitoring (from pg_exporter 9-panel structure)
        "mariadb_exporter_collector_scrape_duration_seconds",
        "mariadb_exporter_collector_last_scrape_success",
        "mariadb_exporter_collector_last_scrape_timestamp_seconds",
        "mariadb_exporter_process_cpu_percent",
        "mariadb_exporter_process_cpu_cores",
        "mariadb_exporter_process_resident_memory_bytes",
        "mariadb_exporter_process_virtual_memory_bytes",
        "mariadb_exporter_metrics_total",
        "mariadb_exporter_process_open_fds",
        "mariadb_exporter_process_start_time_seconds",
        // Connection & thread metrics (default collector)
        "mariadb_global_status_threads_connected",
        "mariadb_global_status_threads_running",
        "mariadb_global_status_max_used_connections",
        "mariadb_global_status_aborted_connects",
        "mariadb_global_status_connections",
        // Query metrics (default collector)
        "mariadb_global_status_queries_total",
        "mariadb_global_status_questions_total",
        "mariadb_global_status_slow_queries",
        // InnoDB metrics (default collector)
        "mariadb_innodb_buffer_pool_read_requests",
        "mariadb_innodb_buffer_pool_reads",
        "mariadb_innodb_buffer_pool_pages_data",
        "mariadb_innodb_buffer_pool_pages_free",
        "mariadb_innodb_buffer_pool_pages_dirty",
        "mariadb_innodb_buffer_pool_size_bytes",
    ];

    for metric in &required_metrics {
        assert!(
            dashboard_metrics.contains(*metric),
            "Dashboard should use metric: {metric}"
        );
    }

    // Dashboard should have multiple rows
    if let Some(panels) = dashboard.get("panels").and_then(Value::as_array) {
        let row_count = panels.iter().filter(|p| p["type"] == "row").count();
        assert!(
            row_count >= 3,
            "Dashboard should have at least 3 rows (Instance Status, Connection/Performance, Exporter Self-Monitoring), found {row_count}"
        );
    }
}

fn extract_metrics_from_panels(panels: &[Value], metrics: &mut HashSet<String>) {
    for panel in panels {
        // Check nested panels (row panels)
        if let Some(nested) = panel.get("panels").and_then(Value::as_array) {
            extract_metrics_from_panels(nested, metrics);
        }

        // Extract from targets
        if let Some(targets) = panel.get("targets").and_then(Value::as_array) {
            for target in targets {
                if let Some(expr) = target.get("expr").and_then(Value::as_str) {
                    // Extract metric names from PromQL expressions
                    extract_metric_names(expr, metrics);
                }
            }
        }
    }
}

fn extract_metric_names(expr: &str, metrics: &mut HashSet<String>) {
    // Simple regex-like extraction of mariadb_* metrics
    let words: Vec<&str> = expr
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .collect();
    for word in words {
        if word.starts_with("mariadb_") {
            // For histogram metrics, strip _sum/_count/_bucket to get base name
            // but preserve _total for counters and the full metric name
            if word.ends_with("_sum") || word.ends_with("_count") || word.ends_with("_bucket") {
                let base_metric = word
                    .trim_end_matches("_bucket")
                    .trim_end_matches("_count")
                    .trim_end_matches("_sum");
                metrics.insert(base_metric.to_string());
            }
            // Always insert the full metric name as well
            metrics.insert(word.to_string());
        }
    }
}

#[test]
fn dashboard_has_exporter_self_monitoring_row() {
    let dashboard = load_dashboard();

    let mut found_exporter_row = false;
    if let Some(panels) = dashboard.get("panels").and_then(Value::as_array) {
        for panel in panels {
            if panel["type"] == "row"
                && panel
                    .get("title")
                    .and_then(Value::as_str)
                    .is_some_and(|title| title.contains("Exporter") && title.contains("Monitoring"))
            {
                found_exporter_row = true;
                break;
            }
        }
    }

    assert!(
        found_exporter_row,
        "Dashboard should have an 'Exporter Self-Monitoring' row"
    );
}
