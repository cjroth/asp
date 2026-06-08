# ASP Sync Demo — Build Handoff & Engine-Integration Spec

> Status: **planning / pre-implementation.** This doc is the contract between
> (a) the agent refactoring `asp-wasm` into a single base64-embedded bundle and
> (b) the agent (future me) that builds the demo UI on top of it.
>
> Nothing here is built yet. The design source is preserved under
> `docs/asp-sync-demo/design-reference/` (the original Claude Design handoff
> bundle), because the working container is ephemeral and `/tmp` will be lost.

---

## 1. TL;DR — the decision

Build the design's **P2P sync simulator** faithfully, but back **each node with the
real `asp-core` engine compiled to wasm** instead of the design's hand-written JS
simulation. All sync happens **in-page** (byte `WireRow` frames ferried between
in-tab engines), so it's a **static site with no backend** — which is
simultaneously:

- the **public demo** you want (wasm + TS SDK, deployable anywhere), and
- an **honest debug surface** (the file trees you watch converge are the *real*
  `compute_files` fold + `merge3`, not a mock).

The key unlock: **`MemEngine` already exposes everything an in-page mesh needs**
(`version_vector`, `rows_after_wire`, `integrate`, and `record_*` already return
the authored `WireRow`). There is even a unit test — `two_mem_engines_converge`
(`crates/asp-core/src/memengine.rs:385`) — that *is* this demo in miniature:
two in-memory engines converging through wire-row exchange with a concurrent
3-way merge. We are productionizing that test into a UI.

**Required engine work is small:** ~4 thin new methods on `asp-wasm` + 1 tiny
read-accessor on `MemEngine`. No protocol/merge/fold changes to `asp-core`.

**Out of scope for v1 (agreed):** the heterogeneous "each node is a different
implementation (CLI/Obsidian/desktop)" interop harness. Fuzzing + conformance +
SDK⇄`asp` parity already prove cross-surface byte-identity far better than
hand-driving a UI. A cheap interop hook (dial a real `asp watch --listen` over
`ws://`) can be added later **without changing the architecture**, because the
existing `Vault.sync()` already does exactly that (`sdks/typescript/src/vault.ts:81`).

---

## 2. Conversation summary

1. **The ask:** implement the Claude Design file *"ASP Sync Demo.html"* — a visual
   demo of ASP's P2P sync — to (a) show users how sync works and (b) help debug.
2. **The design** (read in full, all imports traced): a polished
   *technical-blueprint dark-mode* React prototype. A top network map animates
   packets along peer edges; per-node panels carry a file tree + tiny editor + a
   raw event log that reads like real ASP output; a Tweaks panel tunes
   layout/latency/debounce/accent. It demonstrates `init` → `clone` → live
   propagation → **gossip-forward through a hub** → **offline→edit→reconnect→
   version-vector catch-up**, plus the full file-op set.
   **Its `asp-engine.js` is a *simulation*** — a JS re-implementation of the
   protocol, not the real engine.
3. **The open question (yours):** should each node run a *different* real ASP
   instance (Rust SDK / CLI / Obsidian) so the demo doubles as a cross-impl test
   bed — or, since this should become a **public website**, should each node just
   be an instance of **wasm + TS SDK + OPFS**? And given we trust tests/fuzzing,
   is the cross-impl angle even worth it?
4. **What I found (grounding the answer):** the real wasm engine is **sans-IO**
   (`connect_start()` → `feed(frame)`), and `MemEngine` exposes the catch-up
   primitives directly. So "public wasm website" and "real/debuggable" are **the
   same build**, not a trade-off — each node is a real engine, sync is real fold,
   and it all runs in one browser tab with no server.
5. **Where we landed:** real wasm engine per node (this doc), pure all-wasm,
   static site; skip the interop harness for now; OPFS persistence as a
   fast-follow. You're now having another agent refactor `asp-wasm` into a single
   `main.js` with the wasm embedded as base64 (the ideal deployable artifact),
   after which I continue the demo on top of it.

---

## 3. The design — what to build (pixel reference)

Source preserved at `docs/asp-sync-demo/design-reference/`. Build to **match the
visual output**; the React-UMD-+-Babel-in-browser structure is a prototype, not a
prescription — reimplement in whatever build fits (recommendation: a small Vite +
React + TS app importing the combined SDK; see §6/§11).

