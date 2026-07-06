//! Integration tests for **phase 2 of genesis** — importing open (unmerged)
//! branches at clone (`specs/git-open-branches.md` §1–§2, §7): pack bytes (HEAD +
//! all open-branch tips) → [`GitObjectDb`] → [`plan_import`] with `open_branch_tips`
//! → [`synthesize_genesis`] → sealed rows folded in a fresh [`MemEngine`].
//!
//! Load-bearing checks:
//! * **Ground truth per branch** — after a checkbox clone, `fold(branch)` equals
//!   `git ls-tree -r <branch tip>` for every imported live branch (and `main`), and
//!   the model-level per-commit replay matches `git ls-tree -r` for EVERY phase-2
//!   commit (the §3.1 fidelity invariant extended to phase 2).
//! * **Zero regression / shared prefix** — a plain clone's phase-1 commit rows are a
//!   byte-identical prefix of a checkbox clone's; a default plan has no live lanes
//!   and an empty `skipped_reachable`.
//! * **Structure** — fork-off-a-side-lane (`nested/deep`), internal-merge tombstone
//!   (`with-merge`), orphan-vs-empty (`orphan`), `-N` name dedup (`feature-1`),
//!   skip-reachable (`stale-pointer`).
//! * **Determinism** — two differently-laid-out packs → byte-identical plans AND
//!   rows, phase 2 included.
//! * **Depth interaction** — a depth-cut phase 1 + open branches still holds
//!   fidelity for every branch (the snapshot-boundary rule).
//! * **LCG fuzz** — random repos with random open branches never panic, hold the
//!   per-branch invariant, and emit deterministically across two builds.
//!
//! System git is a sanctioned dev-only dependency (spec §10); tests skip when absent.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use asp_core::gitgenesis::{synthesize_genesis, DbBlobSource, GenesisOutput};
use asp_core::gitimport::{
    no_base_lookup, plan_import, FileOp, GitObjectDb, ImportOptions, ImportPlan, LaneId, MAIN_LANE,
};
use asp_core::identity::Identity;
use asp_core::log::{Kind, LogRow, MAIN_BRANCH_ID};
use asp_core::memengine::MemEngine;
use asp_core::store::{BlobStore, MemBlobStore};
use asp_core::wire::{WireBlob, WireRow};
use asp_e2e::gitfix::{open_branches, FixtureRepo};

// ---------------------------------------------------------------------------
// git plumbing (mirrors git_import_model.rs / git_genesis.rs)
// ---------------------------------------------------------------------------

fn git_available() -> bool {
    Command::new("git").arg("version").stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

fn git_in(repo: &Path, args: &[&str]) -> Vec<u8> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output().expect("spawn git");
    assert!(out.status.success(), "git -C {} {:?} failed: {}", repo.display(), args, String::from_utf8_lossy(&out.stderr));
    out.stdout
}

fn git_str(repo: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&git_in(repo, args)).trim().to_string()
}

fn pack_with_stdin(bare: &Path, args: &[&str], stdin: &str) -> Vec<u8> {
    let mut child = Command::new("git").arg("-C").arg(bare).args(args)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().expect("spawn git pack-objects");
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().expect("pack-objects output");
    assert!(out.status.success() && !out.stdout.is_empty(), "pack-objects failed");
    out.stdout
}

/// Pack every object reachable from `tips` (HEAD + all open-branch tips) via
/// `pack-objects --revs`.
fn pack_revs(bare: &Path, tips: &[String]) -> Vec<u8> {
    let stdin = tips.iter().map(|t| format!("{t}\n")).collect::<String>();
    pack_with_stdin(bare, &["pack-objects", "--revs", "--stdout", "-q"], &stdin)
}

/// Same object set, different invocation → a differently-ordered pack (determinism).
fn pack_object_list(bare: &Path, tips: &[String]) -> Vec<u8> {
    let mut args = vec!["rev-list", "--objects"];
    for t in tips {
        args.push(t.as_str());
    }
    let list = String::from_utf8_lossy(&git_in(bare, &args)).to_string();
    pack_with_stdin(bare, &["pack-objects", "--stdout", "-q"], &list)
}

