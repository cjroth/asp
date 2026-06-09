#!/usr/bin/env bash
#
# install.sh — set up the ASP Obsidian debug bridge.
#
# Run this IN THE VM. It:
#   1. installs the MCP server's deps (bun install),
#   2. copies the "ASP Debug Bridge" companion plugin into the target vault and
#      enables it in community-plugins.json,
#   3. registers the MCP server with Claude Code (user scope).
#
# After running, fully quit & reopen Obsidian on the Mac (or toggle the plugin
# in Settings → Community plugins) so it loads, then `obsidian_ping` from Claude.
#
# Env overrides (must match the MCP + plugin):
#   ASP_VAULT         default /mnt/mac/Users/chris/shared/careerbot-sync-2
#   ASP_DEBUG_PORT    default 28900
#   ASP_DEBUG_TOKEN   default asp-debug-bridge
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VAULT="${ASP_VAULT:-/mnt/mac/Users/chris/shared/careerbot-sync-2}"
PORT="${ASP_DEBUG_PORT:-28900}"
TOKEN="${ASP_DEBUG_TOKEN:-asp-debug-bridge}"

c_grn=$'\033[32m'; c_yel=$'\033[33m'; c_rst=$'\033[0m'
say()  { printf '%s==>%s %s\n' "$c_grn" "$c_rst" "$*"; }
warn() { printf '%s[!]%s %s\n' "$c_yel" "$c_rst" "$*"; }

[[ -d "$VAULT/.obsidian" ]] || { echo "no .obsidian in $VAULT — is ASP_VAULT right?" >&2; exit 1; }

say "Installing MCP server deps…"
( cd "$here/mcp" && bun install >/dev/null )

say "Deploying companion plugin → $VAULT/.obsidian/plugins/asp-debug/"
dest="$VAULT/.obsidian/plugins/asp-debug"
mkdir -p "$dest"
cp "$here/plugin/main.js" "$here/plugin/manifest.json" "$dest/"

# Enable it in community-plugins.json (a JSON array of enabled plugin ids).
cp_json="$VAULT/.obsidian/community-plugins.json"
say "Enabling asp-debug in community-plugins.json"
if [[ -f "$cp_json" ]]; then
  node -e '
    const fs=require("fs"); const f=process.argv[1];
    let a=[]; try{a=JSON.parse(fs.readFileSync(f,"utf8"))}catch{}
    if(!a.includes("asp-debug")) a.push("asp-debug");
    fs.writeFileSync(f, JSON.stringify(a, null, 2));
  ' "$cp_json"
else
  printf '[\n  "asp-debug"\n]\n' > "$cp_json"
fi

say "Registering MCP server with Claude Code (user scope)…"
claude mcp remove asp-obsidian 2>/dev/null || true
ASP_VAULT="$VAULT" ASP_DEBUG_PORT="$PORT" ASP_DEBUG_TOKEN="$TOKEN" \
  claude mcp add asp-obsidian -s user \
    -e "ASP_VAULT=$VAULT" -e "ASP_DEBUG_PORT=$PORT" -e "ASP_DEBUG_TOKEN=$TOKEN" \
    -- bun run "$here/mcp/server.ts"

say "Done."
warn "Now QUIT & REOPEN Obsidian on the Mac (or toggle ASP Debug Bridge in"
warn "Settings → Community plugins) so the bridge starts listening on :$PORT."
warn "Then run obsidian_ping from Claude to confirm."
