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