/// `git ls-tree -r <sha>` → path → blob sha (gitlinks excluded).
fn ls_tree_blobs(bare: &Path, sha: &str) -> BTreeMap<String, String> {
    let out = String::from_utf8_lossy(&git_in(bare, &["ls-tree", "-r", sha])).to_string();
    let mut map = BTreeMap::new();
    for line in out.lines() {
        let (meta, path) = line.split_once('\t').expect("ls-tree line");
        let mut parts = meta.split_whitespace();
        let _mode = parts.next().unwrap();
        let typ = parts.next().unwrap();
        let oid = parts.next().unwrap();
        if typ == "commit" {
            continue;
        }
        map.insert(path.to_string(), oid.to_string());
    }
    map
}

/// Expected `path -> content bytes` for a commit's tree.
fn tree_content(db: &GitObjectDb, bare: &Path, sha: &str) -> BTreeMap<String, Vec<u8>> {
    ls_tree_blobs(bare, sha)
        .into_iter()
        .map(|(path, oid)| (path, db.get(&oid).map(|(_, b)| b.to_vec()).unwrap_or_default()))
        .collect()
}

/// `path -> (blob sha, git octal mode)` for a commit's tree (model-fidelity ground).
fn ls_tree_modes(bare: &Path, sha: &str) -> BTreeMap<String, (String, u32)> {
    let out = String::from_utf8_lossy(&git_in(bare, &["ls-tree", "-r", sha])).to_string();
    let mut map = BTreeMap::new();
    for line in out.lines() {
        let (meta, path) = line.split_once('\t').expect("ls-tree line");
        let mut parts = meta.split_whitespace();
        let mode = u32::from_str_radix(parts.next().unwrap(), 8).unwrap();
        let typ = parts.next().unwrap();
        let oid = parts.next().unwrap();
        if typ == "commit" {
            continue;
        }
        map.insert(path.to_string(), (oid.to_string(), mode));
    }
    map
}

// ---------------------------------------------------------------------------
// helpers: open-branch tips, plan/synthesize, load, folds
// ---------------------------------------------------------------------------

/// `(ref_name, tip_sha)` for every `refs/heads/*` except `main` — the open-branch
/// tips a `--all-branches` clone would import.
fn open_tips(bare: &Path) -> Vec<(String, String)> {
    let out = git_str(bare, &["for-each-ref", "--format=%(refname:short) %(objectname)", "refs/heads"]);
    out.lines()
        .filter_map(|l| {
            let (n, s) = l.split_once(' ')?;
            if n == "main" {
                None
            } else {
                Some((n.to_string(), s.to_string()))
            }
        })
        .collect()
}

/// All ref tips (main + heads) — the wants of a single fetch (§6 single pack).
fn all_tips(bare: &Path) -> Vec<String> {
    let mut v: Vec<String> = vec![git_str(bare, &["rev-parse", "HEAD"])];
    for (_n, s) in open_tips(bare) {
        v.push(s);
    }
    v
}

fn to_wires(rows: &[LogRow], store: &MemBlobStore) -> Vec<WireRow> {
    rows.iter()
        .map(|r| {
            let mut blobs = Vec::new();
            for h in [r.base_hash.clone(), r.result_hash.clone()].into_iter().flatten() {
                if let Some(bytes) = store.get_blob(&h).ok().flatten() {
                    if !blobs.iter().any(|b: &WireBlob| b.hash == h) {
                        blobs.push(WireBlob { hash: h, bytes });
                    }
                }
            }
            WireRow { row: r.clone(), blobs }
        })
        .collect()
}

/// Synthesize `plan` and load into a fresh engine adopting the derived vault id.
fn load(db: &GitObjectDb, plan: &ImportPlan) -> (GenesisOutput, MemBlobStore, MemEngine) {
    let store = MemBlobStore::new();
    let g = synthesize_genesis(plan, &DbBlobSource::new(db), &store).unwrap();
    let e = MemEngine::create(Identity::from_seed(&[42; 32]), &g.vault_id);
    e.set_batch(true);
    for page in to_wires(&g.rows, &store).chunks(256) {
        e.integrate_many(page).unwrap();
    }
    e.set_batch(false);
    e.materialize().unwrap();
    (g, store, e)
}

