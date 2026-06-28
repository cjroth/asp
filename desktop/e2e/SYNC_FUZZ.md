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

## Backend verdict

Thousands of randomized rounds across 1/2/3-engine topologies and many seeds, with
vaults up to ~1000 files: **zero divergences.** Concurrent same-file writes from
all sides converge; renames (including swaps and case-only) propagate; unicode and
spaced paths round-trip. The `asp-core` sync path that both desktop and web share
is solid; the bugs live in the UI/refresh layer above it (findings #1, #2).
