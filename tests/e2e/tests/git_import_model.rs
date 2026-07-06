//! Integration tests for the git-import **model** (`asp_core::gitimport`,
//! git-bridge §3): pack bytes from real fixture repos → [`GitObjectDb`] →
//! [`plan_import`] → the §3.1 **fidelity-invariant precursor**.
//!
//! The load-bearing check: replaying each planned commit's `ops` (a first-parent
//! tree diff) on top of its first parent's replayed state must reproduce
//! `git ls-tree -r <sha>` **exactly** (path → blob sha + mode) for **every commit
//! of every fixture** — criss-cross, octopus, foxtrot, and mid-history roots
//! included. This is the model-level half of the §3.1 fidelity invariant; the
//! row-synthesis layer inherits it because its rows state these `result_hash`es
//! verbatim.
//!
//! Also covered here: lane topology per fixture (merged_prs → two side lanes named
//! from the PR subjects, delete-after-merge), plan determinism across two
//! differently-produced packs, depth-cut correctness against a real repo, and a
//! deterministic LCG fuzz that grows random (conflict-free) git histories and
//! asserts the invariant end-to-end.
//!
//! System git is a sanctioned dev-only dependency (spec §10); tests skip politely
//! when it is absent.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use asp_core::gitimport::{
    no_base_lookup, plan_import, EntryMode, FileOp, GitObjectDb, ImportOptions, ImportPlan,
    ImportWarning, MAIN_LANE,
};
use asp_e2e::gitfix::{all_fixtures, linear_basic, merged_prs, mid_history_root, FixtureRepo};

// ---------------------------------------------------------------------------
// git plumbing helpers
// ---------------------------------------------------------------------------

fn git_available() -> bool {
    Command::new("git")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run `git -C <repo> <args>`, asserting success; returns stdout bytes.
fn git_in(repo: &Path, args: &[&str]) -> Vec<u8> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git -C {} {:?} failed: {}",
        repo.display(),
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn git_str(repo: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&git_in(repo, args)).trim().to_string()
}

/// Pack every object reachable from `tip` via `pack-objects --revs --stdout`.
fn pack_via_revs(bare: &Path, tip: &str) -> Vec<u8> {
    pack_with_stdin(bare, &["pack-objects", "--revs", "--stdout", "-q"], &format!("{tip}\n"))
}

/// Same object set, different invocation: explicit `rev-list --objects` listing
/// piped to `pack-objects --stdout` (no `--revs`) — a differently-ordered pack.
fn pack_via_object_list(bare: &Path, tip: &str) -> Vec<u8> {
    let list = String::from_utf8_lossy(&git_in(bare, &["rev-list", "--objects", tip])).to_string();
    pack_with_stdin(bare, &["pack-objects", "--stdout", "-q"], &list)
}

fn pack_with_stdin(bare: &Path, args: &[&str], stdin: &str) -> Vec<u8> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(bare)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn git pack-objects");
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().expect("pack-objects output");
    assert!(out.status.success(), "pack-objects failed");
    assert!(!out.stdout.is_empty(), "empty pack");
    out.stdout
}

/// `git ls-tree -r <sha>` → path → (blob sha, git octal mode). Gitlink (`160000`)
/// entries are excluded — the importer emits no FileOp for them (spec §3.3).
fn ls_tree(bare: &Path, sha: &str) -> BTreeMap<String, (String, u32)> {
    let out = String::from_utf8_lossy(&git_in(bare, &["ls-tree", "-r", sha])).to_string();
    let mut map = BTreeMap::new();
    for line in out.lines() {
        // "<mode> <type> <sha>\t<path>"
        let (meta, path) = line.split_once('\t').expect("ls-tree line");
        let mut parts = meta.split_whitespace();
        let mode = u32::from_str_radix(parts.next().unwrap(), 8).unwrap();
        let typ = parts.next().unwrap();
        let oid = parts.next().unwrap();
        if typ == "commit" {
            continue; // gitlink
        }
        map.insert(path.to_string(), (oid.to_string(), mode));
    }
    map
}

