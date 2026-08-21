//! Golden-file guard over the exported metric metadata surface.
//!
//! A refactor must never silently rename a metric, change its Prometheus type, or
//! reword its `# HELP` text: those are part of the exporter's public wire contract
//! and downstream dashboards, recording rules and alerts depend on them.
//!
//! The fixture in `tests/fixtures/metric_metadata.tsv` records one
//! `name<TAB>type<TAB>help` row per metric family. The rules are asymmetric on
//! purpose:
//!
//! * Every family observed in a live scrape **must** be present in the fixture with
//!   an identical type and help string. This catches renames, type changes and help
//!   drift.
//! * Families in the fixture that are absent from the scrape are tolerated, because
//!   the exported surface legitimately varies with the server version, installed
//!   plugins and granted privileges — and, since the settlement contract landed, a
//!   family whose source is unavailable is *absent* rather than reported as a
//!   fabricated zero.

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]
#![allow(clippy::panic)]

use mariadb_exporter::collectors::{
    COLLECTOR_NAMES, config::CollectorConfig, registry::CollectorRegistry,
};
use std::collections::BTreeMap;

mod common;

const FIXTURE: &str = include_str!("fixtures/metric_metadata.tsv");

#[derive(Debug, PartialEq, Eq)]
struct Metadata {
    kind: String,
    help: String,
}

fn parse_fixture() -> BTreeMap<String, Metadata> {
    let mut out = BTreeMap::new();
    for (lineno, line) in FIXTURE.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(3, '\t');
        let (Some(name), Some(kind), Some(help)) = (parts.next(), parts.next(), parts.next())
        else {
            panic!("malformed fixture row at line {}: {line:?}", lineno + 1);
        };
        out.insert(
            name.to_string(),
            Metadata {
                kind: kind.to_string(),
                help: help.to_string(),
            },
        );
    }
    out
}

/// Parse `# HELP` / `# TYPE` comment lines out of a Prometheus exposition body.
fn parse_exposition(body: &str) -> BTreeMap<String, Metadata> {
    let mut helps: BTreeMap<String, String> = BTreeMap::new();
    let mut kinds: BTreeMap<String, String> = BTreeMap::new();

    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("# HELP ") {
            let (name, help) = rest.split_once(' ').unwrap_or((rest, ""));
            helps.insert(name.to_string(), help.to_string());
        } else if let Some(rest) = line.strip_prefix("# TYPE ") {
            let (name, kind) = rest.split_once(' ').unwrap_or((rest, ""));
            kinds.insert(name.to_string(), kind.to_string());
        }
    }

    kinds
        .into_iter()
        .map(|(name, kind)| {
            let help = helps.get(&name).cloned().unwrap_or_default();
            (name, Metadata { kind, help })
        })
        .collect()
}

#[tokio::test]
async fn exported_metric_metadata_matches_golden_fixture() -> anyhow::Result<()> {
    let Ok(pool) = common::create_test_pool().await else {
        eprintln!("MariaDB not reachable, skipping metric metadata golden test");
        return Ok(());
    };

    let config = CollectorConfig::new().with_enabled(
        &COLLECTOR_NAMES
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
    );
    let registry = CollectorRegistry::new(&config);

    // Two scrapes: some families (exporter self-observation) only materialise once a
    // previous scrape has been recorded.
    let _ = registry.collect_all(&pool).await?;
    let body = registry.collect_all(&pool).await?;

    let golden = parse_fixture();
    let observed = parse_exposition(&body);

    assert!(
        !observed.is_empty(),
        "scrape produced no metric families at all"
    );

    let mut unknown = Vec::new();
    let mut changed = Vec::new();

    for (name, meta) in &observed {
        match golden.get(name) {
            None => unknown.push(format!("{name}\t{}\t{}", meta.kind, meta.help)),
            Some(expected) => {
                if expected.kind != meta.kind {
                    changed.push(format!(
                        "{name}: TYPE changed {:?} -> {:?}",
                        expected.kind, meta.kind
                    ));
                }
                if expected.help != meta.help {
                    changed.push(format!(
                        "{name}: HELP changed\n     expected: {:?}\n     observed: {:?}",
                        expected.help, meta.help
                    ));
                }
            }
        }
    }

    assert!(
        changed.is_empty(),
        "metric metadata drifted from tests/fixtures/metric_metadata.tsv.\n\
         Metric names, types and help strings are part of the public wire contract; \
         changing them breaks dashboards and alerts.\n{}",
        changed.join("\n  ")
    );

    assert!(
        unknown.is_empty(),
        "scrape exposed {} metric family/families missing from \
         tests/fixtures/metric_metadata.tsv.\n\
         If these are intentionally new metrics, append these exact rows to the fixture:\n{}",
        unknown.len(),
        unknown.join("\n")
    );

    Ok(())
}

#[test]
fn fixture_is_sorted_and_free_of_duplicates() {
    let mut seen: Vec<&str> = Vec::new();
    for line in FIXTURE.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let name = line.split('\t').next().unwrap_or_default();
        seen.push(name);
    }

    let mut sorted = seen.clone();
    sorted.sort_unstable();
    assert_eq!(
        seen, sorted,
        "tests/fixtures/metric_metadata.tsv must be sorted by metric name"
    );

    let mut deduped = sorted.clone();
    deduped.dedup();
    assert_eq!(
        sorted.len(),
        deduped.len(),
        "tests/fixtures/metric_metadata.tsv contains duplicate metric names"
    );
}
