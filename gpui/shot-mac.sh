#!/usr/bin/env bash
# macOS screenshot/inspection harness for aspgui.
#
# Unlike the Linux scripts (Xvfb + lavapipe), macOS needs no virtual display:
# stock GPUI's `Window::render_to_image` does an offscreen Metal readback once
# `--features capture` turns on `test-support`. This builds the `shoot` driver,
# runs it (it seeds its own temp vault — it does NOT touch your real vaults),
# and writes one PNG per scripted state into the output dir.
#
# Usage:
#   ./shot-mac.sh [outdir]            # full scripted driver -> outdir/*.png
#   ./shot-mac.sh --smoke [out.png]   # minimal render smoke test -> one PNG
set -euo pipefail
cd "$(dirname "$0")"

if [[ "${1:-}" == "--smoke" ]]; then
  OUT="${2:-/tmp/aspshots/smoke.png}"
  mkdir -p "$(dirname "$OUT")"
  cargo build --bin smoke --features capture
  ./target/debug/smoke "$OUT"
  echo "wrote $OUT"
  exit 0
fi

OUTDIR="${1:-/tmp/aspshots}"
rm -rf "$OUTDIR"; mkdir -p "$OUTDIR"
cargo build --bin shoot --features capture
# ASP_NO_RELAY keeps the seeded vault fully offline during the run.
ASP_NO_RELAY=1 ./target/debug/shoot "$OUTDIR"
echo "--- shots in $OUTDIR ---"
ls -1 "$OUTDIR"
