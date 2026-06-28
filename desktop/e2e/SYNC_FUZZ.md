# Cross-surface sync fuzzer & live-sync tests

Automated harness that answers the question *"if I spin up a vault with the CLI,
connect the desktop/web app, and start changing files, do they actually sync and
does the UI update?"* — across hundreds of randomized edge-case rounds, until it
can't find a bug for N rounds in a row.

It exercises the **real** stack end to end, no mocks on the sync path:

- a real `asp watch --listen` **CLI vault** (the same binary users run), and
- one or more real **`DesktopEngine`** instances (the exact backend the Tauri
  shell and the web app drive — both link `asp-core`).

## 1. `sync_fuzz` — the cross-surface convergence fuzzer

`engine/examples/sync_fuzz.rs`. It clones the CLI vault into N desktop engines,
then fuzzes file ops on **every** side (CLI disk writes that the watcher captures,
and engine-API writes that broadcast over the standing connection). After each
round it asserts **three-way convergence**:

```
CLI vault on disk  ==  each engine's dir on disk  ==  each engine's API view
                                                       (list_files / read_file —
                                                        exactly what the UI renders)
```

Byte-identical content and identical live file sets, or it's a bug. With
`--peers 2`+ it also proves **transitive** sync (engine A → CLI hub → engine B).

Scenarios: edit / new / rename / delete / delete+recreate / rapid-burst (20 fast
edits) / large file (~600 KB) / 30-file batch / concurrent same-file (every side
writes the same path → must converge to one) / empty file / deep nesting /
truncate-to-empty / **name swap** (a↔b) / rename-onto-existing / case-only rename
/ rename-then-edit. Filenames include unicode (`café-menü.md`), spaces, and
`weird_#$.md`.

```bash
# build deps first: cargo build --release -p asp && \
#                   cargo build --release -p asp-desktop-engine --example sync_fuzz
target/release/examples/sync_fuzz --seed 1 --peers 2 --rounds 250 --clean-streak 10
```

Flags: `--seed`, `--peers N`, `--rounds N`, `--clean-streak N` (stop after N clean
rounds in a row), `--timeout SECS`, `--debounce MS`. Exit 0 = clean streak with no
failures; exit 1 = a divergence or perf regression was found (details printed).
`ASP_NO_RELAY=1` is forced (hermetic, loopback dialing).

It reports convergence-latency p50/p95/max and **engine op latency** p50/max.

### Soak runner

`e2e/sync-soak.sh` builds everything and runs a battery of topologies (CLI + 1/2/3
engines) over several seeds **sequentially** and fails on the first real issue:

```bash
bash e2e/sync-soak.sh                  # seeds 1-4 × {1,2,3} engines
ROUNDS=500 bash e2e/sync-soak.sh 7 8   # custom
```

> Run it sequentially. Running many fuzzers in parallel starves the CPU; because
> the convergence check re-snapshots the **whole** vault each poll (O(files)),
> CPU starvation shows up as false `>10s converged` perf flags at ~450 files. The
> per-op engine latency in the report stays accurate (~tens of ms) and is the
> signal to trust for real engine perf.

## 2. `App.livesync.test.tsx` — UI reflects remote pushes

`src/App.livesync.test.tsx` (vitest). Mounts the real `<App/>` against a mock
backend, simulates a **remote peer push** by mutating backend state with no local
user action, and asserts the UI catches up: a peer-created file appears in the
tree, and a peer edit to the **open** file updates the editor — both without a
manual refresh. Regression cover for the bug below.

## Findings

