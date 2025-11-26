#!/usr/bin/env bash
# Combined entrypoint that starts both MariaDB and mariadb_exporter
set -e

# Source the original MariaDB entrypoint functions
source /usr/local/bin/docker-entrypoint.sh

# Function to start the exporter in background
start_exporter() {
  echo "Starting mariadb_exporter in background..."
  /usr/local/bin/start-exporter.sh > /var/log/mariadb_exporter.log 2>&1 &
  EXPORTER_PID=$!
  echo "mariadb_exporter started with PID: $EXPORTER_PID"

  # Store PID for cleanup
  echo $EXPORTER_PID > /var/run/mariadb_exporter.pid
}

# Trap to ensure exporter is stopped when container stops
cleanup() {
  echo "Stopping mariadb_exporter..."
  if [ -f /var/run/mariadb_exporter.pid ]; then
    kill $(cat /var/run/mariadb_exporter.pid) 2>/dev/null || true
    rm -f /var/run/mariadb_exporter.pid
  fi
}
trap cleanup EXIT TERM INT

# If EXPORTER_ENABLED is not set or is true, start the exporter
if [ "${EXPORTER_ENABLED:-true}" = "true" ]; then
  # Start MariaDB initialization in background
  echo "Initializing MariaDB..."
  _main "$@" &
  MARIADB_PID=$!

  # Wait a bit for MariaDB to initialize
  sleep 5

  # Start the exporter
  start_exporter

  # Wait for MariaDB process
  wait $MARIADB_PID
else
  # Just start MariaDB normally
  exec _main "$@"
fi
