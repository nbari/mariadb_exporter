#!/usr/bin/env bash
# Start mariadb_exporter with socket connection
set -euo pipefail

# Wait for MariaDB to be ready
echo "Waiting for MariaDB to be ready..."
timeout 30 bash -c '
  until mysqladmin ping -h localhost --silent 2>/dev/null; do
    sleep 1
  done
' || {
  echo "MariaDB failed to start within 30 seconds"
  exit 1
}

echo "MariaDB is ready!"

# Detect socket path
SOCKET_PATH=""
for path in /var/run/mysqld/mysqld.sock /run/mysqld/mysqld.sock /var/lib/mysql/mysql.sock /tmp/mysql.sock; do
  if [ -S "$path" ]; then
    SOCKET_PATH="$path"
    break
  fi
done

if [ -z "$SOCKET_PATH" ]; then
  echo "ERROR: Could not find MySQL socket file"
  exit 1
fi

echo "Found MySQL socket at: $SOCKET_PATH"

# Set DSN if not already set
export MARIADB_EXPORTER_DSN="${MARIADB_EXPORTER_DSN:-mysql:///mysql?socket=${SOCKET_PATH}&user=exporter}"
export MARIADB_EXPORTER_PORT="${MARIADB_EXPORTER_PORT:-9306}"

echo "Starting mariadb_exporter..."
echo "  DSN: $MARIADB_EXPORTER_DSN"
echo "  Port: $MARIADB_EXPORTER_PORT"

exec /usr/local/bin/mariadb_exporter "$@"
