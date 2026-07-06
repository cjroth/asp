#!/usr/bin/env bash
# Git-in-the-loop soak (git-bridge spec §10 "Soak"): drive the git-bridge slice
# under the sync-soak discipline — long clean streaks of a git remote bridged into
# the ASP mesh, asserting the ASP fold and the git remote stay byte-consistent.
#
# Two layers, same spirit as `sync-soak.sh`:
#
#   Layer A (RUNNABLE HERE) — the engine-level property soak. The heavy lifting is
#   already in the e2e suite as deterministic property/round-trip tests over the
#   hermetic `git http-backend` fixture (real wire bytes, no network):
#     * git_convergence_prop  — two bridge nodes, random interleaving of local
#       edits + plans + upstream ingests → converged fold AND identical synthesized
#       commit SHAs ("any node may bridge").
#     * git_push              — clone → edit → plan → push → `git log`/`git show`;
#       then upstream commit → pull → push again → linear history.
#     * git_ingest_race       — two bridges ingest the same commit → content
#       identical, single collapsed marker.
#     * git_policy            — interval auto-plan + the `asp git diff`/`plan` hook.
#     * git_clone_pull        — two independent clones of one repo converge.
#   Looping these across seeds under a clean-streak bar is the soak: a regression in
#   determinism or round-trip fidelity fails loudly, not silently in a vault.
#
#   Layer B (SCENARIO — needs a git host, see SKILL.md "Layer 3") — the live CLI
#   mesh follow: a real `asp watch` bridges a git remote while a second ASP peer
#   follows over iroh; edits land on BOTH the git side and the ASP side; assert the
#   whole mesh working tree == the git remote tip tree. The native git transport is
#   HTTPS/SSH-only (loopback plain-HTTP is refused, by design — see
#   crates/asp-core/src/gitwire.rs), so this layer needs a reachable https git host
#   or the crate-internal test transport; it is documented in SKILL.md, not run here.
#
#   ROUNDS=… bash e2e/git-soak.sh            # re-run the battery N times (streak bar)
#
# Each prop test sweeps its own set of deterministic LCG seeds internally; the
# ROUNDS loop simply re-runs the whole battery as the "no regression across N tries"
# clean-streak bar (matching how sync-soak.sh treats repeated topologies).
set -uo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
ROUNDS=${ROUNDS:-3}
SEEDS="(internal, per-test)"

command -v git >/dev/null 2>&1 || { echo "system git required for the git fixtures"; exit 2; }

echo "building asp-core + e2e (release test binaries)…"
( cd "$ROOT" && cargo build --release --tests -p asp-e2e ) >/dev/null || {
  echo "build failed — the git-bridge tests need asp-core with the git modules"; exit 2; }

TESTS=(git_convergence_prop git_push git_ingest_race git_policy git_clone_pull)
fail=0
for round in $(seq 1 "$ROUNDS"); do
  echo "=== git-bridge soak round ${round}/${ROUNDS} (seeds ${SEEDS}) ==="
  for t in "${TESTS[@]}"; do
    echo "--- $t ---"
    ( cd "$ROOT" && cargo test --release -p asp-e2e --test "$t" -- --nocapture ) 2>&1 | tail -6
    rc=${PIPESTATUS[0]}
    [ "$rc" -ne 0 ] && { echo "!!! $t FAILED (round ${round}, rc=$rc)"; fail=1; }
  done
done

if [ "$fail" -eq 0 ]; then
  echo "GIT SOAK PASSED: git bridge stayed deterministic + round-trip-consistent across all rounds."
else
  echo "GIT SOAK FOUND ISSUES (see above)."
fi
exit $fail
