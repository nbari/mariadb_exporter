# Testing MariaDB Exporter with Unix Socket Connection

This guide explains how to test `mariadb_exporter` using Unix socket connections in a containerized environment, which mirrors typical production deployments.

## Why Test with Sockets?

In production, `mariadb_exporter` is typically deployed on the same host as MariaDB and connects via Unix socket for:
- **Better performance**: No TCP overhead
- **Enhanced security**: No network exposure
- **Simpler authentication**: Can use socket-based auth without passwords

## Prerequisites

- Podman
- Just command runner (`cargo install just`)

## Quick Start

### Test Socket Connection (Combined Container)

This tests MariaDB and exporter running together in one container - exactly like production deployment:

```bash
just test-socket
```

This will:
1. Build a container image with both MariaDB and the exporter
2. Start the container with exporter auto-starting via socket
3. Verify both services are running
4. Test metrics collection via Unix socket
5. Clean up

**Expected output:**
```
✅ Both services are ready!
✅ Socket connection successful!

📊 Sample metrics:
mariadb_up 1
mariadb_version_info{version="11.4.0-MariaDB"} 1
mariadb_exporter_build_info{version="0.1.0"} 1
```

**Why this is the most realistic test:**
- ✅ Single container (matches sidecar or DaemonSet deployment)
- ✅ True Unix socket connection (no network)
- ✅ User created automatically on startup
- ✅ Both services lifecycle managed together

## Understanding the DSN Format

The default DSN for socket connections:

```
mysql:///mysql?socket=/var/run/mysqld/mysqld.sock&user=exporter
```

Breaking it down:
- `mysql://` - Protocol
- `///mysql` - Database name (empty host means socket)
- `?socket=/var/run/mysqld/mysqld.sock` - Socket file path
- `&user=exporter` - Username (no password needed with socket auth)

### Alternative Socket Paths

Different MariaDB distributions use different socket paths:

```bash
# Debian/Ubuntu
mysql:///mysql?socket=/var/run/mysqld/mysqld.sock&user=exporter

# RHEL/CentOS/Fedora
mysql:///mysql?socket=/var/lib/mysql/mysql.sock&user=exporter

# Alpine (in containers)
mysql:///mysql?socket=/run/mysqld/mysqld.sock&user=exporter
```

## Manual Testing Steps

If you want to test manually without the just recipes:

### 1. Start MariaDB Container

```bash
podman run -d --name mariadb_test \
  -e MARIADB_ROOT_PASSWORD=root \
  -p 3306:3306 \
  mariadb:11.4
```

### 2. Create Exporter User (Secure Configuration)

```bash
podman exec mariadb_test mysql -uroot -proot -e "
  CREATE USER 'exporter'@'localhost' IDENTIFIED BY '';
  GRANT SELECT, PROCESS, REPLICATION CLIENT ON *.* TO 'exporter'@'localhost';
  ALTER USER 'exporter'@'localhost' WITH MAX_USER_CONNECTIONS 3;
  FLUSH PRIVILEGES;
"
```

Or use the provided SQL script:

```bash
podman exec mariadb_test mysql -uroot -proot < scripts/setup-exporter-user.sql
```

### 3. Build Exporter Image

```bash
podman build -t mariadb_exporter:latest -f Containerfile .
```

### 4. Run Exporter with Socket

```bash
# Find the socket path
SOCKET_DIR=$(podman exec mariadb_test sh -c 'ls -d /var/run/mysqld 2>/dev/null || echo /run/mysqld')

# Run exporter in same pod (shares network namespace)
podman run --rm \
  --pod container:mariadb_test \
  -e MARIADB_EXPORTER_DSN="mysql:///mysql?socket=${SOCKET_DIR}/mysqld.sock&user=exporter" \
  mariadb_exporter:latest -v
```

### 5. Test Metrics

In another terminal:

```bash
# Using curl
curl localhost:9306/metrics

# Using podman exec
podman exec mariadb_test wget -qO- http://localhost:9306/metrics
```

## Troubleshooting

### Socket Not Found

**Error:** `Can't connect to local MySQL server through socket`

**Solution:** Check socket path in the container:

```bash
podman exec mariadb_exporter_db find / -name "*.sock" 2>/dev/null
```

Common locations:
- `/var/run/mysqld/mysqld.sock`
- `/run/mysqld/mysqld.sock`
- `/var/lib/mysql/mysql.sock`
- `/tmp/mysql.sock`

### Permission Denied

**Error:** `Access denied for user 'exporter'@'localhost'`

**Solution:** Verify user grants:

```bash
podman exec mariadb_exporter_db mysql -uroot -proot -e "
  SHOW GRANTS FOR 'exporter'@'localhost';
"
```

