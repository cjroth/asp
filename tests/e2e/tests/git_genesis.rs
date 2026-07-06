//! Integration tests for the git-import **row synthesis** (`asp_core::gitgenesis`,
//! git-bridge §3.1/§3.2, §4.2): pack bytes → [`GitObjectDb`] → [`plan_import`] →
//! [`synthesize_genesis`] → sealed [`LogRow`]s → folded in a fresh [`MemEngine`].
//!
//! The load-bearing check (git-bridge §3.1 **fidelity invariant**): after loading a
//! fixture's synthesized genesis rows into a pristine engine, the fold of `main`
//! equals `git ls-tree -r <tip>` (path → content), and each side lane's fold equals
//! its own tip commit's tree. Plus: byte determinism (two independent packs → the
//! same rows + vault id), branch structure, rename-follows-file_id, `.aspignore`
//! seeding, mode/symlink tables, and a deterministic LCG fuzz.
//!
//! System git is a sanctioned dev-only dependency (spec §10); tests skip when absent.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use asp_core::gitgenesis::{
    git_site_id, git_vault_id, synthesize_genesis, DbBlobSource, GenesisOutput,
};
use asp_core::gitimport::{
    no_base_lookup, plan_import, GitObjectDb, ImportOptions, ImportPlan, LaneId,
};
use asp_core::identity::Identity;
use asp_core::log::{Kind, LogRow, MAIN_BRANCH_ID};
use asp_core::memengine::MemEngine;
use asp_core::store::{BlobStore, MemBlobStore};
use asp_core::wire::{WireBlob, WireRow};
use asp_e2e::gitfix::{all_fixtures, gitignore_nested, merged_prs, modes_and_symlinks, octopus, renames_across_merge, FixtureRepo};

// ---------------------------------------------------------------------------
// git plumbing (mirrors git_import_model.rs)
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

fn pack_via_revs(bare: &Path, tip: &str) -> Vec<u8> {
    pack_with_stdin(bare, &["pack-objects", "--revs", "--stdout", "-q"], &format!("{tip}\n"))
}

fn pack_via_object_list(bare: &Path, tip: &str) -> Vec<u8> {
    let list = String::from_utf8_lossy(&git_in(bare, &["rev-list", "--objects", tip])).to_string();
    pack_with_stdin(bare, &["pack-objects", "--stdout", "-q"], &list)
}

fn pack_with_stdin(bare: &Path, args: &[&str], stdin: &str) -> Vec<u8> {
    let mut child = Command::new("git").arg("-C").arg(bare).args(args)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().expect("spawn git pack-objects");
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().expect("pack-objects output");
    assert!(out.status.success() && !out.stdout.is_empty(), "pack-objects failed");
    out.stdout
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
            continue; // gitlink
        }
        map.insert(path.to_string(), oid.to_string());
    }
    map
}

/// Expected `path -> content bytes` for a commit's tree (blob bytes from the db;
/// a symlink's blob bytes are its target text — exactly what the fold materializes).
fn tree_content(db: &GitObjectDb, bare: &Path, sha: &str) -> BTreeMap<String, Vec<u8>> {
    ls_tree_blobs(bare, sha)
        .into_iter()
        .map(|(path, oid)| {
            let bytes = db.get(&oid).map(|(_, b)| b.to_vec()).unwrap_or_default();
            (path, bytes)
        })
        .collect()
}

fn build(bare: &Path, opts: &ImportOptions) -> (GitObjectDb, ImportPlan) {
    let tip = git_str(bare, &["rev-parse", "HEAD"]);
    let pack = pack_via_revs(bare, &tip);
    let db = GitObjectDb::from_pack(&pack, no_base_lookup).expect("pack decodes");
    let plan = plan_import(&db, &tip, opts).expect("plan_import");
    (db, plan)
}

// ---------------------------------------------------------------------------
// synthesize + load into a fresh MemEngine
// ---------------------------------------------------------------------------

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

