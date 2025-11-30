#!/usr/bin/env bash
#
# Generate self-signed SSL certificates for MariaDB TLS testing
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CERT_DIR="${SCRIPT_DIR}/certs"

echo "🔐 Generating SSL certificates for MariaDB..."

# Create certs directory
mkdir -p "${CERT_DIR}"

# Generate CA key and certificate
echo "📝 Generating CA certificate..."
openssl genrsa 2048 > "${CERT_DIR}/ca-key.pem"
openssl req -new -x509 -nodes -days 3650 \
  -key "${CERT_DIR}/ca-key.pem" \
  -out "${CERT_DIR}/ca-cert.pem" \
  -subj "/CN=MariaDB_CA"

# Generate server certificate
echo "📝 Generating server certificate..."
openssl req -newkey rsa:2048 -days 3650 -nodes \
  -keyout "${CERT_DIR}/server-key.pem" \
  -out "${CERT_DIR}/server-req.pem" \
  -subj "/CN=localhost"

# Process server certificate request
openssl rsa -in "${CERT_DIR}/server-key.pem" -out "${CERT_DIR}/server-key.pem"
openssl x509 -req -in "${CERT_DIR}/server-req.pem" -days 3650 \
  -CA "${CERT_DIR}/ca-cert.pem" \
  -CAkey "${CERT_DIR}/ca-key.pem" \
  -set_serial 01 \
  -out "${CERT_DIR}/server-cert.pem"

# Generate client certificate
echo "📝 Generating client certificate..."
openssl req -newkey rsa:2048 -days 3650 -nodes \
  -keyout "${CERT_DIR}/client-key.pem" \
  -out "${CERT_DIR}/client-req.pem" \
  -subj "/CN=mariadb_exporter"

# Process client certificate request
openssl rsa -in "${CERT_DIR}/client-key.pem" -out "${CERT_DIR}/client-key.pem"
openssl x509 -req -in "${CERT_DIR}/client-req.pem" -days 3650 \
  -CA "${CERT_DIR}/ca-cert.pem" \
  -CAkey "${CERT_DIR}/ca-key.pem" \
  -set_serial 02 \
  -out "${CERT_DIR}/client-cert.pem"

# Clean up CSR files
rm -f "${CERT_DIR}"/*-req.pem

# Set appropriate permissions (readable by all since this is for testing)
chmod 644 "${CERT_DIR}"/*.pem
chmod 600 "${CERT_DIR}"/*-key.pem

echo "✅ SSL certificates generated in ${CERT_DIR}"
echo ""
echo "Files created:"
ls -lh "${CERT_DIR}"
