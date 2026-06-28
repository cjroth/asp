#!/usr/bin/env bash
# Cross-surface sync soak: drive a REAL `asp watch --listen` CLI vault against N
# REAL desktop engines (the Tauri/web backend), fuzzing file ops on every side and
# asserting cross-surface convergence. This is the automated version of "spin up a
# vault with the CLI, connect the app, change files, watch them sync."
#
# Runs several topologies/seeds SEQUENTIALLY (not in parallel — parallel runs
# starve the CPU and produce false perf flags). Stops nonzero on the first real
# divergence or perf regression.
#
#   bash e2e/sync-soak.sh            # default battery
#   ROUNDS=500 PEERS=2 bash e2e/sync-soak.sh 1 2 3   # custom seeds
set -uo pipefail
ROOT=/home/chris/asp
BIN="$ROOT/target/release/examples/sync_fuzz"
ROUNDS=${ROUNDS:-250}
STREAK=${STREAK:-10}
TIMEOUT=${TIMEOUT:-20}
SEEDS=("$@")
[ ${#SEEDS[@]} -eq 0 ] && SEEDS=(1 2 3 4)

echo "building asp + sync_fuzz (release)…"
( cd "$ROOT" && cargo build --release -p asp --bin asp && \
  cargo build --release -p asp-desktop-engine --example sync_fuzz ) >/dev/null || exit 2

fail=0
for peers in 1 2 3; do
  for s in "${SEEDS[@]}"; do
    echo "--- topology: CLI + ${peers} engine(s), seed ${s}, ${ROUNDS} rounds ---"
    "$BIN" --seed "$s" --peers "$peers" --rounds "$ROUNDS" \
           --clean-streak "$STREAK" --timeout "$TIMEOUT" 2>&1 | tail -12
    rc=${PIPESTATUS[0]}
    [ "$rc" -ne 0 ] && { echo "!!! topology peers=${peers} seed=${s} FAILED (rc=$rc)"; fail=1; }
  done
done

# Reap any CLI hubs a killed run may have orphaned.
pkill -9 -f 'sync_fuzz' 2>/dev/null || true

if [ "$fail" -eq 0 ]; then
  echo "SOAK PASSED: no divergence or perf regression across all topologies/seeds."
else
  echo "SOAK FOUND ISSUES (see above)."
fi
exit $fail
