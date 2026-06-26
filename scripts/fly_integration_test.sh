#!/usr/bin/env bash
#
# Real-network integration test for the all-in-one ASP box (`asp watch --listen
# --relay`) on fly.io. It provisions an EPHEMERAL fly app, deploys the all-in-one
# vault, then clones it from THIS machine over the public internet (through the
# box's co-hosted relay — fly exposes no UDP, so this exercises the real relay
# forwarding path), asserts the content and that the clone beats a latency
# budget, and ALWAYS tears the app down again (even on failure/interrupt).
#
# This costs money and takes a few minutes (a remote Docker build + deploy), so
# it is opt-in. Run it directly:
#
#     scripts/fly_integration_test.sh
#
# Requirements: flyctl (authenticated), a built local `asp` binary (the script
# builds one if missing), and network access to fly.io.
#
# Env knobs:
#   FLY_REGION       fly region (default: iad — closest to Raleigh, NC)
#   CLONE_BUDGET_S   max seconds the clone may take (default: 10)
#   KEEP_APP=1       skip teardown (debugging only)

set -euo pipefail

REGION="${FLY_REGION:-iad}"
CLONE_BUDGET_S="${CLONE_BUDGET_S:-10}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Unique-but-deterministic-enough suffix; fly app names must be DNS-safe.
SUF="$(date +%s | tail -c 7)$$"
APP="asp-itest-vault-${SUF}"
AUTH_KEY="itest-$(date +%s)-secret"
WORK="$(mktemp -d)"
CREATED=""   # apps to destroy on exit

log()  { printf '\n\033[1;36m== %s\033[0m\n' "$*"; }
fail() { printf '\n\033[1;31mFAIL: %s\033[0m\n' "$*" >&2; exit 1; }

cleanup() {
  local code=$?
  if [ "${KEEP_APP:-0}" = "1" ]; then
    log "KEEP_APP=1 — leaving $CREATED for inspection"
  else
    for app in $CREATED; do
      log "tearing down $app"
      flyctl apps destroy "$app" --yes >/dev/null 2>&1 || echo "  (warn: destroy $app failed; remove it manually)"
    done
  fi
  rm -rf "$WORK"
  exit $code
}
# Teardown on ANY exit: success, failure (set -e), or signal.
trap cleanup EXIT INT TERM

# ---- locate / build the local asp binary -------------------------------------
ASP_BIN=""
for p in target/debug/asp target/release/asp; do
  [ -x "$p" ] && ASP_BIN="$REPO_ROOT/$p" && break
done
if [ -z "$ASP_BIN" ]; then
  log "building local asp binary"
  cargo build -p asp
  ASP_BIN="$REPO_ROOT/target/debug/asp"
fi
echo "using asp binary: $ASP_BIN"

# ---- provision ephemeral all-in-one vault ------------------------------------
log "creating fly app $APP in $REGION"
flyctl apps create "$APP" --org personal >/dev/null
CREATED="$APP"

flyctl volumes create asp_vault_data -a "$APP" -r "$REGION" --size 1 --yes >/dev/null
# Auth key (clients present it) + pin THIS app's URL as the co-hosted home relay.
flyctl secrets set \
  "ASP_AUTH_KEY=${AUTH_KEY}" \
  "ASP_RELAY_URL=https://${APP}.fly.dev" \
  -a "$APP" --stage >/dev/null

log "deploying all-in-one ($APP) — remote build, ~few min"
flyctl deploy -c fly.vault.toml -a "$APP" --remote-only >/dev/null

# ---- capture the ticket the box prints on start ------------------------------
log "waiting for the box to announce its ticket"
TICKET=""
for _ in $(seq 1 30); do
  TICKET="$(flyctl logs -a "$APP" --no-tail 2>/dev/null | grep -oE 'ticket: [a-z0-9]+' | tail -1 | cut -d' ' -f2 || true)"
  [ -n "$TICKET" ] && break
  sleep 4
done
[ -n "$TICKET" ] || fail "box never printed a ticket"
echo "ticket: ${TICKET:0:48}..."

# ---- seed a file on the box, then clone it back over the network -------------
MARKER="itest-marker-$(date +%s)"
log "seeding a file on $APP"
flyctl ssh console -a "$APP" -C "/bin/sh -c 'echo ${MARKER} > /mnt/workspace/vault/itest.md'" >/dev/null
sleep 3

log "cloning $APP from this machine (budget ${CLONE_BUDGET_S}s)"
export ASP_HOME="$WORK/home"
# Retry the cold dial a couple of times: right after deploy the first connection
# can pay a one-time relay/hole-punch warmup. The budget is asserted on the
# winning attempt, not the warmup.
ELAPSED=0
CLONE_OK=0
for attempt in 1 2 3; do
  rm -rf "$WORK/vault"
  START=$(date +%s)
  if "$ASP_BIN" --relay-url "https://${APP}.fly.dev" \
       clone "$TICKET" "$WORK/vault" --auth-key "$AUTH_KEY" >/dev/null 2>&1; then
    ELAPSED=$(( $(date +%s) - START ))
    CLONE_OK=1
    echo "  clone succeeded on attempt $attempt in ${ELAPSED}s"
    break
  fi
  echo "  attempt $attempt failed (cold warmup?), retrying..."
  sleep 3
done
[ "$CLONE_OK" = "1" ] || fail "clone failed after retries"

GOT="$(cat "$WORK/vault/itest.md" 2>/dev/null || true)"
[ "$GOT" = "$MARKER" ] || fail "cloned content mismatch (got '$GOT', want '$MARKER')"
[ "$ELAPSED" -le "$CLONE_BUDGET_S" ] || fail "clone took ${ELAPSED}s (> ${CLONE_BUDGET_S}s budget)"

log "PASS — cloned over the real network in ${ELAPSED}s, content verified"
# cleanup() runs on exit and destroys the app.
