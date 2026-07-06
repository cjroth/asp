# Spec addendum: import open (unmerged) branches at clone

Extends `specs/git-bridge.md` (the base spec). That spec's v1 deliberately
imported **only HEAD's ancestry** — merged PRs become ASP branches, but open
remote branches don't exist in the vault at all. Real active repos (e.g.
thunderbird/thunderbolt: 130 branches, 128 open) are mostly *made of* open
branches, so v1's framing inverts their value. This addendum adds an opt-in.

**Decision (Chris, 2026-07-06):** a clone-time checkbox ("also import all open
branches"), spec'd here; delivered with the same verification layers as the
base bridge.

## 1. Scope

- **In:** clone-time import of every `refs/heads/*` whose tip is **not**
  reachable from the default branch's tip, as **live** ASP branches (create
  record, no delete tombstone), on all three surfaces (CLI flag
  `--all-branches`, desktop/web checkbox). Pull-time handling when a
  previously-imported open branch later merges upstream (§4).
- **Out (unchanged from base spec):** pushing any ASP branch other than
  `main`; ongoing per-branch pull/refresh of open branches (a checkbox clone
  is a **snapshot** of the open branches; only the default branch keeps
  syncing — §5); importing tags-as-branches; remote refs outside
  `refs/heads/*`.
- Branch refs whose tip IS reachable from HEAD (old release pointers, just-
  merged branches) are **skipped** with a per-clone report count — they carry
  no commits that aren't already imported, and materializing empty pointer
  branches adds noise without content. (Revisit if a "track ref pointers"
  need appears.) **Pinned (implementation):** "reachable" means reachable from
  the HEAD tip, NOT "present in the planned set" — under `--depth` a tip that
  is a cut ancestor of HEAD is still skipped (it carries no unique work even
  though the depth window excluded it from the plan).

## 2. Deterministic emission — phase 2 of genesis

Genesis emission becomes two phases, both under the existing identity rules
(same repo `site_id`, dense `seq`, `lamport = seq+1`, domains unchanged):

- **Phase 1 — unchanged:** HEAD's full DAG exactly as the base spec §3. A
  checkbox clone's phase-1 **commit rows** are **byte-identical** to a plain
  clone's — the checkbox only appends. (Property: a plain clone and a checkbox
  clone of the same snapshot share their phase-1 history prefix and dedup over
  ASP sync. Pinned exception: the seeded `.aspignore` row is authored LAST,
  after phase 2, so that single row's seq differs between plain and checkbox
  clones and does not cross-dedup — benign, base-spec §4.3 class.)
