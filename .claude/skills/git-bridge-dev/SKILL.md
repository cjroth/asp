---
name: git-bridge-dev
description: Working on the git bridge (clone from / two-way sync with git remotes) — module map, frozen identity contracts, cross-surface API contracts (Tauri invoke names, wasm fetch_fn, proxy URL shape), the gotchas that already bit us, and the full test-suite map with run commands. Use when changing anything matching crates/asp-core/src/git*.rs, crates/asp/src/gitcli.rs, tests/e2e git tests, desktop git commands, webApi/api git methods, or the relay --git-proxy.
---

# Git-bridge development guide

Design doc: `specs/git-bridge.md` (+ its "Implementation status" appendix).
User guide: `docs/git-bridge.md`. Core idea: **the git remote is one more ASP
site** — upstream commits replay as ordinary synced rows under a repo-derived
`site_id`; pushes are deterministic commit synthesis from synced `GitPlan`
records, so any node computes identical SHAs and races are idempotent.

## Module map (crates/asp-core/src/)

| Module | Role | wasm? |
|---|---|---|
| `gitwire` | pkt-line + smart-HTTP protocol-v2 framing, URL parsing. Pure bytes-in/bytes-out. | yes |
| `gitimport` | pack → `GitObjectDb` → `plan_import()` → `ImportPlan` (canonical order, lane assignment, diffs) | yes |
| `gitgenesis` | `ImportPlan` → sealed `LogRow`s: `synthesize_genesis` (pristine clone) / `synthesize_ingest` (live pull) | yes |
| `gitrecord` | `Kind::{GitCommit,GitIngest,GitPlan}` payload structs + sealed-row builders | yes |
| `gitbridge` | HTTPS (reqwest/rustls) + SSH (spawned `ssh`) transports, `RemoteStore` (.asp/gitremote), `write_pack`, `push_pack` | native |
| `gitremote` | driver: `clone_from_git`, `pull_once`, `git_status`, `rebaseline`; `git_remotes`/`git_modes` tables; keyring auth | native |
| `gitpush` | `author_plan`, effective-frontier fold, deterministic `synthesize_commits`, `push` (non-FF retry), `pending_git_diff` | native |
| `gitpolicy` | `interval` auto-plan tick + params; llm = external agent via `asp git diff`/`plan` (engine never calls a model) | native |
| `gitproxy` | relay-cohosted CORS proxy for browser clone (SSRF guards, caps) | native |

Wire/proto: `PROTO = 4` (wire.rs). The three git Kinds fail msgpack decode on
proto-3 peers — any future Kind addition needs another bump + fleet coordination.

## Frozen contracts (identity-bearing — changing any forks existing vaults)

- Id domains, plain concat + single sha256: `site_id = hex(sha256("asp-git-site/v1"‖root_sha))[..32]`,
  `vault_id = hex(sha256("asp-git-vault/v1"‖root_sha))`, `file_id` domain
  `asp-git-file/v1`. Tripwire vector pinned in `gitgenesis` tests — if that test
  fails you broke identity, don't "fix" the test.
- Canonical topo order v1: parents first; ready set by `(committer_SECONDS, sha)`.
- Genesis emission order per commit: branch-creates → Merge rows → diff ops
  (bytewise by result path) → commit marker → GitIngest → branch-deletes;
  `seq` dense 0-based, `lamport = seq+1`, `ts` = committer **seconds**.
- Branch naming: PR-subject → text after first `/`; `Merge branch 'x'` → x;
  else `git/<7hex>`; collisions `-2,-3` in lane-creation order.

## Cross-surface contracts

- **Tauri invoke names (bind by Rust param name — f6c1d07):**
  `clone_git{dest,url,token,depth}` `git_pull{id}` `git_status{id}`
  `git_push{id,message}` `git_pending_diff{id}`. Guard test:
  `desktop/src/lib/tauriApi.git.test.ts` — extend it for any new command.
  DTOs to TS are `#[serde(rename_all="camelCase")]` (`GitStatus {remoteUrl, atSha, …}`).
