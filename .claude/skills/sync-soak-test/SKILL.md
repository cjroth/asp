---
name: sync-soak-test
description: Stress-test and verify cross-surface vault sync for the ASP desktop/web app — spin up a real `asp watch --listen` CLI vault, connect real desktop engines, fuzz file operations on every side, and assert convergence + that the UI live-updates. Use when changing anything under crates/asp-core, desktop/engine, the Tauri shell, or the vault-editor UI (desktop/src), or when asked to find sync bugs, reproduce a "changes don't show up / don't sync" report, or confirm CLI↔app↔app convergence.
---

# Cross-surface sync soak testing

The ASP sync path is shared by three surfaces that all link `asp-core`: the `asp`
CLI, the desktop Tauri shell (via `asp-desktop-engine`), and the web app (via
`asp-wasm`). A sync change is only proven when a **real CLI vault**, a **real
desktop engine**, and the **UI that renders engine state** all agree. This skill
drives that end to end and hunts for divergence, perf regressions, and stale-UI
bugs.

Two layers, run both:

## Layer 1 — backend convergence fuzzer (where sync bugs live)

`desktop/engine/examples/sync_fuzz.rs` spins up a real `asp watch --listen` CLI
vault and N real `DesktopEngine`s, clones, then fuzzes ops on every side and after
each round asserts three-way convergence: **CLI disk == each engine's disk == each
engine's API view** (`list_files`/`read_file`, i.e. exactly what the UI renders).
`--peers 2`+ proves transitive sync (engine A → CLI hub → engine B).

```bash
# from repo root (/home/chris/asp)
cargo build --release -p asp --bin asp
cargo build --release -p asp-desktop-engine --example sync_fuzz
target/release/examples/sync_fuzz --seed 1 --peers 2 --rounds 250 --clean-streak 10
# battery across topologies (CLI + 1/2/3 engines):
bash desktop/e2e/sync-soak.sh
```

Flags: `--seed`, `--peers N`, `--rounds N`, `--clean-streak N` (stop after N clean
rounds in a row — the "found no bug for N tries" bar), `--timeout SECS`,
`--debounce MS`. Exit 0 = clean streak, no failures. Exit 1 = a divergence or perf
flag (the report prints which file diverged and on which surface).

Scenarios covered: edit / new / rename / delete / delete+recreate / rapid 20-edit
burst / ~600 KB file / 30-file batch / concurrent same-file from every side /
empty file / deep nesting / truncate / name swap (a↔b) / rename-onto-existing /
case-only rename / rename-then-edit, with unicode, spaced, and `weird_#$`
filenames. Add new edge cases as `Scenario` variants in `apply_scenario`.

### Running it well (learned the hard way)

- **Run sequentially, not in parallel.** Each run spawns a CLI `asp watch`
  subprocess; many in parallel starve the CPU. The convergence check re-snapshots
  the *whole* vault each poll (O(files)), so under CPU starvation a converged
  round can blow past the 10s perf threshold and **false-flag** at ~450 files.
  Trust the **engine op latency** line in the report (p50 ~tens of ms) for real
  perf; treat a lone `>10s converged` flag under load as a harness artifact and
  re-run that seed alone to confirm.
- **Reap orphans.** If you `kill -9` a run, its CLI hub leaks. Clean up with
  `kill -9 $(pgrep -x asp)` (match the exact `asp` comm — `pgrep -f asp` matches
  the repo path and lies).
- It forces `ASP_NO_RELAY=1` (hermetic loopback dialing); no network needed.
- **Rebuild the release binary first** — harnesses shell out to
  `target/release/asp`, which goes stale silently (it once predated
  `relay --git-proxy` entirely): `cargo build --release -p asp`.
