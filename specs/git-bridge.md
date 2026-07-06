# Spec: Git bridge — clone from, and two-way sync with, git remotes

## 0. Goal, scope, non-goals

**Goal.** A user can paste a git remote URL anywhere they can paste an invite code
— CLI, desktop, web — and get a live ASP vault whose timeline contains the repo's
full commit history. The vault then stays connected to the remote: upstream commits
flow into the vault, and vault changes roll up into real git commits that push back,
grouped by a pluggable policy (manual, time-based, LLM-authored messages).

**In scope:** engine-level git client (fetch + push) usable from every surface,
full-DAG replay into the ASP log (merged side branches become ASP branches), an
ingest ledger for ongoing pulls, a
deterministic commit-synthesis pipeline driven by synced *CommitPlan* records so
**any** node may push idempotently, credential handling (HTTPS token + SSH),
a relay-co-hosted CORS proxy for the browser, UI/CLI surface, protocol/versioning,
tests.

**Non-goals (v1):** git submodule recursion, git-LFS smudging, importing *unmerged*
remote refs (only HEAD's ancestry imports — which, per decision 1, includes merged
side branches; **superseded by the clone-time opt-in in
`specs/git-open-branches.md`**), pushing ASP branches other than `main`,
auto-healing after an upstream force-push, browser-side push, shallow→deepen
workflows, `git://` protocol.

**Decisions locked with Chris (2026-07-06, two interview rounds):**
1. **History:** replay the **full commit DAG** reachable from the default branch's
   tip — merged PRs' side-branch commits import as real ASP branches with real
   merge nodes, not squashed first-parent points. ("Need full DAG in v1.")
2. **Web:** the wasm engine speaks git-over-HTTPS itself; in the browser its requests
   route through a stateless CORS proxy co-hosted with the relay (git hosts don't send
   CORS headers on smart-HTTP endpoints, so direct browser fetch is impossible — even
   for read-only clone). Browser is clone/pull only in v1; push is native-only.
3. **Topology:** any native node may bridge (pull and push). Safety comes from
   determinism + a synced ledger, not from electing a single bridge node.
4. **Auth:** anonymous HTTPS, HTTPS + token (PAT), and `ssh://`/`git@` URLs.
5. **Proto rollout:** `PROTO` 3→4 ships as a coordinated same-day upgrade of the
   current small fleet (no two-step release; see §6.2).
6. **Branch mapping:** push tracks ASP `main` ↔ the remote default branch only;
   other ASP branches get a UI hint ("won't reach git until merged").
7. **Default rollup policy:** `manual` — nothing pushes without explicit user action.

---

## 1. Current substrate (what we build on)

- **Store**: append-only `LogRow` log (`crates/asp-core/src/log.rs:127`), Merkle
  `id` over semantic fields (`oid.rs` `merkle_id`, `log.rs` `seal()`), so identical
  rows self-deduplicate on sync. `(site_id, seq)` unique + dense per site → version
  vectors (`branch.rs:20`). Content is hex-SHA-256 blobs behind `BlobStore`
  (`store.rs:16`). Fold + 3-way merge (`fold.rs`, `merge.rs`) converge everything.
- **Two engines, one protocol**: native `Engine` (SQLite, `engine.rs`) and wasm-safe
  `MemEngine` (`memengine.rs`) implement `SessionVault` (`session.rs:31`); the sans-IO
  `Session` runs over iroh natively (`iroh_net.rs`) and relay-only in the browser
  (`iroh_wasm.rs`). `PROTO = 3` (`wire.rs:19`).
- **Ingest primitive**: `Engine::record_write` (`engine.rs:298`) dedups by content
  hash, classifies (`log.rs:108`), chains onto the branch-scoped tip; `capture_rescan`
  (`engine.rs:838`) batches a whole disk diff and folds once. This is the shape the
  git importer reuses.
- **Derived git export** (read-only, unrelated to remotes): hand-rolled object writer
  in `gitexport.rs` (sha1 + flate2, deterministic single commit under `.asp/git`).
  Proves we already know how to write git objects; it is *not* pushable (no shared
  ancestry with any real repo).
- **`asp git` passthrough** (`crates/asp/src/gitcli.rs`): read-only allowlist against
  the derived repo; every mutating verb is currently refused — so `pull`/`push`/
  `remote` are free namespace for the bridge.
- **Scope**: `.git` is always ignored at any depth (`scope.rs`, `ALWAYS_IGNORE_DIRS`)
  — the bridge's git state never leaks into the synced log.
- **Clone-from-peer UX**: pristine vault adopts the peer's `vault_id` on `Hello`
  (`session.rs:169,222`); desktop modal at `desktop/src/App.tsx:2074` with
  `api.cloneRemote`; Tauri commands bind invoke args **by name** (see fix `f6c1d07`).

---

## 2. Design overview: the git remote is a peer

The whole design reduces to one idea: **model the git remote as one more ASP site.**