// ---------------------------------------------------------------------------
// The fidelity-invariant precursor (§3.1 at the model level)
// ---------------------------------------------------------------------------

type State = BTreeMap<String, (String, u32)>;

fn apply_ops(state: &mut State, ops: &[FileOp]) {
    for op in ops {
        match op {
            FileOp::Create { path, blob_sha, mode } => {
                state.insert(path.clone(), (blob_sha.clone(), mode.git_mode()));
            }
            FileOp::Edit { path, blob_sha, mode, .. } => {
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

/// Replay every planned commit on top of its first parent's replayed state (the
/// lane map: a lane IS a first-parent chain, and ops after a merge are the diff
/// vs first parent) and require equality with `git ls-tree -r` at every step.
fn assert_fidelity(plan: &ImportPlan, bare: &Path, label: &str) {
    let mut after: BTreeMap<&str, State> = BTreeMap::new();
    for c in &plan.commits {
        let base: State = if c.is_depth_cut_snapshot {
            State::new()
        } else {
            match c.parents.first() {
                Some(p0) => after.get(p0.as_str()).cloned().unwrap_or_default(),
                None => State::new(),
            }
        };
        let mut state = base;
        apply_ops(&mut state, &c.ops);
        let expect = ls_tree(bare, &c.sha);
        assert_eq!(
            state, expect,
            "[{label}] replayed state diverges from git ls-tree -r {} (lane {})",
            c.sha, c.lane
        );
        after.insert(c.sha.as_str(), state);
    }
}

/// Structural invariants every plan must satisfy regardless of fixture.
fn assert_plan_wellformed(plan: &ImportPlan, label: &str) {
    let idx: BTreeMap<&str, usize> =
        plan.commits.iter().enumerate().map(|(i, c)| (c.sha.as_str(), i)).collect();
    for (i, c) in plan.commits.iter().enumerate() {
        assert!(c.lane < plan.lanes.len(), "[{label}] lane out of range");
        for p in &c.parents {
            if let Some(&pi) = idx.get(p.as_str()) {
                assert!(pi < i, "[{label}] parent {p} after child {}", c.sha);
            }
        }
        for m in &c.merges {
            assert!(m.source_lane < plan.lanes.len(), "[{label}] merge lane out of range");
            assert!(
                idx.contains_key(m.source_tip_sha.as_str()),
                "[{label}] merge tip {} not in plan",
                m.source_tip_sha
            );
        }
    }
    assert_eq!(plan.lanes[MAIN_LANE].name, "main");
    assert!(plan.lanes[MAIN_LANE].fork.is_none());
    // Lane fork indexes point at the fork commit's canonical position.
    for lane in &plan.lanes {
        if let Some(f) = &lane.fork {
            assert_eq!(plan.commits[f.commit_index].sha, f.commit_sha, "[{label}] fork index");
        }
    }
}

fn build_plan(bare: &Path, opts: &ImportOptions) -> (String, ImportPlan) {
    let tip = git_str(bare, &["rev-parse", "HEAD"]);
    let pack = pack_via_revs(bare, &tip);
    let db = GitObjectDb::from_pack(&pack, no_base_lookup).expect("pack decodes");
    let plan = plan_import(&db, &tip, opts).expect("plan_import");
    (tip, plan)
}

// ---------------------------------------------------------------------------
// 1. Fidelity for EVERY commit of EVERY fixture
// ---------------------------------------------------------------------------

#[test]
fn fidelity_invariant_all_fixtures() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    for (name, build) in all_fixtures() {
        let repo = build();
        let (tip, plan) = build_plan(&repo.bare, &ImportOptions::default());
        assert_eq!(plan.tip_sha, tip, "[{name}] tip");
        // Every reachable commit is planned exactly once.
        let count: usize = git_str(&repo.bare, &["rev-list", "--count", &tip]).parse().unwrap();
        assert_eq!(plan.commits.len(), count, "[{name}] commit count");
        // root_sha = the first-parent root of HEAD.
        let fp_root = git_str(&repo.bare, &["rev-list", "--first-parent", "--max-parents=0", &tip]);
        assert_eq!(plan.root_sha, fp_root, "[{name}] root sha");
        assert_plan_wellformed(&plan, name);
        assert_fidelity(&plan, &repo.bare, name);
    }
}

// ---------------------------------------------------------------------------
// 2. Lane topology per fixture
// ---------------------------------------------------------------------------

#[test]
fn merged_prs_lane_topology() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    let repo = merged_prs();
    let (_tip, plan) = build_plan(&repo.bare, &ImportOptions::default());

    assert_eq!(plan.lanes.len(), 3, "main + two PR lanes: {:?}", plan.lanes);
    let names: Vec<&str> = plan.lanes[1..].iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, vec!["feature-1", "feature-2"], "named from the PR subjects");
    for lane in &plan.lanes[1..] {
        assert!(lane.deleted_after_merge, "delete-after-merge default");
        assert_eq!(lane.fork.as_ref().unwrap().lane, MAIN_LANE, "PR lanes fork off main");
        assert!(lane.merged_at_commit.is_some());
    }
    // Each merge commit on main carries exactly one MergeInfo to its PR lane.
    let merges: Vec<_> = plan.commits.iter().filter(|c| !c.merges.is_empty()).collect();
    assert_eq!(merges.len(), 2);
    assert_eq!(merges[0].merges[0].source_lane, 1);
    assert_eq!(merges[1].merges[0].source_lane, 2);

    // keep_imported_branches flips off every delete.
    let (_t, keep) =
        build_plan(&repo.bare, &ImportOptions { depth: None, keep_imported_branches: true, ..Default::default() });
    assert!(keep.lanes.iter().all(|l| !l.deleted_after_merge));
}

#[test]
fn fixture_lane_shapes() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    for (name, build) in all_fixtures() {
        let repo = build();
        let (_tip, plan) = build_plan(&repo.bare, &ImportOptions::default());
        let side = plan.lanes.len() - 1;
        let expect_side = match name {
            // Linear histories: no side lanes.
            "linear_basic" | "modes_and_symlinks" | "gitignore_nested" | "pointers" => 0,
            // One merged side branch each.
            "renames_across_merge" | "mid_history_root" => 1,
            // merged_prs: two PR lanes. merge_into_side: 'side' lane (its
            // main-into-side merge points back at main, no extra lane).
            // foxtrot: mainline C2 becomes the side lane.
            "merged_prs" => 2,
            "merge_into_side" => 1,
            "foxtrot" => 1,
            // octopus: three side lanes, one per extra parent.
            "octopus" => 3,
            // criss-cross: branch-a chain, B's chain, branch-b tip (see the
            // in-crate unit test for the full hand-derived structure).
            "criss_cross" => 3,
            other => panic!("fixture {other} missing a lane expectation"),
        };
        assert_eq!(side, expect_side, "[{name}] side-lane count; lanes: {:?}", plan.lanes);
    }
}

