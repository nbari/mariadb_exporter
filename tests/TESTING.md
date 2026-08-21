# Testing Guide

This document describes the testing strategy for mariadb_exporter to prevent production issues.

## Testing Philosophy

All collectors MUST be tested with:
1. **Feature availability tests** - Handle missing features/plugins gracefully
2. **Edge case tests** - NULL values, empty results, privilege errors
3. **Type compatibility tests** - Ensure SQL types match Rust types
4. **Realistic workload tests** - Test with actual data and queries
5. **Settlement tests** - An unavailable source must *remove* the series it owned

## The settlement contract

> When a collector gracefully publishes nothing because its source is known to be
> unavailable, every series previously owned by that collector disappears.

`Collector::collect_once` returns one of three outcomes, and the provided
`Collector::collect` wrapper settles the registry accordingly:

| Outcome | Meaning | Settlement |
| --- | --- | --- |
| `Collected::Fresh` | A current snapshot was published. A successful **empty** result is `Fresh` when zero rows is the current truth. | Nothing is reset by the wrapper. The collector itself resets-and-repopulates so entities missing from a valid snapshot go away. |
| `Collected::Skipped` | Nothing was published because the source is known unavailable — plugin not installed, `performance_schema` table absent, feature disabled, privilege revoked. | `reset_metrics()` runs: every series the collector owns is removed. |
| `Err` | An unexpected or transient fault — lost connection, timeout, deadlock, malformed data, failed feature probe. | **Nothing is reset.** The last good snapshot is preserved so the collector can resume; the scrape withholds database families instead. |

Rules a test must be able to demonstrate:

* A collector that refreshed **part** of its surface never returns `Skipped` — that would
  erase the fresh part. Independently optional sources are separate `Collector`
  implementations so they settle independently.
* A query failure is never laundered into an absence. `unwrap_or(0)`, `vec![]`, and
  `Err(e) => debug!(…)` fallbacks are forbidden for that purpose; classify with
  `collectors::util::classify_query_error`, which keys on the MariaDB **error number**.
* A success-path `reset()` runs **after** the fallible read, immediately before publishing.
* A zero is only published when zero is a fact ("TLS not in use", "no replica configured").
  Absence is the honest state when the source could not be read.

### Where settlement tests live

| Scope | Location |
| --- | --- |
| Trait mechanics (`Fresh`/`Skipped`/`Err`, sibling isolation, zero-label wire format) | `src/collectors/mod.rs` — `settlement_contract` |
| Scrape rendering (HTTP-200 collector-error mode, DB-down mode) | `src/collectors/registry.rs` — `scrape_outcome_tests` |
| Live-server settlement (fresh snapshot clears vanished labels, error preserves snapshot, denied source clears, TLS certificate absence, `performance_schema` unavailable) | `tests/collectors/settlement.rs` |
| Feature transitions that mutate global server state | `tests/settlement_transitions.rs` |
| Replica → no-replica channel clearing | `tests/testcontainers.rs` |
| Host-metrics settlement (`system` collector) | `tests/collectors/system/` |
| Metric name/type/help wire contract | `tests/metric_metadata.rs` + `tests/fixtures/metric_metadata.tsv` |

### The metric metadata golden fixture

Because settlement makes an unreadable source *absent* rather than zero, the set of
families in a scrape now varies legitimately with server version, plugins and
privileges. What must **never** vary is a family's name, Prometheus type or `# HELP`
string — dashboards, recording rules and alerts are written against those.

`tests/fixtures/metric_metadata.tsv` pins one `name<TAB>type<TAB>help` row per family.
The check is deliberately asymmetric:

* every family observed in a live scrape **must** appear in the fixture with an
  identical type and help string — this catches renames, type changes and help drift;
* fixture rows with no matching family are tolerated, since the source may simply be
  unavailable on the server under test.

When you intentionally add a metric, run the test and paste the exact rows it prints
into the fixture, keeping it sorted by name (a second test enforces sorting and
rejects duplicates).

