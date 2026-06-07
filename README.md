# ASP — Agent Sync Protocol

ASP is the storage and sync layer for **agent context and a human's Markdown
vault**, shared across a person's devices and agent sessions. An agent works on
one machine, closes the session, and resumes the same context on another; a human
edits notes on a phone and a laptop and they converge; either can roll the
workspace back to any earlier moment.

The shape is git's — a versioned, content-addressed history of files — but the
*operation* is the opposite: **automatic** (capture and sync on change, no
explicit commit), **real-time** (≈1s propagation), **peer-to-peer** (the hub is
just a peer), and **convergent without a human** (every node deterministically
reaches the same state). One line: *git's content-addressed storage, with an
automatic, deterministic merge, on an embedded database.*

ASP is the successor to **csp** (Context Sync Protocol). It keeps csp's spine — a
signed/Merkle-id'd event log, a deterministic fold, real-time push, SSH-pubkey
connection auth, and a stock-git-compatible read-only derived history — and
changes what csp got expensive or wrong: the **substrate** moves from a git
object model to a **SQLite event log**, ordering becomes a **two-layer causal +
Lamport fold**, code conflicts are **surfaced** (not silently dropped), and
renames carry a **stable `file_id`** instead of delete+create.

## Architecture

One core, thin bindings. All protocol/merge/convergence logic lives in
`asp-core` and nowhere else.

```
crates/
  asp-core/        the engine (pure protocol + storage)
    log.rs         the append-only log row (Merkle-id'd, tamper-evident)
    store.rs       the SQLite substrate (log, blobs, files, authorized_keys, …)
    fold.rs        deterministic two-layer fold (causal topo + lamport tiebreak)
    merge.rs       3-way merge: text clean-resolve / code conflict-surface / binary LWW
    order.rs       NodeId + the (lamport, site_id, id) tiebreak key
    identity.rs    ed25519 identity, OpenSSH pubkey format
    authkeys.rs    admission-set logic: expiry / ttl / migration
    session.rs     the sans-IO sync state machine (handshake + catch-up + integrate)
    wire.rs        msgpack frame protocol + signed handshake transcript
    engine.rs      capture → fold → materialize, snapshots/PITR, admission
    gitexport.rs   minimal stock-git object writer (derived read-only history)
    config.rs      synced config; genesis-immutable tiebreak_key
    scope.rs       hand-rolled .aspignore matcher
  asp/             the native CLI (full node)
    main.rs        clap CLI; flag > env > config resolution
    net.rs         tokio + WebSocket driver over the sans-IO Session
    idstore.rs     device-global identity (~/.asp/id_ed25519, never synced)
    gitcli.rs      read-only `asp git` allowlist
tests/e2e/         multi-process tests against the real `asp` binary
```

### How convergence works

1. Every change to any file is one **append-only log row** targeting a stable
   `file_id` (path is a mutable attribute). The row's `id` is the SHA-256 of its
   fields — its Merkle id — so it is tamper-evident and self-deduplicating.
2. State is the **deterministic fold** of the log in a canonical order: a causal
   topological sort (a row folds only after its `parent`), with concurrent ties
   broken by `(lamport, site_id, id)`. Implemented as Kahn's algorithm over a
   min-heap — one total order, identical on every node holding the same rows.
3. Folding runs a per-`file_id` state machine: a 3-way merge against the LCA, by
   `merge_class`. The later-in-fold-order row is "theirs" and wins a same-region
   contention — identically everywhere, so nodes compute byte-identical state.
4. Live-path collisions (concurrent create, rename-into-occupied) resolve
   deterministically with a ` (n)` suffix — the identity-convergence gate.
5. Only genuine log rows cross the wire; merged state is recomputed, never synced.

### Sync & security

- The **log is the synced unit**; blobs ride along with the rows that reference
  them (causal ordering guarantees the base is present).
- Replication is the sans-IO `Session`: a mutual **ed25519 handshake** over a
  signed transcript (both nonces + advertised channel binding + vault id), then
  **version-vector catch-up** (each side sends exactly what the other lacks),
  then optimistic real-time `Push`. A hub is a peer: **forward-then-merge**.
- Admission is the load-bearing trust gate: a listener admits only peers in its
  node-local **`authorized_keys` table** (per-key expiry, listen-start default-
  fill migration). Bootstrap via **`AUTH_KEY` enrollment** (`Authorization:
  Bearer`, `?auth_key=`, or `bearer.<key>` subprotocol; 401 on mismatch) or
  **TOFU** bounded to the empty-set window (`--no-tofu` disables it).