**#1 (fixed) — the desktop UI did not reflect remote peer pushes.** The backend
converges (the standing connector materializes a peer's edits to disk + log), but
the editor-screen poll refreshed only `getStatus` every 10 s — never the file tree
(`listFiles`), the history, or the open file's bytes. Result: a file a peer
created never appeared in the tree, and a peer's edit to the file you were viewing
never showed, until the vault was reopened. Fixed in `App.tsx`: the editor poll now
also `refreshFiles` + `scheduleHistory` + re-reads the active file (only when not
dirty and on the live head, so it never clobbers unsaved local edits). Proven by
`App.livesync.test.tsx` (fails before, passes after).

**#2 (open) — the web app never pulls remote changes after the initial clone.**
The web (wasm) backend syncs one-shot during `cloneRemote` and then never again:
`api.syncNow` is not called anywhere in `App.tsx`, the upstream ticket isn't
persisted (`webApi.cloneRemote` discards it), browsers can't listen, and there is
no manual "sync" control. So on web, remote edits are simply invisible after clone.
Suggested fix: persist the upstream ticket+authKey in the web registry and have the
editor poll call `syncNow` for vaults that have one (the desktop engine gets this
for free via its persistent connector; only the web thin client needs it).

**#3 (fixed) — `rescan` and snapshot `restore` did not push their changes to
live peers.** Both `DesktopEngine` forwarders authored rows into the log but
*discarded* them: `rescan` called `eng.capture_rescan()?;` (dropping the returned
`Vec<WireRow>`) and `restore` called `eng.restore(target)?;` likewise — while the
structurally-identical `create_dir` broadcasts its `capture_rescan` rows. Effect: a
user who edits files behind the engine (external editor, `git pull`, a script) and
hits refresh, or restores a snapshot, sees it locally but **connected peers stay
stale** until some unrelated event triggers a sync. The bug sat in the surface the
cross-surface fuzzer never exercised (it only wrote via `write_file`). Fixed in
`lib.rs`: both now broadcast their authored rows like `write_file`/`create_dir`.
Proven by `desktop/engine/tests/sync_surface_probes.rs` (rescan + restore probes
fail before, pass after; `restore_file_at` is the passing control that isolates the
bug to the missing broadcast). The fuzzer now has an `ExternalRescan` scenario
(write straight to an engine's vault dir, then `rescan`) so the regression is
guarded by the soak battery.

**#4 (fixed) — the ignore scope froze at `Engine::open`; mid-session `.aspignore`
changes were silently ignored.** `Engine::reload_scope()` existed but had **zero
callers anywhere in the source** — scope was loaded once in `Engine::open` and
never refreshed. A long-running engine (`asp watch` or the desktop engine) that
gained or changed an `.aspignore` — edited locally, or **pushed by a peer** (it's a
normal in-scope synced file) — kept authoring and syncing files the new rules say
to ignore. Peers could thus disagree on what's in scope → divergent synced sets.
(`asp scope` looked correct only because it opens a throwaway engine per call.)
Fixed in `asp-core/engine.rs`: scope moved behind a `RefCell` (Engine is already
`!Sync`, like the `batch` `Cell`) and reloaded at the two disk chokepoints —
`capture_rescan` start (external/rescan edits) and `materialize` end when
`.aspignore` was written/removed (local-API + peer-push). Made non-destructive: a
file that *becomes* ignored but still exists on disk is dropped from management,
not tombstoned (no surprise delete-on-all-peers). Proven by
`asp-core/tests/disk_capture.rs::aspignore_added_after_open_takes_effect_without_reopen`
(fails before, passes after) and the cross-surface
`sync_surface_probes2.rs::aspignore_added_mid_session_takes_effect_both_sides`
(local API both directions, external+rescan, and peer-push reload).

**#5 (fixed) — the desktop engine ignored `ASP_RELAY_URL`.** The CLI honors
`--relay-url`/`ASP_RELAY_URL` to pin a self-hosted `asp relay`, but the desktop
engine only ever called `iroh_net::bind_endpoint(seed, relays)` and
`ticket(ep, relays)` — the no-relay-URL variants (`bind_endpoint` hardcodes
`relay_url: None`). So a NAT'd desktop user who configured their own relay got
silently dropped onto the public n0 relays (or nothing, under `ASP_NO_RELAY=1`),
and the tickets it minted never advertised the relay — undiallable for a peer that
needs it. Fixed in `lib.rs`: a `relay_url()` helper reads `ASP_RELAY_URL` and feeds
`bind_endpoint_relay` + `ticket_with_relay` at every binding site (clone, connector,
listener, one-shot sync). Proven by
`sync_surface_probes2.rs::ticket_advertises_configured_relay_url` (the minted ticket
carries only a direct IP before the fix; the configured relay after).

### Coverage added this round (surface expansion, no bug)

- **auth wrong-key rejection** — a desktop clone with the wrong auth key is rejected
  and leaks no vault content; the right key converges (`wrong_auth_key_is_rejected_and_leaks_no_data`).
- **reconnect catch-up** — disconnect a peer (`remove_vault`), accumulate a batch of
  edits while it's gone, reconnect by re-cloning → version-vector catch-up pulls the
  whole batch with no loss (`reconnect_after_disconnect_catches_up_accumulated_edits`).
  Note: true *simultaneous bidirectional* offline editing isn't simulable in-process —
  the desktop API has no "pause sync but keep the folder writable" primitive
  (`set_allow_connections(false)` only stops *new* accepts; `set_enabled` is a UI flag).
- **scale** — 5000-file clone converges; capture ~3.5s, clone+converge ~4.2s over
  loopback, no cliff past the fuzzer's ~1000 (`clone_at_scale_5000_files_converges`).
- **relay topology** is already exercised transitively by the fuzzer's `--peers 2/3`
  (engine → CLI hub → engine) and by the CLI e2e (`relay_topology.rs`/`relay_cohost.rs`).

## Backend verdict

Thousands of randomized rounds across 1/2/3-engine topologies and many seeds, with
vaults up to ~1000 files: **zero divergences.** Concurrent same-file writes from
all sides converge; renames (including swaps and case-only) propagate; unicode and
spaced paths round-trip. The `asp-core` sync path that both desktop and web share
is solid; the bugs live in the engine forwarders, the scope/ignore plumbing, and
the UI/refresh + relay-config layer above it (findings #1–#5) — specifically in
entry points the fuzzer hadn't exercised, which is why expanding the *surface*
(not just re-running) is what surfaces them. Every confirmed bug so far sat in a
desktop-engine forwarder or wiring gap, never in the `asp-core` convergence core.
