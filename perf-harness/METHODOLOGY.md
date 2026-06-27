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
