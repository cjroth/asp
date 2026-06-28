---
name: aspgui-ui-debug
description: Find and fix UI/rendering bugs and performance/scale problems in the native GPUI vault editor at /Users/chris/dev/asp/gpui. Use when asked to debug the aspgui app, hunt UI bugs, fix slowness/jank/freezes, fix broken clicks or overlapping/garbled/blank rendering, or measure/improve performance at scale. Drives the real app with the existing perf + visual harnesses, reproduces, fixes, and locks each fix in with a regression assertion.
---

# aspgui UI bug & performance hunt

You are working on the native GPUI vault editor at `/Users/chris/dev/asp/gpui`
(branch `gpui`). It reimplements the Tauri/React "Context Desktop" app over the
same `asp-desktop-engine` → `asp-core` engine. HARD INVARIANT: all behavior goes
through the engine; the UI is just a shell.

- App entry: `src/bin/aspgui.rs`
- Root view + all handlers: `src/app.rs`
- Markdown render: `src/markdown.rs`
- Engine wrapper (only path to behavior): `src/backend.rs`

## Goal

Find and fix UI/rendering bugs and performance/scale problems the way we did
before: reproduce in a harness, fix the smallest thing, re-measure, and lock it
in with an assertion so it can't regress. There are still many bugs. Work through
several this session — don't stop at one.

If the user gave a specific complaint (in the arguments or message), make that
the first target: reproduce it directly before scanning for others. Otherwise
scan `src/app.rs` for O(N) render patterns and drive the main interactions.

## Tools that already exist

All capture/perf harnesses need `--features capture` (turns on
`gpui_platform/test-support` so stock GPUI's offscreen Metal readback
`Window::render_to_image` works — no Xcode, no display). Run GUI/perf harnesses
with `dangerouslyDisableSandbox: true` (they open a real Metal window). Set
`HOME` to a throwaway dir so the user's real saved vaults don't load in the
background and skew/contaminate the run.

- **harness** (`src/bin/harness.rs`) — PERF. Seeds a vault at scale, times
  `Window::draw` (layout+paint — the thing that re-runs every keystroke/scroll/
  selection), asserts a per-step budget, saves a PNG per step, measures "ink"
  (non-blank guard).
  ```sh
  cargo run --release --bin harness --features capture -- <nfiles> <biglines> <nhist> <outdir>
  ```
  USE `--release` for real numbers (debug inflates absolute ms ~15x). The bug
  signal is draw time that **scales** with file/line/history count = an O(N)
  render. Flat across scale = fixed.

- **vtest** (`src/bin/vtest.rs`) — VISUAL + INTERACTION regression. Drives
  canonical scenes with REAL mouse clicks, asserts the right regions have ink,
  and diffs each frame against goldens in `tests/golden/`.
  ```sh
  HOME=/tmp/vth cargo run --bin vtest --features capture             # check
  HOME=/tmp/vth cargo run --bin vtest --features capture -- --update # regenerate goldens
  ```
  EXTEND THIS: every bug you fix gets a new scene/assertion here (a real click +
  a state and/or pixel check) that fails on the old code.

- **probe** (`src/bin/probe.rs`) — dispatches real MouseDown/Up through GPUI
  hit-testing; the minimal "is this actually clickable" check.
- **shoot / smoke** (+ `./shot-mac.sh`) — scripted screenshots / minimal render.

After capturing a PNG, **Read the image** to actually see overlap / garbled /
blank rendering — don't trust internal state alone.

## The loop (this is what worked)

1. **Pick a target.** A screen/interaction the user calls slow or buggy, or scan
   `src/app.rs` for O(N) render: `for … in self.files / self.history /
   self.content.split('\n')` building elements; whole-document renders; missing
   virtualization or scroll containers.
2. **Reproduce at real scale and MEASURE** (draw ms vs N), or drive the
   interaction with a real click and assert **what the user sees** (screen
   changed, region has ink, content actually updated) — not just internal state.
3. **Diagnose by category:** O(N) render → render only the viewport
   (`uniform_list`) or cap counts; main-thread block during an action → move
   engine calls off-thread + update optimistically; stale/racey state; dead or
   wrong-sized click target.
4. **Fix the smallest thing** that removes the cost/bug. Re-measure. Confirm flat
   across scale or correct behavior.
5. **Add a regression assertion** to vtest (and/or a harness budget) that fails
   on the old code. Regenerate goldens only after visually confirming the new
   frame is correct.

## Gotchas (assume these)

- macOS `cx.quit()` terminates the process — print reports / compute exit codes
  **inside** the gpui run-loop spawn, not after `run()` returns. Flush stdout
  before `process::exit`.
- Run with isolated `HOME` or real vaults (incl. a ~14k-file one) load in the
  background and contaminate timing and which vault/file a click hits.
- `uniform_list` rows need `.w_full()` or only the text is clickable. Build list
  items via `cx.processor(...)` + `cx.listener` so `on_click` registers.
- Known-good click pattern:
  `div().id(..).cursor_pointer().on_click(cx.listener(..))`.
- Already fixed — do NOT re-report: metal compiler / `runtime_shaders`,
  `font-kit` (NoopTextSystem → no text), background incremental `reopen_saved`,
  file-tree virtualization, history-tick cap (~240), file-row `.w_full()`.
- New vault / Connect vault buttons are intentionally unwired (need a native
  folder picker) — wiring them is fair game.

## Deliver

For each bug: a one-line repro (which harness + args), the root cause, the fix
(`file:line`), before/after numbers or before/after screenshots, and the vtest
assertion that now guards it. Finish by building the real app
(`cargo build --bin aspgui`) to confirm it still compiles. Don't commit unless
asked.
