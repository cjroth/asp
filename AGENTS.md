# ASP — agent guide

Local-first sync engine ("git's shape, CRDT semantics"). One protocol engine
(`crates/asp-core`) linked by three surfaces: the `asp` CLI, the desktop Tauri
shell (`desktop/engine` + `desktop/src-tauri` + React in `desktop/src`), and the
web app (`crates/asp-wasm` → OPFS). `tests/e2e` spawns real `asp` binaries.
`desktop/src-tauri` is excluded from the cargo workspace (Tauri toolchain builds it).

Before writing tests or judging coverage, read `.claude/skills/verification-playbook/SKILL.md`.
Before touching any `git*` module, read `.claude/skills/git-bridge-dev/SKILL.md`.

## Build & test gates (mirror CI)

```bash
cargo build --workspace
cargo test --workspace --exclude asp-e2e --exclude asp-desktop-engine  # deterministic lane
cargo test -p asp-e2e -p asp-desktop-engine -- --test-threads=1        # networked lane (CI retries 3x)
cargo clippy --workspace --all-targets   # CI enforces -D warnings
cd desktop && bun run typecheck
```

- **Do NOT run `cargo fmt`** — the repo does not use rustfmt (pristine files are
  not fmt-clean; a fmt pass creates a huge noisy diff). There is no CI fmt job.
- wasm check: `RUSTFLAGS='--cfg getrandom_backend="wasm_js"' cargo build -p asp-wasm --target wasm32-unknown-unknown`
- Full wasm SDK rebuild: `cd sdks/typescript && bun run build:wasm`, then
  `desktop/scripts/sync-wasm.sh` (bun copies `file:` deps — must drop+reinstall).

## Environment constraints (OrbStack VM, 11 GiB / 8 cores)

- `~/.cargo/config.toml` caps `jobs = 4` — **do not remove**; parallel rustc
  OOM-kills the VM (the macOS host kills OrbStack). Run ONE cargo command at a
  time; never run two agents' builds concurrently.
- **Never run the full `bun test src`** — loading App.tsx + jsdom + the ~6 MB
  wasm in one process OOMs (exit 137). Run targeted files/dirs:
  `bun test src/lib`, `bun test src/App.git.test.tsx`, etc.
- Headless (no DISPLAY/WAYLAND): `bun dev` preflights and refuses with guidance.
  Use `bun run dev:web` (vite :1420, OrbStack forwards to the host browser) or
  `xvfb-run -a bun dev` for an invisible native window (e2e drivers).
- The networked e2e lane (iroh QUIC through a relay) is **flaky under VM load**
  regardless of your change — e.g. `concurrent_merge` can fail on a clean HEAD.
  Before chasing a networked-e2e failure, baseline it: build a clean worktree at
  HEAD and run the same test there. Only a clean-pass/dirty-fail delta is yours.
- `target/release/asp` goes stale silently; rebuild (`cargo build --release -p asp`)
  before running any e2e/soak harness that shells out to it.

## Conventions that bite

- **Fuzzing** = deterministic LCG loops inside ordinary `#[test]`s (see
  `memengine.rs` `fuzz_random_ops…`, `branch.rs` graph fuzz). **No proptest, no
  cargo-fuzz** — don't add them; match the house style.
- Hermetic tests: `tempfile::tempdir()` + `Identity::from_seed`. E2E git
  fixtures: `tests/e2e/src/gitfix.rs` (deterministic repos + a real
  `git http-backend` smart-HTTP server).
- **Tauri invoke args bind by Rust param NAME.** JS `invoke('cmd', {…})` keys
  must exactly match the `#[tauri::command]` fn params (bug f6c1d07). When adding
  a command, extend the guard test `desktop/src/lib/tauriApi.git.test.ts`
  (mock invoke, assert names). DTOs returned to TS use
  `#[serde(rename_all = "camelCase")]`.
- New SQLite tables: append `CREATE TABLE IF NOT EXISTS` to `SCHEMA` in
  `sqlite.rs` (idempotent, runs every open); guarded `ALTER TABLE` for new
  columns (see `migrate_branching`/`migrate_git_push`). No version table.
- wasm seam: wasm-safe modules go in the always-compiled section of
  `asp-core/src/lib.rs`; native-only (tokio/fs/sqlite/reqwest) modules in the
  `cfg(not(target_arch = "wasm32"))` section. Pure sans-IO protocol code
  (e.g. `gitwire`) must stay transport-free.
- Desktop engine: never `rt.block_on` inside a Tauri command (nested-runtime
  panic) — use `DesktopEngine::block()`; for futures that borrow the `!Sync`
  `Engine` across `.await` (not `Send`), use `run_off_thread` (fresh OS thread +
  current-thread runtime).

## Frozen, identity-bearing rules — never change without a migration plan

Merkle row ids hash these; changing any silently forks every vault:
- `LogRow::canonical_fields()` order and `oid::merkle_id` framing.
- `LogRow.ts` is unix **seconds** (wasm divides `Date.now()/1000`).
- Git-bridge identity domains (`"v1"`, plain concat, single sha256):
  `asp-git-site/v1`, `asp-git-vault/v1`, `asp-git-file/v1`, `asp-git-remote/v1`.
- Canonical git topo order: parents first; ready set by `(committer_seconds, sha)`.
- `PROTO` (wire.rs) bumps hard-refuse old peers at Hello — coordinate a fleet
  upgrade (see RELEASING.md "Protocol version").

## Skills index

- `verification-playbook` — how to verify work here; which test types earn
  confidence (evidence-ranked) and the three high-leverage test patterns.
- `git-bridge-dev` — git-bridge module map, contracts, gotchas, test-suite map.
- `sync-soak-test` — cross-surface soak/fuzz harness (CLI + desktop + web, and
  the git-remote layer).
