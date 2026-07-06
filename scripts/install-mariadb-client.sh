#!/usr/bin/env bash
set -euo pipefail

# Install the MariaDB client tools (mariadb + mariadb-admin) used by the tooling
# scripts (setup-local-test-db.sh, monitor-exporter.sh, the soak harness).
#
# Unlike PostgreSQL, MariaDB ships the interactive client (`mariadb`) and the admin
# tool (`mariadb-admin`) together in the distro's client package, so this is a thin
# wrapper: it is idempotent, prefers the distro package, and adds the MariaDB.org
# APT repo only if the distro package is unavailable.
#
# MARIADB_MAJOR can pin a major series when installing from the MariaDB.org repo
# (defaults to 11.4 LTS, matching .devcontainer/compose.yaml).

MARIADB_MAJOR="${MARIADB_MAJOR:-11.4}"

# Already have a real client? Nothing to do (idempotent).
if command -v mariadb >/dev/null 2>&1 && command -v mariadb-admin >/dev/null 2>&1; then
  echo "MariaDB client already installed: $(mariadb --version)"
  exit 0
fi

sudo apt-get update -qq

# Prefer the distro's client metapackage (pulls in mariadb + mariadb-admin). On
# Debian/Ubuntu this is `mariadb-client`; fall back to `default-mysql-client`.
if sudo apt-get install -y -qq mariadb-client >/dev/null 2>&1; then
  :
elif sudo apt-get install -y -qq default-mysql-client >/dev/null 2>&1; then
  :
else
  echo "Distro client package unavailable; adding the MariaDB.org APT repo..." >&2
  sudo apt-get install -y -qq ca-certificates curl gnupg lsb-release >/dev/null
  sudo install -d /etc/apt/keyrings
  sudo curl -fsSL https://mariadb.org/mariadb_release_signing_key.pgp \
    -o /etc/apt/keyrings/mariadb-keyring.pgp
  codename="$(. /etc/os-release && echo "${VERSION_CODENAME}")"
  distro_id="$(. /etc/os-release && echo "${ID}")"
  echo "deb [signed-by=/etc/apt/keyrings/mariadb-keyring.pgp] https://mirror.mariadb.org/repo/${MARIADB_MAJOR}/${distro_id} ${codename} main" |
    sudo tee /etc/apt/sources.list.d/mariadb.list >/dev/null
  sudo apt-get update -qq
  sudo apt-get install -y -qq mariadb-client >/dev/null
fi

echo "✓ Installed: $(mariadb --version)"
