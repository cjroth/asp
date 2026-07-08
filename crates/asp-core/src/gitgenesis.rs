//! gitgenesis — the pure, wasm-safe **row-synthesis** half of the git bridge
//! (git-bridge §3.1/§3.2, §4.2). It turns the deterministic history model produced
//! by [`crate::gitimport`] ([`ImportPlan`]) into sealed [`LogRow`]s ready to fold
//! into a vault — with byte-deterministic genesis identity so two nodes that clone
//! the same repo author byte-identical rows and vault id.
//!
//! Two entry points, one shared per-commit batch builder:
//!
//! * [`synthesize_genesis`] — a **pristine** clone. Identity fields are *globally*
//!   deterministic (git-bridge §3.2): `site_id`/`vault_id` derive from the root
//!   commit, `seq` is a dense 0-based counter over the whole emission order, and
//!   `lamport = 1 + row index`. Also seeds a `.aspignore` and the mode/symlink/
//!   gitlink tables the push side needs.
//! * [`synthesize_ingest`] — an **ongoing** pull into a live vault (git-bridge §4.2).
//!   Same batch *shape*, but identity fields are *local*: the caller supplies the
//!   next dense `seq`, the next `lamport`, and the current imported-chain tips via
//!   [`IngestContext`], so a raced local edit becomes an ordinary concurrent fork
//!   that the fold's 3-way merge resolves.
//!
//! **Everything here is a pure function** of the plan + git blob bytes: no fs, no
//! clock, no RNG. It compiles to `wasm32` unchanged (only `std` + the always-on
//! `asp-core` surface), so the native `Engine` and the wasm `MemEngine` synthesize
//! byte-identical rows.
//!
//! ## Frozen `"v1"` genesis-identity byte layouts (IDENTITY-BEARING — do not change)
//!
//! All three hash a domain tag **immediately followed by** the hex-ASCII git sha(s)
//! and (for a file) the UTF-8 path, hashed **once** with SHA-256 (plain
//! concatenation — *not* [`crate::oid::merkle_id`] length-prefixing):
//!
//! * [`git_site_id`]  = `hex(sha256(b"asp-git-site/v1"  ++ root_sha))[..32]`
//! * [`git_vault_id`] = `hex(sha256(b"asp-git-vault/v1" ++ root_sha))`         (64 hex)
//! * [`git_file_id`]  = `hex(sha256(b"asp-git-file/v1"  ++ root_sha ++ first_commit_sha ++ first_path))` (64 hex)
//!
//! where `root_sha`/`first_commit_sha` are the 40-char lowercase hex commit shas and
//! `first_path` is the UTF-8 path bytes. These are pinned by `genesis_id_vectors`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::branch::{encode_branch_record, Branch, VersionVector};
use crate::error::AspResult;
use crate::gitimport::{
    EntryMode, FileOp, GitObjKind, GitObjectDb, ImportPlan, ImportWarning, LaneId, PlannedCommit,
    PlannedLane, MAIN_LANE,
};
use crate::gitrecord::{
    build_commit_marker_row, build_ingest_row, GitCommitMarker, GitIngestRecord, GitRowIdentity,
};
use crate::log::{classify, Kind, LogRow, MergeClass, MAIN_BRANCH_ID};
use crate::oid::content_hash;
use crate::store::BlobStore;

/// The default remote ref a ledger record names when the caller doesn't override it.
pub const DEFAULT_REMOTE_REF: &str = "refs/heads/main";

// ===========================================================================
// Git blob source — the tiny read seam (decoupled from `GitObjectDb`)
// ===========================================================================

/// The one thing row synthesis needs from the git object store: a blob's bytes by
/// sha. A trait (not a concrete `GitObjectDb`) so callers can back it with a
/// `HashMap` in tests and the wasm engine can back it with whatever it decoded.
pub trait GitBlobSource {
    /// The decompressed bytes of blob `sha` (40-hex), or `None` if absent.
    fn blob(&self, sha: &str) -> Option<Vec<u8>>;
}

/// Adapts a [`GitObjectDb`] to [`GitBlobSource`] (returns only *blob* objects).
pub struct DbBlobSource<'a> {
    db: &'a GitObjectDb,
}

impl<'a> DbBlobSource<'a> {
    pub fn new(db: &'a GitObjectDb) -> DbBlobSource<'a> {
        DbBlobSource { db }
    }
}

impl GitBlobSource for DbBlobSource<'_> {
    fn blob(&self, sha: &str) -> Option<Vec<u8>> {
        match self.db.get(sha) {
            Some((GitObjKind::Blob, bytes)) => Some(bytes.to_vec()),
            _ => None,
        }
    }
}

/// A `HashMap`-backed blob source (`sha -> bytes`) — the wasm-safe, git-free source
/// used by unit tests and by callers that already hold the blobs in memory.
impl GitBlobSource for HashMap<String, Vec<u8>> {
    fn blob(&self, sha: &str) -> Option<Vec<u8>> {
        self.get(sha).cloned()
    }
}

// ===========================================================================
// Genesis identity derivation ("v1", FROZEN — see module docs)
// ===========================================================================

fn hash_concat(parts: &[&[u8]]) -> String {
    let mut buf = Vec::new();
    for p in parts {
        buf.extend_from_slice(p);
    }
    content_hash(&buf)
}

/// The repo-stable, remote-URL-independent authoring `site_id` for all imported rows
/// (git-bridge §3.2). 32 hex chars (matches ordinary `file_id`/site widths).
pub fn git_site_id(root_sha: &str) -> String {
    hash_concat(&[b"asp-git-site/v1", root_sha.as_bytes()])[..32].to_string()
}

/// The vault identity derived from the repo root (git-bridge §3.2). 64 hex chars.
pub fn git_vault_id(root_sha: &str) -> String {
    hash_concat(&[b"asp-git-vault/v1", root_sha.as_bytes()])
}

/// The `file_id` allocated at a path's first appearance (git-bridge §3.2); it then
/// **follows renames**. `first_commit_sha`/`first_path` are the commit + path where
/// the id was minted. 64 hex chars.
pub fn git_file_id(root_sha: &str, first_commit_sha: &str, first_path: &str) -> String {
    hash_concat(&[
        b"asp-git-file/v1",
        root_sha.as_bytes(),
        first_commit_sha.as_bytes(),
        first_path.as_bytes(),
    ])
}

/// Deterministic `file_id` for a merge marker edge (a fold no-op, so the id only
/// needs to be stable + collision-free). 32 hex chars.
fn merge_marker_file_id(merge_sha: &str, source_tip_sha: &str) -> String {
    hash_concat(&[
        b"asp-git-merge/v1",
        merge_sha.as_bytes(),
        b"|",
        source_tip_sha.as_bytes(),
    ])[..32]
        .to_string()
}

// ===========================================================================
// Outputs
// ===========================================================================

/// The result of a pristine [`synthesize_genesis`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenesisOutput {
    /// The derived vault identity (`git_vault_id`). A pristine vault adopts this.
    pub vault_id: String,
    /// Every synthesized row, in canonical emission order, each `.seal()`ed.
    pub rows: Vec<LogRow>,
    /// One ledger record per imported commit (git-bridge §4.1) — for the caller's
    /// node-private mode cache. The equivalent `GitIngest` rows are already in `rows`.
    pub ledger: Vec<GitIngestRecord>,
    /// The generated `.aspignore` content (also emitted as a row on `main`).
    pub aspignore: String,
    /// `path -> git mode` for executable files at the imported tip (git-bridge §3.3).
    pub mode_table: Vec<(String, u32)>,
    /// Symlink paths at the imported tip (materialized as target-text on web).
    pub symlinks: Vec<String>,
    /// Gitlink/submodule paths seen in the history (materialized as nothing).
    pub gitlinks: Vec<String>,
}

/// The imported-chain tip of one file, as seen locally — the seed an ongoing ingest
/// chains onto so a raced local edit forks concurrently (git-bridge §4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedFile {
    pub path: String,
    pub file_id: String,
    /// The last imported row id for this file on `main` (the chain tip).
    pub row_id: String,
    /// Its content hash (`None` if the imported tip is a delete).
    pub content_hash: Option<String>,
}

/// The local state an ongoing [`synthesize_ingest`] threads in (git-bridge §4.2).
#[derive(Debug, Clone)]
pub struct IngestContext {
    /// The derived repo `site_id` (same as genesis: `git_site_id(root)`).
    pub site_id: String,
    /// The next dense `seq` for that site as seen locally.
    pub next_seq: u64,
    /// The local `lamport` to assign the first ingested row (`local max + 1`).
    pub next_lamport: u64,
    /// The remote ref the ledger records name.
    pub remote_ref: String,
    /// The imported-chain tips on `main` (chain onto these, not the local-edit tips).
    pub main_state: Vec<ImportedFile>,
    /// The last imported row id on `main` (marker chaining), if any.
    pub main_last_row: Option<String>,
    /// Commit shas already ingested (skip — another bridge won the race).
    pub seen: HashSet<String>,
}

