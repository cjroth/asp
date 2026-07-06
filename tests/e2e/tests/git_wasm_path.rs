//! The **wasm browser-clone path's Rust half**, driven end-to-end from the recorded
//! smart-HTTP protocol-v2 wire fixtures (git-bridge §7.3, §10 "wasm path").
//!
//! This is the exact chain `asp_wasm::WasmEngine::git_clone` runs *after* the JS
//! `fetch()` transport hands back bytes: parse the info/refs advertisement, parse the
//! ls-refs response (→ HEAD symref + tip), side-band-demux the fetch response's
//! packfile, decode it, plan the import, synthesize deterministic genesis, and fold
//! it into a pristine `MemEngine` (the wasm engine). Fully hermetic — no system git,
//! no network — so it always runs, unlike the pack-building fixture tests.
//!
//! The fixtures were recorded from a real `git http-backend` (see
//! `src/bin/record_fixtures.rs`) against the `linear_basic` repo, so this proves the
//! browser code path against genuine GitHub-shaped wire bytes.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use asp_core::gitgenesis::{git_vault_id, synthesize_genesis, DbBlobSource};
use asp_core::gitimport::{no_base_lookup, plan_import, GitObjectDb, ImportOptions};
use asp_core::gitwire::{parse_capability_advertisement, parse_ls_refs_response, FetchResponseParser};
use asp_core::identity::Identity;
use asp_core::log::{Kind, MAIN_BRANCH_ID};
use asp_core::store::{BlobStore, MemBlobStore};
use asp_core::wire::{WireBlob, WireRow};
use asp_core::{LogRow, MemEngine, SessionVault};
use asp_e2e::gitfix::open_branches;

fn fixture(name: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures").join(name);
    std::fs::read(&p).unwrap_or_else(|e| panic!("read fixture {}: {e}", p.display()))
}

/// Bundle each row with its content blobs (mirrors `asp_wasm::to_wires`).
fn to_wires(rows: &[LogRow], store: &MemBlobStore) -> Vec<WireRow> {
    rows.iter()
        .map(|r| {
            let mut blobs: Vec<WireBlob> = Vec::new();
            for h in [r.base_hash.clone(), r.result_hash.clone()].into_iter().flatten() {
                if let Some(bytes) = store.get_blob(&h).ok().flatten() {
                    if !blobs.iter().any(|b| b.hash == h) {
                        blobs.push(WireBlob { hash: h, bytes });
                    }
                }
            }
            WireRow { row: r.clone(), blobs }
        })
        .collect()
}