/// Synthesize `plan` into a store and load into a fresh engine that adopts the
/// derived vault id (a pristine clone).
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

/// The engine's fold of `branch_id`, as `path -> bytes`, minus the seeded `.aspignore`.
fn fold_content(e: &MemEngine, branch_id: &str) -> BTreeMap<String, Vec<u8>> {
    e.checkout(branch_id).unwrap();
    let mut m = e.files_map().unwrap();
    m.remove(".aspignore");
    m
}

/// The derived branch id for a lane name (from its `Kind::Branch` record's file_id).
fn branch_id_for(rows: &[LogRow], name: &str) -> Option<String> {
    rows.iter().find(|r| r.kind == Kind::Branch && r.path.as_deref() == Some(name)).map(|r| r.file_id.clone())
}

/// The tip commit sha of a lane = its last commit in canonical order.
fn lane_tip(plan: &ImportPlan, lane: LaneId) -> String {
    plan.commits.iter().rev().find(|c| c.lane == lane).unwrap().sha.clone()
}

// ---------------------------------------------------------------------------
// 1. Fidelity invariant — main + per-lane tip folds
// ---------------------------------------------------------------------------

#[test]
fn fidelity_main_and_side_lanes_all_fixtures() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    for (name, mk) in all_fixtures() {
        let repo = mk();
        let tip = git_str(&repo.bare, &["rev-parse", "HEAD"]);
        let (db, plan) = build(&repo.bare, &ImportOptions::default());
        let (g, _store, e) = load(&db, &plan);

        // vault id / site id are pure functions of the root.
        assert_eq!(g.vault_id, git_vault_id(&plan.root_sha), "[{name}] vault id");
        assert!(g.rows.iter().all(|r| r.site_id == git_site_id(&plan.root_sha)), "[{name}] repo site");

        // main fold == tip tree.
        let main = fold_content(&e, MAIN_BRANCH_ID);
        assert_eq!(main, tree_content(&db, &repo.bare, &tip), "[{name}] main fold != tip tree");

        // each side lane's fold == its tip commit's tree.
        for lane in &plan.lanes {
            if lane.id == 0 {
                continue;
            }
            let bid = branch_id_for(&g.rows, &lane.name).expect("branch record");
            let got = fold_content(&e, &bid);
            let want = tree_content(&db, &repo.bare, &lane_tip(&plan, lane.id));
            assert_eq!(got, want, "[{name}] lane {} ({}) fold != its tip tree", lane.id, lane.name);
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Byte determinism (§10a)
// ---------------------------------------------------------------------------

#[test]
fn genesis_is_byte_deterministic_across_pack_layouts() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    for mk in [merged_prs as fn() -> FixtureRepo, asp_e2e::gitfix::criss_cross] {
        let repo = mk();
        let tip = git_str(&repo.bare, &["rev-parse", "HEAD"]);
        let db_a = GitObjectDb::from_pack(&pack_via_revs(&repo.bare, &tip), no_base_lookup).unwrap();
        let db_b = GitObjectDb::from_pack(&pack_via_object_list(&repo.bare, &tip), no_base_lookup).unwrap();
        let plan_a = plan_import(&db_a, &tip, &ImportOptions::default()).unwrap();
        let plan_b = plan_import(&db_b, &tip, &ImportOptions::default()).unwrap();

        let s1 = MemBlobStore::new();
        let s2 = MemBlobStore::new();
        let g1 = synthesize_genesis(&plan_a, &DbBlobSource::new(&db_a), &s1).unwrap();
        let g2 = synthesize_genesis(&plan_a, &DbBlobSource::new(&db_a), &s1).unwrap(); // rebuild
        let g3 = synthesize_genesis(&plan_b, &DbBlobSource::new(&db_b), &s2).unwrap(); // other pack
        assert_eq!(g1.rows, g2.rows, "rebuild determinism");
        assert_eq!(g1.rows, g3.rows, "pack-layout independence");
        assert_eq!(g1.vault_id, g3.vault_id);
    }
}

// ---------------------------------------------------------------------------
// 3. Branch structure
// ---------------------------------------------------------------------------

#[test]
fn merged_prs_branch_records_and_octopus_merges() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    // merged_prs: two side branches feature-1/feature-2, each a create + delete record.
    let repo = merged_prs();
    let (db, plan) = build(&repo.bare, &ImportOptions::default());
    let (g, _s, _e) = load(&db, &plan);
    let branch_rows: Vec<&LogRow> = g.rows.iter().filter(|r| r.kind == Kind::Branch).collect();
    // 2 lanes × (create + delete).
    assert_eq!(branch_rows.len(), 4, "create+delete per PR lane");
    for name in ["feature-1", "feature-2"] {
        let mine: Vec<_> = branch_rows.iter().filter(|r| r.path.as_deref() == Some(name)).collect();
        assert_eq!(mine.len(), 2, "{name}: one create + one delete");
    }
    assert!(g.rows.iter().filter(|r| r.kind == Kind::Merge).count() == 2, "two merge markers");

    // keep_imported_branches → no delete records.
    let (db2, plan2) = build(&repo.bare, &ImportOptions { depth: None, keep_imported_branches: true });
    let store = MemBlobStore::new();
    let g2 = synthesize_genesis(&plan2, &DbBlobSource::new(&db2), &store).unwrap();
    assert_eq!(g2.rows.iter().filter(|r| r.kind == Kind::Branch).count(), 2, "creates only, no deletes");

    // octopus: the 4-parent merge authors 3 chained Merge rows.
    let repo = octopus();
    let (db3, plan3) = build(&repo.bare, &ImportOptions::default());
    let store = MemBlobStore::new();
    let g3 = synthesize_genesis(&plan3, &DbBlobSource::new(&db3), &store).unwrap();
    assert_eq!(g3.rows.iter().filter(|r| r.kind == Kind::Merge).count(), 3, "octopus → 3 merge rows");
}

