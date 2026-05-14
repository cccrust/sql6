#!/usr/bin/env bash
# sql6web.sh — Start sql6 Web Admin

set -uo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")" && pwd)"
BINARY="$PROJECT_DIR/target/release/sql6"

PORT="${SQL6_PORT:-8080}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
RESET='\033[0m'

echo ""
echo "=============================================="
echo -e "${BLUE}sql6 Web Admin${RESET}"
echo "=============================================="
echo ""

if [[ ! -x "$BINARY" ]]; then
    echo -e "${YELLOW}Building Rust binary...${RESET}"
    cd "$PROJECT_DIR" && cargo build --release
fi

export SQL6_BINARY="$BINARY"

echo -e "Starting on ${GREEN}http://127.0.0.1:${PORT}${RESET}"
echo -e "Press ${YELLOW}Ctrl+C${RESET} to stop"
echo ""

cd "$PROJECT_DIR/sql6_pypi"

exec uv run python -c "
import sys
import os
import asyncio
import uvicorn
from sql6.web import app, start_server, close_server

async def main():
    start_server(None)
    config = uvicorn.Config(app, host='127.0.0.1', port=$PORT, log_level='info')
    server = uvicorn.Server(config)
    await server.serve()

try:
    asyncio.run(main())
finally:
    close_server()
"