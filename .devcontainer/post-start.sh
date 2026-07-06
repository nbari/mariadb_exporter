#!/usr/bin/env bash
set -uo pipefail

# Runs on every container start (DevPod postStartCommand). Best-effort: it must not
# fail `devpod up`. Waits for the mariadb sibling to be ready and seeds the local
# test database (sample tables) so `just test` is ready to go.

export PATH="$HOME/.local/bin:$HOME/.local/share/mise/shims:$PATH"
cd /workspaces/mariadb_exporter 2>/dev/null || exit 0

# Re-apply optional git identity/signing on every start so updates to forwarded
# DevPod workspace env are reflected without rebuilding the container.
sh .devcontainer/configure-git.sh || true

MARIADB_HOST="${MARIADB_HOST:-mariadb}"
MARIADB_PORT="${MARIADB_PORT:-3306}"
MARIADB_USER="${MARIADB_USER:-root}"
MARIADB_PASS="${MARIADB_PASS:-root}"

# Wait for MariaDB (compose healthcheck usually has it ready already).
for _ in $(seq 1 30); do
  if mariadb-admin ping -h "$MARIADB_HOST" -P "$MARIADB_PORT" -u"$MARIADB_USER" -p"$MARIADB_PASS" --silent >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

if [ -x scripts/setup-local-test-db.sh ]; then
  if MARIADB_HOST="$MARIADB_HOST" MARIADB_PORT="$MARIADB_PORT" \
     MARIADB_USER="$MARIADB_USER" MARIADB_PASS="$MARIADB_PASS" \
     scripts/setup-local-test-db.sh >/dev/null 2>&1; then
    echo "✓ Workspace ready. MariaDB seeded at ${MARIADB_HOST}:${MARIADB_PORT}."
    echo "  Run: just test"
  else
    echo "post-start: test DB seeding did not complete (continuing)." >&2
    echo "  You can run it manually: scripts/setup-local-test-db.sh" >&2
  fi
fi

exit 0