fn fold_content(e: &MemEngine, branch_id: &str) -> BTreeMap<String, Vec<u8>> {
    e.checkout(branch_id).unwrap();
    let mut m = e.files_map().unwrap();
    m.remove(".aspignore");
    m
}

/// Branch id derived for a lane name (from its `Kind::Branch` record's file_id).
fn branch_id_for(rows: &[LogRow], name: &str) -> Option<String> {
    rows.iter().find(|r| r.kind == Kind::Branch && r.path.as_deref() == Some(name)).map(|r| r.file_id.clone())
}

/// A live open-branch lane's tip commit = its last commit in canonical order.
fn lane_tip(plan: &ImportPlan, lane: LaneId) -> String {
    plan.commits.iter().rev().find(|c| c.lane == lane).unwrap().sha.clone()
}

// ---------------------------------------------------------------------------
// model-level fidelity (per-commit, phase 1 AND phase 2) — from git_import_model
// ---------------------------------------------------------------------------

type State = BTreeMap<String, (String, u32)>;

fn apply_ops(state: &mut State, ops: &[FileOp]) {
    for op in ops {
        match op {
            FileOp::Create { path, blob_sha, mode } | FileOp::Edit { path, blob_sha, mode, .. } => {
                state.insert(path.clone(), (blob_sha.clone(), mode.git_mode()));
            }
            FileOp::Delete { path } => {
                state.remove(path);
            }
            FileOp::RenameExact { from, to, blob_sha, mode } => {
                state.remove(from);
                state.insert(to.clone(), (blob_sha.clone(), mode.git_mode()));
            }
            FileOp::DirCreate { .. } => {}
        }
    }
}

/// Replay every planned commit (both phases) on top of its first parent's replayed
/// state and require equality with `git ls-tree -r` at every step. A phase-2 commit
/// whose first parent isn't in the plan (orphan / depth boundary) bases on empty —
/// exactly what its snapshot ops encode.
fn assert_model_fidelity(plan: &ImportPlan, bare: &Path, label: &str) {
    let mut after: BTreeMap<String, State> = BTreeMap::new();
    for c in &plan.commits {
        let base: State = if c.is_depth_cut_snapshot {
            State::new()
        } else {
            match c.parents.first() {
                Some(p0) => after.get(p0).cloned().unwrap_or_default(),
                None => State::new(),
            }
        };
        let mut state = base;
        apply_ops(&mut state, &c.ops);
        assert_eq!(
            state,
            ls_tree_modes(bare, &c.sha),
            "[{label}] replayed state != git ls-tree -r {} (lane {})",
            c.sha,
            c.lane
        );
        after.insert(c.sha.clone(), state);
    }
}

// ---------------------------------------------------------------------------
// 1. Ground truth per branch — the load-bearing test
// ---------------------------------------------------------------------------

