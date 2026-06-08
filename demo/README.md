# ASP Sync Demo

An in-browser, **backend-free** peer-to-peer sync demo for the Agent Sync
Protocol. Spin up nodes, clone them from a peer, edit files, take a node
offline, and watch edits propagate and converge — live.

**Each node is the real `asp-core` engine** compiled to WebAssembly (`WasmEngine`
from `@asp/sdk`). The protocol is real: append-only log rows with SHA-256 Merkle
ids, the deterministic two-layer fold, 3-way merge, stable `file_id` identity,
version vectors, and catch-up all run in the browser — byte-identical to the
native `asp` daemon. Only the **network** is simulated (latency, packet
animation, the offline link, gossip routing, commit debounce); it moves real
`WireRow` payloads between in-tab engines via `rows_after()` → `integrate()`.

This is both a **public demo** of how sync feels and an honest **debug surface**:
what you watch converge is the real fold, not a mock.

## Run

```sh
# 1. Build the wasm engine (writes crates/asp-wasm/pkg-web). Needs wasm-pack.
bun run build:wasm          # = (cd ../sdks/typescript && bun run build:wasm)

# 2. Bundle the demo (esbuild inlines the web wasm as base64 → one main.js)
bun install
bun run build               # → demo/dist (index.html + main.js + main.css)

# 3. Preview
bun run serve               # http://localhost:5173
```

`bun run dev` rebuilds without minification (inline sourcemap).

## What it demonstrates

- **`init`** — the first node creates a genesis vault with seed files.
- **`clone`** — a new node picks a remote, runs a (cosmetic) handshake + a real
  full version-vector catch-up.
- **live propagation** — edit on one node; it debounces into a commit row and
  pushes to peers.
- **gossip / hub-as-peer** — on a chain `A — B — C`, an edit on `A` reaches `C`
  forwarded through `B`.
- **offline-first** — take a node offline, edit elsewhere, reconnect → anti-
  entropy delivers exactly the missing rows.
- **file ops** — edit, create, new folder, inline rename, delete (tombstone,
  remove-wins), move (drag → rename row).

Settings (bottom-right) tune sync latency, commit debounce, layout
(columns/rows/focus), accent, and the network map/grid — the same knobs as the
original design.

## Layout

```
src/
  main.tsx              entry: decode inlined wasm → initAsp(bytes) → render
  asp.css               design system (blueprint dark) + settings panel
  engine/network.ts     the demo "network": real WasmEngine nodes + simulated transport
  ui/
    App.tsx             app shell, layouts, wiring
    components.tsx      NodePanel, FileTree, Editor, EventLog, NetworkMap, AddNodeDialog…
    settings.tsx        self-contained settings panel (no design-tool host protocol)
build.mjs               esbuild build (inlines pkg-web wasm as base64)
serve.mjs               minimal static server
test/                   headless checks (engine convergence, web glue, SSR render)
```

## Tests

```sh
bun test/mesh.test.mjs      # real-engine convergence: clone, gossip-via-hub, offline catch-up, concurrent merge
bun test/web-glue.test.mjs  # the browser path: web-target wasm init from inlined bytes + clone/converge
bun test/ui-smoke.tsx       # server-render the component tree against the real engine
```

See `../docs/asp-sync-demo/HANDOFF.md` for the design provenance and the
engine-integration spec, and `../docs/asp-sync-demo/design-reference/` for the
original Claude Design prototype this is built from.
