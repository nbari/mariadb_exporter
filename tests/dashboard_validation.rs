#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
#![allow(clippy::panic)]

use regex::Regex;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn dashboard_metrics_are_exported() -> anyhow::Result<()> {
    let exported = collect_exported_metrics("src/collectors")?;
    assert!(
        !exported.is_empty(),
        "should discover exported metrics from collectors"
    );

    let dashboard = collect_dashboard_metrics("grafana/dashboard.json")?;
    assert!(
        !dashboard.is_empty(),
        "dashboard should reference at least one metric"
    );

    for metric in &dashboard {
        if exported.contains(metric) {
            continue;
        }

        // Accept histogram suffixes as long as the base metric exists.
        let is_hist_suffix = ["_bucket", "_sum", "_count"]
            .iter()
            .any(|s| metric.ends_with(s));
        if is_hist_suffix {
            let base = metric
                .trim_end_matches("_bucket")
                .trim_end_matches("_sum")
                .trim_end_matches("_count");
            if exported.contains(base) {
                continue;
            }
        }

        panic!("Dashboard metric '{metric}' is not exported by collectors");
    }

    Ok(())
}

#[test]
fn replication_lag_panel_uses_current_gauge_value() -> anyhow::Result<()> {
    let data: Value = serde_json::from_str(&fs::read_to_string("grafana/dashboard.json")?)?;
    let panel = find_panel_by_title(&data, "Replication Lag (Seconds Behind Master)")
        .ok_or_else(|| anyhow::anyhow!("replication lag panel not found"))?;
    let expr = panel
        .get("targets")
        .and_then(Value::as_array)
        .and_then(|targets| targets.first())
        .and_then(|target| target.get("expr"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("replication lag panel expr not found"))?;

    assert!(
        expr.contains("mariadb_replica_seconds_behind_master_seconds"),
        "replication lag panel should use replica lag metric, got {expr}"
    );
    assert!(
        expr.contains("mariadb_replica_configured"),
        "replication lag panel should filter to configured replicas, got {expr}"
    );
    assert!(
        !expr.contains("max_over_time("),
        "replication lag panel should show current lag, not peak lag over range: {expr}"
    );

    Ok(())
}

fn collect_exported_metrics(dir: &str) -> anyhow::Result<BTreeSet<String>> {
    let mut metrics = BTreeSet::new();
    let pattern = Regex::new(r"mariadb_[a-z0-9_]+").expect("valid regex");
    for path in walk_files(Path::new(dir))? {
        let content = fs::read_to_string(&path)?;
        for mat in pattern.find_iter(&content) {
            metrics.insert(mat.as_str().trim_matches('"').to_string());
        }
    }
    Ok(metrics)
}

fn collect_dashboard_metrics(path: &str) -> anyhow::Result<BTreeSet<String>> {
    let data: Value = serde_json::from_str(&fs::read_to_string(path)?)?;
    let mut exprs = Vec::new();
    if let Some(panels) = data.get("panels").and_then(|p| p.as_array()) {
        for panel in panels {
            collect_exprs(panel, &mut exprs);
        }
    }

    let pattern = Regex::new(r"mariadb_[a-z0-9_]+").expect("valid regex");
    let mut metrics = BTreeSet::new();
    for expr in exprs {
        for mat in pattern.find_iter(&expr) {
            metrics.insert(mat.as_str().to_string());
        }
    }
    Ok(metrics)
}

fn collect_exprs(panel: &Value, exprs: &mut Vec<String>) {
    if let Some(targets) = panel.get("targets").and_then(|t| t.as_array()) {
        for target in targets {
            if let Some(expr) = target.get("expr").and_then(|e| e.as_str()) {
                exprs.push(expr.to_string());
            }
        }
    }

    if let Some(children) = panel.get("panels").and_then(|p| p.as_array()) {
        for child in children {
            collect_exprs(child, exprs);
        }
    }
}

fn find_panel_by_title<'a>(panel: &'a Value, title: &str) -> Option<&'a Value> {
    if panel.get("title").and_then(Value::as_str) == Some(title) {
        return Some(panel);
    }

    panel
        .get("panels")
        .and_then(Value::as_array)
        .and_then(|children| {
            children
                .iter()
                .find_map(|child| find_panel_by_title(child, title))
        })
}

fn walk_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];

    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Some(ext) = path.extension()
                && ext == "rs"
            {
                files.push(path);
            }
        }
    }

    Ok(files)
}