// ---------------------------------------------------------------------------
// 4. Rename follows file_id across a merge
// ---------------------------------------------------------------------------

#[test]
fn rename_follows_file_id() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    let repo = renames_across_merge();
    let (db, plan) = build(&repo.bare, &ImportOptions::default());
    let store = MemBlobStore::new();
    let g = synthesize_genesis(&plan, &DbBlobSource::new(&db), &store).unwrap();

    // The rename row (foo.txt -> bar.txt) reuses the from-path's file_id: the Create
    // of foo.txt and the Rename to bar.txt share one file_id on the side lane.
    let create_foo = g.rows.iter().find(|r| r.kind == Kind::Create && r.path.as_deref() == Some("foo.txt")).expect("create foo");
    let rename = g.rows.iter().find(|r| r.kind == Kind::Rename && r.path.as_deref() == Some("bar.txt")).expect("rename to bar");
    assert_eq!(rename.file_id, create_foo.file_id, "rename keeps the file_id");
    assert_eq!(rename.parent.as_deref(), Some(create_foo.id.as_str()), "rename chains onto the create");
}

// ---------------------------------------------------------------------------
// 5. .aspignore seeding
// ---------------------------------------------------------------------------

#[test]
fn aspignore_root_verbatim_and_nested_prefixed() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    let repo = gitignore_nested();
    let (db, plan) = build(&repo.bare, &ImportOptions::default());
    let (g, _s, _e) = load(&db, &plan);
    let a = &g.aspignore;

    // Header + sentinel.
    assert!(a.contains("generated from the repository's .gitignore"), "header");
    assert!(a.contains("# --- from .gitignore above; edit freely ---"), "sentinel");
    // Root patterns verbatim (including the negation, which roots keep).
    assert!(a.contains("\n*.log\n"), "root *.log verbatim: {a}");
    assert!(a.contains("\n!keep.log\n"), "root negation verbatim");
    assert!(a.contains("\nbuild/\n"), "root build/ verbatim");
    // Nested pattern gets its dir prefix; its negation is dropped (noted).
    assert!(a.contains("sub/*.tmp"), "nested prefixed: {a}");
    assert!(a.contains("dropped negation"), "nested negation noted");
    assert!(!a.contains("\n!important.tmp\n"), "nested negation not emitted raw");
}

