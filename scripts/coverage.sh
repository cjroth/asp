#!/usr/bin/env bash
# Repeatable coverage for the Rust core (§Testing). Prints a per-file summary and
# fails if line coverage drops below the floor — so the cold integration seams
# (engine capture, net protocol) can't silently regress. Needs:
#   rustup component add llvm-tools-preview && cargo install cargo-llvm-cov
set -euo pipefail
FLOOR="${COVERAGE_FLOOR:-92}"   # asp-core line-coverage floor, in percent
cd "$(dirname "$0")/.."

cargo llvm-cov --package asp-core --summary-only "$@"

pct=$(cargo llvm-cov --package asp-core --summary-only 2>/dev/null \
  | awk '/^TOTAL/ {gsub(/%/,"",$10); print $10}')
echo "asp-core line coverage: ${pct}% (floor ${FLOOR}%)"
awk -v p="$pct" -v f="$FLOOR" 'BEGIN { exit (p+0 < f+0) ? 1 : 0 }' \
  || { echo "::error::coverage ${pct}% is below the ${FLOOR}% floor"; exit 1; }