#[test]
fn wasm_clone_from_recorded_wire_fixtures() {
    // 1. info/refs advertisement → protocol-v2 capabilities (must be sha1).
    let info = fixture("info_refs_v2.bin");
    let caps = parse_capability_advertisement(&info).expect("parse advertisement");
    assert!(caps.supports("ls-refs") && caps.supports("fetch"));
    caps.object_format().expect("sha1 object format accepted");

    // 2. ls-refs → HEAD symref target + tip (what `resolve_head` extracts in wasm).
    let ls = fixture("ls_refs_v2.bin");
    let refs = parse_ls_refs_response(&ls).expect("parse ls-refs");
    let head = refs.iter().find(|r| r.name == "HEAD").expect("HEAD advertised");
    assert_eq!(head.symref_target.as_deref(), Some("refs/heads/main"));
    let tip = head.oid.clone();
    assert_eq!(tip, "89d2010db2188ca6e11e8eaf7a844e7eea72f869");

    // 3. fetch response → side-band-demuxed packfile.
    let fetch = fixture("fetch_v2.bin");
    let resp = FetchResponseParser::parse(&fetch).expect("parse fetch response");
    assert!(resp.saw_packfile, "fetch response carried a packfile section");
    assert_eq!(&resp.pack[..4], b"PACK", "reassembled band-1 pack bytes");

    // 4/5. decode → plan → deterministic genesis (full clone: no external bases).
    let db = GitObjectDb::from_pack(&resp.pack, no_base_lookup).expect("decode pack");
    let plan = plan_import(&db, &tip, &ImportOptions::default()).expect("plan import");
    assert_eq!(plan.tip_sha, tip);
    let scratch = MemBlobStore::new();
    let g = synthesize_genesis(&plan, &DbBlobSource::new(&db), &scratch).expect("genesis");
    assert_eq!(g.vault_id, git_vault_id(&plan.root_sha), "vault id is repo-derived");

    // 6. fold into a pristine MemEngine (the wasm engine) and check the tip tree.
    let eng = MemEngine::create(Identity::from_seed(&[5u8; 32]), "");
    eng.adopt_vault_id(&g.vault_id).unwrap();
    eng.integrate_many(&to_wires(&g.rows, &scratch)).unwrap();
    let files = eng.files_map().unwrap();

    // linear_basic's tip tree: a.txt renamed → a2.txt (final 3-line content),
    // dir/b.txt deleted, dir/c.txt added — plus the clone-seeded .aspignore.
    assert_eq!(files.get("a2.txt").map(|v| v.as_slice()), Some(&b"alpha\nalpha2\nalpha3\n"[..]));
    assert_eq!(files.get("dir/c.txt").map(|v| v.as_slice()), Some(&b"charlie\n"[..]));
    assert!(!files.contains_key("a.txt"), "a.txt was renamed away");
    assert!(!files.contains_key("dir/b.txt"), "dir/b.txt was deleted");
    assert!(files.contains_key(".aspignore"), "clone seeds .aspignore");

    // Determinism: an independent decode+genesis authors byte-identical rows (so two
    // browsers cloning the same URL converge over ordinary ASP sync, git-bridge §3.2).
    let db2 = GitObjectDb::from_pack(&resp.pack, no_base_lookup).unwrap();
    let plan2 = plan_import(&db2, &tip, &ImportOptions::default()).unwrap();
    let scratch2 = MemBlobStore::new();
    let g2 = synthesize_genesis(&plan2, &DbBlobSource::new(&db2), &scratch2).unwrap();
    assert_eq!(g.rows, g2.rows, "independent clones author identical rows");
    assert_eq!(g.vault_id, g2.vault_id);
}

// ---------------------------------------------------------------------------
// The wasm-path equivalent of the ground-truth test for the `all_branches`
// checkbox (`specs/git-open-branches.md` §5): the exact Rust chain
// `WasmEngine::git_clone(..., all_branches=true, ...)` runs after the JS transport —
// build one pack over HEAD + every open-branch tip, plan with `open_branch_tips`,
// synthesize genesis, fold, and assert every live open branch folds to its git tip
// tree (the phase-2 fidelity invariant, through the wasm engine). Uses a live pack
// from the `open_branches` fixture; skips when system git is absent (spec §10).
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

