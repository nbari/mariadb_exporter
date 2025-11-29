# Testing Guide

This document describes the testing strategy for mariadb_exporter to prevent production issues.

## Testing Philosophy

All collectors MUST be tested with:
1. **Feature availability tests** - Handle missing features/plugins gracefully
2. **Edge case tests** - NULL values, empty results, privilege errors
3. **Type compatibility tests** - Ensure SQL types match Rust types
4. **Realistic workload tests** - Test with actual data and queries

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
**Solution:** Handle permission errors gracefully, skip metrics

## Test Coverage Requirements

Before merging:
- [ ] All new collectors have registration tests
- [ ] All new collectors have feature availability tests
- [ ] Edge cases (NULL, zero, empty) are tested
- [ ] Type conversions are tested with realistic data
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