#[test]
fn checkbox_clone_ground_truth_per_branch() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    let repo = open_branches();
    let head = git_str(&repo.bare, &["rev-parse", "HEAD"]);
    let tips = all_tips(&repo.bare);
    let open = open_tips(&repo.bare);

    let db = GitObjectDb::from_pack(&pack_revs(&repo.bare, &tips), no_base_lookup).unwrap();
    let opts = ImportOptions { open_branch_tips: open.clone(), ..Default::default() };
    let plan = plan_import(&db, &head, &opts).unwrap();

    // stale-pointer (an ancestor of main) is skipped; nothing else is.
    assert_eq!(plan.skipped_reachable, vec!["stale-pointer".to_string()], "only stale-pointer skipped");

    // Live open-branch lanes, in ref-name-bytewise creation order. `feature-1`
    // collides with the merged PR#1 branch name → deduped to `feature-1-2`.
    let live: Vec<&str> = plan.lanes.iter().filter(|l| l.live).map(|l| l.name.as_str()).collect();
    assert_eq!(
        live,
        vec!["feat/simple", "feature-1-2", "nested/deep", "orphan", "with-merge"],
        "live open-branch names + dedup + order"
    );

    // The model-level invariant for every phase-1 AND phase-2 commit.
    assert_model_fidelity(&plan, &repo.bare, "open_branches");

    // Row-level: fold each live branch == git ls-tree of its tip; main too.
    let (g, _s, e) = load(&db, &plan);
    assert_eq!(fold_content(&e, MAIN_BRANCH_ID), tree_content(&db, &repo.bare, &head), "main fold");

    // git ref name -> asp branch name (only feature-1 differs, via dedup).
    let ref_to_asp = |r: &str| -> String {
        if r == "feature-1" { "feature-1-2".to_string() } else { r.to_string() }
    };
    for (ref_name, tip) in &open {
        if ref_name == "stale-pointer" {
            assert!(branch_id_for(&g.rows, ref_name).is_none(), "skipped ref has no branch record");
            continue;
        }
        let asp = ref_to_asp(ref_name);
        let bid = branch_id_for(&g.rows, &asp).unwrap_or_else(|| panic!("branch record for {asp}"));
        assert_eq!(
            fold_content(&e, &bid),
            tree_content(&db, &repo.bare, tip),
            "fold({asp}) != git ls-tree -r {tip} ({ref_name})"
        );
    }

    // No delete tombstone for a live branch; its create record exists.
    for asp in ["feat/simple", "feature-1-2", "nested/deep", "orphan", "with-merge"] {
        let recs: Vec<_> = g.rows.iter().filter(|r| r.kind == Kind::Branch && r.path.as_deref() == Some(asp)).collect();
        assert_eq!(recs.len(), 1, "{asp}: exactly one (create) branch record, no delete");
    }
}

// ---------------------------------------------------------------------------
// 2. Zero regression: plain phase-1 rows are a byte-identical prefix
// ---------------------------------------------------------------------------

#[test]
fn plain_clone_is_a_byte_identical_prefix() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    let repo = open_branches();
    let head = git_str(&repo.bare, &["rev-parse", "HEAD"]);
    let tips = all_tips(&repo.bare);
    let db = GitObjectDb::from_pack(&pack_revs(&repo.bare, &tips), no_base_lookup).unwrap();

    // Plain clone: default opts → no live lanes, nothing skipped.
    let plain_plan = plan_import(&db, &head, &ImportOptions::default()).unwrap();
    assert!(plain_plan.lanes.iter().all(|l| !l.live), "default plan has no live lanes");
    assert!(plain_plan.skipped_reachable.is_empty(), "default plan skips nothing");
    let s1 = MemBlobStore::new();
    let plain = synthesize_genesis(&plain_plan, &DbBlobSource::new(&db), &s1).unwrap();

    // Checkbox clone.
    let opts = ImportOptions { open_branch_tips: open_tips(&repo.bare), ..Default::default() };
    let cb_plan = plan_import(&db, &head, &opts).unwrap();
    let s2 = MemBlobStore::new();
    let cb = synthesize_genesis(&cb_plan, &DbBlobSource::new(&db), &s2).unwrap();

    // Phase-1 commit rows (everything except the trailing `.aspignore`, which both
    // clones author last) are a byte-identical prefix — the checkbox only appends.
    assert!(cb.rows.len() > plain.rows.len(), "checkbox appends phase-2 rows");
    let n = plain.rows.len() - 1; // drop the last (.aspignore) row of the plain clone
    assert_eq!(plain.rows[..n], cb.rows[..n], "phase-1 rows must be a byte-identical prefix");
    // The phase-1 commit shas are identical between plans (positions unchanged).
    let plain_p1: Vec<&str> = plain_plan.commits.iter().map(|c| c.sha.as_str()).collect();
    let cb_p1: Vec<&str> = cb_plan.commits[..plain_plan.commits.len()].iter().map(|c| c.sha.as_str()).collect();
    assert_eq!(plain_p1, cb_p1, "phase-1 commit order unchanged by the checkbox");
    // Same derived vault id (identity keys off the root only).
    assert_eq!(plain.vault_id, cb.vault_id);
}

// ---------------------------------------------------------------------------
// 3. Fork off a side lane (nested/deep) + 4. internal-merge tombstone (with-merge)
// ---------------------------------------------------------------------------

