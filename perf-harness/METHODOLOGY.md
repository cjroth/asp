# How we made the Vault Editor fast (and how to reproduce it)

The app started out unusable on real vaults: opening a large folder froze, every
keystroke took ~3s, adding/deleting files did "random stuff." This is the exact
loop we used to fix it, plus every change with before/after numbers, so you can
re-run the process on any future regression.

## The loop

1. **Reproduce at real scale, in the real engine.** Build the frontend, serve it,
   and drive it in real WebKit (`web-run.sh`) with a mock backend holding 1000s of
   files / a large file / 1000s of history events. Measure one interaction at a
   time (open, type, create, delete, scroll, switch, scrub).
2. **Find the bottleneck by category.** When a step is slow or wrong, ask which
   layer: (a) **main-thread blocking** (sync Tauri command), (b) **O(N) render**
   (whole tree / whole document in the DOM), (c) **O(N) backend per op**
   (`materialize` re-reads every file), (d) **stale/racey state** (optimistic UI
   vs. lagging backend reads).
3. **Fix the smallest thing that removes the cost**, re-measure, commit.
4. **Assert on what the user sees** (editor content, scroll position, rendered row
   count) — not just "did the row appear." Wrong assertions = false green.
5. **For every bug, add a test that fails on the old code** so it can't return.
6. **Use a slow/lagging mock** in tests. A fast, self-consistent mock hides exactly
   the timing/race bugs that hit real users (and the slow `tauri dev` debug build).

## Diagnose-by-category cheatsheet

- UI freezes *during* an action, even a cheap one → the Tauri command is
  **synchronous** (runs on the WebView main thread). Make it
  `#[tauri::command(async)]`.
- Opening/expanding is slow and the freeze scales with item count → you're
  **rendering every item**. Virtualize (render only the viewport window).
- Typing pause hitches and scales with file size → you're **re-rendering the whole
  document**. Re-render only the edited line.
- An op (save/create/delete) is slow and scales with *vault* size → the backend is
  **O(N) per op** (here: `materialize` reads every blob + every file). Run it
  off-main-thread, take it off the interaction's critical path, and don't refetch
  the whole list — update optimistically.
- "Random stuff / earlier actions replay / blank editor" → the UI **re-reads the
  backend while writes are draining**. Keep an in-memory working copy as the source
  of truth; stop re-reading what you already hold.

## The fixes (before → after, real WebKit @ 1000–5000 files)

| # | Problem | Fix | Result |
|---|---|---|---|
| 1 | UI froze during *every* action | `#[tauri::command(async)]` on all commands (sync commands run on the WebView main thread) | no main-thread freeze |
| 2 | Full folder re-hash on every save | removed the per-folder fs watcher (in-app edits already capture via `record_*` + push via broadcast) | save no longer O(vault) |
| 3 | Open froze; sidebar rendered every file | **virtualized** file tree (render only the viewport window) | open ~100ms, **27** rows rendered for 5000 files |
| 4 | ~3s per keystroke | debounced re-highlight + don't re-render React per keystroke (buffer in a ref) | **3–4 ms/key** |
| 5 | 600ms hitch typing in a big file | **line-level** re-highlight (re-render only the edited line; full only on structural/code-fence changes) | 4000-line settle **647ms → 47ms** |
| 6 | `history()` folded the whole log on the critical path | debounced, fire-and-forget after mutations | off the critical path |
| 7 | "adds every other time" | reserve the untitled name + show the row **synchronously** before any await | every click adds a distinct file |
| 8 | "delete removes nothing, a lot of the time" | optimistic delete is **synchronous** (atomic read-modify-write of the file list, no await between), persist in background | rapid multi-delete leaves none stuck |
| 9 | new file blank; editor showed stale content ("random stuff") | **in-memory content cache** (`vault::path`) is the editor's source of truth; never re-read a file we already hold | editor always shows the right content |
| 10 | expand/collapse scrolled to the selected file | auto-scroll only when the **selection** changes, not when the row set changes | scroll stays put on expand/collapse |
| 11 | history track rendered a node per event | cap rendered ticks (~240) regardless of event count | 5000 events → ~240 tick nodes |
| 12 | status poll folded the log frequently | poll only the active vault, every 10s | negligible |

Backend reality check (`bench_ops.rs`, release): at 1000 files / 5000 rows,
`write_file`/`delete_file` ≈ **50ms**, `list_files` < 1ms, `history` ≈ 11ms. The
backend was never the freeze — the frontend render and main-thread blocking were.
Note the **debug** build (`tauri dev`) is ~10–30× slower, which amplifies every
latency-sensitive bug; the fixes are correct regardless of build speed.

## Why the first round of tests missed the bugs

The harness was green while the app was buggy because:
- the mock backend was **instant and self-consistent** → stale-read races couldn't
  reproduce;
- tests drove **one action at a time with waits** → no backlog/concurrency;
- assertions checked **structure** ("row appeared") not **what's on screen**
  ("editor shows the right content", "scroll didn't jump").

The closing moves: a **slow** mock (`App.content.test.tsx`, 120ms writes),
**content/scroll assertions** (`FileTree.test.tsx`, harness "editor shows content
after create"), and **rapid/overlapping actions** (rapid-multi-delete, file
bouncing). Each new test fails on the pre-fix code.