#[test]
fn octopus_merge_edges_and_mid_history_root() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    // Octopus: the merge commit carries one MergeInfo per extra parent, in
    // parent order, each to a distinct lane.
    let repo = asp_e2e::gitfix::octopus();
    let (_tip, plan) = build_plan(&repo.bare, &ImportOptions::default());
    let oct = plan.commits.iter().find(|c| c.parents.len() == 4).expect("4-parent merge");
    assert_eq!(oct.merges.len(), 3);
    assert_eq!(oct.merges.iter().map(|m| &m.source_tip_sha).collect::<Vec<_>>(),
               oct.parents[1..].iter().collect::<Vec<_>>());
    let mut lanes: Vec<_> = oct.merges.iter().map(|m| m.source_lane).collect();
    lanes.dedup();
    assert_eq!(lanes.len(), 3, "each extra parent gets its own lane");

    // Mid-history root: the grafted chain becomes a side lane with NO fork point
    // (its first commit has no parent at all).
    let repo = mid_history_root();
    let (_tip, plan) = build_plan(&repo.bare, &ImportOptions::default());
    assert_eq!(plan.lanes.len(), 2);
    let graft = &plan.lanes[1];
    assert!(graft.fork.is_none(), "grafted root has no fork point: {graft:?}");
    let graft_root = plan.commits.iter().find(|c| c.sha == graft.created_at_commit).unwrap();
    assert!(graft_root.parents.is_empty());
    assert_eq!(graft_root.message, "independent root");
}