#[test]
fn fork_off_side_lane_and_internal_merge() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    let repo = open_branches();
    let head = git_str(&repo.bare, &["rev-parse", "HEAD"]);
    let db = GitObjectDb::from_pack(&pack_revs(&repo.bare, &all_tips(&repo.bare)), no_base_lookup).unwrap();
    let opts = ImportOptions { open_branch_tips: open_tips(&repo.bare), ..Default::default() };
    let plan = plan_import(&db, &head, &opts).unwrap();

    let lane_named = |name: &str| plan.lanes.iter().find(|l| l.name == name).unwrap();

    // --- nested/deep forks off feature-1's PR lane (a NON-main side lane) ---
    let nd = lane_named("nested/deep");
    assert!(nd.live);
    let fork = nd.fork.as_ref().expect("nested/deep must fork somewhere");
    assert_ne!(fork.lane, MAIN_LANE, "nested/deep forks off a side lane, not main");
    // The lane it forks off is the merged PR#1 lane "feature-1".
    assert_eq!(plan.lanes[fork.lane].name, "feature-1", "forks off the PR#1 side lane");
    assert!(!plan.lanes[fork.lane].live, "the parent side lane is a merged (non-live) lane");
    // The fork index points at the fork commit's canonical position.
    assert_eq!(plan.commits[fork.commit_index].sha, fork.commit_sha);

    // --- with-merge: the branch's own lane is live; its internal side lane tombstones ---
    let wm = lane_named("with-merge");
    assert!(wm.live && !wm.deleted_after_merge && wm.merged_at_commit.is_none());
    // The internal merge commit lives on the with-merge lane and carries one edge.
    let internal_merge = plan
        .commits
        .iter()
        .find(|c| c.lane == wm.id && !c.merges.is_empty())
        .expect("with-merge has an internal merge");
    assert_eq!(internal_merge.merges.len(), 1);
    let sub = internal_merge.merges[0].source_lane;
    assert_ne!(sub, wm.id, "the internal side lane is distinct");
    assert!(plan.lanes[sub].deleted_after_merge, "internal side lane IS tombstoned");
    assert!(!plan.lanes[sub].live, "internal side lane is not a live open branch");
    assert_eq!(plan.lanes[sub].name, "wm-side");

    // Row-level: with-merge folds to its tip, and the internal side lane gets a
    // create AND a delete record.
    let (g, _s, e) = load(&db, &plan);
    let wm_bid = branch_id_for(&g.rows, "with-merge").unwrap();
    assert_eq!(
        fold_content(&e, &wm_bid),
        tree_content(&db, &repo.bare, &lane_tip(&plan, wm.id)),
        "with-merge fold != its tip tree"
    );
    let sub_recs = g.rows.iter().filter(|r| r.kind == Kind::Branch && r.path.as_deref() == Some("wm-side")).count();
    assert_eq!(sub_recs, 2, "internal side lane: one create + one delete");
}

// ---------------------------------------------------------------------------
// 5. Orphan branch: fork=None, root diffs vs empty, fidelity holds
// ---------------------------------------------------------------------------

#[test]
fn orphan_branch_forks_nowhere() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    let repo = open_branches();
    let head = git_str(&repo.bare, &["rev-parse", "HEAD"]);
    let db = GitObjectDb::from_pack(&pack_revs(&repo.bare, &all_tips(&repo.bare)), no_base_lookup).unwrap();
    let opts = ImportOptions { open_branch_tips: open_tips(&repo.bare), ..Default::default() };
    let plan = plan_import(&db, &head, &opts).unwrap();

    let orphan = plan.lanes.iter().find(|l| l.name == "orphan").unwrap();
    assert!(orphan.live);
    assert!(orphan.fork.is_none(), "orphan forks nowhere (unrelated root)");
    // Its oldest commit is a real root in the plan (no in-plan first parent) and its
    // ops are a full-tree snapshot (all Creates).
    let root = plan.commits.iter().find(|c| c.sha == orphan.created_at_commit).unwrap();
    assert!(root.ops.iter().all(|o| matches!(o, FileOp::Create { .. } | FileOp::DirCreate { .. })), "orphan root = full-tree creates");

    let (g, _s, e) = load(&db, &plan);
    let bid = branch_id_for(&g.rows, "orphan").unwrap();
    assert_eq!(
        fold_content(&e, &bid),
        tree_content(&db, &repo.bare, &git_str(&repo.bare, &["rev-parse", "orphan"])),
        "orphan fold != its tip tree"
    );
}