### Collectors that read the operating system, not `MariaDB`

The `system` collector reads `/proc`, FreeBSD sysctls and `sysinfo` instead of the
database, so its tests assert the *shape* of the published series rather than a value
that a fixture could seed. Two rules apply:

- **Guard on the platform, not on the data.** Use `cfg!(any(target_os = "linux",
  target_os = "freebsd"))` to decide what must be published. A supported platform that
  matches zero `mariadbd`/`mysqld` processes is a **`Fresh` zero**, not a skip — CI
  usually runs `MariaDB` in a container, so an empty process group is the normal case.
- **An OS read error must not become `Err`.** In `mariadb_exporter` a collector `Err`
  withholds *every* database-dependent family for that scrape, so an optional host
  collector warns and preserves instead. `tests/collectors/system/*` pin this by
  asserting `collect()` is `Ok` across repeated scrapes.

### Tests that mutate global server state

Installing/uninstalling a plugin or flipping a global variable such as `@@userstat` affects
every other test sharing that server. Such tests **must** run against their own isolated
container (see `tests/settlement_transitions.rs`) and seed everything they need. Never
depend on a lived-in database, and never leave a plugin installed or a global variable
changed behind you.

When no container runtime is reachable these tests skip with an explicit message. Set
`MARIADB_EXPORTER_REQUIRE_TESTCONTAINERS=1` (or `CI=true`) to make them hard failures
instead, so an environment-driven skip is never mistaken for a pass.

### Mutation-checking a settlement test

A settlement test is only worth having if it fails when the fix is reverted. Verify it:

```bash
# Temporarily make the safe wrapper ignore Skipped, then:
cargo test --lib settlement_contract          # must FAIL
cargo test --test settlement_transitions      # must FAIL
```

## Running Tests

### Local Testing

```bash
# Set up MariaDB connection
export MARIADB_EXPORTER_DSN="mysql://root:root@localhost:3306/mysql"

# Or use Unix socket
export MARIADB_EXPORTER_DSN="mysql:///mysql?socket=/var/run/mysqld/mysqld.sock&user=exporter"

# Run all tests
cargo test

# Run specific collector tests
cargo test --test collectors_tests default

# Run with output
cargo test -- --nocapture
```

### Using justfile

```bash
# Start MariaDB container and run all tests
just test

# Clean up containers
just stop-containers
```

For rootless Podman with `testcontainers`, export:

```bash
export DOCKER_HOST="unix:///run/user/$UID/podman/podman.sock"
```

### CI Testing

The CI pipeline automatically:
- Tests against MariaDB 11.x
- Configures required plugins and variables
- Runs all integration tests

## Writing Collector Tests

When adding a new collector, you MUST include these test categories:

### 1. Registration Test
```rust
#[tokio::test]
async fn test_collector_registers_without_error() -> Result<()> {
    let collector = MyCollector::new();
    let registry = Registry::new();
    collector.register_metrics(&registry)?;
    Ok(())
}
```

### 2. Feature Availability Test
```rust
#[tokio::test]
async fn test_collector_handles_missing_feature() -> Result<()> {
    let pool = common::create_test_pool().await?;
    let collector = MyCollector::new();
    let registry = Registry::new();
    
    collector.register_metrics(&registry)?;
    let result = collector.collect(&pool).await;
    
    // Should not panic
    assert!(result.is_ok());
    Ok(())
}
```

### 3. Edge Case Tests

Test for common edge cases that cause panics:

```rust
#[tokio::test]
async fn test_collector_handles_null_values() -> Result<()> {
    // Test queries that may return NULL
    // Empty result sets
    // Zero values
    // Missing privileges
}

#[tokio::test]
async fn test_collector_handles_type_mismatches() -> Result<()> {
    // Ensure SQL types (DECIMAL, BIGINT) match Rust types
    // Use explicit CAST in SQL if needed
}
```

