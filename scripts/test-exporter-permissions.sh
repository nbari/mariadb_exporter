#!/usr/bin/env bash
# Test script to verify exporter user has correct permissions for all collectors
set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Configuration
MYSQL_CMD="${MYSQL_CMD:-mysql}"
EXPORTER_USER="${EXPORTER_USER:-exporter}"
EXPORTER_HOST="${EXPORTER_HOST:-localhost}"
TEST_DB="${TEST_DB:-mysql}"

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "Testing permissions for ${EXPORTER_USER}@${EXPORTER_HOST}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Function to run query and check success
test_query() {
    local test_name="$1"
    local query="$2"
    local expected="${3:-}"

    echo -n "Testing ${test_name}... "

    if result=$($MYSQL_CMD -u"${EXPORTER_USER}" -h"${EXPORTER_HOST}" -D"${TEST_DB}" -sN -e "${query}" 2>&1); then
        if [ -n "${expected}" ] && ! echo "${result}" | grep -q "${expected}"; then
            echo -e "${RED}✗ FAILED${NC} (unexpected result)"
            echo "  Expected: ${expected}"
            echo "  Got: ${result}"
            return 1
        fi
        echo -e "${GREEN}✓ PASSED${NC}"
        return 0
    else
        echo -e "${RED}✗ FAILED${NC}"
        echo "  Error: ${result}"
        return 1
    fi
}

PASSED=0
FAILED=0

# Test 1: Basic connectivity
if test_query "Basic connectivity" "SELECT 1" "1"; then
    ((PASSED++))
else
    ((FAILED++))
fi

# Test 2: SELECT permission on system tables
if test_query "SELECT on mysql.user" "SELECT COUNT(*) FROM mysql.user" ""; then
    ((PASSED++))
else
    ((FAILED++))
fi

# Test 3: PROCESS privilege (for default collector)
if test_query "PROCESS privilege" "SHOW PROCESSLIST" ""; then
    ((PASSED++))
else
    ((FAILED++))
fi

# Test 4: REPLICATION CLIENT privilege (for replication collector)
if test_query "REPLICATION CLIENT privilege" "SHOW MASTER STATUS" ""; then
    ((PASSED++))
else
    ((FAILED++))
fi

# Test 5: Access to information_schema (for schema collector)
if test_query "information_schema access" "SELECT COUNT(*) FROM information_schema.tables" ""; then
    ((PASSED++))
else
    ((FAILED++))
fi

# Test 6: Access to performance_schema (for statements/locks collectors)
if test_query "performance_schema access" "SELECT COUNT(*) FROM performance_schema.global_status" ""; then
    ((PASSED++))
else
    ((FAILED++))
fi

# Test 7: Global status variables (for default collector)
if test_query "SHOW GLOBAL STATUS" "SHOW GLOBAL STATUS LIKE 'Uptime'" "Uptime"; then
    ((PASSED++))
else
    ((FAILED++))
fi

# Test 8: Global variables (for default collector)
if test_query "SHOW GLOBAL VARIABLES" "SHOW GLOBAL VARIABLES LIKE 'version'" "version"; then
    ((PASSED++))
else
    ((FAILED++))
fi

# Test 9: InnoDB status (for default collector)
if test_query "SHOW ENGINE INNODB STATUS" "SHOW ENGINE INNODB STATUS" ""; then
    ((PASSED++))
else
    ((FAILED++))
fi

# Test 10: Binary log info (for replication collector)
if test_query "SHOW BINARY LOGS" "SHOW BINARY LOGS" ""; then
    ((PASSED++))
else
    ((FAILED++))
fi

# Test 11: Verify max_user_connections
echo -n "Checking MAX_USER_CONNECTIONS limit... "
MAX_CONN=$($MYSQL_CMD -uroot -proot -sN -e "SELECT max_user_connections FROM mysql.user WHERE User='${EXPORTER_USER}' AND Host='${EXPORTER_HOST}'" 2>/dev/null || echo "0")
if [ "${MAX_CONN}" = "3" ]; then
    echo -e "${GREEN}✓ PASSED${NC} (set to 3)"
    ((PASSED++))
else
    echo -e "${YELLOW}⚠ WARNING${NC} (set to ${MAX_CONN}, recommended: 3)"
    ((FAILED++))
fi

# Test 12: Verify user is local-only (security check)
echo -n "Checking user is localhost-only... "
REMOTE_USER=$($MYSQL_CMD -uroot -proot -sN -e "SELECT COUNT(*) FROM mysql.user WHERE User='${EXPORTER_USER}' AND Host != 'localhost'" 2>/dev/null || echo "0")
if [ "${REMOTE_USER}" = "0" ]; then
    echo -e "${GREEN}✓ PASSED${NC} (no remote access)"
    ((PASSED++))
else
    echo -e "${YELLOW}⚠ WARNING${NC} (user has remote access - security risk)"
    ((FAILED++))
fi

# Test 13: Verify no password is set (for socket auth)
echo -n "Checking password-less authentication... "
PLUGIN=$($MYSQL_CMD -uroot -proot -sN -e "SELECT plugin FROM mysql.user WHERE User='${EXPORTER_USER}' AND Host='${EXPORTER_HOST}'" 2>/dev/null || echo "")
AUTH_STRING=$($MYSQL_CMD -uroot -proot -sN -e "SELECT authentication_string FROM mysql.user WHERE User='${EXPORTER_USER}' AND Host='${EXPORTER_HOST}'" 2>/dev/null || echo "")
if [ -z "${AUTH_STRING}" ] || [ "${AUTH_STRING}" = "NULL" ]; then
    echo -e "${GREEN}✓ PASSED${NC} (socket authentication)"
    ((PASSED++))
else
    echo -e "${YELLOW}⚠ WARNING${NC} (password is set - socket auth recommended)"
    ((FAILED++))
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "Summary: ${PASSED} passed, ${FAILED} failed"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

if [ ${FAILED} -eq 0 ]; then
    echo -e "${GREEN}✓ All tests passed! User is properly configured.${NC}"
    exit 0
else
    echo -e "${RED}✗ Some tests failed. Please review the configuration.${NC}"
    exit 1
fi