**Aesthetic / tokens** (`design-reference/project/asp.css` is the source of truth):
- Cool-navy **OKLCH** palette. Surfaces `--ink … --raised`; lines `--line/-2`.
- Accents share `L≈0.80 C≈0.115`, hue-varied: `--cyan 208`, `--green 150`,
  `--amber 78`, `--violet 292`, `--red 25` (+ `*-dim` at `/0.16`).
- Fonts: **IBM Plex Mono** (data/log/ids) + **IBM Plex Sans** (chrome).
- Blueprint **grid backdrop** (120px + 24px line layers); radii 3/5/8px.

**Layout / components** (`asp-components.jsx`, `asp-app.jsx`):
- **Top bar:** brand mark (rotated-square "tick" + `ASP`), node count, log-row
  count, Reset, **Add node**.
- **Empty state:** cornered blueprint block, eyebrow, headline, legend
  (in sync / syncing / offline / catch-up).
- **Network map** (`NetworkMap`): SVG, nodes laid out across the width, edges
  dashed when a link is down / solid+cyan when live; **packets** animate along
  edges (cyan = push, amber = catch-up) with `drop-shadow`; syncing nodes get a
  pulsing ring; node label + status sublabel.
- **Node panel** (`NodePanel`) — the redesigned **two-row header** (see the
  screenshot in `design-reference/project/uploads/`):
  - **Top row:** avatar (2-letter, node color), name (dbl-click to rename),
    **StatusPill** (`In sync / Syncing / Offline / Solo`), `Go offline`/`Reconnect`,
    focus (⤢), remove (✕).
  - **Meta row:** `SITE <id>` · `PEERS <names>` (or `CLONED ← <remote>` / `none`),
    divided by a thin rule, truncating not overlapping.
  - **Body:** `166px` file tree | flexible editor.
  - **Event log:** fixed-height, raw colored token stream.
- **File tree** (`FileTree`/`TreeRow`): folders (twist ▸/▾), files (`·` icon),
  active highlight + left cyan bar, **dirty** rows amber, drag-to-move (onto
  folder or root), inline rename, amber collision dot. Toolbar: new file (＋),
  new folder (⊞), rename (✎), delete (✕).
- **Editor** (`Editor`): tab shows `path` + `merge_class` pill + `@result_hash`;
  textarea (type → debounced commit); footer shows `file_id` + lines/bytes or a
  "staged · auto-commit…" amber indicator.
- **Event log** (`EventLog`): monospace lines `HH:MM:SS.mmm  LEVEL  tag  …tokens`.
  Token classes drive color: kinds (`create` green / `edit` cyan / `delete` red /
  `rename` violet), ops (`push/integrate/catch-up/commit` cyan), `peer/handshake`
  amber; fresh lines flash. **This grammar must read like real ASP output** —
  keep `commit / push / integrate / catch-up / handshake / anti-entropy` verbs and
  include `file_id`, `lamport`, `seq`, `base→result` hash on the relevant lines.
- **Add-node dialog** (`AddNodeDialog`): first node = `asp init`; subsequent =
  `asp clone` with a **remote picker** (radio list of existing nodes, showing
  `site_id` + `wss://name.local:9000`).
- **Tweaks panel** (`tweaks-panel.jsx`): floating panel. Controls used:
  Sync latency (80–1600ms), Commit debounce (200–2500ms), Arrange
  (columns/rows/focus), Network map toggle, Accent color, Blueprint grid toggle.
  > Note: the panel embeds a Claude-Design **host protocol** (`postMessage`
  > `__activate_edit_mode`, etc.). For the public site, **strip the host
  > protocol** and keep it as a plain in-app settings panel (or drop entirely).

**Flows to preserve** (all proven in the design's chat transcript,
`design-reference/chats/chat1.md`): add first node → genesis vault; add node →
pick remote → clone (handshake + full catch-up); live edit propagation; 3-node
chain where an edit reaches the far node **forwarded through the middle hub**;
offline → edit elsewhere → reconnect → **VV anti-entropy** delivers exactly the
missing rows; file ops edit/create/mkdir/rename/delete(tombstone, remove-wins)/
move(=rename row).

---

## 4. Architecture: real engine, simulated network

