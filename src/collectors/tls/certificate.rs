use anyhow::Result;
use chrono::{DateTime, NaiveDateTime, Utc};

/// Parse SSL certificate timestamp from `MariaDB` format.
///
/// `MariaDB` returns timestamps in format: `"Nov 28 05:59:29 2035 GMT"`
/// or `"May 24 11:46:23 2020 GMT"`
///
/// # Errors
///
/// Returns an error if the timestamp string cannot be parsed.
pub fn parse_ssl_timestamp(timestamp_str: &str) -> Result<f64> {
    // Parse the timestamp string
    // Format: "Nov 28 05:59:29 2035 GMT"
    let dt = NaiveDateTime::parse_from_str(timestamp_str, "%b %d %H:%M:%S %Y GMT")
        .or_else(|_| {
            // Try alternative format without GMT suffix
            NaiveDateTime::parse_from_str(
                timestamp_str.trim_end_matches(" GMT"),
                "%b %d %H:%M:%S %Y",
            )
        })
        .map_err(|e| anyhow::anyhow!("Failed to parse timestamp '{timestamp_str}': {e}"))?;

    // Convert to UTC DateTime
    let utc_dt: DateTime<Utc> = DateTime::from_naive_utc_and_offset(dt, Utc);

    // Return Unix timestamp as f64
    // Note: i64 to f64 conversion is safe for timestamps in the valid range
    // (years ~1677-2262), precision loss only matters for nanoseconds
    #[allow(clippy::cast_precision_loss)]
    Ok(utc_dt.timestamp() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_parse_ssl_timestamp() {
        // Test valid timestamp
        let ts = parse_ssl_timestamp("Nov 28 05:59:29 2035 GMT").unwrap();
        assert!(ts > 0.0);

        // Test another format
        let ts2 = parse_ssl_timestamp("May 24 11:46:23 2020 GMT").unwrap();
        assert!(ts2 > 0.0);
        assert!(ts > ts2); // 2035 should be after 2020

        // Test invalid timestamp
        assert!(parse_ssl_timestamp("invalid").is_err());
    }

    #[test]
    #[allow(clippy::unwrap_used, clippy::float_cmp)]
    fn test_ssl_timestamp_conversion() {
        // Known timestamp for verification
        let ts = parse_ssl_timestamp("Jan 01 00:00:00 2020 GMT").unwrap();
        // 2020-01-01 00:00:00 UTC = 1577836800
        assert_eq!(ts, 1_577_836_800.0);
    }
}