/// An imported **open branch**'s replay state, threaded into an ongoing ingest so
/// that when that branch later merges upstream (`specs/git-open-branches.md` §4) the
/// delta side lane which re-derives its commits reuses the EXISTING ASP branch
/// instead of forking a duplicate: its already-imported commits are skipped, any
/// post-clone commits chain onto its tip, its consuming merge points at it, and it
/// gets a delete tombstone right after that merge. Reconstructed by the driver from
/// the branch's create record + its imported rows.
#[derive(Debug, Clone)]
pub struct ImportedBranchSeed {
    pub branch_id: String,
    pub name: String,
    pub parent_branch: Option<String>,
    pub fork_vv: VersionVector,
    pub created_lamport: u64,
    pub created_ts: i64,
    /// The last imported row on the branch (its tip commit's marker) — new commits
    /// and the consuming merge chain onto this.
    pub tip_row: Option<String>,
    /// The branch's live file tips (`path`/`file_id`/tip row/content hash).
    pub files: Vec<ImportedFile>,
}

/// The result of an ongoing [`synthesize_ingest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestOutput {
    pub rows: Vec<LogRow>,
    pub ledger: Vec<GitIngestRecord>,
    /// The next dense `seq` after this batch (caller persists it).
    pub next_seq: u64,
    /// The next `lamport` after this batch.
    pub next_lamport: u64,
    /// Executable-mode deltas introduced by this batch.
    pub mode_table: Vec<(String, u32)>,
    pub symlinks: Vec<String>,
    pub gitlinks: Vec<String>,
}

// ===========================================================================
// Public entry points
// ===========================================================================

/// Synthesize a **pristine** clone's rows from `plan` (git-bridge §3.1/§3.2). Blob
/// bytes are read from `objects`; content + payload blobs are written to `out_store`
/// (so every row's `base_hash`/`result_hash` resolves against it). Deterministic:
/// the same plan + blobs always yields byte-identical rows and `vault_id`.
pub fn synthesize_genesis(
    plan: &ImportPlan,
    objects: &dyn GitBlobSource,
    out_store: &dyn BlobStore,
) -> AspResult<GenesisOutput> {
    synthesize_genesis_with_progress(plan, objects, out_store, |_, _| {})
}

/// Like [`synthesize_genesis`], but reports `(commits_done, commit_count)` to
/// `progress` as the emission loop walks `plan.commits` — drives the clone
/// "importing" phase. Coarse (every ~1000 commits + a final tick), a pure side
/// channel: emission order and row bytes are identical to [`synthesize_genesis`].
pub fn synthesize_genesis_with_progress(
    plan: &ImportPlan,
    objects: &dyn GitBlobSource,
    out_store: &dyn BlobStore,
    progress: impl FnMut(u64, u64),
) -> AspResult<GenesisOutput> {
    genesis_inner(plan, objects, out_store, None, progress)
}

/// Like [`synthesize_genesis_with_progress`], but the caller supplies the per-blob
/// `git sha -> (content_hash, is_binary)` map **already computed by pack decode** (the
/// full-history clone spill path — see [`GitObjectDb::spilled_blob_meta`]). The blob
/// bytes already live in `out_store` (decode spilled them there), so this skips the
/// parallel hashing pre-pass AND every blob-byte read: the emitter reads each blob's
/// `content_hash` / `is_binary` straight from the map. Byte-identical output to the
/// hashing path (the map only relocates where the SHA-256 ran).
///
/// [`GitObjectDb::spilled_blob_meta`]: crate::gitimport::GitObjectDb::spilled_blob_meta
pub fn synthesize_genesis_with_meta(
    plan: &ImportPlan,
    objects: &dyn GitBlobSource,
    out_store: &dyn BlobStore,
    blob_meta: HashMap<String, (String, bool)>,
    progress: impl FnMut(u64, u64),
) -> AspResult<GenesisOutput> {
    genesis_inner(plan, objects, out_store, Some(blob_meta), progress)
}

fn genesis_inner(
    plan: &ImportPlan,
    objects: &dyn GitBlobSource,
    out_store: &dyn BlobStore,
    precomputed: Option<HashMap<String, (String, bool)>>,
    progress: impl FnMut(u64, u64),
) -> AspResult<GenesisOutput> {
    let site_id = git_site_id(&plan.root_sha);
    let vault_id = git_vault_id(&plan.root_sha);

    let mut em = Emitter::new(objects, out_store, plan, site_id, 0, 1, DEFAULT_REMOTE_REF.to_string());
    // The `main` lane exists from the start, seeded empty (git-bridge §3.2).
    em.lanes.insert(MAIN_LANE, LaneState::main());
    match precomputed {
        // Decode already hashed + binary-sniffed every blob and spilled its bytes into
        // `out_store`; adopt that map directly (no re-hash, no byte read).
        Some(map) => {
            em.precomp = Some(
                map.into_iter()
                    .map(|(sha, (content_hash, is_binary))| (sha, BlobMeta { content_hash, is_binary }))
                    .collect(),
            );
        }
        // Native (no spill): hash every referenced blob in parallel up front (see
        // `precompute_blobs`), pre-populating `out_store` and the `blob sha ->
        // content_hash/is_binary` map the sequential emission loop then consults —
        // moving the SHA-256 work off the single thread while keeping emission order +
        // row bytes identical. wasm has no threads, so it keeps the inline sequential
        // path (`precomp` stays `None`).
        None => {
            #[cfg(not(target_arch = "wasm32"))]
            {
                em.precomp = Some(precompute_blobs(objects, plan, out_store)?);
            }
        }
    }
    em.run_with_progress(plan, progress)?;

    // `.aspignore` seeding (git-bridge §3.3): generated from the tip's .gitignores,
    // emitted as an ordinary `main` file row authored last (so it syncs + is editable).
    let aspignore = em.build_aspignore();
    em.emit_aspignore(&plan.root_sha, &plan.tip_sha, &aspignore)?;

    let (mode_table, symlinks, gitlinks) = em.finish_tables();
    Ok(GenesisOutput {
        vault_id,
        rows: em.rows,
        ledger: em.ledger,
        aspignore,
        mode_table,
        symlinks,
        gitlinks,
    })
}

/// Synthesize an **ongoing** ingest delta's rows (git-bridge §4.2). `plan_delta`
/// contains only the newly-fetched commits; identity fields are local, seeded from
/// `base`. Commits whose sha is in `base.seen` are skipped. Shares the per-commit
/// batch builder with [`synthesize_genesis`].
pub fn synthesize_ingest(
    plan_delta: &ImportPlan,
    base: &IngestContext,
    objects: &dyn GitBlobSource,
    out_store: &dyn BlobStore,
) -> AspResult<IngestOutput> {
    synthesize_ingest_with_open_branches(plan_delta, base, &HashMap::new(), objects, out_store)
}

/// Like [`synthesize_ingest`], but with imported **open branches** pre-seeded
/// (`specs/git-open-branches.md` §4). `imported_lanes` maps a delta-plan lane id to
/// the existing ASP branch it resolves to (a live open branch imported at clone that
/// is now merging upstream). Each such lane is seeded from the branch's imported
/// state — so its already-imported commits are skipped, post-clone commits chain onto
/// its tip, its consuming merge's `merge_parent` is the branch's real tip row, and its
/// delete-after-merge tombstone reuses the existing branch id/lineage — instead of the
/// base-spec behavior of forking a fresh duplicate branch. Empty map = base-spec pull.
pub fn synthesize_ingest_with_open_branches(
    plan_delta: &ImportPlan,
    base: &IngestContext,
    imported_lanes: &HashMap<LaneId, ImportedBranchSeed>,
    objects: &dyn GitBlobSource,
    out_store: &dyn BlobStore,
) -> AspResult<IngestOutput> {
    let mut em = Emitter::new(
        objects,
        out_store,
        plan_delta,
        base.site_id.clone(),
        base.next_seq,
        base.next_lamport,
        base.remote_ref.clone(),
    );
    em.seen = base.seen.clone();
    // Seed `main` from the imported-chain tips the caller threaded in.
    let mut main = LaneState::main();
    main.last_row = base.main_last_row.clone();
    for f in &base.main_state {
        main.path_fid.insert(f.path.clone(), f.file_id.clone());
        main.file_tip
            .insert(f.file_id.clone(), (f.row_id.clone(), f.content_hash.clone()));
    }
    em.lanes.insert(MAIN_LANE, main);
    // Seed each merged open branch's lane from its imported state (§4). Marking the
    // lane `preseeded` suppresses a duplicate branch-create record; the existing
    // Emitter machinery then chains new commits, the merge, and the delete correctly.
    for (lane_id, seed) in imported_lanes {
        let mut ls = LaneState {
            branch_id: seed.branch_id.clone(),
            name: seed.name.clone(),
            parent_branch: seed.parent_branch.clone(),
            fork_vv: seed.fork_vv.clone(),
            created_lamport: seed.created_lamport,
            created_ts: seed.created_ts,
            path_fid: HashMap::new(),
            file_tip: HashMap::new(),
            last_row: seed.tip_row.clone(),
        };
        for f in &seed.files {
            ls.path_fid.insert(f.path.clone(), f.file_id.clone());
            ls.file_tip
                .insert(f.file_id.clone(), (f.row_id.clone(), f.content_hash.clone()));
        }
        em.lanes.insert(*lane_id, ls);
        em.preseeded.insert(*lane_id);
    }
    em.run(plan_delta)?;

    let (mode_table, symlinks, gitlinks) = em.finish_tables();
    Ok(IngestOutput {
        next_seq: base.next_seq + em.n,
        next_lamport: base.next_lamport + em.n,
        rows: em.rows,
        ledger: em.ledger,
        mode_table,
        symlinks,
        gitlinks,
    })
}