```
┌───────────────────────────── browser tab (static site) ─────────────────────────────┐
│                                                                                      │
│   React UI (design) ──reads──>  per-node snapshot {files, fileMeta, vv, rowCount}    │
│        │                                                                             │
│        │ user actions (edit/create/rename/delete/move, go offline, add/clone node)  │
│        ▼                                                                             │
│   Demo "network" orchestrator (SIMULATED layer — kept from asp-engine.js):          │
│     • edges/topology, latency, packet animation, offline queue, gossip routing,     │
│       debounce timers, event-log emission                                           │
│        │  payloads are REAL WireRows                                                 │
│        ▼                                                                             │
│   N × real WasmEngine (asp-core/MemEngine in wasm) — REAL layer:                     │
│     • record_write/remove/rename  → authors real LogRows (SHA-256 Merkle ids)       │
│     • version_vector / rows_after  → real anti-entropy diff                          │
│     • integrate                    → real compute_files fold + merge3               │
│     • files_json / files_detail    → real materialized tree + metadata             │
└──────────────────────────────────────────────────────────────────────────────────────┘
```

- **Real (protocol):** identities, log rows + Merkle ids, the fold, 3-way merge,
  collision `(n)`-suffixing, version vectors, catch-up payloads. Byte-identical to
  native `asp`.