- **Phase 2 — open branches**, in canonical order: branches sorted by **ref
  name, bytewise**; per branch, its unique commits (reachable from the branch
  tip, not already emitted in phase 1 or an earlier phase-2 branch) in the
  same canonical topo order (`committer_seconds, sha` — domain "v1",
  unchanged). Within a branch's subgraph, lane assignment recurses exactly as
  base-spec §3.1: the branch tip's first-parent chain is the branch's own
  lane; merges *inside* it spawn sub-lanes (which ARE tombstoned after their
  merge marker, as in phase 1 — they're merged with respect to this branch).
- **Fork point** = the first already-emitted commit hit walking the branch's
  first-parent chain; the lane forks from whichever lane owns that commit
  (main, a merged side lane, or an earlier open branch — commit→lane
  ownership is tracked across the whole emission). `fork_vv` = the frontier
  at that commit's last emitted row, same rule as phase 1. A branch sharing
  no history (orphan ref) forks nowhere: `fork=None`, diff-vs-empty root.
- **Branch identity:** name = the git ref name verbatim (`cjroth/acp` stays
  `cjroth/acp`); collisions with phase-1 names dedup `-2`, `-3` in emission
  order. `branch_id = derive_id(...)` as always. Live (no delete record).
- A per-branch `GitCommit` marker is authored for every imported commit, as
  in phase 1 (markers are what make pull dedup work — §4).

## 3. Determinism, honestly

For **equal ref snapshots**, checkbox clones are byte-deterministic end to
end (same rows, same ids) — test-enforced. But open branches move constantly,
so two checkbox clones taken at different times will emit different phase-2
row sets: shared-prefix rows do NOT generally dedup across them (seq shifts),
and the same git branch can materialize as two ASP branch entities (`x`,
`x-2`) after sync. This is the same benign-content/duplicate-history class as
base-spec §4.3, but at branch granularity — content converges, history
duplicates.

**Guidance encoded in UI copy + docs:** the checkbox is for the *first* clone
of a repo; additional devices should clone **from that vault over ASP** (the
normal invite-code path), not re-run a checkbox git clone. The base spec's
"paste the same URL on two machines" property is only guaranteed for the
default-branch history (phase 1).

## 4. Pull after an open branch merges upstream (the R-risk of this addendum)

Base-spec pull assigns lanes for a merged-PR delta by walking until an
"already-assigned" commit. With phase-2 imports, commits can be already
assigned **to an existing live ASP branch**. Required behavior:

1. The pull driver builds the assigned-set from existing `GitCommit` markers
   (`sha → (branch_id, tip row)`) — phase-2 commits are therefore never
   re-imported (no duplicate rows).
2. The upstream merge commit imports as a `Merge` row on main whose
   `merge_parent` is the **existing imported branch's tip row**, and the
   ledgered branch gets its delete record right after (delete-after-merge now
   applies — it's merged). Net effect: the open branch you imported at clone
   becomes a merged branch with a real merge node, exactly as if it had been
   merged pre-clone.
3. Commits pushed to the open branch upstream *after* the clone (between
   snapshot and merge) are imported as part of the merge delta, chained onto
   the imported branch tip (they extend the existing lane, not a new one).

Force-push/rebase of an open branch upstream does NOT freeze the bridge (that
rule stays scoped to the default branch); the stale imported branch simply
stays as-snapshotted, and the eventual merge of the rebased branch imports
its (new-sha) commits as ordinary delta — the old snapshot branch remains as
local history. Document; don't auto-heal.

## 5. Surfaces

- **CLI:** `asp clone <git-url> --all-branches` (with `--depth`: depth applies
  to phase 1 as today; phase 2 imports each open branch's unique commits in
  full — they're typically shallow relative to main).
- **Desktop/web modal:** Advanced section checkbox — "Also import open
  branches" with a live count when cheap (`ls-refs` already returns all heads:
  show "(N open branches)") and the §3 one-primary-clone caveat as helper
  text. Default OFF.
- **Invoke/wasm contracts (extend, keeping f6c1d07 discipline):**
  `invoke('clone_git', { dest, url, token, depth, allBranches })` ↔ Rust
  `all_branches: bool`; wasm `git_clone(..., all_branches: bool, ...)`;
  `webApi.cloneGit(url, token, depth, allBranches, onProgress)` (adjust `Api`
  signature accordingly). Clone report gains `open_branches_imported`,
  `refs_skipped_reachable`.
- **Status:** `asp git status` / desktop chip unchanged (they track the
  default branch); `asp branch list` (and the vault switcher) simply shows the
  live imported branches.

## 6. Cost guardrails

- Fetch: one pack with wants = HEAD tip + all qualifying open-branch tips
  (single negotiation; the pre-flight size warning from base §3.4 covers the
  bigger pack).
- 128 live branches is nothing for the graph after the O(lanes) fix, but the
  R3 `branch_scale` guardrail must gain a live-branch (no-tombstone) variant
  at ~10k lanes to keep `build_graph`/`BranchSet` honest for pathological
  repos (thousand-open-branch monorepos exist).

## 7. Verification (same layers as the base bridge)

- **Fixture:** `gitfix::open_branches()` — a repo with: 2 merged PRs (phase-1
  material), 4+ open branches incl. one forked from a *side* lane, one
  containing its own internal merge, one orphan (unrelated root), one ref
  pointing at an ancestor of main (must be skipped), names that collide with
  a merged-branch name.
- **Ground truth per branch:** after checkbox clone, for every imported live
  branch: `fold(branch)` == `git ls-tree -r <branch tip>`; branch list ==
  `git branch` minus skipped refs; phase-1 prefix rows byte-identical to a
  plain clone's.
- **Determinism:** two checkbox clones of the same snapshot → identical row
  sets + branch ids (e2e, both engines).
- **Merge-after-import pull (§4):** clone w/ checkbox → merge one open branch
  upstream (with an extra post-clone commit on it) → `pull_once` → single
  merge node onto the existing ASP branch, no duplicate rows, branch
  tombstoned, fold(main) == new upstream tree. THE load-bearing test of this
  addendum.
- **Fuzz:** LCG loop generating random open-branch topologies (random fork
  points incl. off side lanes, random internal merges) → plan/emission never
  panics, every branch's fidelity invariant holds, emission deterministic
  across two builds.
- **Surfaces:** invoke-arg guard extended for `allBranches`; modal test
  (checkbox visible for git URLs, passed through); wasm path test with a
  multi-branch fixture pack; CLI e2e with `--all-branches`.
- **Perf:** `branch_scale` live-lane variant (§6).