## Round 2 — the backend WAS the freeze at 10k–50k files

The first round proved the backend was fine at 1k files. At 10k–50k it is not —
`bench_ops.rs 50000 0` exposed O(vault)-per-op costs that scale right past the
point a real vault hits. Same loop (measure one op, find the layer, fix the
smallest thing, re-measure), backend edition.

| # | Problem (at scale) | Fix | Result |
|---|---|---|---|
| 13 | every `write_file` was O(vault): `materialize` rewrote the whole `files` table with a per-row INSERT and **no transaction**, so SQLite auto-committed once per row | wrap the rewrite in ONE `unchecked_transaction` + a cached prepared statement | `replace_files` **585ms → 17ms**, `write_file` **824ms → 265ms** @ 10k (3.1×) |
| 14 | `get_status` (10s poll) loaded **every** log row for `max(ts)` and materialized every `FileRow` to count them | `Store::max_ts()` (`SELECT MAX(ts)`) + `Store::live_file_count()` (`SELECT COUNT`) | `get_status` **220ms → 2.8ms** @ 10k |
| 15 | the fold read **every** content blob, even on linear edits, to keep an in-memory `content` it only used for merges | lazy content in `compute_files`: track `content_hash`; read `ours/base/theirs` only when a real 3-way merge fires (byte-identical FileRows) | speeds up every materialize + every history fold |
| 16 | history slider: `read_file_at` rebuilt the **whole-vault** snapshot (read all blobs) to extract one file, fired on every pointermove | `Engine::file_at(path,t)` reads exactly one blob; debounce the time-travel read 60ms in `App.tsx` | `read_file_at` **137ms → 48ms** @ 10k; a scrub is one read, not one-per-pixel |

Each fix is locked by a test that asserts the *behavior*, not the speed:
`store_and_config` (max_ts/live_file_count match the scan), `engine_snapshot`
(`file_at` ≡ `state_as_of` across edits/renames/deletes/merges),
`fold::concurrent_disjoint_edits_both_survive_in_the_fold` (the lazy fold still
merges), and the full `sync_fuzz` battery (the transaction + fold changes don't
break convergence: 4 seeds × 3 peers × 300 rounds, 0 divergences).

Frontend companion: a **loading overlay** while opening/adding a large folder
(`withOpening` in `App.tsx`, 140ms threshold so a quick switch never flashes) —
the open path is genuinely slow at 50k files (first capture hashes + git-exports
every file), so it gets the same progress affordance cloning already had.

Cold start: `reopen_saved()` ran synchronously before the Tauri window was
built, so a large saved vault froze first paint — fixed by moving it to a
background thread in `.setup()` + a `vaults-ready` event the UI listens for, with
a "Loading your vaults…" hint until it fires.

**Still O(vault):** the *first* `add_local_folder` capture at 50k files (hash
every file + store every blob + git-export every file) — inherent one-time
ingest cost, covered by the loading overlay.

## Round 3 — kill the whole-log re-fold (merges were O(N))

Even after Round 2, every state change re-folded the ENTIRE log: `materialize`
did `compute_files(all_rows)` on each local save AND each peer row integrated.
The data model is incremental-friendly (per-`file_id` independence; concurrent
rows order by an intrinsic `(lamport,site_id,id)` key) but the engine recomputed
from scratch for trivial correctness. Round 3 makes it incremental.

| # | Problem | Fix | Result |
|---|---|---|---|
| 17 | a local save still re-folded the whole log inside `materialize` | fast path in `record_write` for a linear edit on the tip: update one files-table row + write one file + refresh git, no fold | `write_file` edit **1061ms → 485ms** @50k |
| 18 | path-collision resolution keyed on the global fold-order index (only valid in a whole-log fold) | key it on the path-setting row's intrinsic `OrderKey` (behavior-preserving; collisions are always cross-file ⇒ concurrent ⇒ same order) | enables folding a file in isolation |
| 19 | peer-row integration and concurrent merges still O(total log) | `FoldState`: per-`file_id` state cache; the engine keeps it in memory + a `dirty` set fed by every append (local & peer), and `materialize` re-folds ONLY dirty files, then resolves paths over all | integration **O(total log) → O(touched files' rows + file count)** |

Deep-history payoff (`bench_ops 100 50000` — 100 files, 50k rows, previously an
O(n²) seed that had to be killed): it now completes, and `write_file` edit is
**12.9ms**, `delete_file` **16ms** — versus the old whole-log fold of 50k rows on
every op. (A structural op right after a long burst of fast-path-only edits pays
a one-time catch-up to reconcile all the deferred `dirty` files; in real use
materializes interleave, so `dirty` stays tiny.)

The correctness gate is generative, not a spot check (`tests/fold_props.rs`):
across 300 random concurrent histories, feed a `FoldState` one row at a time in
a random arrival order and assert it equals `compute_files` over the rows seen so
far **after every row** — covering out-of-order arrival, concurrent forks/merges,
renames that create/break path collisions, and delete+recreate. Plus
`compute_files` permutation-invariance over 400 histories, and the full
`sync_fuzz` battery now also asserts the derived **git head** converges across
every surface (seeds 1/7/13/23/99 × 1/2/3 peers + 240-round runs, 0 divergences).