## Build

Requirements: Rust (stable, 1.89+), a C compiler (for bundled SQLite), and `git`
on `PATH` (used only by the read-only `asp git` inspector).

```sh
cargo build --release -p asp        # the `asp` CLI → target/release/asp
cargo build --workspace             # core + CLI + e2e
```

## Test

```sh
cargo test --workspace              # 31 core unit tests + 39 multi-process e2e
```

The e2e suite spawns **real `asp` processes** in isolated temp dirs (each with
its own `$ASP_HOME` device identity) plus a listening relay, and asserts
byte-identical working trees, derived-`main` SHA convergence, genuine git
coherence, PITR, and the full auth matrix. See `tests/e2e/tests/`.

## Quick start (two devices through a relay)

```sh
# Device A: create a vault and author some notes
ASP_HOME=~/.asp-a  asp init --dir ./vault-a
echo "# plan" > ./vault-a/plan.md

# A relay/hub (a peer like any other), enrolling peers with a shared secret
ASP_HOME=~/.asp-hub  asp watch --listen --no-tls --port 9000 \
    --auth-key SECRET --dir ./hub        # prints: listening on ws://0.0.0.0:9000

# A publishes to the relay
ASP_HOME=~/.asp-a  asp sync ws://127.0.0.1:9000 --auth-key SECRET --dir ./vault-a

# Device B bootstraps from the relay
ASP_HOME=~/.asp-b  asp clone ws://127.0.0.1:9000 ./vault-b --auth-key SECRET
cat ./vault-b/plan.md                    # → "# plan"
```

For continuous real-time sync, run `asp watch --peer ws://… --auth-key SECRET`
on each device instead of one-shot `sync`.

### CLI

`asp init | clone | watch | sync | commit | key | authorize | revoke | auth
list|extend | status | snapshot | restore | log | git | scope | completions`.
Every deployment knob has a flag, an `ASP_*` env var, and (where applicable) a
config key, resolved **flag > env > config**: `--dir/ASP_DIR`,
`--no-tls/ASP_NO_TLS`, `--port/PORT`, `--auth-key/ASP_AUTH_KEY`,
`--authorized-keys/ASP_AUTHORIZED_KEYS`, `--default-key-ttl/ASP_DEFAULT_KEY_TTL`,
`--no-tofu/ASP_NO_TOFU`, `--debounce/ASP_DEBOUNCE`, `--log/ASP_LOG`,
`--debug/ASP_DEBUG`. All read/status commands support `--json`. The derived repo
is read-only: `asp git` is a deny-by-default allowlist and every mutating verb is
refused.

## Status — what's implemented

**v1 spine, fully working and e2e-covered:**

- SQLite event-log substrate; deterministic two-layer (causal + Lamport) fold;
  stable `file_id` identity with deterministic ` (n)` path-collision resolution.
- 3-way merge: text clean-resolve (no markers), code conflict-surface
  (byte-deterministic `ASP:A`/`ASP:B` markers), binary whole-file LWW; delete
  remove-wins truth table; `reclass` boundary.
- ed25519 identity + `authorized_keys` table (admission / expiry / listen-start
  migration / `AUTH_KEY` enrollment / TOFU / `--no-tofu`); mutual-auth handshake.
- Real-time WebSocket sync (`ws://`), version-vector catch-up, relay
  forward-then-merge; debounced file capture with self-write suppression,
  startup reconciliation, and content-similarity rename inference.
- Snapshots + point-in-time restore (named-exact and "as of T"); stock-git
  compatible read-only derived history with cross-node SHA convergence.
- `embeddings` table + read/write/search API (substrate only — never populated
  in v1, per spec).

**Deferred (documented as post-v1 in the spec, or secondary):**

- Transport phase 2: `wss://` self-signed TLS + live channel-binding enforcement
  (the binding-**disabled** degraded path is implemented and used behind a TLS-
  terminating proxy / `--no-tls`); **iroh** QUIC P2P.
- The wasm/TS SDK, Context Desktop (Tauri), and the Obsidian plugin — the engine
  is structured for them (sans-IO `Session`, storage trait shape) but the
  surfaces are not built here.
- `fold_cache` memoization, keyframe+diff for large binaries, tombstone/blob GC,
  frame chunking on throttled links, and the central debug-log **collector**
  (the local `--debug` source is wired; the network upload is post-v1).
- `wall_clock` tiebreak (a post-v1 offline re-fold experiment by design; v1 is
  Lamport-only and both `lamport` and `ts` are stored on every row).