// ===========================================================================
// The shared batch builder
// ===========================================================================

/// Per-lane replay state (per-lane so concurrent renames/creates on sibling lanes
/// stay independent — a side lane is *seeded* from its parent lane at the fork).
#[derive(Default, Clone)]
struct LaneState {
    branch_id: String,
    name: String,
    parent_branch: Option<String>,
    fork_vv: VersionVector,
    created_lamport: u64,
    created_ts: i64,
    /// Live `path -> file_id` on this lane.
    path_fid: HashMap<String, String>,
    /// `file_id -> (last row id, content hash)` on this lane (`None` hash = deleted).
    file_tip: HashMap<String, (String, Option<String>)>,
    /// The last row id emitted on this lane (marker/merge chaining).
    last_row: Option<String>,
}

impl LaneState {
    fn main() -> LaneState {
        LaneState {
            branch_id: MAIN_BRANCH_ID.to_string(),
            name: "main".to_string(),
            ..LaneState::default()
        }
    }
}

/// A snapshot of one lane's replay state captured right after a fork-base commit's
/// batch, so a side lane forking there seeds from the exact parent state.
#[derive(Clone, Default)]
struct ForkSnapshot {
    path_fid: HashMap<String, String>,
    file_tip: HashMap<String, (String, Option<String>)>,
    last_row: Option<String>,
}

struct Emitter<'a> {
    objects: &'a dyn GitBlobSource,
    store: &'a dyn BlobStore,
    root_sha: String,
    site_id: String,
    seq_base: u64,
    lamport_base: u64,
    /// Rows emitted so far (row `i` gets `seq = seq_base + i`, `lamport = lamport_base + i`).
    n: u64,
    remote_ref: String,

    rows: Vec<LogRow>,
    ledger: Vec<GitIngestRecord>,
    lanes: HashMap<LaneId, LaneState>,

    /// Commit shas that some lane forks off (only these need a snapshot).
    fork_bases: HashSet<String>,
    /// `fork-base sha -> the forked lane's state at that commit`.
    snapshots: HashMap<String, ForkSnapshot>,
    /// `commit sha -> the seq of that commit's marker row` (the ingest/fork frontier).
    frontier_seq: HashMap<String, u64>,
    /// Set while emitting the current commit's marker (its seq, for the ledger).
    cur_marker_seq: u64,

    seen: HashSet<String>,
    /// Lanes pre-seeded from an existing imported open branch (git-open-branches §4):
    /// their branch-create record is NOT re-emitted (the branch already exists).
    preseeded: HashSet<LaneId>,
    /// Optional precomputed `git blob sha -> (content_hash, is_binary)` map. When
    /// `Some` (a pristine native genesis), the CPU-heavy blob hashing + binary sniff
    /// have already run in parallel and the `out_store` is pre-populated, so `emit_op`
    /// just consults the map instead of hashing/storing inline. `None` (wasm genesis,
    /// and every ongoing ingest) keeps the original sequential inline path. Both paths
    /// emit byte-identical rows — the map only relocates *where* the hashing happened.
    precomp: Option<HashMap<String, BlobMeta>>,

    // Tip mode/symlink table (main-lane tip state) + gitlink paths (from warnings).
    tip_modes: BTreeMap<String, EntryMode>,
    gitlink_paths: BTreeSet<String>,
}

