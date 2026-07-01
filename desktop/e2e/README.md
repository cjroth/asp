# Desktop e2e / scale test harnesses

Tools for testing the app at the scale of a real vault (1000s of files, 1000s of
db rows, large files). Three complementary layers:

## 1. Real WebKit browser (the gold standard)

Drives the **built frontend** in a real WebKit engine (MiniBrowser via
`WebKitWebDriver` — the same engine the Tauri app uses), with a mock Tauri backend
serving a large in-memory vault. Measures real rendering/interaction cost and
checks correctness.

```sh
bun run e2e:web                 # 1000 files, 1500-line README, 2000 history events
bash e2e/web-run.sh 5000 0 0    # 5000 files, small README, no history
# params: <nfiles> <readme-lines> <history-events>
```

Requires `WebKitWebDriver` + `Xvfb` (headless). Covers: open, virtualization
(bounded DOM rows), scroll, typing latency + large-file re-highlight settle,
create ×2 (no collision), delete, rapid-multi-delete (race), rename, vault switch,
history time-travel, and the history tick cap.

Pieces: `serve.mjs` (serves `dist/` + injects `mock-backend.js`), `mock-backend.js`
(in-memory backend; URL params `n`, `big`, `nest`, `hist`), `web-drive.mjs` (the
WebDriver script).

## 2. jsdom component tests

`src/App.scale.test.tsx` — drives the real `<App/>` against a mocked backend at
1000 files with realistic latency. Fast, runs in `bun run test`. Locks the
virtualization bound, the create/delete races, and rename.

## 3. Rust backend benchmark + seeder

```sh
bun run bench 1000 4000         # time backend ops at 1000 files / ~5000 rows
# seed a real vault + app config (for manual testing of the actual app):
HOME=/tmp/h target/debug/examples/seed_vault /tmp/h/vault 1000 200
```

`engine/examples/bench_ops.rs` reports per-op latency (release: write/delete ~50ms
at 5000 rows). `engine/examples/seed_vault.rs` creates a vault + `desktop_folders.json`
so the app auto-loads it.

## 4. Real-engine computer-use drives (branching / tags / history)

These drive the app **without a mock** — against the REAL engine — and click
through the whole branching UX (auto-branch-on-edit-in-the-past, tags, the
timeline network graph, diff popup, branch switching). Screenshots land in
`e2e/shots/` (web) and `e2e/shots-desktop/` (desktop); both dirs are gitignored.

### 4a. Web build, real wasm engine (Playwright + chromium)

Runs the built web app on the real `asp-core` wasm engine (OPFS-backed) in a real
headed chromium under Xvfb. `verify-serve.mjs` serves `dist/` with no mock;
`verify-drive.mjs` drives it.

```sh
bun run build:web
# start the static server (background), then drive it:
node e2e/verify-serve.mjs dist &            # serves http://127.0.0.1:5601
DISPLAY=:99 node e2e/verify-drive.mjs        # needs Xvfb :99 + playwright-core + chromium
```

Covers (18 checks): create vault, edit, timeline dots + connecting line, file
count, Time/Edits layout toggle, click-dot→diff popup, tag create/flag,
jump-to-tag time travel, auto-branch on edit-in-the-past, branch lanes + pill,
switch branch via lane label, tag-input outside-click close, tag delete confirm.

### 4b. Real desktop Tauri app (tauri-driver + WebKitWebDriver)

Drives the actual built Tauri binary (native SQLite engine, real Tauri IPC) headed
under Xvfb. A vault is pre-registered in `$HOME/.asp/desktop_folders.json` (the
native folder picker can't be scripted), so it reopens on launch. `desktop-drive.mjs`
uses `tauri-driver` + `WebKitWebDriver` via `selenium-webdriver`.

```sh
# Linux deps: libwebkit2gtk-4.1-dev libgtk-3-dev + webkit2gtk-driver; cargo install tauri-driver
(cd src-tauri && cargo build --bin context-desktop)
# the debug build loads devUrl (localhost:1420) → serve dist there:
node e2e/verify-serve.mjs dist &   # then PORT=1420 for the desktop devUrl
HOME=/path/to/fakehome DISPLAY=:99 node e2e/desktop-drive.mjs
```

Same flow as 4a but exercising the native engine + IPC end to end.

### Prereqs recap
- `Xvfb :99` running (`Xvfb :99 -screen 0 1280x820x24 &`).
- Web drive: `playwright-core` (devDep) + a chromium at `$CHROME` (defaults to the
  pre-installed `/opt/pw-browsers/chromium-*/chrome-linux/chrome`).
- Desktop drive: system webkit2gtk/gtk3 dev libs, `WebKitWebDriver`, `tauri-driver`.
