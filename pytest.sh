#!/usr/bin/env bash
# pytest.sh — sql6 Python client integration tests
# 使用 pytest 測試 Python client 與 Rust server 的整合

set -uo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")" && pwd)"
BINARY="$PROJECT_DIR/target/debug/sql6"
PYTHON_DIR="$PROJECT_DIR/sql6_pypi"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
RESET='\033[0m'

echo "=============================================="
echo "sql6 Python Client Integration Tests (pytest)"
echo "=============================================="

# Build if needed
if [[ ! -x "$BINARY" ]]; then
    echo -e "${YELLOW}Building release binary...${RESET}"
    cd "$PROJECT_DIR" && cargo build --release
    if [[ ! -x "$BINARY" ]]; then
        echo -e "${RED}ERROR: Build failed${RESET}"
        exit 1
    fi
    BINARY="$PROJECT_DIR/target/release/sql6"
fi

echo -e "${GREEN}Binary: $BINARY${RESET}"
echo ""

# Set environment to use local binary
export SQL6_BINARY="$BINARY"
export PYTHONPATH="${PYTHON_DIR}:${PYTHONPATH:-}"

# Run pytest integration tests
cd "$PYTHON_DIR"
uv run pytest tests/test_sql6.py -v
exit $?