#!/usr/bin/env bash
# Seed a vault of <nfiles>/<nedits>, then drive the real app headless via
# tauri-driver under Xvfb. Usage: run.sh [nfiles] [nedits]
set -u
NFILES=${1:-1000}
NEDITS=${2:-200}
ROOT=/home/chris/asp
BIN="$ROOT/desktop/src-tauri/target/debug/context-desktop"
SEED="$ROOT/target/debug/examples/seed_vault"

[ -x "$BIN" ] || { echo "missing app binary: $BIN (build it first)"; exit 2; }
[ -x "$SEED" ] || { echo "missing seeder: $SEED"; exit 2; }

TH=$(mktemp -d)
trap 'rm -rf "$TH"' EXIT
echo "seeding $NFILES files / $NEDITS edits into $TH/vault ..."
HOME="$TH" "$SEED" "$TH/vault" "$NFILES" "$NEDITS" || { echo "seed failed"; exit 2; }

export APP_BIN="$BIN" TEST_HOME="$TH" VAULT_NAME=vault NATIVE_DRIVER=/usr/bin/WebKitWebDriver
export WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1 GDK_BACKEND=x11 NO_AT_BRIDGE=1
echo "driving the app (headless) ..."
xvfb-run -a node "$ROOT/desktop/e2e/drive.mjs"