// ---------------------------------------------------------------------------
// 6. Depth interaction: depth-cut phase 1 + open branches → fidelity holds
// ---------------------------------------------------------------------------

#[test]
fn depth_cut_phase1_plus_open_branches_holds_fidelity() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    let repo = open_branches();
    let head = git_str(&repo.bare, &["rev-parse", "HEAD"]);
    let db = GitObjectDb::from_pack(&pack_revs(&repo.bare, &all_tips(&repo.bare)), no_base_lookup).unwrap();
    // Depth 1 cuts phase 1 hard — nested/deep forks off F1a, now OUTSIDE the window,
    // so its lane must snapshot vs empty (fork=None) rather than off a planned commit.
    let opts = ImportOptions {
        depth: Some(1),
        open_branch_tips: open_tips(&repo.bare),
        ..Default::default()
    };
    let plan = plan_import(&db, &head, &opts).unwrap();

    // Model-level fidelity holds for every commit (phase 1 snapshot + each branch).
    assert_model_fidelity(&plan, &repo.bare, "open_branches(depth=1)");

    // Row-level fidelity for each live branch tip.
    let (g, _s, e) = load(&db, &plan);
    for lane in plan.lanes.iter().filter(|l| l.live) {
        let bid = branch_id_for(&g.rows, &lane.name).unwrap();
        assert_eq!(
            fold_content(&e, &bid),
            tree_content(&db, &repo.bare, &lane_tip(&plan, lane.id)),
            "[depth=1] fold({}) != its tip tree",
            lane.name
        );
    }
    // nested/deep forked off a now-cut commit → it snapshots (fork=None).
    let nd = plan.lanes.iter().find(|l| l.name == "nested/deep").unwrap();
    assert!(nd.fork.is_none(), "under depth=1, nested/deep's fork base is cut → snapshot");
}

// ---------------------------------------------------------------------------
// 7. Determinism across pack layouts (phase 2 included)
// ---------------------------------------------------------------------------

#[test]
fn plan_and_rows_deterministic_across_pack_layouts() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    let repo = open_branches();
    let head = git_str(&repo.bare, &["rev-parse", "HEAD"]);
    let tips = all_tips(&repo.bare);
    let open = open_tips(&repo.bare);

    let db_a = GitObjectDb::from_pack(&pack_revs(&repo.bare, &tips), no_base_lookup).unwrap();
    let db_b = GitObjectDb::from_pack(&pack_object_list(&repo.bare, &tips), no_base_lookup).unwrap();
    assert_eq!(db_a.len(), db_b.len(), "same object set");

    let opts = ImportOptions { open_branch_tips: open, ..Default::default() };
    let p_a = plan_import(&db_a, &head, &opts).unwrap();
    let p_a2 = plan_import(&db_a, &head, &opts).unwrap();
    let p_b = plan_import(&db_b, &head, &opts).unwrap();
    assert_eq!(format!("{p_a:?}"), format!("{p_a2:?}"), "rebuild determinism");
    assert_eq!(format!("{p_a:?}"), format!("{p_b:?}"), "pack-layout independence (plan)");

    let sa = MemBlobStore::new();
    let sb = MemBlobStore::new();
    let ga = synthesize_genesis(&p_a, &DbBlobSource::new(&db_a), &sa).unwrap();
    let gb = synthesize_genesis(&p_b, &DbBlobSource::new(&db_b), &sb).unwrap();
    assert_eq!(ga.rows, gb.rows, "byte-identical rows across pack layouts (phase 2 included)");
    assert_eq!(ga.vault_id, gb.vault_id);
}

// ---------------------------------------------------------------------------
// 8. LCG fuzz: random open-branch topologies (repo idiom, no proptest)
// ---------------------------------------------------------------------------