- Its edits (upstream commits) enter the log as ordinary rows authored under a
  derived, repo-stable `site_id`, chained to each other exactly like a real peer's
  chain. Local edits that raced an upstream commit fork the per-file chain and the
  existing 3-way merge folds them — no new merge machinery.
- Outbound, the vault's rows roll up into synthesized git commits built **on top of
  the imported upstream history**, so pushes are ordinary fast-forwards that GitHub
  accepts and humans can read.
- Everything a bridge node needs in order to act (what was ingested, what to commit,
  which message) is either **deterministically derived from the log** or **carried in
  synced log records**. Two bridge nodes therefore compute byte-identical git commits
  and byte-identical ingested rows; racing pushes update the ref to the *same* SHA,
  and racing pulls dedup by Merkle id. "Any node may bridge" falls out of determinism
  instead of coordination.

Three new engine modules in `asp-core`, following the existing native/wasm seam:

| Module | Contents | wasm? |
|---|---|---|
| `gitwire.rs` | pkt-line + smart-HTTP protocol-v2 framing (`ls-refs`, `fetch`, `send-pack` request/response builders + parsers). Pure, sans-IO: bytes in, bytes out. | yes |
| `gitimport.rs` | pack decode → commit walk → deterministic replay into `SessionVault` rows; ledger logic. Pure over `BlobStore` + a pack reader. | yes |
| `gitbridge.rs` | native driver: transports (HTTPS via `reqwest`, SSH via spawned `ssh`), the local bare repo under `.asp/gitremote/`, commit synthesis + push, pull loop. | `cfg(not(wasm32))` |

The browser gets `gitwire` + `gitimport` plus a `fetch()`-backed transport in
`asp-wasm` (routing through the relay proxy). Same protocol logic everywhere; only
the byte transport differs — the same pattern as `session.rs` vs `iroh_net`/`iroh_wasm`.

### 2.1 Dependency strategy

Preferred: **`gix` (gitoxide) component crates, not the top-level `gix` facade**:

- `gix-pack` for packfile decode/encode (delta resolution is the one genuinely hard
  part; do not hand-roll it). It is pure Rust; **verify it builds for wasm32 with
  default features off** — this is Risk R1 (§14).
- `gix-object`/`gix-hash` for loose object encode/decode if convenient (we already
  hand-roll this in `gitexport.rs`; reuse whichever is smaller).
- Do **not** take `gix-transport`/`gix-protocol` — we need transports the crates
  don't model (browser `fetch()` through a proxy), and protocol v2's framing is small
  enough that owning it in `gitwire.rs` keeps the sans-IO seam clean, matching how
  `wire.rs`/`session.rs` already own the ASP protocol.
- **No `git2`/libgit2** (C dependency, no wasm), **no shelling out to system git**
  for protocol work (may be absent; fragile). Exception: the SSH *transport* spawns
  the user's `ssh` binary (§10) — that is the industry-standard approach (gix and
  jgit do the same) and only applies on native.

Push (`send-pack`) is implemented in `gitwire.rs` + a pack *writer*: client sends
`old-oid new-oid ref` + a pack of missing objects. We generate every object we push
(commits/trees from the fold, blobs from `BlobStore`), so "missing objects" is
exactly "objects we created since the remote tip" — no negotiation subtleties.

---

## 3. Import: full-DAG replay

### 3.1 What is replayed

