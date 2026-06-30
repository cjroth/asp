#!/usr/bin/env bash
# Local CI gate — runs the DETERMINISTIC checks CI enforces, fast, before a push,
# so failures surface here instead of in a 10-minute CI round. Mirrors
# .github/workflows/ci.yml's `test`, `coverage`, and the wasm build of the `sdk`
# job. Networked e2e/demo tests (relay/clone-catchup/live-wss interop) are
# LOAD-FLAKY by nature (real iroh QUIC under parallel load) — they are NOT part of
# this gate; run them in isolation (`--e2e`) or just re-run the CI job on a flake.
#
# Usage:
#   scripts/check.sh            # core: build + test + clippy + wasm compile
#   scripts/check.sh --cov      # also enforce the asp-core coverage floor
#   scripts/check.sh --vectors  # also assert the SDK conformance vectors are fresh
#   scripts/check.sh --e2e      # also run the networked e2e suite (serially, flaky)
#   scripts/check.sh --all      # everything
set -uo pipefail
cd "$(dirname "$0")/.."

do_cov=0 do_vec=0 do_e2e=0
for a in "$@"; do
  case "$a" in
    --cov) do_cov=1 ;;
    --vectors) do_vec=1 ;;
    --e2e) do_e2e=1 ;;
    --all) do_cov=1; do_vec=1; do_e2e=1 ;;
    *) echo "unknown flag: $a"; exit 2 ;;
  esac
done

fail=0
step() { echo; echo "──▶ $*"; }
run() { "$@" || { echo "::FAIL:: $*"; fail=1; }; }

step "build workspace"
run cargo build --workspace

step "test asp-core (unit + property + fuzz — deterministic)"
run cargo test -p asp-core

step "test desktop engine (engine logic, no live network)"
run cargo test -p asp-desktop-engine --lib

step "clippy (workspace, all targets, -D warnings)"
run cargo clippy --workspace --all-targets -- -D warnings

step "wasm compile (the sdk job builds asp-wasm to wasm32)"
if rustup target list --installed 2>/dev/null | grep -q wasm32-unknown-unknown; then
  run cargo build -p asp-wasm --target wasm32-unknown-unknown
else
  echo "  (skip: wasm32 target not installed — \`rustup target add wasm32-unknown-unknown\`)"
fi

if [ "$do_vec" = 1 ]; then
  step "SDK conformance vectors are fresh"
  cargo run -q -p asp-core --example gen_vectors > /tmp/asp-vectors.json 2>/dev/null
  if diff -q sdks/typescript/test-vectors.json /tmp/asp-vectors.json >/dev/null; then
    echo "  vectors up to date"
  else
    echo "::FAIL:: test-vectors.json is STALE — regenerate: cargo run -p asp-core --example gen_vectors > sdks/typescript/test-vectors.json"
    fail=1
  fi
fi

if [ "$do_cov" = 1 ]; then
  step "asp-core coverage floor"
  run ./scripts/coverage.sh
fi

if [ "$do_e2e" = 1 ]; then
  step "networked e2e (serial; LOAD-FLAKY — a single failure is usually a flake, re-run)"
  run cargo test -p asp-e2e -- --test-threads=1
fi

echo
if [ "$fail" = 0 ]; then echo "✅ local CI gate PASSED"; else echo "❌ local CI gate FAILED"; fi
exit $fail
