#!/usr/bin/env bash
# Real-WebKit-browser harness: serve the built frontend + mock backend, then drive
# it headless via WebKitWebDriver (MiniBrowser) at <n> files. Usage: web-run.sh [n]
set -u
N=${1:-1000}
ROOT=/home/chris/asp/desktop
BIG=${2:-1500}
HIST=${3:-2000}
export PORT=${PORT:-5599}
export URL="http://127.0.0.1:${PORT}/?n=${N}&big=${BIG}&hist=${HIST}"
export WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1 GDK_BACKEND=x11 NO_AT_BRIDGE=1

[ -f "$ROOT/dist/index.html" ] || { echo "build the frontend first (bun run build:web)"; exit 2; }

node "$ROOT/e2e/serve.mjs" &
SRV=$!
trap 'kill $SRV 2>/dev/null' EXIT
sleep 1

xvfb-run -a node "$ROOT/e2e/web-drive.mjs"