impl<'a> Emitter<'a> {
    fn new(
        objects: &'a dyn GitBlobSource,
        store: &'a dyn BlobStore,
        plan: &ImportPlan,
        site_id: String,
        seq_base: u64,
        lamport_base: u64,
        remote_ref: String,
    ) -> Emitter<'a> {
        let fork_bases: HashSet<String> = plan
            .lanes
            .iter()
            .filter_map(|l| l.fork.as_ref().map(|f| f.commit_sha.clone()))
            .collect();
        let mut gitlink_paths = BTreeSet::new();
        for w in &plan.warnings {
            if let ImportWarning::Submodule { path, .. } = w {
                gitlink_paths.insert(path.clone());
            }
        }
        Emitter {
            objects,
            store,
            root_sha: plan.root_sha.clone(),
            site_id,
            seq_base,
            lamport_base,
            n: 0,
            remote_ref,
            rows: Vec::new(),
            ledger: Vec::new(),
            lanes: HashMap::new(),
            fork_bases,
            snapshots: HashMap::new(),
            frontier_seq: HashMap::new(),
            cur_marker_seq: 0,
            seen: HashSet::new(),
            preseeded: HashSet::new(),
            precomp: None,
            tip_modes: BTreeMap::new(),
            gitlink_paths,
        }
    }

    /// Reserve the next `(seq, lamport)` and advance the row counter.
    fn tick(&mut self) -> (u64, u64) {
        let seq = self.seq_base + self.n;
        let lamport = self.lamport_base + self.n;
        self.n += 1;
        (seq, lamport)
    }

    fn push(&mut self, row: LogRow) {
        self.rows.push(row);
    }

    fn run(&mut self, plan: &ImportPlan) -> AspResult<()> {
        self.run_with_progress(plan, |_, _| {})
    }

    /// [`run`], reporting `(commits_done, commit_count)` coarsely (every ~1000 commits
    /// plus a final tick). Progress is a side channel — it never touches emission order
    /// or row bytes. `done` counts every commit walked (skipped-as-`seen` included), so
    /// the bar reaches its total even on a mostly-already-ingested plan.
    ///
    /// [`run`]: Emitter::run
    fn run_with_progress(
        &mut self,
        plan: &ImportPlan,
        mut progress: impl FnMut(u64, u64),
    ) -> AspResult<()> {
        const PROGRESS_STRIDE: u64 = 1000;
        let total = plan.commits.len() as u64;
        progress(0, total);
        let mut done: u64 = 0;
        for c in &plan.commits {
            if !self.seen.contains(&c.sha) {
                self.emit_commit_batch(plan, c)?; // else already ingested (§4.2 step 1)
            }
            done += 1;
            if done.is_multiple_of(PROGRESS_STRIDE) {
                progress(done, total);
            }
        }
        progress(total, total);
        Ok(())
    }

    fn emit_commit_batch(&mut self, plan: &ImportPlan, c: &PlannedCommit) -> AspResult<()> {
        // 1. branch-create record(s) for any lane whose first commit is this one.
        let mut new_lanes: Vec<PlannedLane> = plan
            .lanes
            .iter()
            .filter(|l| l.id != MAIN_LANE && l.created_at_commit == c.sha && !self.preseeded.contains(&l.id))
            .cloned()
            .collect();
        new_lanes.sort_by_key(|l| l.id);
        for lane in &new_lanes {
            self.create_side_lane(lane, c)?;
        }

        // 2. merge marker row(s) — one per non-first parent, chained.
        if !c.merges.is_empty() {
            self.emit_merges(c);
        }

        // 3. diff rows (ops already sorted bytewise by resulting path).
        for op in &c.ops {
            self.emit_op(c, op)?;
        }

        // 4. commit marker row.
        self.emit_marker(c)?;
        // 5. ingest ledger row (inline, right after the marker).
        self.emit_ingest(c)?;

        // Record this commit's frontier + snapshot its lane state if it's a fork base.
        self.frontier_seq.insert(c.sha.clone(), self.cur_marker_seq);
        if self.fork_bases.contains(&c.sha) {
            let ls = &self.lanes[&c.lane];
            self.snapshots.insert(
                c.sha.clone(),
                ForkSnapshot {
                    path_fid: ls.path_fid.clone(),
                    file_tip: ls.file_tip.clone(),
                    last_row: ls.last_row.clone(),
                },
            );
        }

        // 6. branch-delete record(s) for any lane merged (and deleted) at this commit.
        let mut del_lanes: Vec<PlannedLane> = plan
            .lanes
            .iter()
            .filter(|l| l.deleted_after_merge && l.merged_at_commit.as_deref() == Some(c.sha.as_str()))
            .cloned()
            .collect();
        del_lanes.sort_by_key(|l| l.id);
        for lane in &del_lanes {
            self.emit_branch_delete(c, lane.id)?;
        }
        Ok(())
    }

    /// Seed a side lane from its fork snapshot + author its branch-create record.
    fn create_side_lane(&mut self, lane: &PlannedLane, c: &PlannedCommit) -> AspResult<()> {
        let (snap, parent_branch, fork_vv) = match &lane.fork {
            Some(f) => {
                let snap = self.snapshots.get(&f.commit_sha).cloned().unwrap_or_default();
                let parent_branch = self
                    .lanes
                    .get(&f.lane)
                    .map(|l| l.branch_id.clone())
                    .unwrap_or_else(|| MAIN_BRANCH_ID.to_string());
                // fork_vv = imported frontier at the fork = {site: fork commit's marker seq}.
                let cap = *self.frontier_seq.get(&f.commit_sha).unwrap_or(&0);
                let mut vv = VersionVector::new();
                vv.insert(self.site_id.clone(), cap as i64);
                (snap, parent_branch, vv)
            }
            // A grafted/unrelated root: no fork, seeds empty, parents `main` nominally.
            None => (ForkSnapshot::default(), MAIN_BRANCH_ID.to_string(), VersionVector::new()),
        };

        let (seq, lamport) = self.tick();
        let branch_id = Branch::derive_id(&lane.name, &parent_branch, &fork_vv, lamport, &self.site_id);
        // `LogRow.ts` is unix SECONDS everywhere in asp-core (PITR / timeline compare
        // against second-granularity `t`); git commit ts is carried in ms, so divide.
        let created_ts = c.committer_ts_ms / 1000;
        let brec = Branch {
            branch_id: branch_id.clone(),
            name: lane.name.clone(),
            parent: Some(parent_branch.clone()),
            fork_vv: fork_vv.clone(),
            created_lamport: lamport,
            created_ts,
            deleted: false,
        };
        let blob = encode_branch_record(&brec);
        let h = self.store.put_blob(&blob)?;
        let row = LogRow {
            site_id: self.site_id.clone(),
            lamport,
            seq,
            ts: created_ts,
            file_id: branch_id.clone(),
            kind: Kind::Branch,
            merge_class: MergeClass::Text,
            parent: None,
            base_hash: None,
            result_hash: Some(h),
            path: Some(lane.name.clone()),
            branch_id: MAIN_BRANCH_ID.to_string(),
            merge_parent: None,
            sig: vec![],
            id: String::new(),
        }
        .seal();
        self.push(row);

        self.lanes.insert(
            lane.id,
            LaneState {
                branch_id,
                name: lane.name.clone(),
                parent_branch: Some(parent_branch),
                fork_vv,
                created_lamport: lamport,
                created_ts,
                path_fid: snap.path_fid,
                file_tip: snap.file_tip,
                last_row: snap.last_row,
            },
        );
        Ok(())
    }

    fn emit_merges(&mut self, c: &PlannedCommit) {
        let dest_lane = c.lane;
        let dest_branch = self.lanes[&dest_lane].branch_id.clone();
        let mut dest_tip = self.lanes[&dest_lane].last_row.clone();
        for m in &c.merges {
            let source_tip = self.lanes.get(&m.source_lane).and_then(|l| l.last_row.clone());
            let (seq, lamport) = self.tick();
            let row = LogRow {
                site_id: self.site_id.clone(),
                lamport,
                seq,
                ts: c.committer_ts_ms / 1000,
                file_id: merge_marker_file_id(&c.sha, &m.source_tip_sha),
                kind: Kind::Merge,
                merge_class: MergeClass::Binary,
                parent: dest_tip.clone(),
                base_hash: None,
                result_hash: None,
                path: None,
                branch_id: dest_branch.clone(),
                merge_parent: source_tip,
                sig: vec![],
                id: String::new(),
            }
            .seal();
            dest_tip = Some(row.id.clone());
            self.push(row);
        }
        self.lanes.get_mut(&dest_lane).unwrap().last_row = dest_tip;
    }

    fn emit_op(&mut self, c: &PlannedCommit, op: &FileOp) -> AspResult<()> {
        let lane = c.lane;
        let branch_id = self.lanes[&lane].branch_id.clone();
        let is_main = lane == MAIN_LANE;
        // `LogRow.ts` is unix SECONDS (asp-core convention); git ts is carried in ms.
        let ts = c.committer_ts_ms / 1000;

        match op {
            FileOp::Create { path, blob_sha, mode } => {
                let (h, mc) = self.resolve_blob(blob_sha, path, *mode)?;
                let fid = match self.lanes[&lane].path_fid.get(path) {
                    Some(f) => f.clone(),
                    None => git_file_id(&self.root_sha, &c.sha, path),
                };
                let (seq, lamport) = self.tick();
                let row = self.content_row(
                    &branch_id, lamport, seq, ts, &fid, Kind::Create, mc, None, None,
                    Some(h.clone()), Some(path.clone()),
                );
                let id = row.id.clone();
                self.push(row);
                let ls = self.lanes.get_mut(&lane).unwrap();
                ls.path_fid.insert(path.clone(), fid.clone());
                ls.file_tip.insert(fid, (id.clone(), Some(h)));
                ls.last_row = Some(id);
                if is_main {
                    self.tip_modes.insert(path.clone(), *mode);
                }
            }
            FileOp::Edit { path, blob_sha, mode, .. } => {
                let (h, mc) = self.resolve_blob(blob_sha, path, *mode)?;
                let (fid, parent, base) = self.file_ref(lane, path);
                let (seq, lamport) = self.tick();
                let row = self.content_row(
                    &branch_id, lamport, seq, ts, &fid, Kind::Edit, mc, parent, base,
                    Some(h.clone()), None,
                );
                let id = row.id.clone();
                self.push(row);
                let ls = self.lanes.get_mut(&lane).unwrap();
                ls.file_tip.insert(fid, (id.clone(), Some(h)));
                ls.last_row = Some(id);
                if is_main {
                    self.tip_modes.insert(path.clone(), *mode);
                }
            }
            FileOp::Delete { path } => {
                let (fid, parent, base) = self.file_ref(lane, path);
                let (seq, lamport) = self.tick();
                let row = self.content_row(
                    &branch_id, lamport, seq, ts, &fid, Kind::Delete, MergeClass::Text,
                    parent, base, None, None,
                );
                let id = row.id.clone();
                self.push(row);
                let ls = self.lanes.get_mut(&lane).unwrap();
                ls.path_fid.remove(path);
                ls.file_tip.insert(fid, (id.clone(), None));
                ls.last_row = Some(id);
                if is_main {
                    self.tip_modes.remove(path);
                }
            }
            FileOp::RenameExact { from, to, blob_sha, mode } => {
                let (h, mc) = self.resolve_blob(blob_sha, to, *mode)?;
                let (fid, parent, base) = self.file_ref(lane, from);
                let (seq, lamport) = self.tick();
                let row = self.content_row(
                    &branch_id, lamport, seq, ts, &fid, Kind::Rename, mc, parent, base,
                    Some(h.clone()), Some(to.clone()),
                );
                let id = row.id.clone();
                self.push(row);
                let ls = self.lanes.get_mut(&lane).unwrap();
                ls.path_fid.remove(from);
                ls.path_fid.insert(to.clone(), fid.clone());
                ls.file_tip.insert(fid, (id.clone(), Some(h)));
                ls.last_row = Some(id);
                if is_main {
                    if let Some(m) = self.tip_modes.remove(from) {
                        let _ = m;
                    }
                    self.tip_modes.insert(to.clone(), *mode);
                }
            }
            FileOp::DirCreate { path } => {
                let fid = git_file_id(&self.root_sha, &c.sha, path);
                let (seq, lamport) = self.tick();
                let row = self.content_row(
                    &branch_id, lamport, seq, ts, &fid, Kind::Create, MergeClass::Dir,
                    None, None, None, Some(path.clone()),
                );
                let id = row.id.clone();
                self.push(row);
                self.lanes.get_mut(&lane).unwrap().last_row = Some(id);
            }
        }
        Ok(())
    }

    /// Look up a live file's `(file_id, parent row, base content hash)` on `lane`,
    /// falling back to a fresh id if (defensively) the path isn't tracked.
    fn file_ref(&self, lane: LaneId, path: &str) -> (String, Option<String>, Option<String>) {
        let ls = &self.lanes[&lane];
        match ls.path_fid.get(path) {
            Some(fid) => {
                let (row, hash) = ls.file_tip.get(fid).cloned().unwrap_or((String::new(), None));
                let parent = if row.is_empty() { None } else { Some(row) };
                (fid.clone(), parent, hash)
            }
            None => (git_file_id(&self.root_sha, "orphan", path), None, None),
        }
    }

    /// Resolve a file op's blob to `(content_hash, merge_class)`. With a precomputed
    /// map (native pristine genesis) both are read from the map and the `out_store` was
    /// already populated by the parallel pre-pass — no inline hashing. Without one (wasm
    /// genesis, ongoing ingest, or a defensive precomp miss) it falls back to the
    /// original inline path: fetch bytes, `put_blob` (hashes), `classify`. Both yield
    /// byte-identical `(hash, class)` for the same blob + path + mode.
    fn resolve_blob(
        &self,
        blob_sha: &str,
        class_path: &str,
        mode: EntryMode,
    ) -> AspResult<(String, MergeClass)> {
        if let Some(map) = &self.precomp {
            if let Some(m) = map.get(blob_sha) {
                return Ok((m.content_hash.clone(), mode_class_pre(mode, class_path, m.is_binary)));
            }
        }
        let bytes = self.objects.blob(blob_sha).unwrap_or_default();
        let h = self.store.put_blob(&bytes)?;
        Ok((h, mode_class(mode, class_path, &bytes)))
    }

    #[allow(clippy::too_many_arguments)]
    fn content_row(
        &self,
        branch_id: &str,
        lamport: u64,
        seq: u64,
        ts: i64,
        file_id: &str,
        kind: Kind,
        merge_class: MergeClass,
        parent: Option<String>,
        base_hash: Option<String>,
        result_hash: Option<String>,
        path: Option<String>,
    ) -> LogRow {
        LogRow {
            site_id: self.site_id.clone(),
            lamport,
            seq,
            ts,
            file_id: file_id.to_string(),
            kind,
            merge_class,
            parent,
            base_hash,
            result_hash,
            path,
            branch_id: branch_id.to_string(),
            merge_parent: None,
            sig: vec![],
            id: String::new(),
        }
        .seal()
    }

    fn emit_marker(&mut self, c: &PlannedCommit) -> AspResult<()> {
        let lane = c.lane;
        let (branch_id, parent) = {
            let ls = &self.lanes[&lane];
            (ls.branch_id.clone(), ls.last_row.clone())
        };
        let (seq, lamport) = self.tick();
        let marker = GitCommitMarker {
            sha: c.sha.clone(),
            author_name: c.author_name.clone(),
            author_email: c.author_email.clone(),
            committer_ts: c.committer_ts_ms / 1000,
            message: c.message.clone(),
            parents: c.parents.clone(),
            branch_id,
        };
        let ident = GitRowIdentity { site_id: self.site_id.clone(), lamport, seq, ts: c.committer_ts_ms / 1000, parent };
        let row = build_commit_marker_row(self.store, &ident, &marker)?;
        let id = row.id.clone();
        self.push(row);
        self.lanes.get_mut(&lane).unwrap().last_row = Some(id);
        self.cur_marker_seq = seq;
        Ok(())
    }

    fn emit_ingest(&mut self, c: &PlannedCommit) -> AspResult<()> {
        let mut modes: Vec<(String, u32)> = Vec::new();
        let mut symlinks: Vec<String> = Vec::new();
        for op in &c.ops {
            match op {
                FileOp::Create { path, mode, .. } | FileOp::Edit { path, mode, .. } => {
                    push_mode(&mut modes, &mut symlinks, path, *mode);
                }
                FileOp::RenameExact { to, mode, .. } => {
                    push_mode(&mut modes, &mut symlinks, to, *mode);
                }
                _ => {}
            }
        }
        let rec = GitIngestRecord {
            commit_sha: c.sha.clone(),
            upto_site: self.site_id.clone(),
            upto_seq: self.cur_marker_seq,
            modes,
            symlinks,
            gitlinks: Vec::new(),
            remote_ref: self.remote_ref.clone(),
            rebaselined: false,
        };
        let (seq, lamport) = self.tick();
        let ident = GitRowIdentity { site_id: self.site_id.clone(), lamport, seq, ts: c.committer_ts_ms / 1000, parent: None };
        let row = build_ingest_row(self.store, &ident, &rec)?;
        self.push(row);
        self.ledger.push(rec);
        Ok(())
    }

    /// Author a branch-delete tombstone for `lane_id` (merged + deleted at `c`).
    /// Reconstructs the branch record with `deleted = true`; its higher lamport wins
    /// the last-writer-wins reconcile (git-bridge §3.1 delete-after-merge).
    fn emit_branch_delete(&mut self, c: &PlannedCommit, lane_id: LaneId) -> AspResult<()> {
        let Some(ls) = self.lanes.get(&lane_id).cloned() else { return Ok(()) };
        let brec = Branch {
            branch_id: ls.branch_id.clone(),
            name: ls.name.clone(),
            parent: ls.parent_branch.clone(),
            fork_vv: ls.fork_vv.clone(),
            created_lamport: ls.created_lamport,
            created_ts: ls.created_ts,
            deleted: true,
        };
        let blob = encode_branch_record(&brec);
        let h = self.store.put_blob(&blob)?;
        let (seq, lamport) = self.tick();
        let row = LogRow {
            site_id: self.site_id.clone(),
            lamport,
            seq,
            ts: c.committer_ts_ms / 1000,
            file_id: ls.branch_id.clone(),
            kind: Kind::Branch,
            merge_class: MergeClass::Text,
            parent: None,
            base_hash: None,
            result_hash: Some(h),
            path: Some(ls.name.clone()),
            branch_id: MAIN_BRANCH_ID.to_string(),
            merge_parent: None,
            sig: vec![],
            id: String::new(),
        }
        .seal();
        self.push(row);
        Ok(())
    }

    /// Build the `.aspignore` content from the main tip's `.gitignore` files
    /// (git-bridge §3.3): header, root patterns verbatim, sentinel, then nested
    /// gitignores with their directory prefix applied.
    fn build_aspignore(&self) -> String {
        let main = match self.lanes.get(&MAIN_LANE) {
            Some(m) => m,
            None => return aspignore_header(),
        };
        let read = |path: &str| -> Option<String> {
            let fid = main.path_fid.get(path)?;
            let (_, hash) = main.file_tip.get(fid)?;
            let bytes = self.store.get_blob(hash.as_deref()?).ok().flatten()?;
            String::from_utf8(bytes).ok()
        };

        let mut out = aspignore_header();
        if let Some(root) = read(".gitignore") {
            out.push_str(root.trim_end());
            out.push('\n');
        }
        out.push_str("# --- from .gitignore above; edit freely ---\n");

        // Nested gitignores, deterministic by path.
        let mut nested: Vec<String> = main
            .path_fid
            .keys()
            .filter(|p| p.ends_with("/.gitignore"))
            .cloned()
            .collect();
        nested.sort();
        for gi in nested {
            let dir = gi.trim_end_matches("/.gitignore");
            let Some(contents) = read(&gi) else { continue };
            out.push_str(&format!("\n# from {gi}\n"));
            for line in contents.lines() {
                let t = line.trim_end();
                if t.is_empty() {
                    continue;
                }
                if let Some(stripped) = t.strip_prefix('#') {
                    out.push_str(&format!("#{stripped}\n"));
                } else if t.starts_with('!') {
                    // A negation can't be safely re-rooted under a prefix — drop it,
                    // noting the drop inline (best-effort, git-bridge §3.3).
                    out.push_str(&format!("# (dropped negation `{t}` from {gi})\n"));
                } else {
                    let pat = t.strip_prefix('/').unwrap_or(t);
                    out.push_str(&format!("{dir}/{pat}\n"));
                }
            }
        }
        out
    }

    /// Emit the generated `.aspignore` as an ordinary `main` file row, authored last.
    fn emit_aspignore(&mut self, root_sha: &str, tip_sha: &str, content: &str) -> AspResult<()> {
        let bytes = content.as_bytes().to_vec();
        let h = self.store.put_blob(&bytes)?;
        let existing = self.lanes[&MAIN_LANE].path_fid.get(".aspignore").cloned();
        let (seq, lamport) = self.tick();
        let (fid, kind, parent, base, path) = match existing {
            Some(fid) => {
                let (row, hash) = self.lanes[&MAIN_LANE].file_tip.get(&fid).cloned().unwrap_or((String::new(), None));
                let parent = if row.is_empty() { None } else { Some(row) };
                (fid, Kind::Edit, parent, hash, None)
            }
            None => {
                let fid = git_file_id(root_sha, tip_sha, ".aspignore");
                (fid, Kind::Create, None, None, Some(".aspignore".to_string()))
            }
        };
        let row = self.content_row(
            MAIN_BRANCH_ID, lamport, seq, 0, &fid, kind, MergeClass::Text, parent, base, Some(h.clone()), path,
        );
        let id = row.id.clone();
        self.push(row);
        let ls = self.lanes.get_mut(&MAIN_LANE).unwrap();
        ls.path_fid.insert(".aspignore".to_string(), fid.clone());
        ls.file_tip.insert(fid, (id, Some(h)));
        Ok(())
    }

    fn finish_tables(&self) -> (Vec<(String, u32)>, Vec<String>, Vec<String>) {
        let mut mode_table = Vec::new();
        let mut symlinks = Vec::new();
        for (path, mode) in &self.tip_modes {
            match mode {
                EntryMode::Executable => mode_table.push((path.clone(), mode.git_mode())),
                EntryMode::Symlink => symlinks.push(path.clone()),
                _ => {}
            }
        }
        let gitlinks: Vec<String> = self.gitlink_paths.iter().cloned().collect();
        (mode_table, symlinks, gitlinks)
    }
}