- **Simulated (network, legitimately — it's one tab):** latency, packet visuals,
  offline link toggle + local queueing, edge topology, gossip forwarding,
  commit debounce, the event-log text. These **wrap real `WireRow` payloads**.
- **Not modeled in v1:** the real `Session` handshake **wire bytes** /
  `authorized_keys` admission (we integrate `WireRow`s directly). The handshake is
  a cosmetic log line. Real ed25519 identity is still shown (`node_ssh()`).
  Upgrading to the real in-page handshake = the deferred "full Session in wasm"
  option (listener role + multi-peer session map); not needed for the demo.

---

## 5. EXACT engine changes

Verified against current source. Two crates touched: a one-line accessor in
`asp-core`, and ~4 binding methods in `asp-wasm`. **No fold/merge/session logic
changes.**

### 5a. `crates/asp-core/src/memengine.rs` — 1 read accessor

`MemEngine.files` (the `Vec<FileRow>` fold output) is private; expose it so hosts
can render more than `path→bytes` (the merge-class pill, `@result_hash`,
`file_id`, conflict badge). `FileRow` is already in scope (imported at the top).

```rust
// impl MemEngine  (near files_map, ~line 270)

/// The materialized file rows (fold output) — surface-independent metadata for
/// hosts that render more than path→bytes (merge_class, result_hash, file_id,
/// conflict). Callers filter `deleted` as needed.
pub fn files_detail(&self) -> Vec<FileRow> {
    self.files.borrow().clone()
}
```

> Alternative that avoids *any* asp-core edit: derive merge-class and a content
> hash in JS like the prototype does. **Not recommended** — that's exactly the
> "mock drift" we're eliminating. The 3-line accessor is the honest path.

### 5b. `crates/asp-wasm/src/lib.rs` — new `WasmEngine` methods

Add imports (top `use asp_core::{…}`): **`WireRow`** and **`store::FileRow`**.
`WireRow` is re-exported at the asp-core crate root alongside `Msg` (which this
file already imports from there); if not, add `pub use wire::WireRow;` to
`asp-core/src/lib.rs`. `MergeClass`, `SessionVault`, `BTreeMap`, `serde_json` are
already imported.

```rust
#[wasm_bindgen]
impl WasmEngine {
    /// This node's version vector as JSON `{site_id: max_seq}` (catch-up cursor).
    pub fn version_vector(&self) -> Result<String, JsError> {
        let vv = SessionVault::version_vector(&self.eng).map_err(to_err)?;
        serde_json::to_string(&vv).map_err(to_err)
    }

    /// Given a *peer's* version vector (JSON `{site_id: seq}`), return the wire
    /// rows that peer is missing, as a JSON array — the exact anti-entropy /
    /// catch-up payload. (Drives live push, gossip-forward, AND reconnect.)
    pub fn rows_after(&self, peer_vv_json: &str) -> Result<String, JsError> {
        let peer_vv: BTreeMap<String, i64> =
            serde_json::from_str(peer_vv_json).map_err(to_err)?;
        let mine = SessionVault::version_vector(&self.eng).map_err(to_err)?;
        let mut out: Vec<WireRow> = Vec::new();
        for site in mine.keys() {
            let after = peer_vv.get(site).copied().unwrap_or(-1);
            let mut rows = SessionVault::rows_after_wire(&self.eng, site, after)
                .map_err(to_err)?;
            out.append(&mut rows);
        }
        serde_json::to_string(&out).map_err(to_err)
    }

    /// Integrate a JSON array of wire rows (real id-check + blob-verify + fold).
    /// Returns how many *new* rows were integrated.
    pub fn integrate(&self, wire_rows_json: &str) -> Result<usize, JsError> {
        let rows: Vec<WireRow> = serde_json::from_str(wire_rows_json).map_err(to_err)?;
        let mut n = 0;
        for wr in &rows {
            if self.eng.integrate(wr).map_err(to_err)? { n += 1; }
        }
        Ok(n)
    }

    /// Per-file metadata for rich rendering. JSON array of
    /// `{file_id, path, result_hash, merge_class, deleted, conflict}`.
    pub fn files_detail_json(&self) -> Result<String, JsError> {
        #[derive(serde::Serialize)]
        struct FileMeta<'a> {
            file_id: &'a str,
            path: &'a str,
            result_hash: Option<&'a str>,
            merge_class: &'static str,
            deleted: bool,
            conflict: bool,
        }
        let detail = self.eng.files_detail();
        let metas: Vec<FileMeta> = detail.iter().map(|f| FileMeta {
            file_id: &f.file_id,
            path: &f.path,
            result_hash: f.result_hash.as_deref(),
            // confirm variant idents (Text/Code/Binary) in asp-core/src/store.rs
            merge_class: match &f.merge_class {
                MergeClass::Code => "code",
                MergeClass::Binary => "binary",
                _ => "text",
            },
            deleted: f.deleted,
            conflict: f.conflict,
        }).collect();
        serde_json::to_string(&metas).map_err(to_err)
    }
}
```

**Optional (nice-to-have):** change `record_write/remove/rename` to *return* the
authored `WireRow` JSON (`Result<Option<String>, JsError>`) so a live edit can push
exactly the new row instead of recomputing a VV diff. `MemEngine::record_*`
already returns `Option<WireRow>` (`memengine.rs:113/160/184`) — just serialize it
instead of discarding. Not required (the `rows_after` diff model covers live push
uniformly), but cleaner and cheaper per-keystroke. If adopted, update the TS
`record_*` return type from `void` to `string | undefined` (callers that ignore it
are unaffected — e.g. `Vault.writeFile`).

### 5c. `sdks/typescript/src/engine.ts` — interface + types

```ts
export interface WireBlob { hash: string; bytes: number[] }  // serde_bytes→number[] in JSON
export interface WireRow  { row: unknown; blobs: WireBlob[] } // row = opaque LogRow
export interface FileMeta {
  file_id: string;
  path: string;
  result_hash: string | null;
  merge_class: 'text' | 'code' | 'binary';
  deleted: boolean;
  conflict: boolean;
}

export interface WasmEngineInstance {
  // …existing…
  version_vector(): string;               // JSON Record<string, number>
  rows_after(peerVvJson: string): string; // JSON WireRow[]
  integrate(wireRowsJson: string): number;
  files_detail_json(): string;            // JSON FileMeta[]
  // if the optional 5b change lands:
  // record_write(path: string, content: Uint8Array): string | undefined;
  // record_remove(path: string): string | undefined;
  // record_rename(from: string, to: string): string | undefined;
}
```

Optionally surface typed helpers (the demo can also call the engine directly):

```ts
// sdks/typescript/src/vault.ts (additions) — or a new PeerNode helper for the demo
versionVector(): Record<string, number> { return JSON.parse(this.eng.version_vector()); }
rowsAfter(peerVv: Record<string, number>): WireRow[] {
  return JSON.parse(this.eng.rows_after(JSON.stringify(peerVv)));
}
integrateRows(rows: WireRow[]): number { return this.eng.integrate(JSON.stringify(rows)); }
filesDetail(): FileMeta[] { return JSON.parse(this.eng.files_detail_json()); }
```

> The demo's per-node truth is best driven through the **low-level
> `WasmEngine`** (or a thin `PeerNode` wrapper), not the existing `Vault` —
> `Vault` is built around a single `ws://` sync and one session. Leave
> `Vault.sync()` untouched (it stays the `ws://` interop path, §1).

---

## 6. Integration contract with the combined base64 `main.js`

What the demo needs from the other agent's refactor (the single-file,
wasm-embedded bundle):