#[test]
fn fuzz_random_open_branches_hold_invariants() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    let mut state: u64 = 0x9E3779B97F4A7C15;
    let mut next = move || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 33) as u32
    };

    for trial in 0..3u32 {
        let mut r = FixtureRepo::init(&format!("obfuzz{trial}"));
        r.commit_file("base.txt", "base\n", "base");
        // Branch-private files → merges never conflict (matches git_import_model fuzz).
        let mut branches: Vec<String> = vec!["main".into()];
        let mut versions: BTreeMap<String, u32> = BTreeMap::new();

        for step in 0..16 {
            match next() % 5 {
                // New branch off a random existing branch.
                0 => {
                    let at = branches[(next() as usize) % branches.len()].clone();
                    let name = format!("b-{trial}-{step}");
                    r.checkout(&at);
                    r.checkout_new(&name, None);
                    branches.push(name);
                }
                // Merge a random branch into another (--no-ff) → internal/side merges.
                // Skip orphans: git refuses to merge unrelated histories without a flag.
                1 if branches.len() >= 2 => {
                    let a = branches[(next() as usize) % branches.len()].clone();
                    let b = branches[(next() as usize) % branches.len()].clone();
                    if a != b && !a.starts_with("orph-") && !b.starts_with("orph-") {
                        r.checkout(&b);
                        r.merge(&a, &format!("Merge branch '{a}'"), true);
                    }
                }
                // Occasionally graft an orphan root.
                2 if step == 7 => {
                    let name = format!("orph-{trial}");
                    r.orphan(&name);
                    r.commit_file(&format!("{name}.txt"), "o\n", "orphan root");
                    branches.push(name);
                }
                // Commit on a random branch (branch-private file).
                _ => {
                    let br = branches[(next() as usize) % branches.len()].clone();
                    let v = versions.entry(br.clone()).or_insert(0);
                    *v += 1;
                    let v = *v;
                    r.checkout(&br);
                    r.commit_file(&format!("{br}.txt"), &format!("v{v}\n"), &format!("{br} v{v}"));
                }
            }
        }
        // Merge a couple of branches into main so some refs become reachable (→ skipped).
        r.checkout("main");
        for b in branches.clone().into_iter().filter(|b| b != "main" && !b.starts_with("orph-")).take(2) {
            r.merge(&b, &format!("Merge branch '{b}'"), true);
        }
        let repo = r.finish();

        let head = git_str(&repo.bare, &["rev-parse", "HEAD"]);
        let tips = all_tips(&repo.bare);
        let open = open_tips(&repo.bare);
        let db_a = GitObjectDb::from_pack(&pack_revs(&repo.bare, &tips), no_base_lookup).unwrap();
        let db_b = GitObjectDb::from_pack(&pack_object_list(&repo.bare, &tips), no_base_lookup).unwrap();
        let opts = ImportOptions { open_branch_tips: open, ..Default::default() };

        let plan = plan_import(&db_a, &head, &opts).unwrap_or_else(|e| panic!("[obfuzz{trial}] plan: {e}"));
        // Never panics; every commit (both phases) matches git ls-tree.
        assert_model_fidelity(&plan, &repo.bare, &format!("obfuzz{trial}"));
        // Row synthesis + fold: each live branch folds to its tip tree.
        let (g, _s, e) = load(&db_a, &plan);
        for lane in plan.lanes.iter().filter(|l| l.live) {
            let bid = branch_id_for(&g.rows, &lane.name).unwrap();
            assert_eq!(
                fold_content(&e, &bid),
                tree_content(&db_a, &repo.bare, &lane_tip(&plan, lane.id)),
                "[obfuzz{trial}] fold({}) != tip tree",
                lane.name
            );
        }
        // Deterministic across two independent pack builds.
        let plan_b = plan_import(&db_b, &head, &opts).unwrap();
        assert_eq!(format!("{plan:?}"), format!("{plan_b:?}"), "[obfuzz{trial}] plan determinism");
        let sb = MemBlobStore::new();
        let gb = synthesize_genesis(&plan_b, &DbBlobSource::new(&db_b), &sb).unwrap();
        assert_eq!(g.rows, gb.rows, "[obfuzz{trial}] row determinism across builds");
    }
}
