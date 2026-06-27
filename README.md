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
    iroh_net.rs    native iroh (QUIC) driver over the sans-IO Session; relay server
    iroh_wasm.rs   the browser iroh driver (relayed; one owned future, wasm-bindgen)
    net.rs         shared driver helpers (conns/fanout, debounced fs watcher)
    memengine.rs   the wasm-safe in-memory engine (thin node, same fold/Session)
  asp/             the native CLI (full node)
    main.rs        clap CLI; flag > env > config resolution
    idstore.rs     device-global identity (~/.asp/id_ed25519, never synced)
    gitcli.rs      read-only `asp git` allowlist
  asp-wasm/        wasm-bindgen surface over asp-core (the one engine in wasm)
sdks/typescript/   @asp/sdk — Vault over the wasm engine (iroh-in-wasm transport)
plugins/obsidian/  Context for Obsidian — reference thin-client over @asp/sdk
desktop/engine/    asp-desktop-engine — multi-vault manager linking asp-core
desktop/src-tauri/ Context Desktop — Tauri shell over the engine (+ React UI)
tests/e2e/         multi-process tests against the real `asp` binary
```

One engine, every surface: the native CLI/desktop link `asp-core` directly; the
SDK + Obsidian plugin drive the *same* engine compiled to wasm. A wasm node
computes byte-identical fold/merge/state to native — proven by the SDK
conformance vectors and the SDK⇄real-`asp` parity e2e.

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

## Download & install

Grab the latest build from the
[**Releases**](https://github.com/cjroth/asp/releases/latest) page. Every release
ships the desktop app, the `asp` CLI, the TypeScript SDK, the wasm packages, and
the Obsidian plugin.

> **Heads-up: the builds are not code-signed.** They're plain unsigned binaries,
> so macOS Gatekeeper and Windows SmartScreen will warn that the developer is
> "unidentified" / the app is "unrecognized." That's the OS being cautious about
> *any* unsigned download — not an actual malware detection. The one-time steps
> below tell each OS to trust it. (Signing them properly is possible later — it
> needs paid Apple/Windows developer certs.)

### Desktop app

**macOS** — download `Context.Desktop_<version>_universal.dmg` (runs on both
Apple Silicon and Intel), open it, and drag the app to Applications. On first
launch Gatekeeper blocks it because it's unsigned. Get past it once:

- **Right-click** (Control-click) the app in Applications → **Open** → **Open**
  in the dialog, **or**
- launch it once (it gets blocked), then go to **System Settings → Privacy &
  Security**, scroll down, and click **Open Anyway**, **or**
- from a terminal, clear the quarantine flag:
  ```sh
  xattr -dr com.apple.quarantine "/Applications/Context Desktop.app"
  ```

After that first approval it opens normally forever.

**Windows** — download `Context.Desktop_<version>_x64_en-US.msi` (or the
`_x64-setup.exe` NSIS installer) and run it. SmartScreen may show "Windows
protected your PC" — click **More info → Run anyway** (unsigned-installer
warning, one time).

**Linux** — download the package for your distro and install it:

```sh
# Debian / Ubuntu
sudo dpkg -i Context.Desktop_<version>_amd64.deb || sudo apt-get -f install

# Fedora / RHEL / openSUSE
sudo rpm -i Context.Desktop-<version>-1.x86_64.rpm
```

(No AppImage is published — the `.deb`/`.rpm` cover the mainstream desktops.)

### `asp` CLI

Download the archive for your platform, unpack it, and put `asp` on your `PATH`:

| Platform | Asset |
| --- | --- |
| Linux x86-64 | `asp-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| macOS (Apple Silicon) | `asp-<version>-aarch64-apple-darwin.tar.gz` |
| macOS (Intel) | `asp-<version>-x86_64-apple-darwin.tar.gz` |
| Windows x86-64 | `asp-<version>-x86_64-pc-windows-msvc.zip` |

```sh
tar xzf asp-<version>-x86_64-unknown-linux-gnu.tar.gz
sudo install asp-<version>-x86_64-unknown-linux-gnu/asp /usr/local/bin/asp
asp --version
```

On macOS the downloaded binary is quarantined too; clear it before first run:

```sh
xattr -d com.apple.quarantine ./asp   # then: chmod +x ./asp && ./asp --version
```

### SDK, wasm, and the Obsidian plugin

- **TypeScript SDK** — `asp-sdk-<version>.tgz`; install with
  `npm install ./asp-sdk-<version>.tgz` (or `bun add ./asp-sdk-<version>.tgz`).
- **wasm packages** — `asp-wasm-<version>.tar.gz` contains both the `nodejs`
  (`pkg/`) and `web` (`pkg-web/`) targets.