Minimum required grants:
```sql
GRANT PROCESS, REPLICATION CLIENT, SELECT ON *.* TO 'exporter'@'localhost';
```

### Container Network Issues

**Error:** Cannot access metrics endpoint

**Solution:** Ensure containers share network namespace:

```bash
# Check if exporter is in same pod/network
podman ps --format "{{.Names}}\t{{.Networks}}"

# If not, use --pod option:
podman run --pod container:mariadb_exporter_db ...
```

## Production Deployment

For production deployments:

1. **Use socket authentication:**
   ```bash
   --dsn "mysql:///mysql?socket=/var/run/mysqld/mysqld.sock&user=exporter"
   ```

2. **Create dedicated user with secure settings:**
   ```sql
   CREATE USER 'exporter'@'localhost' IDENTIFIED BY '';
   GRANT SELECT, PROCESS, REPLICATION CLIENT ON *.* TO 'exporter'@'localhost';
   ALTER USER 'exporter'@'localhost' WITH MAX_USER_CONNECTIONS 3;
   FLUSH PRIVILEGES;
   ```

3. **Mount socket directory:**
   ```bash
   podman run -d \
     -v /var/run/mysqld:/var/run/mysqld:ro \
     -e MARIADB_EXPORTER_DSN="mysql:///mysql?socket=/var/run/mysqld/mysqld.sock&user=exporter" \
     mariadb_exporter:latest
   ```

4. **Use systemd service:**
   ```bash
   # Install service file
   sudo cp contrib/systemd/mariadb_exporter.service /etc/systemd/system/

   # Configure DSN
   sudo nano /etc/mariadb_exporter/mariadb_exporter.env

   # Start service
   sudo systemctl enable --now mariadb_exporter
   ```

## Security Best Practices

### Why These Permissions?

The recommended user setup provides **minimal required privileges** for all collectors:

```sql
GRANT SELECT, PROCESS, REPLICATION CLIENT ON *.* TO 'exporter'@'localhost';
```

**Permission breakdown:**

| Permission | Used By | Purpose |
|------------|---------|---------|
| `SELECT` | All collectors | Read system tables (`mysql.*`, `information_schema.*`, `performance_schema.*`) |
| `PROCESS` | `default` | View running processes (`SHOW PROCESSLIST`) |
| `REPLICATION CLIENT` | `replication` | View replication status (`SHOW SLAVE STATUS`, `SHOW MASTER STATUS`) |

### Why MAX_USER_CONNECTIONS 3?

```sql
ALTER USER 'exporter'@'localhost' WITH MAX_USER_CONNECTIONS 3;
```

The exporter's connection pool is configured with:
- **Min connections:** 1
- **Max connections:** 3
- **Max lifetime:** 120s

Setting `MAX_USER_CONNECTIONS 3` ensures:
- ✅ Prevents connection pool exhaustion
- ✅ Limits resource usage per exporter instance
- ✅ Protects against connection leaks
- ✅ Allows graceful connection rotation

### Why 'exporter'@'localhost' (Not '%')?

```sql
CREATE USER 'exporter'@'localhost' IDENTIFIED BY '';
```

**Security benefits:**
- ✅ **No network exposure**: User can only connect via Unix socket
- ✅ **No password needed**: Socket authentication is more secure
- ✅ **Reduced attack surface**: Cannot be accessed remotely even if firewall misconfigured
- ✅ **Compliance**: Meets security requirements for password-less authentication

Compare to network access:
```sql
-- ❌ Less secure: allows remote connections
CREATE USER 'exporter'@'%' IDENTIFIED BY 'password';

-- ✅ More secure: local socket only, no password
CREATE USER 'exporter'@'localhost' IDENTIFIED BY '';
```

### Testing Your Security

Run the permission test to verify your setup:

```bash
just test-permissions
```

This validates:
- ✅ User has all required permissions
- ✅ User is restricted to localhost only
- ✅ MAX_USER_CONNECTIONS is set to 3
- ✅ No password is configured (socket auth)
- ✅ All collectors can access required tables

## Changing Default DSN

To change the default DSN in the code:

Edit `src/cli/commands/mod.rs`:

```rust
.arg(
    Arg::new("dsn")
        .long("dsn")
        .help("MariaDB connection string (URL format)")
        .default_value("mysql:///mysql?user=exporter")  // Change this
        .env("MARIADB_EXPORTER_DSN")
        .value_name("DSN"),
)
```

Then rebuild:
```bash
just build-image
```

## See Also

- [README.md](README.md) - General usage documentation
- [CLAUDE.md](CLAUDE.md) - Development guide
- [Containerfile](Containerfile) - Container build configuration
- [.justfile](.justfile) - All available just recipes