/// Pack every object reachable from `tips` (HEAD + all open-branch tips), the single
/// pack an `all_branches` fetch downloads (§6).
fn pack_revs(bare: &Path, tips: &[String]) -> Vec<u8> {
    let stdin = tips.iter().map(|t| format!("{t}\n")).collect::<String>();
    let mut child = Command::new("git")
        .arg("-C").arg(bare).args(["pack-objects", "--revs", "--stdout", "-q"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().expect("spawn git pack-objects");
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().expect("pack-objects output");
    assert!(out.status.success() && !out.stdout.is_empty(), "pack-objects failed");
    out.stdout
}

/// `(ref_name, tip_sha)` for every `refs/heads/*` except `main` — what `git_clone`'s
/// ls-refs walk hands `ImportOptions.open_branch_tips`.
fn open_tips(bare: &Path) -> Vec<(String, String)> {
    git_str(bare, &["for-each-ref", "--format=%(refname:short) %(objectname)", "refs/heads"])
        .lines()
        .filter_map(|l| {
            let (n, s) = l.split_once(' ')?;
            (n != "main").then(|| (n.to_string(), s.to_string()))
        })
        .collect()
}

/// `git ls-tree -r <sha>` → path → blob content bytes (gitlinks excluded).
fn tree_content(db: &GitObjectDb, bare: &Path, sha: &str) -> BTreeMap<String, Vec<u8>> {
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
        map.insert(path.to_string(), db.get(oid).map(|(_, b)| b.to_vec()).unwrap_or_default());
    }
    map
}

#[test]
fn wasm_clone_all_branches_folds_every_open_branch() {
    if !git_available() {
        eprintln!("skipping: system git not available");
        return;
    }
    let repo = open_branches();
    let head = git_str(&repo.bare, &["rev-parse", "HEAD"]);
    let open = open_tips(&repo.bare);
    // wants = HEAD + every open-branch tip (one pack), exactly as `git_clone`'s
    // `all_branches` path builds them.
    let mut wants: Vec<String> = vec![head.clone()];
    for (_n, s) in &open {
        if !wants.contains(s) {
            wants.push(s.clone());
        }
    }

    // decode → plan (with open_branch_tips) → genesis → fold (the wasm `apply_clone_pack`).
    let db = GitObjectDb::from_pack(&pack_revs(&repo.bare, &wants), no_base_lookup).unwrap();
    let opts = ImportOptions { open_branch_tips: open.clone(), ..Default::default() };
    let plan = plan_import(&db, &head, &opts).unwrap();

    // stale-pointer (an ancestor of main) is the only skipped ref; five live lanes.
    assert_eq!(plan.skipped_reachable, vec!["stale-pointer".to_string()]);
    let live: Vec<&str> = plan.lanes.iter().filter(|l| l.live).map(|l| l.name.as_str()).collect();
    assert_eq!(live, vec!["feat/simple", "feature-1-2", "nested/deep", "orphan", "with-merge"]);

    let scratch = MemBlobStore::new();
    let g = synthesize_genesis(&plan, &DbBlobSource::new(&db), &scratch).unwrap();
    let eng = MemEngine::create(Identity::from_seed(&[9u8; 32]), "");
    eng.adopt_vault_id(&g.vault_id).unwrap();
    eng.set_batch(true);
    for page in to_wires(&g.rows, &scratch).chunks(256) {
        eng.integrate_many(page).unwrap();
    }
    eng.set_batch(false);
    eng.materialize().unwrap();

    // main folds to HEAD's tree.
    let fold = |branch: &str| -> BTreeMap<String, Vec<u8>> {
        eng.checkout(branch).unwrap();
        let mut m = eng.files_map().unwrap();
        m.remove(".aspignore");
        m
    };
    assert_eq!(fold(MAIN_BRANCH_ID), tree_content(&db, &repo.bare, &head), "main fold");

    // Every live open branch folds to its git tip tree (phase-2 fidelity, wasm engine).
    let branch_id_for = |name: &str| -> Option<String> {
        g.rows.iter().find(|r| r.kind == Kind::Branch && r.path.as_deref() == Some(name)).map(|r| r.file_id.clone())
    };
    for (ref_name, tip) in &open {
        if ref_name == "stale-pointer" {
            assert!(branch_id_for(ref_name).is_none(), "skipped ref has no branch record");
            continue;
        }
        let asp = if ref_name == "feature-1" { "feature-1-2" } else { ref_name.as_str() };
        let bid = branch_id_for(asp).unwrap_or_else(|| panic!("branch record for {ref_name}"));
        assert_eq!(fold(&bid), tree_content(&db, &repo.bare, tip), "fold({ref_name}) != its git tip tree");
    }

    // Report-count parity: live lanes + skipped match what the clone report surfaces.
    assert_eq!(plan.lanes.iter().filter(|l| l.live).count(), 5, "open_branches count");
    assert_eq!(plan.skipped_reachable.len(), 1, "refs_skipped count");
}