fn aspignore_header() -> String {
    "# .aspignore — generated from the repository's .gitignore files at clone time.\n\
     # ASP always ignores .git and its own state; these patterns extend that.\n\n"
        .to_string()
}

/// The merge class of an imported file: a symlink imports as its target-path text
/// (`Text`, git-bridge §3.3); everything else classifies by path + content.
fn mode_class(mode: EntryMode, path: &str, bytes: &[u8]) -> MergeClass {
    match mode {
        EntryMode::Symlink => MergeClass::Text,
        _ => classify(path, bytes),
    }
}

/// [`mode_class`] driven by a precomputed `is_binary` flag instead of the raw bytes.
/// Byte-identical to `mode_class(mode, path, bytes)` because [`classify`] decides
/// binary-vs-not purely from `is_binary = !utf8 || contains(0)`, and its remaining
/// branch depends only on the path extension (so `classify(path, &[])` reproduces the
/// non-binary class). See `class_matches_precomputed_binary` in the tests.
fn mode_class_pre(mode: EntryMode, path: &str, is_binary: bool) -> MergeClass {
    match mode {
        EntryMode::Symlink => MergeClass::Text,
        _ if is_binary => MergeClass::Binary,
        _ => classify(path, &[]),
    }
}

/// Precomputed per-blob facts the parallel pre-pass hands the sequential emitter: the
/// blob's `content_hash` and whether it classifies as binary. Keyed by git blob sha.
#[derive(Clone)]
struct BlobMeta {
    content_hash: String,
    is_binary: bool,
}