- **Obsidian plugin** — `main.js` + `manifest.json`; drop them into
  `<your vault>/.obsidian/plugins/agent-sync/`, or install via
  [BRAT](https://github.com/TfTHacker/obsidian42-brat) pointing at this repo.

Once you have the CLI, jump to [Quick start](#quick-start-two-devices-dialed-by-key-over-iroh).

## Build

Requirements: Rust (stable, 1.91+, for iroh 1.0), a C compiler (for bundled
SQLite), and `git` on `PATH` (used only by the read-only `asp git` inspector).

```sh
cargo build --release -p asp        # the `asp` CLI → target/release/asp
cargo build --workspace             # core + CLI + e2e
```

## Test

```sh
cargo test --workspace                              # core unit + multi-process e2e + desktop engine
# the one engine, in wasm (iroh's wasm backend needs the getrandom cfg):
RUSTFLAGS='--cfg getrandom_backend="wasm_js"' \
  cargo build -p asp-core --target wasm32-unknown-unknown
cd sdks/typescript && bun run build:wasm && bun test      # conformance + SDK⇄asp parity (iroh-in-wasm)
cd plugins/obsidian && bun test                           # plugin ⇄ asp parity
```

The Rust e2e suite spawns **real `asp` processes** in isolated temp dirs (each
with its own `$ASP_HOME` device identity) plus a listening relay, and asserts
byte-identical working trees, derived-`main` SHA convergence, genuine git
coherence, PITR, and the full auth/transport matrix. The SDK + plugin suites
spawn the real binary too and drive a wasm node against it. See `tests/e2e/`,
`sdks/typescript/test/`, and `plugins/obsidian/test/`.

## Quick start (two devices, dialed by key over iroh)

```sh
# Device A: create a vault and author some notes
ASP_HOME=~/.asp-a  asp init --dir ./vault-a
echo "# plan" > ./vault-a/plan.md

# An always-on hub (a peer like any other), enrolling peers with a shared secret.
# It prints a connection TICKET (and a scannable QR) on start — share it.
ASP_HOME=~/.asp-hub  asp watch --listen --auth-key SECRET --dir ./hub
#   → ticket: endpointaaaa…  (paste this on the other devices)

# A publishes to the hub (dial by ticket; no IP/port, no TLS to configure)
ASP_HOME=~/.asp-a  asp sync <TICKET> --auth-key SECRET --dir ./vault-a

# Device B bootstraps from the hub
ASP_HOME=~/.asp-b  asp clone <TICKET> ./vault-b --auth-key SECRET
cat ./vault-b/plan.md                    # → "# plan"
```

iroh dials a node by its **public key** (the device's ed25519 identity): a direct
hole-punched link when the network allows, a relay only for setup / last resort.
For continuous real-time sync, run `asp watch --peer <TICKET> --auth-key SECRET`
on each device instead of one-shot `sync`. To self-host relay infrastructure run
`asp relay` (a stateless forwarder — stores and sees nothing).

### CLI

`asp init | clone | watch | sync | relay | ticket | commit | key | authorize |
revoke | auth list|extend | status | snapshot | restore | log | git | scope |
completions`. Every deployment knob has a flag, an `ASP_*` env var, and (where
applicable) a config key, resolved **flag > env > config**: `--dir/ASP_DIR`,
`--listen/ASP_LISTEN`, `--relay-url/ASP_RELAY_URL`, `--no-relay/ASP_NO_RELAY`,
`--auth-key/ASP_AUTH_KEY`, `--authorized-keys/ASP_AUTHORIZED_KEYS`,
`--default-key-ttl/ASP_DEFAULT_KEY_TTL`, `--no-tofu/ASP_NO_TOFU`,
`--debounce/ASP_DEBOUNCE`, `--log/ASP_LOG`, `--debug/ASP_DEBUG`. All read/status
commands support `--json`. The derived repo is read-only: `asp git` is a
deny-by-default allowlist and every mutating verb is refused.

## Status — what's implemented

**The engine, fully working and e2e-covered:**

- SQLite event-log substrate; deterministic two-layer (causal + Lamport) fold;
  stable `file_id` identity with deterministic ` (n)` path-collision resolution.
- 3-way merge: text clean-resolve (no markers), code conflict-surface
  (byte-deterministic `ASP:A`/`ASP:B` markers), binary whole-file LWW; delete
  remove-wins truth table; `reclass` boundary.
- ed25519 identity + `authorized_keys` table (admission / expiry / listen-start
  migration / `AUTH_KEY` enrollment / TOFU / `--no-tofu`); mutual-auth handshake.
- Transport: **iroh** (QUIC, dial-by-key) — direct hole-punched links with relay
  fallback, always end-to-end encrypted; the device key is the iroh `NodeId`;
  ticket/`NodeId` addressing with QR pairing; a self-hostable `asp relay`; browser
  nodes run iroh-in-wasm over a relay. Version-vector catch-up, optimistic push,
  hub forward-then-merge; debounced capture with self-write suppression, startup
  reconciliation, rename inference.
- Snapshots + point-in-time restore (named-exact and "as of T"); stock-git
  compatible read-only derived history with cross-node SHA convergence.
- `embeddings` table + read/write/search API (substrate only — never populated
  in v1, per spec).

**All surfaces, one engine:**

- **`asp` CLI** (native full node) — the whole command surface, multi-process
  e2e against the real binary.
- **wasm/TS SDK** (`@asp/sdk`) — the engine compiled to wasm; conformance
  (byte-identical to native vectors) + SDK⇄real-`asp` parity.
- **Obsidian plugin** — reference thin client over the SDK; bridge/controller
  converge with the real CLI headlessly.
- **Context Desktop** — Tauri shell over `asp-desktop-engine` (one engine per
  folder, linked natively); the engine's in-process convergence is tested.

**Deferred (documented as post-v1 in the spec, or secondary):**

- `fold_cache` memoization, keyframe+diff for large binaries, tombstone/blob GC,
  frame chunking on throttled links, and the central debug-log **collector**
  (the local `--debug` source is wired; the network upload is post-v1).
- `wall_clock` tiebreak (a post-v1 offline re-fold experiment by design; v1 is
  Lamport-only and both `lamport` and `ts` are stored on every row).
- The Obsidian *mobile* (Capacitor WebView) wasm-inlining bundle and the Desktop
  tray/menu polish — the desktop path uses the nodejs-target wasm / native engine.