// ---------------------------------------------------------------------------
// 6. Modes + symlinks
// ---------------------------------------------------------------------------

#[test]
fn modes_and_symlinks_tables() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    let repo = modes_and_symlinks();
    let (db, plan) = build(&repo.bare, &ImportOptions::default());
    let store = MemBlobStore::new();
    let g = synthesize_genesis(&plan, &DbBlobSource::new(&db), &store).unwrap();

    assert!(g.mode_table.iter().any(|(p, m)| p == "script.sh" && *m == 0o100755), "exec bit recorded: {:?}", g.mode_table);
    assert!(g.symlinks.contains(&"link".to_string()), "symlink path recorded: {:?}", g.symlinks);

    // The symlink row is Text and materializes to its (retargeted) target text.
    let sym = g.rows.iter().rfind(|r| r.path.as_deref() == Some("link")).expect("symlink row");
    assert_eq!(sym.merge_class, asp_core::log::MergeClass::Text, "symlink imports as Text");
    let e = MemEngine::create(Identity::from_seed(&[1; 32]), &g.vault_id);
    e.integrate_many(&to_wires(&g.rows, &store)).unwrap();
    assert_eq!(e.read_file("link").unwrap().as_deref(), Some(&b"targetB.txt"[..]), "symlink content = final target text");
}

// ---------------------------------------------------------------------------
// 7. LCG fuzz over random git histories (conflict-free)
// ---------------------------------------------------------------------------

#[test]
fn fuzz_synthesis_holds_invariants() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    let mut state: u64 = 0xB5297A4D_68E31DA4;
    let mut next = move || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 33) as u32
    };

    for trial in 0..3u32 {
        let mut r = FixtureRepo::init(&format!("gfuzz{trial}"));
        r.commit_file("base.txt", "base\n", "base");
        let mut branches: Vec<String> = vec!["main".into()];
        let mut versions: BTreeMap<String, u32> = BTreeMap::new();
        for step in 0..14 {
            match next() % 4 {
                0 => {
                    let at = branches[(next() as usize) % branches.len()].clone();
                    let nm = format!("b-{trial}-{step}");
                    r.checkout(&at);
                    r.checkout_new(&nm, None);
                    branches.push(nm);
                }
                1 if branches.len() >= 2 => {
                    let a = branches[(next() as usize) % branches.len()].clone();
                    let b = branches[(next() as usize) % branches.len()].clone();
                    if a != b {
                        r.checkout(&b);
                        r.merge(&a, &format!("Merge branch '{a}'"), true);
                    }
                }
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
        let repo = r.finish();
        let tip = git_str(&repo.bare, &["rev-parse", "HEAD"]);
        let (db, plan) = build(&repo.bare, &ImportOptions::default());
        let (g, _s, e) = load(&db, &plan);

        // Invariants: dense seq, strictly increasing lamport, every row sealed.
        for (i, row) in g.rows.iter().enumerate() {
            assert_eq!(row.seq as usize, i, "[fuzz{trial}] dense seq");
            assert_eq!(row.lamport, row.seq + 1, "[fuzz{trial}] lamport = 1 + index");
            assert!(row.id_valid(), "[fuzz{trial}] row {i} sealed");
        }
        // Main fold == tip tree.
        assert_eq!(fold_content(&e, MAIN_BRANCH_ID), tree_content(&db, &repo.bare, &tip), "[fuzz{trial}] main fidelity");
    }
}