**Every commit reachable from the remote default branch's tip** — the full DAG, so
a merged PR's side-branch commits become real timeline points on a real ASP branch,
joined to `main` by a real merge node. (Unmerged remote refs are still excluded;
only HEAD's ancestry imports.)

**Canonical order.** Commits linearize by a deterministic topological sort: parents
before children; among ready commits, order by `(committer_ts, sha)`. Every identity
rule in §3.2 keys off this order, so the sort is domain-versioned (`"v1"`) and
frozen once shipped, like the hash domains.

**Lane assignment (git graph → ASP branches).**
- HEAD's first-parent chain is ASP `main`.
- For each merge commit `M` assigned to lane `B`: walk each non-first parent's
  first-parent chain until an already-assigned commit; each such unassigned chain
  becomes a new ASP branch `S`, forked where the walk stopped. Recurse — side
  chains contain their own merges. Octopus merges (N > 2 parents, rare): each extra
  parent yields its own side branch and its own chained `Merge` row.
- **Branch names** are pure functions of commit data (deterministic): parsed from
  the merge subject when it matches standard patterns (`Merge pull request #N from
  owner/name`, `Merge branch 'name'`), else `git/<short tip sha>`; collisions dedup
  with a `-2`, `-3` suffix in canonical order.
- Branch metadata rides ordinary `Kind::Branch` rows (`branch.rs`): a create record
  (with `fork_vv` = the imported frontier at the fork commit) authored at the side
  chain's first commit's canonical position, and a **delete record right after the
  merge marker** — mirroring GitHub's delete-after-merge, so the graph UI renders
  the full network-tab history while the live branch list stays clean
  (`git.keep_imported_branches = true` to skip the deletes).

**Per-commit batch**, authored on the commit's assigned branch:

1. For a merge commit: a `Kind::Merge` row first (`parent` = destination tip,
   `merge_parent` = source branch's tip row) so the ASP graph shows an explicit
   merge node, exactly as `merge_branch` would have authored it.
2. Tree-diff vs **first parent** (empty tree for the root commit) → per-file rows:
   - added path → `Kind::Create`
   - modified path → `Kind::Edit` (`base_hash` = previous imported content,
     `result_hash` = new content)
   - deleted path → `Kind::Delete`
   - exact rename (same blob oid disappears at one path, appears at another) →
     `Kind::Rename`. Content-similarity renames import as Delete+Create (matches
     `capture_rescan`'s identical-content-only inference, `engine.rs:875`).
   - new empty tree → `Dir` create (ASP materializes real dirs).
3. One **commit marker row** (`Kind::GitCommit`, §8.1) carrying
   `{sha, author name/email, committer ts, message (subject + body), parents,
   assigned branch}`. The UI attributes the batch's rows to the git author via this
   marker; `LogRow.site_id` stays the repo site (below).

Rows in a batch are `parent`-chained per `file_id` onto the previous imported row
for that file **on that lane**, exactly like a peer's edit chain. `file_id` is
allocated at first appearance and **follows renames** (git rename → same `file_id`,
matching ASP's rename semantics).

**Fidelity invariant.** After each imported batch, `fold(assigned branch)` equals
the commit's git tree — by construction, because the diff rows explicitly state
every changed file's `result_hash` and chain last, so they determine the fold
regardless of what `merge3` would have computed across the merge. Tests enforce
this invariant per-commit on fixture repos (§10); it is the load-bearing check that
DAG import got the branch scoping right.

**Timeline payoff worth calling out:** imported rows carry `ts` = committer time, so
`state_as_of(t)` / `file_at(path, t)` (`engine.rs:1222,1245`) immediately make the
whole git history scrubbable in the existing timeline UI, for free.

### 3.2 Deterministic genesis — independent clones converge

All fields of imported rows are pure functions of the git history, so two nodes that
independently clone the same repo author **byte-identical Merkle ids**:

| LogRow field | Rule |
|---|---|
| `site_id` | `hex(sha256("asp-git-site/v1" ‖ root_commit_sha))[..32]` — repo-stable, remote-URL-independent (survives mirror moves) |
| `seq` | dense counter over (commits in canonical topo order × rows in canonical batch order: branch-create record, Merge row, diff paths sorted bytewise, commit marker, branch-delete record) |
| `lamport` | `1 + global row index` for genesis replay into a pristine vault (local max is 0, so this is also `max+1`-consistent) |
| `ts` | commit's committer timestamp (ms) |
| `branch_id` | the assigned lane: `MAIN_BRANCH_ID` for the first-parent chain, else the imported branch's `derive_id` (`branch.rs:55`) — itself deterministic, since lineage + `fork_vv` derive from deterministic rows |
| `file_id` | `hex(sha256("asp-git-file/v1" ‖ root_sha ‖ first_commit_sha ‖ first_path))` |

And the vault identity itself derives from the repo:
`vault_id = hex(sha256("asp-git-vault/v1" ‖ root_commit_sha))`.

Consequence: if Alice and Bob each paste the same GitHub URL on different machines,
they get vaults that can immediately sync with *each other* over normal ASP
anti-entropy — every genesis row dedups by id. This is the property that makes the
feature feel native rather than bolted on. (Escape hatch: `--new-identity` clones
into a random `vault_id` for users who want two intentionally-separate vaults.)

Clone of a repo whose history you *partially* have (e.g. re-pointing at a fork)
composes for free: shared prefix rows dedup, divergent suffix forks and merges.

### 3.3 Materialization & scope seeding

After replay, the normal `materialize()` writes the working tree. Two git-specific
additions at clone time:

- **`.aspignore` seeding.** Without it, the first `cargo build`/`npm install` would
  capture `target/`/`node_modules/` into the synced log — disastrous. Clone writes a
  root `.aspignore` containing: a generated header comment, the repo's **root**
  `.gitignore` patterns verbatim, then `# --- from .gitignore above; edit freely ---`.
  Nested `.gitignore`s are appended with their directory prefix applied to each
  pattern (best-effort; negations that don't survive prefixing are dropped with a
  logged warning). The file is ordinary vault content — synced, user-editable.
- **File modes.** ASP doesn't model the executable bit. The ledger (§4) records
  `path → mode` for every path at the imported tip; commit synthesis (§5) replays
  the last-known mode (default `100644`) so pushes don't strip `+x`. A local chmod
  is invisible to ASP (documented limitation). Symlinks (`120000`) import as their
  target-path text with `merge_class = Text` and a marker in the ledger so push
  re-encodes them as symlinks; they materialize as real symlinks on native, as text
  files on web.

Skipped/degraded content, each surfaced in the clone report: submodules (gitlink
entries recorded in the ledger, materialized as nothing; `.gitmodules` imports as a
normal file), LFS pointers (import as the pointer text; warn once per repo).

### 3.4 Cost & guardrails

Row count ≈ total changed-paths across the **whole DAG** (every side-branch commit
contributes its own diff) — O(history), not O(commits × tree), but materially more
than first-parent-only on merge-heavy repos, and each merged PR also adds two
branch records. Monorepos are real: `--depth <n>` (CLI) / "Import last N commits"
(UI advanced option) replays only the last `n` first-parent commits of `main`
**plus the full side ancestry merged within that window**, preceded by **one
synthetic snapshot batch** of the tree at the cut point (marker records the cut sha
so the ledger and push ancestry stay correct). Determinism holds for equal `depth`. A
pre-flight `ls-refs` + pack-header size estimate warns above a threshold (default
500 MB pack) before downloading.

---

## 4. Ongoing pull

Native bridge nodes fetch the remote periodically (in the `watch` loop, default every
5 min + jitter, and on demand). Browser pulls on demand / on interval while open.

### 4.1 The ledger

A synced record (`Kind::GitIngest`, §8.1) is appended after each successfully
ingested commit: `{commit_sha, upto: (site_id, seq) of the batch's last row, mode
table delta, remote ref state}`. Every node — bridge or not — can therefore answer
"which git commit is the vault at?" locally; the UI status chip reads it straight
from the fold. Per-remote config (URL, auth ref, policy) lives in a new SQLite
`git_remotes` table (native) / engine state (wasm) — node-private, **never synced**
(credentials and URLs may differ per node).

### 4.2 Ingest algorithm (per new upstream commit, first-parent order)

A fetch typically reveals a merged PR **all at once** (the new merge commit plus its
entire side chain, since we only track the default ref): the ingest delta runs the
same lane assignment as §3.1 — side chain becomes an ASP branch (create record,
rows, merge marker, delete record), then processing continues on `main`.

1. Skip if a `GitIngest` for this sha is already in the fold (another bridge won).
2. Author the import batch as in §3.1, except identity fields, which cannot be
   globally deterministic in a live vault: `site_id` = same derived repo site;
   `seq` = next dense seq for that site *as seen locally*; `lamport` = local
   `max+1`; `parent` = the file's current tip **on the imported chain** (i.e. the
   last imported row for that file_id, not the local-edit tip).
3. Append the `GitIngest` record; fold once per fetch batch; fan out as `Msg::Push`.

Chaining imported rows to the imported chain (not the local tip) is what makes a
raced local edit an ordinary concurrent fork: fold's 3-way merge resolves it with
`base_hash` = the shared imported ancestor. This is identical to how two live peers
converge today; no new semantics.

### 4.3 The double-ingest race, honestly

Two bridges can both ingest commit X in the seconds before their `GitIngest` records
cross. Result: two row-batches with different ids but **identical `result_hash`
content**, forking each file's chain and immediately re-merging to identical bytes —
content convergence is guaranteed, history shows the commit twice (both markers
retained; UI collapses markers with equal `sha`). Two `GitIngest` records for one sha
are harmless (predicate is "any exists"). Mitigation, not prevention: fetch jitter,
and a bridge that holds a live ASP connection to another bridge yields for one jitter
interval after seeing the other's fetch activity. Accepted as a benign race.

### 4.4 Upstream force-push / history rewrite

Detected when the remote ref is not a descendant of the last-ingested sha
(`ls-refs` + local ancestry check against the bare repo). The bridge **stops**,
appends no rows, and surfaces a persistent error state ("remote history was
rewritten — run `asp git rebaseline`"). `rebaseline` (explicit, destructive-ish,
confirmed): ingests the rewritten tip as one snapshot batch (tree-diff current vault
state vs new tip) and records a `GitIngest` with a `rebaselined: true` flag. No
automatic healing in v1.

---

## 5. Push: deterministic commit synthesis from CommitPlans

### 5.1 CommitPlan records

A **CommitPlan** (`Kind::GitPlan`, §8.1) is a synced log record that says "everything
up to frontier F becomes one commit with message M":

```
{ frontier: VersionVector,      // which rows the commit includes
  message: String,              // subject + body
  author:  String,              // "Name <email>", default from vault/node config
  planned_ts: i64 }             // becomes the commit's author/committer date
```

Plans are ordered by (lamport, tiebreak) like every row; each plan's effective
frontier is `max(own frontier, all earlier plans' frontiers, frontier of the last
GitIngest at/below it)` — monotonic by construction, and **including the ingest
frontier guarantees a synthesized commit can never revert upstream changes that were
already merged into the log**.

### 5.2 Synthesis — a pure function, so any node may push

Given the fold, the plans, and the last pushed/ingested base commit `B`:

```
for each unpushed plan P (in order):
    tree   = git tree of fold(state at P.effective_frontier on main)   // modes/symlinks from ledger
    commit = { tree, parent: previous, author/committer: P.author @ P.planned_ts, message: P.message }
base of the chain = B
```

Every input is synced data ⇒ every bridge computes **identical commit SHAs**. Racing
pushes both try `old=B new=X` for the same `X`: one wins, the other sees the ref
already at `X` and treats it as success. Idempotent by construction — this is why
"any node may bridge" needs no leader election. (Contrast: the existing derived
export `gitexport.rs` is deterministic but ancestry-free; this chain is rooted in
real upstream history, so hosts accept it as a fast-forward.)

If upstream advanced past `B` since a plan was created: pull first (§4), which
merges upstream into the log and raises the ingest frontier; unpushed plans then
synthesize on top of the *new* tip (their effective frontier now includes the
ingested rows). Unpushed synthesized SHAs change — fine, they're derived; pushed
ones are immutable. Content conflicts cannot reach the git layer: they were already
resolved by ASP's fold before synthesis.

Push target: the remote's default branch by default; `git_remotes.push_ref` lets
cautious users target e.g. `refs/heads/asp` instead. Non-fast-forward rejection from
the host (a human pushed between our fetch and push) → re-fetch, re-synthesize,
retry (bounded).

### 5.3 Rollup policies — who authors plans, and when

Plan *authorship* is where policy lives; synthesis stays fixed and deterministic.
Per-vault config `git.policy` (node-local execution, but any node's plans are valid):

- **`manual`** (v1 default): a plan is created only by explicit user action —
  `asp git push`, or the desktop "Commit & push to git" button, with an editable
  message pre-filled from a summary of the pending diff.
- **`interval(window, quiescence)`**: a bridge node authors a plan when the vault
  has pending rows and either no row arrived for `quiescence` (default 10 min) or
  `window` (default 4 h) elapsed since the last plan. Message auto-generated:
  `"asp: N files changed (paths…)"`. Guard against duplicate plans from two bridges:
  before authoring, wait `jitter`; skip if an equal-frontier plan arrived.
- **`llm` (hook, not engine)**: the engine never calls a model. It exposes
  `pending_git_diff()` (unified diff + stats since the last plan frontier) and
  `author_plan(frontier, message)`. An external agent — Claude Code via MCP, a cron,
  the desktop app — decides boundaries and writes messages. Because the message is
  *recorded in the synced plan*, determinism of synthesis is unaffected by the
  nondeterministic generator. This is the "LLM-relevance" algorithm slot Chris
  described, kept outside the engine where it belongs.

---

## 6. Data model & protocol changes

### 6.1 New log kinds (`log.rs`)

Three additive `Kind` variants, all content-free of file bytes, metadata carried in
existing fields the way `Kind::Branch` already does (`log.rs:32`), with msgpack
payloads in a blob referenced by `result_hash` where they don't fit:

- `GitCommit` — import marker (§3.1). `path` = commit sha (cheap indexed lookup).
- `GitIngest` — ledger record (§4.1).
- `GitPlan` — commit plan (§5.1).

### 6.2 Protocol version

Unknown `Kind` fails msgpack decode on old peers ⇒ **bump `PROTO` 3 → 4**
(`wire.rs:19`). Session already refuses mismatched protos at `Hello`, so old peers
get a clear "peer speaks proto 4, upgrade" error, not corruption.
**Decided (2026-07-06): coordinated same-day upgrade** of the current small fleet
(the Fly vault was reseeded 2026-07-01 and peers are few) — no two-step
understand-then-author release. Revisit the two-step discipline once vaults exist
that Chris doesn't operate.

### 6.3 Native storage (`sqlite.rs`)

- `git_remotes(remote_id, url, push_ref, policy, auth_ref)` — node-private.
- `git_modes(path, mode)` + symlink/gitlink markers — derived cache of ledger state
  (rebuildable from the fold; a cache like `git_blobs`).
- Bare repo at `.asp/gitremote/<remote_id>/` (objects + refs only, no worktree) —
  the local git object store for fetch negotiation, ancestry checks, and push pack
  assembly. Already inside `.asp` ⇒ never scanned or synced.

Wasm/`MemEngine`: no bare repo. The browser keeps only shas + the ledger (all in the
log) — enough to fetch with `have = last ingested sha` and decode the returned pack
in memory. It never pushes, so it never needs historical objects. Engine state
round-trips through the existing `dump_state`/`load_state` OPFS path unchanged.

---

## 7. Surfaces

### 7.1 CLI (`crates/asp/src/main.rs`)

- `asp clone <source> [dir]` — `<source>` auto-detected: iroh ticket | 64-hex node
  id | git URL (`https://`, `ssh://`, `git@host:path`, or path ending `.git`).
  Detection order: try git-URL syntax first (unambiguous), else `parse_peer`
  (`iroh_net.rs:152`). Flags: `--depth <n>`, `--new-identity`, `--token <t>` /
  `ASP_GIT_TOKEN`, `--watch`.
- `asp git pull|push|status|remote [add|remove|show]` — new real subcommands,
  intercepted in `gitcli.rs` **before** the read-only passthrough (all currently
  refused verbs, so no behavior change for existing users). `asp git push` with
  `manual` policy = author plan (opens `$EDITOR` for the message or `-m`) +
  synthesize + push.
- `asp watch` — when a remote is configured, the loop gains the periodic
  fetch/ingest tick and the policy tick, next to the fs watcher and reconnects
  (`main.rs:730`).
- `asp relay --git-proxy` — see §7.3.

### 7.2 Desktop

- **UI**: the existing connect modal's textarea (`App.tsx:2107`) becomes
  "Invite code **or git URL**". On input matching a git URL: swap the Access-key
  field for Token (only shown for `https://`; `ssh://` shows "uses your SSH agent"),
  keep the destination picker, keep `cloneProg` with phases
  `'fetching' | 'replaying' | 'saving'`.
- **Tauri**: new commands `clone_git(dest, url, token, depth)`, `git_pull(id)`,
  `git_push(id, message)`, `git_status(id)` in `commands.rs`, registered in
  `lib.rs`. **Invoke-arg names must match JS keys exactly** (lesson of `f6c1d07`);
  add the same round-trip test that caught that.
- **Engine** (`desktop/engine/src/lib.rs`): `clone_git` mirrors `clone_remote`
  (`lib.rs:589`) — open engine, run the native bridge via `block()` (never
  `rt.block_on`, `lib.rs:293`), persist to `desktop_folders.json` with a new
  optional `git` field in `FolderCfg`, reconnect/re-arm ticks in
  `reopen_saved_streaming`. Progress bubbles over the existing
  `vault-scan-progress` event channel (`src-tauri/src/lib.rs:60`).
- Vault card gets a git status chip (from the ledger): `↑3 ↓0 · a1b2c3 · pushed 2h ago`.

### 7.3 Web + the relay CORS proxy

- `webApi.ts` `cloneGit(url, token, onProgress)`: new `WasmEngine` → `git_clone(url,
  token, proxy_base, depth, on_progress)` (new wasm-bindgen entry in
  `asp-wasm/src/lib.rs`) → fetch pack via proxy → `gitimport` replay into
  `MemEngine` → persist to OPFS, register with a `git` field in `registry.json`.
  Live-follow: periodic `git_pull` while the tab is open; and because bridges write
  everything into the log, a web vault that is *also* connected to a native peer
  gets git updates over ordinary ASP sync with zero git traffic from the browser.
- **Proxy** (in the relay binary, `--git-proxy`): forwards exactly two shapes —
  `GET <host>/<path>/info/refs?service=git-upload-pack` and
  `POST <host>/<path>/git-upload-pack` — and adds CORS headers. Hard rules:
  HTTPS-only upstreams; resolve + reject private/loopback/link-local ranges (SSRF);
  ports 443 only; no cookies; pass through only `Authorization`,
  `Content-Type`, `Accept`; never log `Authorization`; request/response size caps
  (default 1 GB) and idle timeouts; per-IP rate limit; optional
  `--git-proxy-allow <host>` allowlist for locked-down deployments. It is stateless
  ciphertext-in-ciphertext-out in spirit, like the relay itself — but note the
  git payloads are TLS-terminated at the proxy (unlike relayed ASP traffic, which
  stays E2E-encrypted). Say so in the docs.
- Browser token storage: OPFS `registry.json`, same trust level as the existing
  stored `authKey` — document that a stolen browser profile leaks it; recommend
  fine-grained, single-repo PATs in the UI copy.

---

## 8. Credentials (native)

- **HTTPS**: token per remote. Stored via the `keyring` crate (macOS Keychain /
  Secret Service / Windows Credential Manager); `git_remotes.auth_ref` holds only the
  keyring entry name. CLI fallback when no keyring: `ASP_GIT_TOKEN` env or prompt.
- **SSH**: spawn the user's `ssh` binary (`ssh -o BatchMode=yes git@host
  git-upload-pack '<path>'`) so existing keys, agents, and `~/.ssh/config` (including
  host aliases and hardware keys) just work; we never parse private keys. Host-key
  verification is ssh's own (`known_hosts`). Absent `ssh` → clear error suggesting
  the HTTPS URL.
- Never write tokens into any synced table, the log, or `desktop_folders.json`.

---

## 9. Failure modes & guardrails (summary)

| Failure | Behavior |
|---|---|
| Bad URL / repo not found / auth rejected | Fail clone before creating any vault dir; typed error to UI (`connect-error` surface) |
| Network death mid-clone | Genesis replay is all-or-nothing: rows fold only after the full pack decodes; a torn clone leaves no vault |
| Upstream force-push | Freeze bridge, persistent error, explicit `rebaseline` (§4.4) |
| Two bridges ingest same commit | Converges; duplicate marker collapsed in UI (§4.3) |
| Two bridges push same plans | Same SHA; second push is a no-op (§5.2) |
| Host rejects non-FF (human pushed mid-cycle) | Fetch → ingest → re-synthesize → retry, bounded, then surface |
| Huge repo | Pre-flight size warning; `--depth`; progress + cancel at every phase |
| Proxy abuse | SSRF guards, size/rate caps, optional host allowlist (§7.3) |
| Old peer meets git rows | Proto 4 handshake refusal with upgrade message (§6.2) |

---

## 10. Testing

- **Hermetic git fixtures**: tests create repos with `git init`/`commit` (dev-only
  dependency on system git is fine) and serve smart HTTP via `git http-backend`
  behind a tiny hyper CGI shim in `tests/e2e` — covers real wire bytes without the
  network.
- **`gitwire` unit tests**: pkt-line vectors, protocol-v2 request/response fixtures
  recorded from real GitHub/Gitea responses (checked-in byte fixtures).
- **DAG fidelity**: fixture repos covering criss-cross merges, octopus commits,
  merges into side branches, renames across merges, and a mid-history root commit;
  after every imported commit, assert `fold(assigned branch) == git tree` (§3.1)
  and that the synthesized branch graph matches `git log --graph` topology. Plus a
  corpus run against a few real gnarly public repos (invariant-only, no snapshots).
- **Determinism**: (a) two independent replays of the same fixture repo →
  byte-identical row sets and `vault_id`; (b) two engines with the same plans +
  ledger → identical synthesized commit SHAs; (c) property test: random interleaving
  of local edits, ingests, and plans on two nodes → converged fold AND converged
  synthesized chain.
- **Race tests**: double-ingest of one commit on two live-synced engines → content
  identical, single collapsed marker; simultaneous push → one 200, one no-op.
- **Round-trip**: clone → edit → plan → push → `git log`/`git show` on the fixture
  remote is sane (ancestry, modes, symlinks, renames); then commit on the remote →
  pull → fold contains it → push again → linear history.
- **Force-push** fixture → bridge freezes → `rebaseline` → converges.
- **wasm path**: `MemEngine` + a mock transport replaying recorded pack bytes
  (extend the pattern of the `memengine.rs` clone-receiver test from `73b0b7e`);
  browser e2e via the existing firefox-devtools harness against the CGI shim +
  proxy.
- **Soak**: extend the `sync-soak-test` skill scenario with a git remote in the
  loop (CLI vault bridges; desktop + web follow).

## 11. Milestones

1. **M1 — Read-path core, native (internal checkpoint, not a release).** `gitwire`
   + `gitimport` + HTTPS fetch; first-parent-only replay with deterministic
   genesis, `.aspignore` seeding, `--depth`. Proto 4 groundwork (kinds defined,
   bump shipped to the fleet). First-parent replay is scaffolding for M2, not a
   shippable mode — v1 requires the full DAG.
2. **M2 — Full-DAG import.** Lane assignment, branch synthesis + `fork_vv`
   placement, merge markers, octopus handling, delete-after-merge, branch naming,
   and the per-commit fidelity-invariant test suite. First releasable `asp clone`.
3. **M3 — Ongoing pull.** Ledger, watch-loop fetch tick (including merged-PR delta
   ingestion), force-push freeze + `rebaseline`, desktop clone UI + status chip.
4. **M4 — Push, manual policy.** Plans, deterministic synthesis, bare-repo pack
   assembly, `asp git push`, desktop "Commit & push", token keyring + SSH transport.
5. **M5 — Web.** wasm fetch transport, relay `--git-proxy` with SSRF guards, web
   clone/pull UI.
6. **M6 — Policies.** `interval`, `pending_git_diff()`/`author_plan()` hook for
   LLM-driven rollup; docs + soak coverage.

## 12. Open questions / risks

- **R1 — `gix-pack` on wasm32**: assumed to build with default features off
  (mmap etc. disabled); must be verified first thing in M1. Fallback: hand-rolled
  pack reader (deflate via `flate2/rust_backend` + ofs/ref-delta resolution) —
  bounded but real work.
- **R2 — full-DAG import is the schedule risk of v1.** Lane assignment must survive
  hostile real-world histories: criss-cross merges, foxtrot merges, octopus
  commits, merges *into* side branches, root commits mid-history (subtree grafts).
  The per-commit fidelity invariant (§3.1) is the safety net — any history the
  lane algorithm mishandles fails loudly in tests, not silently in a vault. Budget
  M2 accordingly; corpus-test against a handful of gnarly public repos.
- **R3 — imported-branch volume.** A mature repo can carry tens of thousands of
  merged PRs ⇒ tens of thousands of `Branch` create+delete records. Delete-after-
  merge keeps the *live* list clean, but `BranchSet`, the graph API
  (`build_graph`, `branch.rs:255`), and the network-graph UI must stay fast at that
  cardinality — needs a perf test at ~50k imported branches before M2 ships.
- **R4 — mode/symlink fidelity on web**: browser can't represent symlinks; a web
  edit to a symlink-backed file converts it to a regular file on next push unless
  the ledger marker is honored — synthesis must consult the ledger, not the
  materialized form (spec'd in §5.2, flagging it here as the easiest thing to get
  wrong).

**Resolved in interview (2026-07-06):** replay depth → full DAG in v1 (was R2);
proto rollout → coordinated same-day fleet upgrade (was R3, §6.2); branch mapping →
`main`-only push with a UI hint on other branches; default policy → `manual`.

---

## Implementation status (shipped)

All six milestones landed. `PROTO` is bumped to `4` (`wire.rs`). Module and test
map (paths under `crates/asp-core/src` and `tests/e2e/tests` unless noted):

| Milestone | What shipped | Modules | Tests |
|---|---|---|---|
| **M1 — read-path core** | pkt-line + smart-HTTP v2 framing; pack decode → commit-DAG walk → `ImportPlan`; deterministic genesis (`vault_id`/`site_id`/`file_id` domains); `.aspignore` seeding; `--depth`; proto-4 kinds | `gitwire.rs`, `gitimport.rs`, `gitgenesis.rs`, `gitrecord.rs` | `git_transport.rs`, `git_genesis.rs`, `git_import_model.rs`, `git_wasm_path.rs`; wire fixtures in `tests/e2e/src/bin/*.bin` |
| **M2 — full-DAG import** | lane assignment (first-parent = `main`, side chains → ASP branches), branch synthesis + `fork_vv`, merge markers, octopus, delete-after-merge, deterministic branch naming; per-commit `fold == git tree` fidelity invariant | `gitimport.rs`, `gitgenesis.rs` | `git_import_model.rs`, `git_genesis.rs`; fixtures (criss-cross/foxtrot/octopus/merge-into-side/renames-across-merge/mid-history-root) in `tests/e2e/src/gitfix.rs`, harness self-test `tests/e2e/tests/git_harness.rs` |
| **M3 — ongoing pull** | `GitIngest` ledger; `pull_once` fetch/ingest (incl. merged-PR delta); force-push freeze + `rebaseline`; `git_remotes`/`git_modes` sqlite tables; desktop clone UI + status chip | `gitremote.rs`, `gitgenesis.rs` (ingest) | `git_clone_pull.rs`, `git_coherence.rs`; desktop `desktop/src/App.git.test.tsx` |
| **M4 — push, manual policy** | `GitPlan` records; deterministic commit synthesis; bare-repo pack assembly + `send-pack`; `asp git push`; token keyring + SSH transport; desktop "Commit & push" | `gitpush.rs`, `gitbridge.rs`, `gitremote.rs` | `git_push.rs`, `git_convergence_prop.rs`, `git_ingest_race.rs` |
| **M5 — web** | wasm `fetch()` transport; relay `--git-proxy` with SSRF guards; web clone/pull UI | `gitproxy.rs`, `gitwire.rs`/`gitimport.rs` (wasm), `desktop/src/lib/webApi.ts` (`cloneGit`) | `git_wasm_path.rs`, proxy tests in `gitproxy.rs`; browser e2e `desktop/e2e/git-clone-check.mjs` (written; runs on a capable machine / CI) |
| **M6 — policies** | `interval` auto-plan; `pending_git_diff()` / `author_plan()` LLM hook (`asp git diff` / `asp git plan`) | `gitpolicy.rs`, `gitpush.rs` | `git_policy.rs`; soak scenario in `.claude/skills/sync-soak-test/SKILL.md` (git-in-the-loop variant) |

**Addendum — import open branches at clone** (`specs/git-open-branches.md`): opt-in
phase-2 genesis that imports every unmerged `refs/heads/*` as a **live** ASP branch.
Surfaces: CLI `asp clone --all-branches`; desktop/web Advanced-section checkbox "Also
import open branches" → `cloneGit(…, allBranches, …)` → Tauri `clone_git{…,allBranches}`
/ wasm `git_clone(…, all_branches, …)`; clone report gains `open_branches` +
`refs_skipped`. `ImportOptions.open_branch_tips` drives the planner; `PlannedLane.live`
+ `ImportPlan.skipped_reachable` carry the outcome. Snapshot semantics (only the default
branch keeps syncing); native pull re-attaches a later-merged imported branch via
`synthesize_ingest_with_open_branches` (§4) — the web pull does not (documented
follow-up). Tests: `git_open_branches_model.rs`, `git_open_branches.rs`,
`git_wasm_path.rs` (all-branches fold), `branch_scale.rs` (live-lane variant), desktop
`desktop/engine/tests/git_bridge.rs` + `desktop/src/App.git.test.tsx`.

CLI surface (`crates/asp/src/main.rs` + `gitcli.rs`): `asp clone <git-url>
[--depth] [--all-branches] [--new-identity] [--token] [--watch]`; `asp git status|pull|push [-m]
[--author]|diff [--json]|plan -m|policy [manual|interval|show]|rebaseline --yes|
remote add <url> [--push-ref] [--policy] [--token]|remote remove|remote show`;
`asp watch` (pull + interval-policy ticks); `asp relay --git-proxy [--git-proxy-addr]
[--git-proxy-allow]`.