- **wasm:** `WasmEngine.git_clone(url, token, proxy_base, depth, fetch_fn, on_progress)`;
  JS owns transport via `fetch_fn: async (method, url, headers, body|null) → {status, body: Uint8Array}`;
  progress phases `fetching|replaying|saving`. Browser is **clone/pull only** —
  `webApi.gitPush` throws by design.
- **Proxy URL shape:** `<proxy_base>/git/<host>/<upstream path>` +
  `/info/refs?service=git-upload-pack` or `/git-upload-pack`. Web reads
  `VITE_GIT_PROXY_BASE` (or `globalThis.__ASP_GIT_PROXY_BASE__`).

## Gotchas that already bit (don't re-learn these)

- **Modes/symlinks come from the ledger (`git_modes`), never the materialized
  tree** (spec R4) — the browser can't represent symlinks; synthesis consulting
  the filesystem would strip `120000`/`100755` on push.
- **Root `.aspignore` is excluded from synthesized commits** (`ROOT_ASPIGNORE`
  in gitpush.rs) — it's ASP-local control state like `.asp/`. Tests assert its
  ABSENCE from pushed trees; don't reintroduce it.
- **gix-pack's `wasm` feature** (required for wasm32) compiles out its on-disk
  index writer — so `RemoteStore::record_fetch` explodes packs to loose objects.
  Keep `parallel` off; the working feature set is pinned in root Cargo.toml.
- **SSH is protocol v2 via `SendEnv GIT_PROTOCOL`** (GitHub/GitLab AcceptEnv it);
  a server that won't v2 gets a typed error suggesting HTTPS. `ASP_GIT_SSH`
  overrides the binary (used by the test shim).
- **gitproxy SSRF guard rejects loopback/private upstreams BY DESIGN** — a
  hermetic browser-e2e can't clone through it from a local fixture; use a real
  https repo (`GIT_CLONE_URL`) or the crate-internal test hook.
- **Pull rebuilds the full DAG from `RemoteStore` loose objects** while the
  network fetch stays incremental (`haves=[last]`). `seen` (existing GitIngest
  shas) makes re-ingest a no-op.
- Ingested rows chain onto the **imported** tip, not the local-edit tip — that's
  what makes a raced local edit an ordinary concurrent fork the fold resolves.
- Keyring: token under `asp-git/<remote_id>`; `ASP_GIT_DISABLE_KEYRING=1` in
  tests so they never touch the OS store. Tokens never enter synced state.
- Desktop: clone opens a bare unshared `Engine` first (CLI pattern), then
  `handle()`-wraps; `run_off_thread` for `!Send` engine-across-await futures.

## Test-suite map (all must stay green)

```bash
cargo test -p asp-core            # incl. gitwire/gitrecord/gitgenesis/gitpolicy/gitproxy/gitpush units
cargo test -p asp-core --test branch_scale        # R3 perf guardrail (N-vs-2N ratios)
for t in git_harness git_import_model git_transport git_genesis git_clone_pull \
         git_push git_wasm_path git_policy git_ingest_race git_convergence_prop; do
  cargo test -p asp-e2e --test $t; done
cargo test -p asp-desktop-engine  # incl. tests/git_bridge.rs hermetic clone/pull/push
cd desktop && bun run typecheck && bun test src/lib && \
  bun test src/App.git.test.tsx && bun test src/App.gitpush.test.tsx
```

Key suites: `git_import_model`/`git_genesis` hold the **per-commit fidelity
invariant** (fold == `git ls-tree -r`) over all 11 fixtures (criss-cross,
octopus, foxtrot, mid-history root, modes/symlinks…) — the load-bearing
correctness check; `git_convergence_prop` holds "any node may bridge"
(byte-identical folds AND synthesized SHAs after random interleavings).
Fixtures/harness live in `tests/e2e/src/gitfix.rs` (deterministic repos + real
`git http-backend` server + `advance_tip`/`force_rewrite_tip`).

When adding a fixture or changing lane assignment: the fidelity invariant must
pass for EVERY commit of EVERY fixture — if it fails, the lane algorithm is
wrong, not the test (spec §3.1 calls this the safety net; trust it).
