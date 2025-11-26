# MariaDB Monitoring Dashboard

This directory contains the Grafana dashboard for monitoring MariaDB/MySQL servers using `mariadb_exporter`.

## Dashboard Overview

The dashboard provides a clean, professional interface for monitoring MariaDB instance health and performance. It focuses on essential metrics without unnecessary complexity.

## Structure

```
grafana/
├── dashboard.json          # Grafana dashboard definition
├── README.md              # This file
└── validate-dashboard.sh  # Validation script (in scripts/)
```

## Dashboard Panels

### Instance Status Row

1. **MariaDB Status** (Stat panel)
   - Metric: `mariadb_up{job="$job",instance="$instance"}`
   - Shows: Up (1) or Down (0)
   - Color: Green = Up, Red = Down
   - Action: If 0, check service immediately

2. **Version Information** (Table panel)
   - Metric: `mariadb_version_info{job="$job",instance="$instance"}`
   - Shows: MariaDB version from label
   - Format: Table with version column

3. **Exporter Build Info** (Table panel)
   - Metric: `mariadb_exporter_build_info{job="$job",instance="$instance"}`
   - Shows: Exporter version, commit, architecture
   - Format: Table with build metadata

## Dashboard Variables

The dashboard uses three template variables for filtering:

1. **DS_PROMETHEUS** - Datasource selector
   - Type: Datasource
   - Query: `prometheus`
   - Allows selecting which Prometheus instance to query

2. **job** - Job selector
   - Type: Query
   - Query: `label_values(mariadb_up, job)`
   - Dynamically populated from available jobs

3. **instance** - Instance selector
   - Type: Query
   - Query: `label_values(mariadb_up{job="$job"}, instance)`
   - Filtered by selected job
   - Dynamically populated from available instances

## Design Principles

### Clean and Professional
- No emojis or decorative elements
- Professional color scheme (dark theme)
- Clear, descriptive panel titles
- Actionable descriptions (Goal/Action format)

### Metric Validation
All metrics used in the dashboard are validated against exported metrics using `scripts/validate-dashboard.sh`:

```bash
just validate-dashboard
```

This ensures:
- Dashboard only uses metrics that actually exist
- No typos in metric names
- Histogram suffixes (_bucket, _sum, _count) are properly handled
- JSON structure is valid
- Template variables are correctly configured

### Maintainability
- Metrics align with `mariadb_exporter` collectors
- Uses only core metrics (always available)
- Panel descriptions explain purpose and actions
- Template variables enable multi-instance monitoring

## Requirements

- Grafana 10.0.0 or higher
- Prometheus datasource configured
- `mariadb_exporter` running with at least the `default` collector enabled

## Installation

### Automatic (Podman Testing)

When using the test environment, the dashboard is automatically provisioned:

```bash
# Build and test with combined container
just test-combined

# The dashboard will be available at http://localhost:3000
# Login: admin / admin
```

### Manual Import

1. Open Grafana
2. Navigate to Dashboards → Import
3. Upload `dashboard.json` or paste its contents
4. Select your Prometheus datasource
5. Click Import

### Provisioning

For production deployment, place the dashboard in Grafana's provisioning directory:

```yaml
# /etc/grafana/provisioning/dashboards/mariadb.yaml
apiVersion: 1

providers:
  - name: 'MariaDB'
    orgId: 1
    folder: 'Databases'
    type: file
    options:
      path: /etc/grafana/provisioning/dashboards
```

Then copy `dashboard.json` to the configured path.

## Metrics Reference

### mariadb_up
- Type: Gauge
- Range: 0 (down) or 1 (up)
- Labels: `job`, `instance`
- Purpose: Database availability check

### mariadb_version_info
- Type: Info (constant 1)
- Labels: `job`, `instance`, `version`
- Purpose: MariaDB version information

### mariadb_exporter_build_info
- Type: Info (constant 1)
- Labels: `job`, `instance`, `version`, `commit`, `arch`
- Purpose: Exporter build metadata

## Extending the Dashboard

To add new panels:

1. **Add metrics to collectors** (`src/collectors/`)
2. **Validate metrics are exported** (run `cargo test`)
3. **Add panel to dashboard.json**
4. **Validate dashboard** (`just validate-dashboard`)
5. **Test in Grafana**

Always follow the design principles:
- Clear, actionable descriptions
- Professional appearance
- Use template variables (`$job`, `$instance`)
- Add metrics to validation script

## Testing

### Validate Dashboard Structure

```bash
# Run validation script
just validate-dashboard

# Or manually
./scripts/validate-dashboard.sh
```

This checks:
- All dashboard metrics are exported by collectors
- JSON structure is valid
- Template variables exist and are properly configured
- Query filters use job/instance variables

### Test with Live Data

```bash
# Start test environment
just test-combined

# Access Grafana
open http://localhost:3000

# Login: admin / admin
# Dashboard will be in "Databases" folder
```

## Troubleshooting

### No Data in Panels

1. **Check Prometheus is scraping exporter:**
   ```bash
   curl http://localhost:9306/metrics | grep mariadb_up
   ```

2. **Verify datasource connection in Grafana:**
   - Settings → Datasources → Prometheus
   - Click "Test" button

3. **Check template variables:**
   - Dashboard settings → Variables
   - Ensure job/instance are populated

### Invalid Metrics Error

If validation fails:

```bash
./scripts/validate-dashboard.sh
```

Check the output for invalid metrics and either:
- Remove the metric from the dashboard, or
- Add the metric to the appropriate collector

### Panel Shows "N/A"

- Ensure the collector providing that metric is enabled
- Check exporter logs for errors
- Verify database user has required permissions

## Contributing

When updating the dashboard:

1. Keep it clean and professional (no emojis)
2. Follow the existing panel structure
3. Add clear descriptions (Goal/Action format)
4. Run validation before committing
5. Test with live data

For more information, see the Developer Guidelines section in the main README.
