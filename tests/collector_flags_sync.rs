#![allow(clippy::panic)]
//! Guards the hand-maintained `--collector.*` lists against drift.
//!
//! Several places in the repository spell out every collector flag by hand: the
//! `just watch` development recipe, the `just dev` alias if one exists, the
//! dashboard validation script, and the soak harness. Adding a collector to
//! `register_collectors!` does *not* update them, so they silently stop
//! exercising the new collector — exactly how `system` was initially missed.
//!
//! These tests fail loudly instead.

use mariadb_exporter::collectors::{Collector, all_factories};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

/// Repository root, derived from the manifest directory so the tests work no
/// matter where `cargo test` is invoked from.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &str) -> String {
    let full: PathBuf = repo_root().join(path);
    fs::read_to_string(&full).unwrap_or_else(|e| panic!("failed to read {}: {e}", full.display()))
}

/// Every collector registered through `register_collectors!`.
fn registered_collectors() -> BTreeSet<String> {
    all_factories().keys().map(|k| (*k).to_string()).collect()
}

/// Collectors that are already on without an explicit flag, so a script need
/// not name them to exercise them.
fn default_enabled_collectors() -> BTreeSet<String> {
    all_factories()
        .iter()
        .filter(|(_, factory)| factory().enabled_by_default())
        .map(|(name, _)| (*name).to_string())
        .collect()
}

/// Extracts the `--collector.<name>` flags mentioned anywhere in `haystack`.
fn referenced_collectors(haystack: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (index, _) in haystack.match_indices("--collector.") {
        let Some(rest) = haystack.get(index + "--collector.".len()..) else {
            continue;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            found.insert(name);
        }
    }
    found
}

/// Asserts that `path` exercises every registered collector: each one is either
/// named explicitly with `--collector.<name>` or already on by default.
fn assert_exercises_every_collector(path: &str, snippet: &str) {
    let referenced = referenced_collectors(snippet);
    let registered = registered_collectors();
    let active: BTreeSet<String> = referenced
        .union(&default_enabled_collectors())
        .cloned()
        .collect();

    let missing: Vec<&String> = registered.difference(&active).collect();
    assert!(
        missing.is_empty(),
        "{path} does not enable {missing:?}. Every collector registered in \
         `register_collectors!` must be listed there, otherwise it is never exercised. \
         Registered: {registered:?}"
    );

    let unknown: Vec<&String> = referenced.difference(&registered).collect();
    assert!(
        unknown.is_empty(),
        "{path} references unknown collector(s) {unknown:?}. Registered: {registered:?}"
    );
}

/// Returns the body of a `just` recipe, i.e. the recipe line plus every
/// following indented line.
fn just_recipe(justfile: &str, recipe: &str) -> String {
    let header = format!("\n{recipe}:");
    let Some(start) = justfile.find(&header) else {
        panic!("recipe `{recipe}` not found in .justfile");
    };
    let body = justfile.get(start + 1..).unwrap_or_default();

    let mut collected = String::new();
    for (index, line) in body.lines().enumerate() {
        if index > 0 && !line.starts_with([' ', '\t']) {
            break;
        }
        collected.push_str(line);
        collected.push('\n');
    }
    collected
}

#[test]
fn just_watch_enables_every_registered_collector() {
    let justfile = read(".justfile");
    let recipe = just_recipe(&justfile, "watch");
    assert_exercises_every_collector("the `watch` recipe in .justfile", &recipe);
}

#[test]
fn dashboard_validation_script_enables_every_registered_collector() {
    let script = read("scripts/validate-dashboard.sh");
    assert_exercises_every_collector("scripts/validate-dashboard.sh", &script);
}

#[test]
fn soak_harness_enables_every_registered_collector() {
    let script = read("scripts/benchmark/run-soak.sh");
    assert_exercises_every_collector("scripts/benchmark/run-soak.sh", &script);
}

#[test]
fn readme_documents_every_registered_collector() {
    let readme = read("README.md");
    let registered = registered_collectors();

    for collector in &registered {
        let flag = format!("`--collector.{collector}`");
        assert!(
            readme.contains(&flag),
            "README.md does not document {flag}; every collector needs a documented flag"
        );
    }
}

#[test]
fn referenced_collectors_parses_flags() {
    let sample = "run -- --collector.default --collector.system --collector.query_response_time";
    let found = referenced_collectors(sample);
    assert!(found.contains("default"));
    assert!(found.contains("system"));
    assert!(found.contains("query_response_time"));
    assert_eq!(found.len(), 3);

    // `--no-collector.x` disables a collector, so it must never be read as
    // enabling one.
    let disabled = referenced_collectors("run -- --no-collector.system");
    assert!(disabled.is_empty(), "got {disabled:?}");
}