#[test]
fn modes_symlinks_and_pointer_warnings() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    // Executable + symlink modes survive the model.
    let repo = asp_e2e::gitfix::modes_and_symlinks();
    let (_tip, plan) = build_plan(&repo.bare, &ImportOptions::default());
    let all_ops: Vec<&FileOp> = plan.commits.iter().flat_map(|c| c.ops.iter()).collect();
    assert!(all_ops.iter().any(|o| matches!(o,
        FileOp::Create { path, mode: EntryMode::Executable, .. } if path == "script.sh")));
    assert!(all_ops.iter().any(|o| matches!(o,
        FileOp::Create { path, mode: EntryMode::Symlink, .. } if path == "link")));
    // Retargeting the symlink is an Edit that stays a symlink.
    assert!(all_ops.iter().any(|o| matches!(o,
        FileOp::Edit { path, mode: EntryMode::Symlink, old_mode: EntryMode::Symlink, .. } if path == "link")));

    // Gitlink → Submodule warning (no FileOp); LFS pointer → one repo-level warning.
    let repo = asp_e2e::gitfix::pointers();
    let (_tip, plan) = build_plan(&repo.bare, &ImportOptions::default());
    assert!(plan.warnings.iter().any(|w| matches!(w,
        ImportWarning::Submodule { path, .. } if path == "sub")));
    assert!(plan.warnings.iter().any(|w| matches!(w,
        ImportWarning::LfsPointers { paths } if paths == &vec!["big.bin".to_string()])));
    // .gitmodules imports as a normal file; the gitlink itself produces no op.
    let all_ops: Vec<&FileOp> = plan.commits.iter().flat_map(|c| c.ops.iter()).collect();
    assert!(all_ops.iter().any(|o| matches!(o,
        FileOp::Create { path, .. } if path == ".gitmodules")));
    assert!(!all_ops.iter().any(|o| o.sort_key() == "sub"));
}

// ---------------------------------------------------------------------------
// 3. Determinism across pack layouts
// ---------------------------------------------------------------------------

#[test]
fn plan_is_identical_across_pack_layouts_and_rebuilds() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    for build in [merged_prs as fn() -> FixtureRepo, asp_e2e::gitfix::criss_cross] {
        let repo = build();
        let tip = git_str(&repo.bare, &["rev-parse", "HEAD"]);

        let pack_a = pack_via_revs(&repo.bare, &tip);
        let pack_b = pack_via_object_list(&repo.bare, &tip);

        let db_a = GitObjectDb::from_pack(&pack_a, no_base_lookup).unwrap();
        let db_b = GitObjectDb::from_pack(&pack_b, no_base_lookup).unwrap();
        assert_eq!(db_a.len(), db_b.len(), "same object set");

        let opts = ImportOptions::default();
        let p1 = plan_import(&db_a, &tip, &opts).unwrap();
        let p2 = plan_import(&db_a, &tip, &opts).unwrap(); // rebuild, same db
        let p3 = plan_import(&db_b, &tip, &opts).unwrap(); // different pack layout
        assert_eq!(format!("{p1:?}"), format!("{p2:?}"), "rebuild determinism");
        assert_eq!(format!("{p1:?}"), format!("{p3:?}"), "pack-layout independence");
    }
}