/// The two per-blob byte scans the emission loop needs — SHA-256 (content hash) and the
/// binary sniff — bundled so a worker computes both in one pass over the bytes.
#[cfg(not(target_arch = "wasm32"))]
fn hash_meta(bytes: &[u8]) -> BlobMeta {
    BlobMeta {
        content_hash: content_hash(bytes),
        // Must mirror `crate::log::classify`'s binary test exactly (identity-bearing).
        is_binary: std::str::from_utf8(bytes).is_err() || bytes.contains(&0),
    }
}

/// Parallel genesis pre-pass (native only). Collects every content blob referenced by
/// the plan's file ops, hashes each one's bytes (SHA-256) and binary-sniffs it in
/// parallel over scoped worker threads, pre-populating `store` (via
/// [`BlobStore::put_blob_with_hash`], so no re-hash) and returning a
/// `git blob sha -> BlobMeta` map. This is pure and order-independent — the map only
/// relocates *where* the CPU-heavy hashing runs; [`synthesize_genesis`]'s emission
/// order, seq/lamport assignment, and row bytes are unchanged.
///
/// Reads are single-threaded (a cheap `HashMap` lookup + `Vec` clone from the in-RAM
/// object DB) and chunked by a byte budget so peak RSS stays ≈ object DB + store + one
/// chunk, rather than holding a second full copy of all blob bytes. The parallel work
/// is the hashing itself, done over owned `Vec`s (no shared `!Sync` state).
#[cfg(not(target_arch = "wasm32"))]
fn precompute_blobs(
    objects: &dyn GitBlobSource,
    plan: &ImportPlan,
    store: &dyn BlobStore,
) -> AspResult<HashMap<String, BlobMeta>> {
    // Unique content-blob shas referenced by file ops (dedup: the same blob recurs
    // across commits; order is irrelevant — the result is a map).
    let mut unique: HashSet<&str> = HashSet::new();
    for c in &plan.commits {
        for op in &c.ops {
            match op {
                FileOp::Create { blob_sha, .. }
                | FileOp::Edit { blob_sha, .. }
                | FileOp::RenameExact { blob_sha, .. } => {
                    unique.insert(blob_sha.as_str());
                }
                _ => {}
            }
        }
    }
    let unique: Vec<String> = unique.into_iter().map(|s| s.to_string()).collect();
    let mut map: HashMap<String, BlobMeta> = HashMap::with_capacity(unique.len());

    // Bounded worker count — respect the OrbStack core budget (cap at 6).
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(6);

    // Byte-bounded chunks: read a batch of blobs single-threaded, hash the batch in
    // parallel, drain it into the store, repeat. Keeps the transient copy small.
    const CHUNK_BYTES: usize = 128 * 1024 * 1024;
    let mut idx = 0;
    while idx < unique.len() {
        let mut chunk: Vec<(String, Vec<u8>)> = Vec::new();
        let mut acc = 0usize;
        while idx < unique.len() && (chunk.is_empty() || acc < CHUNK_BYTES) {
            let sha = &unique[idx];
            let bytes = objects.blob(sha).unwrap_or_default();
            acc += bytes.len();
            chunk.push((sha.clone(), bytes));
            idx += 1;
        }

        let metas: Vec<BlobMeta> = if workers <= 1 || chunk.len() < 32 {
            chunk.iter().map(|(_, b)| hash_meta(b)).collect()
        } else {
            let chunk_ref: &[(String, Vec<u8>)] = &chunk;
            let sz = chunk_ref.len().div_ceil(workers).max(1);
            std::thread::scope(|s| {
                let handles: Vec<_> = (0..chunk_ref.len())
                    .step_by(sz)
                    .map(|start| {
                        let end = (start + sz).min(chunk_ref.len());
                        s.spawn(move || {
                            chunk_ref[start..end].iter().map(|(_, b)| hash_meta(b)).collect::<Vec<_>>()
                        })
                    })
                    .collect();
                handles.into_iter().flat_map(|h| h.join().expect("hash worker panicked")).collect()
            })
        };

        for ((sha, bytes), meta) in chunk.into_iter().zip(metas) {
            // Move the owned bytes into the store (no copy) — the read from the object
            // DB above already paid the one unavoidable memcpy.
            store.put_blob_with_hash_owned(&meta.content_hash, bytes)?;
            map.insert(sha, meta);
        }
    }
    Ok(map)
}

fn push_mode(modes: &mut Vec<(String, u32)>, symlinks: &mut Vec<String>, path: &str, mode: EntryMode) {
    match mode {
        EntryMode::Executable => modes.push((path.to_string(), mode.git_mode())),
        EntryMode::Symlink => symlinks.push(path.to_string()),
        _ => {}
    }
}

