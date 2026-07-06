#!/bin/bash
# Git pre-commit hook for mariadb_exporter.
# Install: cp scripts/pre-commit-hook.sh .git/hooks/pre-commit && chmod +x .git/hooks/pre-commit
#
# When collector code changes, verify a local MariaDB is reachable (so the tests
# can run), then enforce formatting. Keep it lightweight: clippy and the full test
# suite run via `just test`.

set -e

echo "🔍 Running pre-commit checks..."

MARIADB_HOST="${MARIADB_HOST:-127.0.0.1}"
MARIADB_PORT="${MARIADB_PORT:-3306}"
MARIADB_USER="${MARIADB_USER:-root}"
MARIADB_PASS="${MARIADB_PASS:-root}"

# Check if we're modifying collector code.
if git diff --cached --name-only | grep -q "src/collectors/"; then
    echo "📊 Collector code changed, verifying local MariaDB is reachable..."

    if ! mariadb-admin ping -h "$MARIADB_HOST" -P "$MARIADB_PORT" \
        -u"$MARIADB_USER" -p"$MARIADB_PASS" --silent >/dev/null 2>&1; then
        echo "⚠️  WARNING: MariaDB is not reachable on ${MARIADB_HOST}:${MARIADB_PORT}"
        echo "   Tests may fail. Start it with: just mariadb"
        echo ""
        read -p "Continue anyway? (y/N): " -n 1 -r
        echo
        if [[ ! $REPLY =~ ^[Yy]$ ]]; then
            exit 1
        fi
    fi
fi

# Enforce formatting on staged Rust changes (fast; clippy/tests run via `just test`).
if git diff --cached --name-only | grep -qE '\.rs$'; then
    echo "🎨 Checking formatting (cargo fmt --check)..."
    if ! cargo fmt --all -- --check >/dev/null 2>&1; then
        echo "❌ Formatting issues found. Run: cargo fmt --all"
        echo ""
        read -p "Continue anyway? (y/N): " -n 1 -r
        echo
        if [[ ! $REPLY =~ ^[Yy]$ ]]; then
            exit 1
        fi
    fi
fi

echo "✅ Pre-commit checks passed"