1. **Browser/web target** (`wasm-pack --target web`/`bundler`; `build-wasm.mjs`
   already emits a `pkg-web`), with the `.wasm` **inlined as base64** so there is
   **no separate `.wasm` fetch** (works from any static host / `file://`, no MIME
   or CORS setup).
2. An **idempotent async init**: `await init()` (or default export) instantiates
   the embedded wasm **once**; safe to call/await repeatedly.
3. The **`WasmEngine` class** exported, carrying **all** methods — existing
   (`record_write/remove/rename`, `commit_files`, `files_json`, `read_file`,
   `node_id`, `node_ssh`, `vault_id`, `row_count`, `connect_start`, `feed`) **plus
   the §5b additions** (`version_vector`, `rows_after`, `integrate`,
   `files_detail_json`).
4. Multiple **independent instances** in one JS context (the demo makes N of
   them). `MemEngine` is per-instance state, so this is fine — just confirm the
   bundle doesn't assume a singleton.

Acceptable export shapes (either works; tell me which you pick):
- **ESM:** `import init, { WasmEngine } from './asp.js'; await init();`
- **Global:** `window.ASP = { init, WasmEngine, /* free fns */ }` (lets the demo
  keep the prototype's script-tag style if we don't add a bundler yet).

The demo only needs `WasmEngine` (+ `init`); the free conformance fns
(`contentHash`, `merkleIdOf`, …) are not required by the UI.

---

## 7. Mapping — design's simulated engine → real engine

The design's network-orchestration layer (`asp-engine.js`) stays; its **fake
protocol core** is replaced by real-engine calls. One-to-one:

| design `asp-engine.js` (simulated) | real engine call |
|---|---|
| `makeRow()` (fnv `shortHash`, fake ids) | `eng.record_write/remove/rename()` → real `WireRow` (SHA-256 Merkle id, real blobs) |
| `materialize(node)` (fake fold + `(n)` suffix) | `eng.files_json()` + `eng.files_detail_json()` → real `compute_files` (incl. collision `conflict` + ` (n)` suffix) |
| `integrate(node,row)` (id-dedup) | `eng.integrate(rowsJson)` (real id-valid + blob verify + fold) |
| `node.vv` (`{site:seq}`) | `eng.version_vector()` |
| `syncPair()` VV diff | `from.rows_after(to.version_vector())` |
| `lamport/seqCtr`, `lastRowFor`, `hex/shortHash` | gone — engine owns clocks, ids, hashes |
| `node.id` / `site_id` badge | `eng.node_id()` (per-vault site id); real pubkey via `eng.node_ssh()` |
| `mergeClassFor(path)` (JS heuristic) | `FileMeta.merge_class` (real `classify`) |
| `result_hash` `@7dad` | `FileMeta.result_hash` (real) |
| `file_id` `f…` | `FileMeta.file_id` (real) |
| `pushRow` / gossip / latency / offline / packets / debounce | **unchanged** — the simulated network layer, now carrying real `WireRow` payloads |

**Clone semantics:** when adding a non-first node, create it with the remote's
vault id — `new WasmEngine(seed, remote.vault_id())` — then deliver
`remote.rows_after(newNode.version_vector())` (a full catch-up, since the new VV
is empty). Each node needs a distinct 32-byte `seed` (random per node).

---

## 8. Offline & catch-up semantics (real engine)

- **Offline:** the node keeps authoring real rows locally (`MemEngine` authors
  fine with no peers). The demo's offline toggle just stops the transport from
  delivering frames.
- **Queued count** (shown on the offline pill): rows a peer lacks =
  `myEngine.rows_after(peer.version_vector()).length`.
- **Reconnect:** for each reconnected peer, both directions run
  `X.rows_after(Y.version_vector())` → `Y.integrate(...)`. That delivers **exactly
  the missing rows** and re-folds — real anti-entropy, identical outcome to native.