// ---------------------------------------------------------------------------
// 4. Depth cut against a real repo
// ---------------------------------------------------------------------------

#[test]
fn depth_cut_matches_git_tree_at_cut_point() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    let repo = linear_basic(); // 5 linear commits
    let tip = git_str(&repo.bare, &["rev-parse", "HEAD"]);
    let pack = pack_via_revs(&repo.bare, &tip);
    let db = GitObjectDb::from_pack(&pack, no_base_lookup).unwrap();

    let opts = ImportOptions { depth: Some(2), keep_imported_branches: false, ..Default::default() };
    let plan = plan_import(&db, &tip, &opts).unwrap();
    // snapshot + the last 2 first-parent commits.
    assert_eq!(plan.commits.len(), 3);
    let snap = &plan.commits[0];
    assert!(snap.is_depth_cut_snapshot);
    let cut = git_str(&repo.bare, &["rev-parse", "HEAD~2"]);
    assert_eq!(snap.sha, cut, "cut point = HEAD~2");
    assert_eq!(plan.root_sha, cut);
    // Snapshot ops = full tree of the cut commit; then normal diffs — the same
    // replay fidelity must hold for the depth plan too.
    assert_fidelity(&plan, &repo.bare, "linear_basic(depth=2)");
    // Equal depth twice → identical plan.
    let plan2 = plan_import(&db, &tip, &opts).unwrap();
    assert_eq!(format!("{plan:?}"), format!("{plan2:?}"));
}

// ---------------------------------------------------------------------------
// 5. Deterministic LCG fuzz over real git histories (conflict-free by design)
// ---------------------------------------------------------------------------

#[test]
fn fuzz_random_git_histories_hold_the_invariant() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    let mut state: u64 = 0x243F6A8885A308D3;
    let mut next = move || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 33) as u32
    };

    for trial in 0..3u32 {
        let mut r = FixtureRepo::init(&format!("fuzz{trial}"));
        r.commit_file("base.txt", "base\n", "base");
        // Each branch only ever touches its own file → merges never conflict.
        let mut branches: Vec<String> = vec!["main".into()];
        let mut versions: BTreeMap<String, u32> = BTreeMap::new();

        for step in 0..14 {
            match next() % 4 {
                // New branch off a random existing branch.
                0 => {
                    let at = branches[(next() as usize) % branches.len()].clone();
                    let name = format!("br-{trial}-{step}");
                    r.checkout(&at);
                    r.checkout_new(&name, None);
                    branches.push(name);
                }
                // Merge a random branch into a random other branch (--no-ff).
                1 if branches.len() >= 2 => {
                    let a = branches[(next() as usize) % branches.len()].clone();
                    let b = branches[(next() as usize) % branches.len()].clone();
                    if a != b {
                        r.checkout(&b);
                        // May be "already up to date" (no commit) — fine either way.
                        r.merge(&a, &format!("Merge branch '{a}'"), true);
                    }
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
        r.checkout("main");
        let r = r.finish();

        let (_tip, plan) = build_plan(&r.bare, &ImportOptions::default());
        assert_plan_wellformed(&plan, &format!("fuzz{trial}"));
        assert_fidelity(&plan, &r.bare, &format!("fuzz{trial}"));

        // Random depth cut also holds fidelity.
        let depth = 1 + next() % 4;
        let tip = git_str(&r.bare, &["rev-parse", "HEAD"]);
        let pack = pack_via_revs(&r.bare, &tip);
        let db = GitObjectDb::from_pack(&pack, no_base_lookup).unwrap();
        let dplan = plan_import(&db, &tip, &ImportOptions { depth: Some(depth), keep_imported_branches: false, ..Default::default() }).unwrap();
        assert_fidelity(&dplan, &r.bare, &format!("fuzz{trial}(depth={depth})"));
    }
}
