#!/usr/bin/env bash
# py_cli_test.sh — Test Python CLI for sql6

set -uo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")" && pwd)"
BINARY="$PROJECT_DIR/target/release/sql6"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
RESET='\033[0m'

echo ""
echo "=============================================="
echo -e "${BLUE}Python CLI Tests${RESET}"
echo "=============================================="
echo ""

if [[ ! -x "$BINARY" ]]; then
    echo -e "${RED}ERROR: Binary not found. Building...${RESET}"
    cd "$PROJECT_DIR" && cargo build --release
fi

export SQL6_BINARY="$BINARY"

TESTS_PASSED=0
TESTS_FAILED=0

run_test() {
    local name="$1"
    local expected="$2"
    local cmd="$3"

    echo -n "  $name ... "
    local result
    result=$(eval "$cmd" 2>&1)

    if echo "$result" | grep -q "$expected"; then
        echo -e "${GREEN}PASS${RESET}"
        ((TESTS_PASSED++))
    else
        echo -e "${RED}FAIL${RESET}"
        echo "    Expected: $expected"
        echo "    Got: $result"
        ((TESTS_FAILED++))
    fi
}

cd "$PROJECT_DIR"

echo -e "${BLUE}[Basic Tests]${RESET}"
run_test "SELECT 1" "Connected to memory" 'uv run python -m sql6 -c "SELECT 1" 2>&1 | head -1'
run_test "SELECT 1 result" "1$" 'uv run python -m sql6 -c "SELECT 1 AS result" 2>&1 | tail -1'
run_test "Multiple columns" "1 | 2 | 3" 'uv run python -m sql6 -c "SELECT 1, 2, 3" 2>&1 | tail -1'

echo ""
echo -e "${BLUE}[Table Tests]${RESET}"
run_test "CREATE TABLE" "table created" 'uv run python -m sql6 -c "CREATE TABLE users(id INTEGER, name TEXT)" 2>&1 | tail -1'
run_test ".tables" "users" "uv run python -m sql6 -c 'CREATE TABLE users(id INTEGER); .tables' 2>&1 | tail -1'
run_test ".schema" "users" "uv run python -m sql6 -c 'CREATE TABLE users(id INTEGER); .schema users' 2>&1 | tail -1'

echo ""
echo -e "${BLUE}[INSERT/SELECT Tests]${RESET}"
run_test "INSERT" "1 row(s) inserted" 'uv run python -m sql6 -c "CREATE TABLE u(id INTEGER); INSERT INTO u VALUES(1)" 2>&1 | tail -1'
run_test "SELECT *" "1$" 'uv run python -m sql6 -c "CREATE TABLE u(id INTEGER); INSERT INTO u VALUES(1); SELECT * FROM u" 2>&1 | tail -1'
run_test "SELECT id FROM u" "affected" 'uv run python -m sql6 -c "CREATE TABLE u(id INTEGER); SELECT id FROM u" 2>&1 | tail -1'

echo ""
echo "=============================================="
echo -e "Results: ${GREEN}$TESTS_PASSED passed${RESET}, ${RED}$TESTS_FAILED failed${RESET}"
echo "=============================================="

if [[ $TESTS_FAILED -gt 0 ]]; then
    exit 1
fi
exit 0