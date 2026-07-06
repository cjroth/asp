#!/usr/bin/env bash
# Rebuild the wasm engine from Rust source and refresh the copy bun placed in
# node_modules, so the browser/web build (localhost:1420, `tauri dev`, `tauri
# build`) can never drift from crates/asp-core.
#
# Why this is needed: the web frontend depends on `asp-wasm` via a `file:`
# dependency (crates/asp-wasm/pkg-web). Nothing in `tauri dev`/vite rebuilds
# that wasm — it's a prebuilt artifact — so an engine change (e.g. the wire
# PROTO bump) silently leaves the browser running a stale bundle until someone
# hand-rebuilds it. That surfaces as `proto mismatch: N != M` against a hub on
# the new proto. Running this before every web build closes the gap.
#
# Two steps, both required:
#   1. wasm-pack rebuilds pkg-web from the current Rust source.
#   2. bun COPIES `file:` deps at install time and won't notice pkg-web changed
#      unless the copy is dropped and reinstalled — so we do exactly that.
set -euo pipefail

desktop_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repo_dir="$(cd "$desktop_dir/.." && pwd)"

# 1. Rebuild the wasm engine (nodejs + web targets). Shares one cargo compile;
#    incremental, so this is ~1s when the Rust source is unchanged.
( cd "$repo_dir/sdks/typescript" && bun run build:wasm )

# 2. Force node_modules to take the freshly built pkg-web.
rm -rf "$desktop_dir/node_modules/asp-wasm"
( cd "$desktop_dir" && bun install )
