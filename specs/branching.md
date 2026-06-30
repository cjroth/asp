# Spec: Git-style branching for asp

## 0. Goal, scope, non-goals

**Goal.** Add first-class branches to asp: a user can scrub to a past point, edit,
and have that edit live on a **separate branch** that does *not* auto-merge into
main; branches are named, switchable, mergeable; and the history view renders the
branch/commit DAG like GitHub's network tab.

**In scope:** branch data model, branch-scoped folding, create/checkout/merge/
delete, "edit-in-past ⇒ branch", multi-ref derived-git export, a graph API +
network-graph UI, full test/eval/fuzz coverage, sync of branch metadata, protocol
versioning/migration.

**Non-goals:** rebasing, cherry-pick, partial-file branch selection, per-file (vs
per-vault) branches, remote branch ACLs. (Leave hooks but don't build.)

**The central design fact.** asp is a CRDT that *always converges to one state*:
`fold_order` linearizes all rows and `merge3` collapses concurrent forks. Branches
must therefore be a **scoped view over the shared log**, not a fork of the log
itself. Concurrent edits *within* a branch still auto-merge (CRDT preserved per
branch); rows on *different* branches simply aren't in each other's fold scope.

---

## 1. Current substrate (what we build on)

- `crates/asp-core/src/log.rs` — `LogRow { id, site_id, lamport, seq, ts, file_id,
  kind, merge_class, parent, base_hash, result_hash, path, sig }`; `id` is a Merkle
  hash of contents (`seal()`); `(site_id, seq)` is UNIQUE.
- `crates/asp-core/src/fold.rs` — `fold_order(rows)` (causal topo sort +
  `(lamport,site_id,id)` tiebreak), `compute_files(store, rows)`, and `FoldState`
  (per-`file_id` incremental cache; `refold_files`, `files()`).
- `crates/asp-core/src/engine.rs` — `record_write/remove/rename`, `tip(file_id)`,
  `current_for_path`, `materialize()` (folds → `files` table → disk →
  `export_git`), `snapshot/restore`, `state_as_of(t)`, `read_file_at`, admission.
- `crates/asp-core/src/sqlite.rs` — `log`, `files`, `snapshots`, `git_blobs`,
  `version_vector()` (`site→max seq`), `rows_for_file`.
- `crates/asp-core/src/gitexport.rs` — writes a real git object store under
  `.asp/git`, single `refs/heads/main`, deterministic commit SHAs.
- `crates/asp-core/src/session.rs` / `net.rs` — version-vector catch-up +
  `integrate`/`integrate_many`.
- Desktop: `desktop/engine/src/lib.rs` (Tauri pass-through), `desktop/src/App.tsx`,
  `desktop/src/vault/HistoryBar.tsx`.
- Tests/fuzz: `crates/asp-core/tests/{fold_props.rs (generative differential),
  fuzz_invariants.rs (adversarial), engine_snapshot.rs}`,
  `desktop/engine/examples/sync_fuzz.rs`,
  `desktop/engine/tests/sync_surface_probes2.rs`.

---

## 2. Model: branches as scoped views

### 2.1 Definitions
- **Branch record** `Branch { branch_id: String (content-hash, stable), name:
  String, parent: Option<branch_id>, fork_vv: VersionVector, created_lamport,
  created_ts, tip_hint }`. The root branch is `main` (`parent = None`,
  `fork_vv = {}`). Branch records are themselves **synced** (see §7).
- Every `LogRow` gains a **`branch_id`** field identifying the branch it was
  authored on. Default/back-compat value = the `main` branch_id.
- **`fork_vv`** = the version vector of the *parent branch's visible rows* at the
  instant of the fork (uses existing `version_vector()` machinery).

### 2.2 Visibility (the one rule everything derives from)
A row `r` is **visible on branch B** iff:
```
visible(B) =  { r : r.branch_id == B }
           ∪  { r ∈ visible(parent(B)) : (r.site_id, r.seq) ≤ B.fork_vv[r.site_id] }
              // ancestor rows, but only up to the fork point
```
- Root: `visible(main) = { r : r.branch_id == main }`.
- Well-defined recursion over the (acyclic) branch tree. Compute once per fold as a
  row predicate.

**Consequences (the invariants tests will pin):**
- *Isolation:* a row authored on B is `branch_id==B`, so it's excluded from
  `visible(sibling)` and from `visible(parent)` (parent only sees rows it tagged).
  Branches don't bleed.
- *Within-branch CRDT:* two devices both on B tag rows `branch_id==B` ⇒ both
  visible on B ⇒ auto-merge as today.
- *Back-compat:* with only `main`, `visible(main)` = all rows ⇒ behavior
  byte-identical to today.

### 2.3 Branch-scoped state
`state(B) = compute_files(store, visible(B))`. The engine materializes the
**currently checked-out branch** (`HEAD`) to disk + `files` table, exactly as it
does today for the single converged state.

### 2.4 Branch-aware authoring
- `tip(file_id)` and `current_for_path(rel)` become **branch-scoped**: the tip of
  file F on B = highest-OrderKey row in `visible(B)` for F. New rows authored on B
  set `branch_id = HEAD`, `parent = branch-scoped tip`. Per-file chains stay
  coherent per branch.

### 2.5 Edit-in-the-past ⇒ branch
1. User scrubs to instant T (or a snapshot) — read-only as today (`read_file_at`).
2. On the **first edit while time-travelling**, the engine: derives `fork_vv` =
   version vector of rows with `ts ≤ T` on the current branch (or the snapshot's
   recorded VV); creates branch `B` (`parent = HEAD`, that `fork_vv`); sets
   `HEAD = B`; then authors the edit `branch_id==B`, `parent = B-scoped tip of that
   file` (the historical row ≤ fork_vv). main continues untouched.
3. UI surfaces "You're now on a new branch (from <date>)" with a rename affordance.

### 2.6 Merge
`merge_branch(src=B, into=A)`:
1. Compute `state(A)`, `state(B)`, and `base = state(common_ancestor(A,B))` (the
   fork point).
2. For each file, author `branch_id==A` rows bringing A to `merge3(base, A, B)`
   per file (reuse the existing merge engine; conflicts surface per `merge_class`
   exactly as today).
3. Author one **merge marker row** (new `Kind::Merge`, `parent = A-tip`, second
   parent recorded in a new `merge_parent` column) so the graph shows an explicit
   2-parent merge node and `asp git` shows a real merge commit.
4. Idempotence: re-merging with no new rows on B authors nothing.

### 2.7 Determinism & convergence (must hold; tested)
- Two nodes holding the same rows + same branch records, with B checked out,
  produce **byte-identical `state(B)`** and **identical `refs/heads/<B>` SHA**.
- The branch-scoped `FoldState` equals a from-scratch `compute_files(visible(B))`
  after every row, any arrival order.

---

## 3. Data-model changes

### 3.1 `LogRow`
- Add `branch_id: String` and `merge_parent: Option<String>` (only set on
  `Kind::Merge`).
- Add `Kind::Merge` (and `Kind::Branch` for synced branch records, see §7).
- These are **covered by the Merkle `id`** (`seal()` must hash them) and the
  **wire format** (`wire.rs`) ⇒ **protocol version bump** (`Msg::Hello.proto`).
  See §9.
- Back-compat: rows without `branch_id` (pre-upgrade DB) read as `main`.

### 3.2 SQLite (`sqlite.rs`)
```sql
ALTER TABLE log ADD COLUMN branch_id TEXT NOT NULL DEFAULT '<main_id>';
ALTER TABLE log ADD COLUMN merge_parent TEXT;
CREATE INDEX IF NOT EXISTS log_branch ON log(branch_id);
CREATE TABLE IF NOT EXISTS branches(
  branch_id TEXT PRIMARY KEY, name TEXT NOT NULL, parent TEXT,
  fork_vv TEXT NOT NULL, created_lamport INTEGER, created_ts INTEGER,
  deleted INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS head(singleton INTEGER PRIMARY KEY CHECK(singleton=0), branch_id TEXT NOT NULL);
```
New `Store` methods: `branches()`, `put_branch(&Branch)`, `branch(id)`,
`set_head(id)`/`head()`, `version_vector_for(predicate)`, and
`rows_visible_on(branch_id) -> Vec<LogRow>` (or a predicate/streaming variant — see
perf §8.6). `files` materializes one state — it now materializes HEAD's state; add a
`branch_id` column to `files` only if we cache multiple branches' materializations
(default: only HEAD is materialized to disk).

### 3.3 HEAD / checkout
- `head` table holds the checked-out branch. `checkout(B)` re-materializes
  disk+`files` to `state(B)`. This is the one expensive user action (full
  re-materialize of the target branch); show the existing "Opening…" overlay
  (`withOpening`).

---

## 4. Core engine (`asp-core`)

1. **Fold scoping.** Add `FoldState::from_rows_scoped` / a `visible` predicate;
   `materialize()` folds `visible(HEAD)`. `FoldState` keying stays per-`file_id`;
   the `dirty`/refold path filters by visibility. The incremental cache is
   per-checked-out-branch — on `checkout`, rebuild `FoldState` from
   `visible(new HEAD)` (keep a small per-branch `FoldState` LRU to avoid thrash).
2. **Branch ops:** `create_branch(name, parent, fork_vv)`, `checkout(branch_id)`,
   `merge_branch(src, into)`, `delete_branch(id)` (soft-delete; rows remain for
   sync/history), `branches()`, `current_branch()`.
3. **Authoring:** thread `branch_id = HEAD` through `record_write/remove/rename/
   dir`; make `tip`/`current_for_path` branch-scoped; implement §2.5 edit-in-past.
4. **`gitexport` (`gitexport.rs`):** export **one ref per branch**
   (`refs/heads/<name>`), each from `state(branch)`; build a real commit DAG — a
   branch commit's parent is the prior commit on that branch; `Kind::Merge` ⇒ a
   2-parent merge commit. Keep deterministic SHAs *per branch*. Reuse the in-memory
   `git_oids` memo.
5. **Graph API:** `graph() -> { nodes: [{ commit_id, branch_id, parents: [..], ts,
   lamport, label }], branches: [{ id, name, parent, head_commit }] }`. Build from
   the branch records + the per-branch commit chain (coarsen rows to "settle"
   commits, as gitexport already does, so the graph isn't one node per keystroke).
   Also extend `history()` to optionally return `parent`, `file_id`, `branch_id`
   for a row-level DAG view.
6. **Time-travel within a branch** (`state_as_of`, `read_file_at`) becomes scoped
   to `visible(HEAD)`.

---

## 5. Desktop engine + Tauri (`desktop/engine`, `src-tauri`)
Add thin pass-through commands (no logic): `list_branches`, `create_branch`,
`checkout_branch`, `merge_branch`, `delete_branch`, `current_branch`, `graph`.
Mirror the existing `#[tauri::command(async)]` pattern. `checkout` and `merge`
broadcast their authored rows + branch records live to peers (like `restore` does
today). Extend `desktop/src/lib/api.ts` interface + the web (`webApi.ts`) impl.

---

## 6. Frontend (`desktop/src`)
1. **Network-graph view** (new `vault/BranchGraph.tsx`): render `graph()`
   GitHub-network-style — one horizontal lane per branch, nodes per settle-commit,
   edges following `parents` (incl. fork edges where a branch's first commit's
   parent is on another lane, and merge edges into 2-parent nodes). Color by
   branch; current branch + HEAD highlighted; hover shows commit/files; click =
   time-travel/checkout. Reuse `HistoryBar`'s virtualization/tick-cap discipline
   (cap rendered nodes; the graph must stay bounded at thousands of commits).
2. **Branch switcher** in the chrome (create / rename / switch / merge / delete),
   next to the vault switcher.
3. **Edit-in-past flow:** when the user edits while scrubbed back, prompt/auto-
   create a branch (§2.5) and show a non-blocking "on branch X" banner.
4. **Loading:** `checkout`/`merge` reuse the `withOpening` overlay (they
   re-materialize).

---

## 7. Sync semantics
- **Rows** carry `branch_id`; they sync exactly as today via version-vector
  catch-up (sync is branch-agnostic at the transport layer — every node holds all
  rows for all branches).
- **Branch records** must also converge. Model each branch record as a special
  **synced row** (a `Kind::Branch` log row carrying the branch metadata,
  content-hashed id, last-writer-wins on `(name, deleted)` by `(lamport, site_id,
  id)`), so branch creation/rename/delete ride the *same* anti-entropy path and
  converge deterministically. (Avoids a separate sync channel.)
- **HEAD is per-device, NOT synced** (like git's checked-out branch). Each node
  folds whatever it has checked out.
- **Concurrent branch creation** with the same name on two devices ⇒ two distinct
  `branch_id`s, deterministic display-name de-dup (` (2)`), same suffixing
  discipline as path collisions.

---

## 8. Testing / evals / fuzzing

This is the gate. Reuse and extend the existing harnesses; every new behavior gets
a test that fails on the pre-change code.

### 8.1 Unit (asp-core)
- `visible(B)` predicate: ancestors-up-to-fork, isolation, root case, multi-level
  lineage.
- Branch-scoped `tip`/`current_for_path`.
- `create_branch`/`checkout`/`merge_branch`/`delete_branch` happy-path + edge
  (merge with no changes = no-op; merge of unrelated branches; delete current
  branch rejected or auto-checkout main).
- `Kind::Branch`/`Kind::Merge` `seal()`/wire round-trip (id covers new fields).

### 8.2 Generative differential (extend `tests/fold_props.rs`)
- Extend the generator to author rows on **multiple branches** with branch records
  + fork_vvs.
- **Primary gate:** branch-scoped `FoldState` == `compute_files(visible(B))`,
  **after every row, in random arrival order**, across hundreds of seeds (mirror
  the current incremental-fold differential).
- **Isolation property:** for siblings A,B, `state(A)` is invariant under adding/
  removing rows tagged `branch_id==B` (and vice versa).
- **Back-compat property:** a single-branch history folds byte-identically to
  today's `compute_files` over all rows.
- **Determinism:** `state(B)` is permutation-invariant (per branch).

### 8.3 Adversarial (extend `tests/fuzz_invariants.rs`)
- Arbitrary/garbage `branch_id`s, `merge_parent`s, and **cyclic/dangling branch
  lineage** (`parent` pointing at unknown or self) — fold/`visible` must never
  panic and stay deterministic; a cycle in branch lineage is broken
  deterministically (like `fold_order`'s cycle handling).
- `fork_vv` referencing unknown sites / huge seqs.
- Tampered branch records rejected (Merkle id check), never corrupt the branch set.

### 8.4 Cross-surface sync fuzz (extend `desktop/engine/examples/sync_fuzz.rs`)
- New scenarios: `CreateBranch`, `Checkout`, `EditOnBranch`, `MergeBranch`,
  `DeleteBranch`, `EditInPast` (the fork flow), interleaved with existing ops,
  under 1/2/3 peers.
- Convergence asserts, **per branch**: every surface holds byte-identical
  `state(B)` (for the checked-out branch's disk), identical **`refs/heads/<B>`**
  SHA (extend the git-head check to all refs), and identical **branch-record
  sets**.
- **Isolation under sync:** edits on B must never change another peer's
  `state(main)`.
- Concurrent branch creation (same name on 2 peers) converges to 2 branches with
  deterministic de-dup.
- Concurrent merges; merge-during-active-editing-on-the-source-branch.
- Reuse `--prefill N` for scale; add `--branches K`.
- Keep disk + git-head + (new) branch-set convergence as the per-round invariant.

### 8.5 Surface probes (extend `desktop/engine/tests/sync_surface_probes2.rs`)
- Branch create/checkout/merge/delete **propagate to a live peer**.
- Offline: fork on a partitioned device, edit, reconnect — branch + its rows catch
  up; both sides agree on the branch set and per-branch state.

### 8.6 Performance / scale
- Branch-scoped fold is **O(|visible(B)|)**, not O(all rows): bench at many
  branches × deep history; assert per-op latency is bounded by the checked-out
  branch's visible size, not total log. (Implement `rows_visible_on` to push the
  predicate into SQL where possible; avoid loading all rows.)
- `checkout` re-materialize cost measured; `graph()` bounded and fast at thousands
  of commits / hundreds of branches (coarsen + cap, like the history tick cap).
- Confirm the in-memory `git_oids` memo + incremental `FoldState` still hold per
  branch.

### 8.7 Frontend (vitest, `desktop/src`)
- `BranchGraph` renders the right nodes/edges/lanes for a fixture `graph()`; fork
  and 2-parent merge edges present; node count bounded at scale.
- Branch switcher: create/switch/rename/merge/delete wire to the api.
- **Edit-in-past creates a branch** end-to-end against the mock backend; main is
  unchanged; the editor follows the new branch.
- Live-update: a peer creating/merging a branch shows up via the poll without
  manual refresh (extend `App.livesync.test.tsx`).

### 8.8 Acceptance bar
- All existing tests green unchanged (back-compat).
- `fold_props` branch differential clears ≥ the current seed budget with 0
  divergences.
- `sync_fuzz` (with branch scenarios) hits a clean streak across 1/2/3 peers and
  `--prefill`/`--branches` scale, 0 disk/git-head/branch-set divergences.
- A manual eval (`desktop/engine/examples/branch_demo.rs`): fork from history, edit
  both lines, merge, dump `.asp/git` and confirm `git log --graph --all` shows the
  expected branchy DAG.

---

## 9. Protocol versioning & migration
- Bump `Msg::Hello.proto`. New↔new speak branches. **New↔old:** old peers don't
  understand `branch_id`; recommend **graceful degrade** — branch/merge rows still
  sync as opaque rows (old peers ignore unknown `Kind`, so data isn't lost) but old
  peers only fold `main`. (Alternative: hard cutover requiring all peers upgraded.)
- DB migration: `ALTER TABLE` adds with defaults; existing rows become `main`;
  create the `main` branch record + `head=main` on first open of an upgraded vault.
  Idempotent; covered by a `persist_roundtrip`-style migration test.

---

## 10. Risks & open questions
- **`branch_id` on every row** inflates the log and changes the Merkle id ⇒
  unavoidable protocol bump; get it right once.
- **Fold-cache per branch:** checkout invalidates/rebuilds `FoldState`; frequent
  switching could thrash — mitigate with a small per-branch `FoldState` LRU.
- **Merge UX for `merge_class`:** code conflicts surface markers (today's
  behavior); text clean-resolves. Confirm that's the desired branch-merge UX or add
  an interactive resolver (out of scope here).
- **Snapshots vs branches:** keep snapshots as lightweight tags; a branch can be
  created *from* a snapshot's recorded VV. Don't conflate.
- **Open:** do we ever materialize more than one branch to disk at once? (Spec
  assumes only HEAD; multi-worktree is a non-goal.)

---

## 11. Phasing (one spec, staged delivery — each phase independently shippable & green)
1. **P1 — Graph view (read-only), no model change.** Expose `parent`/`file_id`/
   (implicit) `branch_id=main` from `history()`; ship `BranchGraph` over the
   existing DAG (forks from real multi-device concurrency; merges implicit). Lands
   value immediately, zero core risk.
2. **P2 — Branch data model + scoped fold + checkout.** `branch_id`/`Kind::Branch`,
   `branches`/`head` tables, `visible()`, scoped `FoldState`, create/checkout,
   edit-in-past. Differential + isolation + back-compat tests. No merge yet.
3. **P3 — Merge + `Kind::Merge` + multi-ref gitexport + real merge nodes.**
4. **P4 — Sync of branch records + multi-branch `sync_fuzz`/probes + protocol
   bump/migration.**
5. **P5 — Frontend polish:** switcher, edit-in-past banner, live-update, scale/perf
   hardening.

Ship P1 first (visible payoff, de-risks the graph UI); P2–P4 are the core lift
gated by the fuzz/differential battery; P5 finishes the UX.

---

## 12. Definition of done
Branches are creatable from any point in history (incl. "edit in the past"),
switchable, mergeable; the history view shows a GitHub-style network graph with
fork and 2-parent merge nodes; branch data and metadata converge across CLI/
desktop/web peers; per-branch state and per-ref git head are deterministic;
isolation holds (no cross-branch bleed except via explicit merge); back-compat is
byte-identical for single-branch vaults; and the full unit/differential/
adversarial/sync-fuzz/UI/perf battery in §8 is green with 0 divergences.
