#!/usr/bin/env bash
# Start mariadb_exporter with socket connection
set -euo pipefail

# Wait for MariaDB to be ready
echo "Waiting for MariaDB to be ready..."
wait_deadline=$((SECONDS + 30))
while true; do
    if mariadb-admin ping -h localhost -uexporter --silent >/dev/null 2>&1; then
        break
    fi
    if [ -n "${MARIADB_ROOT_PASSWORD:-}" ] && mariadb-admin ping -h localhost -uroot -p"${MARIADB_ROOT_PASSWORD}" --silent >/dev/null 2>&1; then
        break
    fi

    if [ "$SECONDS" -ge "$wait_deadline" ]; then
        echo "MariaDB failed to start within 30 seconds"
        exit 1
    fi
    sleep 1
done

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
export MARIADB_EXPORTER_DSN="${MARIADB_EXPORTER_DSN:-mysql://exporter@localhost/mysql?socket=${SOCKET_PATH}}"
export MARIADB_EXPORTER_PORT="${MARIADB_EXPORTER_PORT:-9306}"

echo "Starting mariadb_exporter..."
echo "  DSN: $MARIADB_EXPORTER_DSN"
echo "  Port: $MARIADB_EXPORTER_PORT"

exec /usr/local/bin/mariadb_exporter "$@"
