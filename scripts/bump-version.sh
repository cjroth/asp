#!/usr/bin/env bash
# Bump every release-bearing manifest in the monorepo to a single shared version.
#
# Usage:
#   scripts/bump-version.sh 0.2.0        # set an explicit version
#   scripts/bump-version.sh patch        # 0.1.25 -> 0.1.26
#   scripts/bump-version.sh minor        # 0.1.25 -> 0.2.0
#   scripts/bump-version.sh major        # 0.1.25 -> 1.0.0
#
# It prints the resolved version on stdout (last line) so callers (the release
# workflow) can capture it.
#
# One version, everywhere. The historical wrinkle: the Obsidian plugin shipped
# on its own cadence (manifest.json was ahead of the Cargo workspace), and BRAT
# / the community store require a strictly increasing plugin version. So the
# "current" version we bump from is the MAX of the workspace and plugin
# versions — that unifies the two tracks without ever regressing the plugin.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

die() { echo "bump-version: $*" >&2; exit 1; }

[ $# -eq 1 ] || die "usage: bump-version.sh <major|minor|patch|X.Y.Z>"
ARG="$1"

# read_semver <file> <regex> — extract the first capture group matching a semver.
current_workspace() {
  perl -ne 'if (/^\s*version\s*=\s*"(\d+\.\d+\.\d+)"/) { print $1; exit }' Cargo.toml
}
current_plugin() {
  perl -ne 'if (/"version"\s*:\s*"(\d+\.\d+\.\d+)"/) { print $1; exit }' plugins/obsidian/manifest.json
}

# max of two dotted-triple versions
vmax() {
  printf '%s\n%s\n' "$1" "$2" | sort -t. -k1,1n -k2,2n -k3,3n | tail -1
}

WS="$(current_workspace)"; [ -n "$WS" ] || die "could not read workspace version from Cargo.toml"
PL="$(current_plugin)";    [ -n "$PL" ] || die "could not read plugin version from manifest.json"
CUR="$(vmax "$WS" "$PL")"

case "$ARG" in
  major|minor|patch)
    IFS=. read -r MA MI PA <<<"$CUR"
    case "$ARG" in
      major) MA=$((MA+1)); MI=0; PA=0 ;;
      minor) MI=$((MI+1)); PA=0 ;;
      patch) PA=$((PA+1)) ;;
    esac
    NEW="${MA}.${MI}.${PA}"
    ;;
  [0-9]*.[0-9]*.[0-9]*)
    [[ "$ARG" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "invalid version: $ARG"
    NEW="$ARG"
    ;;
  *)
    die "unrecognized argument: $ARG (want major|minor|patch|X.Y.Z)"
    ;;
esac

# Guard against regressions: the new version must be > the current max.
if [ "$(vmax "$CUR" "$NEW")" = "$CUR" ] && [ "$CUR" != "$NEW" ]; then
  die "refusing to set $NEW: it is lower than the current $CUR"
fi

echo "bump-version: $CUR -> $NEW (workspace was $WS, plugin was $PL)" >&2

# --- TOML: [workspace.package] version in the root Cargo.toml ---------------
# Only the version line inside the [workspace.package] table (not rust-version,
# not any dependency's version) — anchor on the table header.
perl -0pi -e 's/(\[workspace\.package\][^\[]*?\nversion\s*=\s*")\d+\.\d+\.\d+(")/${1}'"$NEW"'${2}/s' Cargo.toml

# --- TOML: standalone desktop/src-tauri/Cargo.toml [package] version ---------
perl -0pi -e 's/(\[package\][^\[]*?\nversion\s*=\s*")\d+\.\d+\.\d+(")/${1}'"$NEW"'${2}/s' desktop/src-tauri/Cargo.toml

# --- Cargo.lock files: the [[package]] blocks for our own crates -------------
# Path/workspace members carry their own version in the lockfile; keep it in
# sync so `cargo build --locked` stays green on the release commit.
lock_bump() {
  local lock="$1"; shift
  [ -f "$lock" ] || return 0
  for pkg in "$@"; do
    perl -0pi -e 's/(name = "'"$pkg"'"\nversion = ")\d+\.\d+\.\d+(")/${1}'"$NEW"'${2}/g' "$lock"
  done
}
lock_bump Cargo.lock asp-core asp asp-wasm asp-desktop-engine
lock_bump desktop/src-tauri/Cargo.lock asp-core asp-wasm asp-desktop-engine context-desktop

# --- JSON manifests: use node for format-preserving, correct edits ----------
node - "$NEW" <<'NODE'
const fs = require("fs");
const v = process.argv[2];
const files = [
  "desktop/src-tauri/tauri.conf.json",
  "desktop/package.json",
  "sdks/typescript/package.json",
  "plugins/obsidian/manifest.json",
  "plugins/obsidian/package.json",
];
for (const f of files) {
  const txt = fs.readFileSync(f, "utf8");
  const j = JSON.parse(txt);
  j.version = v;
  const trailing = txt.endsWith("\n") ? "\n" : "";
  fs.writeFileSync(f, JSON.stringify(j, null, 2) + trailing);
}
NODE

echo "$NEW"