// ===========================================================================
// In-crate unit tests (wasm-safe, no system git — plans built by hand)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitimport::{ForkPoint, MergeInfo};
    use crate::store::MemBlobStore;
    use sha2::{Digest, Sha256};

    fn sha256_hex(parts: &[&[u8]]) -> String {
        let mut h = Sha256::new();
        for p in parts {
            h.update(p);
        }
        hex::encode(h.finalize())
    }

    #[test]
    fn genesis_id_vectors() {
        // Independent recomputation of the FROZEN byte layout (module docs). A change
        // to any domain tag or to the concatenation order trips these.
        let root = "1111111111111111111111111111111111111111";
        assert_eq!(git_site_id(root), sha256_hex(&[b"asp-git-site/v1", root.as_bytes()])[..32]);
        assert_eq!(git_vault_id(root), sha256_hex(&[b"asp-git-vault/v1", root.as_bytes()]));
        let c = "2222222222222222222222222222222222222222";
        assert_eq!(
            git_file_id(root, c, "src/main.rs"),
            sha256_hex(&[b"asp-git-file/v1", root.as_bytes(), c.as_bytes(), b"src/main.rs"])
        );

        // Widths + domain separation + input sensitivity.
        assert_eq!(git_site_id(root).len(), 32);
        assert_eq!(git_vault_id(root).len(), 64);
        assert_eq!(git_file_id(root, c, "a").len(), 64);
        assert_ne!(git_site_id(root)[..], git_vault_id(root)[..32]);
        assert_ne!(git_file_id(root, c, "a"), git_file_id(root, c, "b"));
        assert_ne!(git_vault_id(root), git_vault_id(c));

        // A concrete frozen literal for a fixed input — a hard tripwire against any
        // silent change to the hashed byte layout.
        assert_eq!(
            git_vault_id("0000000000000000000000000000000000000000"),
            GIT_VAULT_ID_ZERO_ROOT,
        );
    }

    /// Frozen vector: `git_vault_id("0"*40)`. Regenerate ONLY if the layout is
    /// intentionally re-versioned (it must not be, per module docs).
    const GIT_VAULT_ID_ZERO_ROOT: &str =
        "53279e4b340e9dbd20f3c2738998e0a8073102277a01a728127e78c3d6bc9d5b";

    /// Build a tiny linear plan: one commit creating two files, one editing one.
    fn linear_plan(objs: &mut HashMap<String, Vec<u8>>) -> ImportPlan {
        let put = |objs: &mut HashMap<String, Vec<u8>>, sha: &str, bytes: &[u8]| {
            objs.insert(sha.to_string(), bytes.to_vec());
        };
        put(objs, "blobA1", b"alpha\n");
        put(objs, "blobB1", b"bravo\n");
        put(objs, "blobA2", b"alpha\nalpha2\n");
        let root = "aaaa000000000000000000000000000000000000";
        let c1 = PlannedCommit {
            sha: root.to_string(),
            lane: MAIN_LANE,
            parents: vec![],
            merges: vec![],
            author_name: "Ada".into(),
            author_email: "ada@x".into(),
            committer_ts_ms: 1_700_000_000_000,
            message: "add a and b".into(),
            ops: vec![
                FileOp::Create { path: "a.txt".into(), blob_sha: "blobA1".into(), mode: EntryMode::Normal },
                FileOp::Create { path: "b.txt".into(), blob_sha: "blobB1".into(), mode: EntryMode::Normal },
            ],
            is_depth_cut_snapshot: false,
        };
        let c2 = PlannedCommit {
            sha: "bbbb000000000000000000000000000000000000".into(),
            lane: MAIN_LANE,
            parents: vec![root.to_string()],
            merges: vec![],
            author_name: "Ada".into(),
            author_email: "ada@x".into(),
            committer_ts_ms: 1_700_000_060_000,
            message: "edit a".into(),
            ops: vec![FileOp::Edit {
                path: "a.txt".into(),
                old_blob_sha: "blobA1".into(),
                blob_sha: "blobA2".into(),
                mode: EntryMode::Normal,
                old_mode: EntryMode::Normal,
            }],
            is_depth_cut_snapshot: false,
        };
        ImportPlan {
            commits: vec![c1, c2],
            lanes: vec![PlannedLane {
                id: MAIN_LANE,
                name: "main".into(),
                fork: None,
                created_at_commit: root.to_string(),
                merged_at_commit: None,
                deleted_after_merge: false,
                live: false,
            }],
            root_sha: root.to_string(),
            tip_sha: "bbbb000000000000000000000000000000000000".into(),
            warnings: vec![],
            skipped_reachable: vec![],
        }
    }

    /// Run genesis forcing the inline **sequential** path (`precomp = None`) — the
    /// exact code wasm runs, and what native ran before the parallel pre-pass. Used to
    /// prove the parallel path relocates hashing without changing any output byte.
    fn synthesize_genesis_seq(
        plan: &ImportPlan,
        objects: &dyn GitBlobSource,
        out_store: &dyn BlobStore,
    ) -> AspResult<GenesisOutput> {
        let site_id = git_site_id(&plan.root_sha);
        let vault_id = git_vault_id(&plan.root_sha);
        let mut em = Emitter::new(objects, out_store, plan, site_id, 0, 1, DEFAULT_REMOTE_REF.to_string());
        em.lanes.insert(MAIN_LANE, LaneState::main());
        // precomp intentionally left None → inline sequential hashing.
        em.run(plan)?;
        let aspignore = em.build_aspignore();
        em.emit_aspignore(&plan.root_sha, &plan.tip_sha, &aspignore)?;
        let (mode_table, symlinks, gitlinks) = em.finish_tables();
        Ok(GenesisOutput { vault_id, rows: em.rows, ledger: em.ledger, aspignore, mode_table, symlinks, gitlinks })
    }

    #[test]
    fn parallel_prepass_matches_sequential_bytes() {
        // The load-bearing relocation guard: the parallel pre-pass path
        // (`synthesize_genesis`, native = precomp Some) must emit byte-identical rows,
        // vault id, ledger, and store contents to the inline sequential path.
        let mut objs = HashMap::new();
        // Add a binary blob (embedded NUL) + a code blob so classify's binary vs
        // extension branches are both exercised through the precomputed `is_binary`.
        objs.insert("blobBin".to_string(), vec![0u8, 1, 2, 3, 0, 255]);
        let plan = linear_plan(&mut objs);
        let mut plan = plan;
        plan.commits[1].ops.push(FileOp::Create {
            path: "data.bin".into(),
            blob_sha: "blobBin".into(),
            mode: EntryMode::Normal,
        });

        let sp = MemBlobStore::new();
        let sq = MemBlobStore::new();
        let gp = synthesize_genesis(&plan, &objs, &sp).unwrap();
        let gq = synthesize_genesis_seq(&plan, &objs, &sq).unwrap();
        assert_eq!(gp.rows, gq.rows, "parallel vs sequential rows byte-identical");
        assert_eq!(gp.vault_id, gq.vault_id);
        assert_eq!(gp.ledger, gq.ledger);
        assert_eq!(gp.mode_table, gq.mode_table);
        // Every blob referenced by a row resolves identically in both stores.
        for r in &gp.rows {
            for h in [r.base_hash.clone(), r.result_hash.clone()].into_iter().flatten() {
                assert_eq!(
                    sp.get_blob(&h).unwrap(),
                    sq.get_blob(&h).unwrap(),
                    "store blob {h} matches across paths"
                );
            }
        }
    }

    #[test]
    fn class_matches_precomputed_binary() {
        // `mode_class_pre` (precomputed is_binary) must equal `mode_class` (raw bytes)
        // for every mode — this equivalence is identity-bearing (MergeClass ∈ merkle id).
        let cases: &[(&str, &[u8])] = &[
            ("main.rs", b"fn main() {}\n"),
            ("notes.txt", b"hello\n"),
            ("img.png", &[0x89, b'P', b'N', b'G', 0, 1, 2]),
            ("weird", &[0xff, 0xfe, 0x00]),
            ("empty.rs", b""),
        ];
        for (path, bytes) in cases {
            let is_bin = std::str::from_utf8(bytes).is_err() || bytes.contains(&0);
            for mode in [EntryMode::Normal, EntryMode::Executable, EntryMode::Symlink] {
                assert_eq!(
                    mode_class(mode, path, bytes),
                    mode_class_pre(mode, path, is_bin),
                    "class mismatch for {path:?} mode {mode:?}"
                );
            }
        }
    }

    #[test]
    fn genesis_is_deterministic_and_dense() {
        let mut objs = HashMap::new();
        let plan = linear_plan(&mut objs);
        let s1 = MemBlobStore::new();
        let s2 = MemBlobStore::new();
        let g1 = synthesize_genesis(&plan, &objs, &s1).unwrap();
        let g2 = synthesize_genesis(&plan, &objs, &s2).unwrap();
        assert_eq!(g1.rows, g2.rows, "byte-identical rows across independent runs");
        assert_eq!(g1.vault_id, g2.vault_id);
        assert_eq!(g1.vault_id, git_vault_id(&plan.root_sha));

        // seq dense 0..n, lamport = seq + 1, every row sealed + repo site.
        let site = git_site_id(&plan.root_sha);
        for (i, r) in g1.rows.iter().enumerate() {
            assert_eq!(r.seq as usize, i, "dense seq");
            assert_eq!(r.lamport, r.seq + 1, "lamport = 1 + index");
            assert!(r.id_valid(), "row {i} sealed");
            assert_eq!(r.site_id, site, "repo site on every row");
        }
        // One ledger record per commit.
        assert_eq!(g1.ledger.len(), 2);
        assert_eq!(g1.ledger[0].commit_sha, plan.commits[0].sha);
    }

    #[test]
    fn genesis_folds_to_the_tip_tree() {
        let mut objs = HashMap::new();
        let plan = linear_plan(&mut objs);
        let store = MemBlobStore::new();
        let g = synthesize_genesis(&plan, &objs, &store).unwrap();

        let e = crate::memengine::MemEngine::create(crate::identity::Identity::from_seed(&[7; 32]), &g.vault_id);
        let wires = to_wires(&g.rows, &store);
        e.integrate_many(&wires).unwrap();
        let files = e.files_map().unwrap();
        assert_eq!(files.get("a.txt").map(|v| v.as_slice()), Some(&b"alpha\nalpha2\n"[..]));
        assert_eq!(files.get("b.txt").map(|v| v.as_slice()), Some(&b"bravo\n"[..]));
        assert!(files.contains_key(".aspignore"), "clone seeds .aspignore");
    }

    #[test]
    fn ingest_raced_edit_converges() {
        // Genesis a file, make a LOCAL edit, then ingest a concurrent upstream edit
        // chained onto the imported tip → the fold 3-way-merges (no panic, both survive).
        let mut objs = HashMap::new();
        let put = |objs: &mut HashMap<String, Vec<u8>>, sha: &str, b: &[u8]| { objs.insert(sha.to_string(), b.to_vec()); };
        put(&mut objs, "base", b"l1\nl2\nl3\n");
        let root = "cccc000000000000000000000000000000000000";
        let plan = ImportPlan {
            commits: vec![PlannedCommit {
                sha: root.into(),
                lane: MAIN_LANE,
                parents: vec![],
                merges: vec![],
                author_name: "up".into(),
                author_email: "up@x".into(),
                committer_ts_ms: 1_700_000_000_000,
                message: "base".into(),
                ops: vec![FileOp::Create { path: "a.txt".into(), blob_sha: "base".into(), mode: EntryMode::Normal }],
                is_depth_cut_snapshot: false,
            }],
            lanes: vec![PlannedLane {
                id: MAIN_LANE, name: "main".into(), fork: None, created_at_commit: root.into(),
                merged_at_commit: None, deleted_after_merge: false, live: false,
            }],
            root_sha: root.into(),
            tip_sha: root.into(),
            warnings: vec![],
            skipped_reachable: vec![],
        };
        let store = MemBlobStore::new();
        let g = synthesize_genesis(&plan, &objs, &store).unwrap();
        let e = crate::memengine::MemEngine::create(crate::identity::Identity::from_seed(&[9; 32]), &g.vault_id);
        e.integrate_many(&to_wires(&g.rows, &store)).unwrap();

        // Local edit (own site), line 1 changed.
        e.record_write("a.txt", b"L1\nl2\nl3\n").unwrap();

        // Reconstruct the imported-chain tip for a.txt from the genesis rows.
        let mut main_state = Vec::new();
        let mut last_row = None;
        for r in &g.rows {
            if r.branch_id == MAIN_BRANCH_ID && matches!(r.kind, Kind::Create | Kind::Edit) && r.path.as_deref() == Some("a.txt") {
                main_state = vec![ImportedFile {
                    path: "a.txt".into(),
                    file_id: r.file_id.clone(),
                    row_id: r.id.clone(),
                    content_hash: r.result_hash.clone(),
                }];
                last_row = Some(r.id.clone());
            }
        }
        let site = git_site_id(root);
        let next_seq = g.rows.iter().filter(|r| r.site_id == site).map(|r| r.seq + 1).max().unwrap();
        let next_lamport = e.row_count() as u64 + 100; // any value > local max

        // Upstream concurrent edit (line 3 changed), chained onto the imported tip.
        put(&mut objs, "up1", b"l1\nl2\nL3\n");
        let delta = ImportPlan {
            commits: vec![PlannedCommit {
                sha: "dddd000000000000000000000000000000000000".into(),
                lane: MAIN_LANE,
                parents: vec![root.into()],
                merges: vec![],
                author_name: "up".into(),
                author_email: "up@x".into(),
                committer_ts_ms: 1_700_000_120_000,
                message: "upstream edit".into(),
                ops: vec![FileOp::Edit {
                    path: "a.txt".into(), old_blob_sha: "base".into(), blob_sha: "up1".into(),
                    mode: EntryMode::Normal, old_mode: EntryMode::Normal,
                }],
                is_depth_cut_snapshot: false,
            }],
            lanes: vec![PlannedLane {
                id: MAIN_LANE, name: "main".into(), fork: None,
                created_at_commit: "dddd000000000000000000000000000000000000".into(),
                merged_at_commit: None, deleted_after_merge: false, live: false,
            }],
            root_sha: root.into(),
            tip_sha: "dddd000000000000000000000000000000000000".into(),
            warnings: vec![],
            skipped_reachable: vec![],
        };
        let ctx = IngestContext {
            site_id: site.clone(),
            next_seq,
            next_lamport,
            remote_ref: DEFAULT_REMOTE_REF.into(),
            main_state,
            main_last_row: last_row,
            seen: HashSet::new(),
        };
        let out = synthesize_ingest(&delta, &ctx, &objs, &store).unwrap();
        e.integrate_many(&to_wires(&out.rows, &store)).unwrap();

        let merged = e.read_file("a.txt").unwrap().unwrap();
        let text = String::from_utf8(merged).unwrap();
        // Both the local and upstream edits are reflected — a converged 3-way merge.
        assert!(text.contains("L1"), "local edit survived: {text:?}");
        assert!(text.contains("L3"), "upstream edit survived: {text:?}");
    }

    fn to_wires(rows: &[LogRow], store: &MemBlobStore) -> Vec<crate::wire::WireRow> {
        rows.iter()
            .map(|r| {
                let mut blobs = Vec::new();
                for h in [r.base_hash.clone(), r.result_hash.clone()].into_iter().flatten() {
                    if let Some(bytes) = store.get_blob(&h).ok().flatten() {
                        if !blobs.iter().any(|b: &crate::wire::WireBlob| b.hash == h) {
                            blobs.push(crate::wire::WireBlob { hash: h, bytes });
                        }
                    }
                }
                crate::wire::WireRow { row: r.clone(), blobs }
            })
            .collect()
    }

    #[test]
    fn side_lane_forks_and_merges_and_deletes() {
        // main C0 (create x), fork side at C0 (create y), merge side into main at M
        // (delete-after-merge). Assert: side branch record + merge marker + delete.
        let mut objs: HashMap<String, Vec<u8>> = HashMap::new();
        objs.insert("bx".into(), b"x\n".to_vec());
        objs.insert("by".into(), b"y\n".to_vec());
        let c0 = "aa00000000000000000000000000000000000000";
        let sidec = "cc00000000000000000000000000000000000000";
        let mrg = "mm00000000000000000000000000000000000000";
        let plan = ImportPlan {
            commits: vec![
                PlannedCommit {
                    sha: c0.into(), lane: 0, parents: vec![], merges: vec![], author_name: "a".into(),
                    author_email: "a@x".into(), committer_ts_ms: 1_000_000, message: "c0".into(),
                    ops: vec![FileOp::Create { path: "x.txt".into(), blob_sha: "bx".into(), mode: EntryMode::Normal }],
                    is_depth_cut_snapshot: false,
                },
                PlannedCommit {
                    sha: sidec.into(), lane: 1, parents: vec![c0.into()], merges: vec![], author_name: "a".into(),
                    author_email: "a@x".into(), committer_ts_ms: 1_060_000, message: "side".into(),
                    ops: vec![FileOp::Create { path: "y.txt".into(), blob_sha: "by".into(), mode: EntryMode::Normal }],
                    is_depth_cut_snapshot: false,
                },
                PlannedCommit {
                    sha: mrg.into(), lane: 0, parents: vec![c0.into(), sidec.into()],
                    merges: vec![MergeInfo { source_lane: 1, source_tip_sha: sidec.into() }],
                    author_name: "a".into(), author_email: "a@x".into(), committer_ts_ms: 1_120_000,
                    message: "Merge branch 'feature'".into(),
                    ops: vec![FileOp::Create { path: "y.txt".into(), blob_sha: "by".into(), mode: EntryMode::Normal }],
                    is_depth_cut_snapshot: false,
                },
            ],
            lanes: vec![
                PlannedLane { id: 0, name: "main".into(), fork: None, created_at_commit: c0.into(), merged_at_commit: None, deleted_after_merge: false, live: false },
                PlannedLane { id: 1, name: "feature".into(), fork: Some(ForkPoint { lane: 0, commit_sha: c0.into(), commit_index: 0 }), created_at_commit: sidec.into(), merged_at_commit: Some(mrg.into()), deleted_after_merge: true, live: false },
            ],
            root_sha: c0.into(),
            tip_sha: mrg.into(),
            warnings: vec![],
            skipped_reachable: vec![],
        };
        let store = MemBlobStore::new();
        let g = synthesize_genesis(&plan, &objs, &store).unwrap();
        let branch_rows: Vec<&LogRow> = g.rows.iter().filter(|r| r.kind == Kind::Branch).collect();
        assert_eq!(branch_rows.len(), 2, "one create + one delete for the side lane");
        assert!(g.rows.iter().any(|r| r.kind == Kind::Merge), "merge marker present");

        // Load + fold: main has x and y; the side branch (pre-delete history) has x + y.
        let e = crate::memengine::MemEngine::create(crate::identity::Identity::from_seed(&[3; 32]), &g.vault_id);
        e.integrate_many(&to_wires(&g.rows, &store)).unwrap();
        let main = e.files_map().unwrap();
        assert!(main.contains_key("x.txt") && main.contains_key("y.txt"));
    }
}