- **This VM (OrbStack, 11 GiB) is memory- and load-constrained**: cargo is
  capped at `jobs=4` (`~/.cargo/config.toml` — leave it), run ONE build/test at
  a time, and expect relay-path e2e (iroh QUIC) to flake under load with no code
  change — `concurrent_merge` fails even on a clean HEAD here. Before treating
  a soak/e2e failure as a regression, baseline the same test in a clean-HEAD
  worktree (see the `verification-playbook` skill's flaky-environment protocol).
  Long soaks belong on CI or a beefier box; short seeds are fine here.

## Layer 2 — UI live-update (where stale-UI bugs live)

The backend converging is necessary but not sufficient: the desktop app must
*show* a peer's pushed change. `desktop/src/App.livesync.test.tsx` mounts the real
`<App/>` against a mock backend, simulates a remote push by mutating backend state
with **no local user action**, then asserts the tree gains the new file and the
open editor reflects a peer edit — both without a manual refresh.

```bash
cd desktop && bunx vitest run src/App.livesync.test.tsx
```

The pattern for any "does the UI reflect backend change X" test: mock `./lib/api`
with a mutable `CONTENT` map, open the vault, mutate `CONTENT` directly (that's
what a peer push does), then `waitFor` the DOM to catch up via the app's ~10s
poll. Give such tests a 15s timeout.

To exercise the real renderer (the same WebKit engine Tauri ships) at scale:
`cd desktop && bun run build:web && bash e2e/web-run.sh 800`. Note its
`select-file` step is flaky (the virtualized tree recycles DOM nodes on scroll,
staling the driver's node ref) independent of app changes — judge a run by the
earlier steps (load/open/virtualized-rows/history-tick-cap), not that one.

## Actively expand the surface — don't just re-run

The fuzzer is generative (random seeds/op sequences explore a large state space),
but its *surface* is bounded by the scenario list and topology. Running it green
proves no regression in **covered** behavior — it says nothing about **uncovered**
behavior. When the task is "find bugs / push to the limits" (not just "check my
change didn't regress"), spend most of the effort growing the surface:

1. **Diff coverage against the real API.** List every public method on
   `DesktopEngine` (`grep 'pub fn' desktop/engine/src/lib.rs`) and every CLI
   subcommand (`crates/asp/src/main.rs`). Anything the fuzzer never calls is a
   blind spot. Known gaps as of this writing, each worth a new scenario or a
   focused test:
   - **snapshot / restore** and **time-travel** (`read_file_at`, `restore_file_at`)
     *while sync is concurrently mutating the vault*.
   - **auth**: `authorize`, revoke, TTL expiry, TOFU vs `--auth-key`, a peer
     presenting the wrong key (must be rejected, not silently dropped).
   - **offline → reconnect → catch-up**: drop a peer mid-edit, keep editing both
     sides, reconnect, assert version-vector catch-up converges (the CLI side has
     `tests/e2e/clone_catchup.rs` to mirror; the desktop engine path is thinner).
   - **`rescan` / external edits**: mutate a file on disk *behind* the engine and
     confirm `rescan` captures it and it syncs.
   - **relay topology**: `--relay` co-hosted and a standalone `asp relay`, so the
     ticket routes through a relay instead of direct loopback.
   - **scope**: `.aspignore` rules, files outside scope, `.asp/` never syncing.
   - **the web wasm+OPFS path**: persistence across reload, one-shot `sync`
     semantics, and the fact that web never auto-syncs after clone (below).
   - **scale**: 5000+ files, deep trees, many peers — watch convergence latency.

2. **Add the case, then prove it fails first.** New scenario in `apply_scenario`,
   or a new focused `#[test]` in `desktop/engine/tests/` / `vitest` file. For a
   suspected bug, write the assertion of *correct* behavior and confirm it fails
   before fixing — a green test that never failed proves nothing.

3. **Vary the adversary, not just the seed.** Change interleaving (apply op then
   sync vs sync mid-op), timing (debounce 0 vs 500ms), and direction (CLI-origin
   vs engine-origin vs both-at-once). Many sync bugs only appear at one ordering.

4. **Loop until a real streak.** Keep adding/attacking until the fuzzer clears
   ~10+ clean rounds in a row across the *expanded* surface — then report what new
   coverage was added, not just "tests pass."

## Layer 3 — git bridge in the loop (git-bridge spec §10 "Soak")

When the change touches the **git bridge** (`crates/asp-core/src/git*.rs`, the
`asp git`/`asp clone` CLI, or the desktop/web git UI), a converging ASP mesh is
only half the story: a git remote is bridged into that mesh, so the soak must also
prove the **ASP fold and the git remote stay byte-consistent** while edits land on
*both* sides. Model: the git remote is just another peer, so this is the same
convergence bar with git as one more writer.

### A — engine-level property soak (runnable now)

The heavy lifting already ships as deterministic property/round-trip tests over the
hermetic `git http-backend` fixture (`tests/e2e/src/gitfix.rs` — real wire bytes, no
network). Run them under a clean-streak bar:

```bash
bash desktop/e2e/git-soak.sh          # loops the git-bridge battery ROUNDS times
ROUNDS=10 bash desktop/e2e/git-soak.sh
```

What each covers (grep `tests/e2e/tests/git_*.rs`):

- **`git_convergence_prop`** — the core soak: two bridge nodes, random interleaving
  of local edits + plan authoring + upstream ingests → converged fold **and**
  byte-identical synthesized commit SHAs. This is the "any node may bridge"
  guarantee and the git analogue of the Layer-1 fuzzer.
- **`git_push`** — clone → edit → plan → push → inspect with system `git`; then an
  upstream commit → pull → push again → linear history (round-trip fidelity:
  ancestry, modes, symlinks, renames).
- **`git_ingest_race`** — two bridges ingest the same commit → content identical,
  single collapsed marker (the §4.3 double-ingest race).
- **`git_policy`** — `interval` auto-plan + duplicate-plan guard, and the
  `asp git diff` / `asp git plan` LLM-hook primitives.
- **`git_clone_pull`** / **`git_genesis`** — two independent clones of one repo
  converge to identical rows + `vault_id` (the "paste the same URL on two machines"
  property).

Expand it the same way as Layer 1: add a fixture in `gitfix.rs` (criss-cross,
foxtrot, octopus, merge-into-side, renames-across-merge, mid-history-root are there;
add hostile real-world shapes) and a new op to the `git_convergence_prop` interleave
(e.g. a `rebaseline` after a `force_rewrite_tip`, a rename racing an upstream edit).

### B — live CLI-mesh follow (scenario; needs a git host)

The full cross-surface scenario the sync-soak skill exists for — a real `asp watch`
process bridging a git remote while a second ASP peer follows over iroh:

1. **Remote:** a bare git repo with a few commits (the `git http-backend` shim, or a
   throwaway private GitHub/Gitea repo for a real run).
2. **Bridge:** `asp clone <git-url> ./bridge` then `asp watch --listen --dir ./bridge`
   (pull tick + interval policy tick run inside the loop).
3. **Follower:** `asp clone <bridge-ticket> ./follow` then `asp watch --peer …` — a
   pure ASP peer that never speaks git; it must receive git updates purely over ASP
   sync (bridges write everything into the log).
4. **Fuzz both writers:** edits/renames/deletes on `./bridge` **and** `./follow`
   (ASP side) interleaved with commits pushed to the remote by a third clone (git
   side); periodically `asp git pull` + `asp git push -m …` on the bridge.
5. **Assert convergence:** `./bridge` disk == `./follow` disk == the git remote's
   HEAD tree (`git archive` the tip and diff), and `asp git status` shows
   ahead/behind settling to 0 after a quiescent pull+push cycle. A racing upstream
   `force_rewrite_tip` must freeze the bridge and clear via `asp git rebaseline`.

**Why this layer isn't run in the hermetic loop yet:** the native git transport is
**HTTPS/SSH-only** — a loopback plain-HTTP server is refused by design (SSRF
hardening in `gitwire.rs`/`gitproxy.rs`). The engine-level tests in Layer A reach
the fixture through the crate-internal `clone_from_git(spec, …)` API (which accepts
the fixture's `http://127.0.0.1` base); the *CLI* path can't, so Layer B needs a
reachable `https://` git host (a scratch private repo) or the crate-internal test
transport. Wire it as a Rust example beside `desktop/engine/examples/sync_fuzz.rs`
(reuse `gitfix::{GitHttpServer, advance_tip, force_rewrite_tip}` +
`gitremote::{clone_from_git, pull_once}` + `gitpush::push`) rather than shell, so it
can use that internal transport hermetically. The web follower variant (a web vault
also connected to the native bridge, getting git updates with zero browser git
traffic) rides on the existing Layer-2 UI harness.

## Where the bugs actually are

Across thousands of fuzz rounds the `asp-core` sync path showed **zero
divergences** — it's solid. The bugs are in the UI/refresh layer above it:

- **The editor poll must refresh the tree + history + open-file bytes, not just
  status.** If it only polls `getStatus`, the backend converges but the UI shows a
  stale snapshot until the vault is reopened (this was a real, fixed bug — see the
  poll in `App.tsx` and `refreshActiveContent`). When re-reading the open file,
  guard on `!dirtyRef.current` and the live head so you never clobber unsaved edits.
- **Web (wasm) never auto-syncs after the initial clone** — `api.syncNow` isn't
  called anywhere, the upstream ticket isn't persisted, and browsers can't listen.
  If asked to make web live-sync work, persist the upstream ticket+authKey in the
  web registry and call `syncNow` from the poll for vaults that have one.

Full write-up: `desktop/e2e/SYNC_FUZZ.md`.