### 4. Realistic Workload Test
```rust
#[tokio::test]
async fn test_collector_with_realistic_data() -> Result<()> {
    // Create test data
    // Generate realistic workload
    // Verify metrics are collected correctly
}
```

## Common Pitfalls and Solutions

### 1. Type Mismatches (CRITICAL)

**Problem:** MariaDB DECIMAL type doesn't match Rust i64/f64  
**Solution:** Always cast in SQL: `CAST(column AS SIGNED) FROM table`

### 2. NULL Values (CRITICAL)

**Problem:** Using direct column access panics on NULL  
**Solution:** Use `COALESCE()` or handle NULL in Rust with `Option<T>`

### 3. Missing Plugins/Features

**Problem:** Assuming plugins are installed  
**Solution:** Check for feature availability and handle gracefully

### 4. Division by Zero

**Problem:** Dividing without checking denominator  
**Solution:** Check `if total > 0` before division

### 5. Privilege Errors

**Problem:** Assuming user has all privileges  
**Solution:** Handle permission errors gracefully, skip metrics — and because a skip clears
the collector's series, warn **once** per process (`util::DeniedOnce`) rather than on every
scrape.

### 6. Stale Values After a Source Disappears (CRITICAL)

**Problem:** A collector returns early when its plugin/table/privilege is gone, leaving the
previous scrape's values in the registry to be served as current forever.  
**Solution:** Return `Collected::Skipped` so the safe wrapper clears the collector's series.
Prometheus scalars cannot be removed after registration, so any scalar owned by a
skip-capable source must be a **zero-label vector** (`IntGaugeVec`/`GaugeVec`/
`IntCounterVec`/`CounterVec` with `&[]` labels, set through
`with_label_values(&NO_LABELS)`, removed through `reset()`). The wire format is identical to
the scalar it replaces.

### 7. Turning a Query Failure Into an Absence (CRITICAL)

**Problem:** `unwrap_or(0)` on a feature probe, or `Err(_) => vec![]` on the data query,
makes a broken connection look exactly like an uninstalled plugin — and then clears real
data.  
**Solution:** Classify with `classify_query_error`. `Absent`/`Denied` are skips; everything
else is `Err` and must propagate.

## Test Coverage Requirements

Before merging:
- [ ] All new collectors have registration tests
- [ ] All new collectors have feature availability tests
- [ ] Edge cases (NULL, zero, empty) are tested
- [ ] Type conversions are tested with realistic data
- [ ] Settlement is tested: the source going away removes the series it owned
- [ ] `reset_metrics()` is implemented explicitly (there is no default) and covers everything the collector publishes
- [ ] Any test that mutates global server state uses an isolated container
- [ ] CI passes on all MariaDB versions

## Debugging Test Failures

```bash
# Run single test with output
cargo test test_name -- --nocapture

# Run with RUST_LOG for detailed tracing
RUST_LOG=debug cargo test test_name -- --nocapture

# Connect to test database to inspect state
mysql -u root -proot -h 127.0.0.1

# Check plugin installation
SHOW PLUGINS;

# Check available privileges
SHOW GRANTS;
```

## MariaDB Version Compatibility

We test against MariaDB 11.x. Some features may vary:

- `userstat` - Requires `userstat=ON`
- `query_response_time` - Requires plugin installation
- `system` - Requires Linux or FreeBSD for CPU and process-group metrics; other platforms
  publish cores + load only and skip the process group
- Always check feature availability before collecting

## When to Skip Tests

Tests should be skipped (not fail) when:
- Required plugin is not installed
- MariaDB version doesn't support a feature
- Running in a restricted environment
- User lacks required privileges

```rust
if feature_check.is_none() {
    println!("Feature not available, skipping test");
    return Ok(());
}
```

## Continuous Improvement

After any production panic:
1. Add a test that reproduces the panic
2. Fix the code
3. Verify the test now passes
4. Update this guide with lessons learned
