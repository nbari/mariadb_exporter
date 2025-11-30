# MariaDB TLS Configuration

This directory contains MariaDB configuration files and SSL/TLS certificates for testing.

## TLS Setup

The MariaDB container is configured with SSL/TLS support. Server-side TLS is always available, but client connections must explicitly enable it.

### Certificate Generation

Generate self-signed certificates for testing:

```bash
./db/conf/generate-ssl-certs.sh
```

This creates:
- `certs/ca-cert.pem` - Certificate Authority certificate
- `certs/ca-key.pem` - CA private key
- `certs/server-cert.pem` - Server certificate
- `certs/server-key.pem` - Server private key
- `certs/client-cert.pem` - Client certificate
- `certs/client-key.pem` - Client private key

**Note**: The `certs/` directory is git-ignored. Run the script after cloning.

### Server Configuration

The `my.cnf` file configures MariaDB with:

```ini
[mysqld]
ssl_ca = /etc/mysql/conf.d/certs/ca-cert.pem
ssl_cert = /etc/mysql/conf.d/certs/server-cert.pem
ssl_key = /etc/mysql/conf.d/certs/server-key.pem
require_secure_transport = OFF
```

- `require_secure_transport = OFF` allows both TLS and non-TLS connections
- Set to `ON` to enforce TLS for all connections

### Verifying TLS is Available

Check if TLS is enabled on the server:

```bash
podman exec mariadb_exporter_db mariadb -uroot -proot -e "SHOW VARIABLES LIKE 'have_ssl';"
```

Expected output:
```
Variable_name | Value
have_ssl      | YES
```

Check SSL configuration:

```bash
podman exec mariadb_exporter_db mariadb -uroot -proot -e "SHOW VARIABLES LIKE 'ssl%';"
```

Check current connection status:

```bash
podman exec mariadb_exporter_db mariadb -uroot -proot -e "STATUS;" | grep SSL
```

## Client TLS Connection

The exporter connects to MariaDB using a DSN. To enable TLS on the client side, use DSN parameters:

### Basic TLS (accepts any certificate):
```bash
mysql://root:root@127.0.0.1:3306/mysql?ssl-mode=REQUIRED
```

### TLS with certificate verification:
```bash
mysql://root:root@127.0.0.1:3306/mysql?ssl-mode=VERIFY_IDENTITY&ssl-ca=/path/to/ca-cert.pem
```

### Available ssl-mode values:
- `DISABLED` - No TLS
- `PREFERRED` - TLS if available (default for TCP connections)
- `REQUIRED` - TLS required, but doesn't verify certificates
- `VERIFY_CA` - TLS required, verify CA certificate
- `VERIFY_IDENTITY` - TLS required, verify CA and hostname

## Testing TLS Collector

The TLS collector queries session variables to detect active TLS connections:

```bash
# Non-TLS connection (will show tls_session_active=0)
cargo run -- --dsn "mysql://root:root@127.0.0.1:3306/mysql" --collector.tls

# TLS connection (should show tls_session_active=1 if session variables exist)
cargo run -- --dsn "mysql://root:root@127.0.0.1:3306/mysql?ssl-mode=REQUIRED" --collector.tls
```

**Note**: MariaDB doesn't expose `@@ssl_version` and `@@ssl_cipher` session variables like MySQL does. The TLS collector will report `tls_session_active=0` even for TLS connections to MariaDB. This is a known limitation - the server supports TLS and the connection uses TLS (verifiable via `SHOW STATUS LIKE 'Ssl%'`), but the session-level variables used by the collector don't exist in MariaDB.

## Security Notes

- These are **self-signed certificates for testing only**
- Do not use these certificates in production
- In production, use certificates from a trusted CA
- Keep private keys (`*-key.pem`) secure and never commit them to git
