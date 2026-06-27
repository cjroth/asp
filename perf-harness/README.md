# Desktop performance harness

A reusable harness for finding and fixing performance problems and scale bugs in
the **Context Desktop / Vault Editor** app (`../desktop`). It drives the *real*
built frontend in a *real* WebKit browser engine — the same engine the Tauri app
ships — against a mock backend serving a vault as large as you want, and measures
real interaction cost (open, scroll, type, create/delete/rename, history).

This folder is a documented snapshot. The **canonical, runnable** copies live in
the desktop project (they need its built `dist/`, deps, and engine binary):

| script | canonical location | purpose |
|---|---|---|
| `mock-backend.js`, `serve.mjs`, `web-drive.mjs`, `web-run.sh` | `../desktop/e2e/` | real-WebKit browser harness |
| `seed_vault.rs` | `../desktop/engine/examples/` | seed a real on-disk vault + app config |
| `bench_ops.rs` | `../desktop/engine/examples/` | time backend ops at scale |

See **METHODOLOGY.md** for the exact loop we used to make the app fast, and the
before/after numbers for every optimization.

## Why a real browser (not jsdom)

The bugs that bit us were *layout*, *timing*, and *what's-on-screen* bugs:
virtualization, scroll position, a 600ms whole-document re-highlight, stale editor
content while writes drained. jsdom has no layout and a fast/consistent mock hides
timing — so jsdom green ≠ app works. We drive **MiniBrowser via `WebKitWebDriver`**
(the WebKitGTK engine the Tauri WebView uses), headless under `Xvfb`. Same renderer,
real layout/paint, real pointer events.

## Prerequisites (Linux)

- `WebKitWebDriver` (package `webkit2gtk-driver`) + `Xvfb`
- `bun` (or node) with the desktop project's deps installed (`selenium-webdriver`)
- `cargo` (for the seeder/bench)
- A built frontend bundle: `cd ../desktop && bun run build:web`

Check: `WebKitWebDriver --help` and `which Xvfb tauri-driver` should resolve.
(`tauri-driver` exists but was flaky under Xvfb here — we use plain
`WebKitWebDriver` against the served frontend instead.)

## Run it

All from `../desktop`:

```sh
# 1. Real-WebKit browser harness. Args: <nfiles> <readme-lines> <history-events>
bun run e2e:web                     # 1000 files, 1500-line file, 2000 history events
bash e2e/web-run.sh 5000 0 0        # 5000 files, small file, no history
bash e2e/web-run.sh 1000 4000 5000  # stress: large file + big history

# 2. jsdom component/scale tests (fast, no browser): virtualization + races +
#    editor content integrity on a SLOW mock backend.
bun run test

# 3. Backend op latency at scale (release build for real numbers).
bun run bench 1000 4000             # 1000 files / ~5000 db rows

# 4. Seed a REAL vault + app config, then launch the actual app against it.
HOME=/tmp/bigvault target/debug/examples/seed_vault /tmp/bigvault/vault 1000 200
HOME=/tmp/bigvault bun run tauri dev   # the app auto-loads the seeded vault
```

The browser harness prints a per-step report (`ms` + pass/fail) and exits non-zero
if any budget/assertion fails. Tune the URL params in `mock-backend.js`:
`?n=<files>&big=<readme lines>&nest=1&hist=<events>`.

## What it checks (and the gaps it closed)

Each step asserts **what the user sees**, not just internal state — that was the
gap that let the early bugs through:

- open time + **rendered DOM row count is bounded** (virtualization)
- scroll smoothness; **no scroll jump** on expand/collapse (jsdom `FileTree.test.tsx`)
- typing latency + **re-highlight settle on a large file** stays small
- create shows the file **and the editor shows its content** (not empty)
- delete removes it; **rapid multi-delete** leaves none stuck (race)
- rename; vault switch; history time-travel; **history tick cap** at 1000s of events
- editor content integrity under a **slow** backend (jsdom `App.content.test.tsx`)