- **Gossip / hub-as-peer:** after a node integrates rows, it re-runs the same
  `rows_after`→deliver to *its* other online peers. Convergence is order- and
  path-independent (that's the whole point of the fold), so chains/stars/meshes
  all converge.

---

## 9. Persistence (OPFS) — fast-follow, **no extra engine work**

Same three methods give free persistence:
- **Save:** `engine.rows_after("{}")` (empty peer VV) → **all** my wire rows →
  serialize to an OPFS file per node.
- **Load:** `new WasmEngine(seed, vaultId)` then `engine.integrate(savedRows)` →
  real re-fold, state restored.

v1 default matches the design: **per-session reset** (clean demo each load). OPFS
is an opt-in follow-up, not a blocker, and needs nothing beyond §5.

---

## 10. Build / deploy notes

- **Recommendation:** a small **Vite + React + TS** app under a new top-level dir
  (e.g. `demo/` — confirm naming so it doesn't collide with the wasm-refactor
  agent's tree) that imports the combined SDK and recreates the design. Static
  `vite build` → host anywhere.
- The prototype's **React-UMD + Babel-standalone** approach also works for a quick
  pass if the bundle exposes `window.ASP` (§6 global shape) — but in-browser Babel
  is dev-only; don't ship it to the public site.
- **Strip the Tweaks host protocol** (`tweaks-panel.jsx` `postMessage`
  handshakes) — keep the controls as a plain settings panel.
- Pin fonts (IBM Plex Mono/Sans) — self-host or keep the Google Fonts `<link>`.

---

## 11. Open decisions (confirm when the wasm refactor lands)

1. **Engine fidelity** — *Recommended:* the §5 "real wasm engine, minimal
   bindings" path. (Your base64-embed refactor is squarely this. The heavier
   "full real `Session` in wasm — listener role + multi-peer session map" is a
   later option, only if you want the real handshake bytes in-tab too.)
2. **Interop angle** — *Recommended:* skip for v1 (pure all-wasm). Later, a single
   "connect to a real peer" button reusing `Vault.sync(ws://…)` gives live
   CLI/Obsidian interop with no backend.
3. **Where the demo lives** — proposed `demo/` (Vite). Confirm the dir name vs.
   the wasm-refactor branch to avoid a collision.
4. **Persistence** — per-session reset for v1, OPFS as a fast-follow (§9).

---

## 12. What I verified (so this spec is trustworthy)

- `MemEngine` catch-up surface: `version_vector` `memengine.rs:320`,
  `rows_after_wire` `:330`, `integrate` `:243/342`, `record_*` return `WireRow`
  `:113/160/184`, `files` private `:49/51`, `files_map` `:270`.
- The in-page mesh already passes as a test: `two_mem_engines_converge`
  `memengine.rs:385`.
- `WasmEngine` single-session limit: `session: Option<Session>` `lib.rs:89`,
  `connect_start` always `Role::Connector` `:161`, `feed` errors without a session
  `:176`.
- `WireRow`/`WireBlob` are `Serialize`/`Deserialize` `wire.rs:17-33`;
  `FileRow{ file_id, path, result_hash, merge_class, deleted, lamport, site_id,
  conflict }` (no serde derive) `store.rs:24-33`.
- `Vault.sync()` is the existing `ws://` connector path `vault.ts:81`;
  build emits `pkg` (nodejs) + `pkg-web` (web) `scripts/build-wasm.mjs:12-15`.

## 13. File manifest

```
docs/asp-sync-demo/
  HANDOFF.md                     ← this file
  design-reference/              ← preserved Claude Design bundle (ephemeral /tmp source)
    README.md                    ← Claude Design "read me first"
    chats/chat1.md               ← the design conversation (intent lives here)
    project/
      ASP Sync Demo.html         ← entry (primary file the user had open)
      asp.css                    ← design system / tokens (source of truth)
      asp-engine.js              ← SIMULATED engine (to be replaced by §5 real engine)
      asp-components.jsx         ← React presentational components
      asp-app.jsx                ← app shell (tweaks defaults, layouts)
      tweaks-panel.jsx           ← Tweaks shell (strip host protocol for prod)
      uploads/Screenshot*.png    ← two-row node-header reference
```
