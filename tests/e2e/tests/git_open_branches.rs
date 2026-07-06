//! End-to-end tests for **importing open branches at clone** and the merge-after-import
//! pull (`specs/git-open-branches.md` §4/§5/§7), driving the native `gitremote` driver
//! (`clone_from_git` with `all_branches` + `pull_once`) against the hermetic smart-HTTP
//! fixture server — real git wire bytes end to end.
//!
//! Load-bearing checks:
//! * **Checkbox clone ground truth** — every imported live branch folds to its git tip
//!   tree; the report counts open branches + skipped refs; `git_remotes` still tracks
//!   only the default branch.
//! * **Zero regression** — a plain (`all_branches=false`) clone is unchanged and its row
//!   ids are a prefix-set of the checkbox clone's (the §2 phase-1 dedup property).
//! * **§4 merge-after-import pull** — a live imported branch that later merges upstream
//!   folds onto the EXISTING branch: one new merge node, no duplicate rows, the branch
//!   is tombstoned, `fold(main)` == the new tip. THE load-bearing test of the addendum.
//! * **Determinism** — two independent checkbox clones converge (vault id + row ids +
//!   branch ids).
//! * **CLI surface** — the real `asp` binary with `--all-branches`.
//! * **LCG fuzz** — random open-branch repos through the driver never panic; fidelity
//!   holds.
//!
//! Tests skip gracefully when system `git` is absent.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use asp_core::gitbridge::{remote_id, GitAuth, GitRemoteSpec};
use asp_core::gitremote::{clone_from_git, pull_once, CloneOptions, PullReport};
use asp_core::gitwire::GitUrl;
use asp_core::identity::Identity;
use asp_core::log::{Kind, LogRow, MAIN_BRANCH_ID};
use asp_core::store::BlobStore;
use asp_core::{Engine, SessionVault};
use asp_e2e::gitfix::{merge_branch_upstream, open_branches, FixtureRepo, GitHttpServer};

// ── harness ─────────────────────────────────────────────────────────────────

fn git_available() -> bool {
    Command::new("git").arg("version").output().map(|o| o.status.success()).unwrap_or(false)
}

fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
}

fn no_keyring() {
    std::env::set_var("ASP_GIT_DISABLE_KEYRING", "1");
    std::env::remove_var("ASP_GIT_TOKEN");
}

fn https(url: &str, auth: GitAuth) -> GitRemoteSpec {
    GitRemoteSpec { url: GitUrl::Https { base: url.to_string() }, auth }
}

fn open_engine(dir: &Path, seed: u8) -> Engine {
    Engine::open(dir, Identity::from_seed(&[seed; 32])).expect("open engine")
}

fn all_branches_opts<'a>() -> CloneOptions<'a> {
    CloneOptions { depth: None, new_identity: false, all_branches: true, on_progress: None }
}

fn rev_parse(bare: &Path, spec: &str) -> String {
    let out = Command::new("git").arg("--git-dir").arg(bare).args(["rev-parse", spec]).output().expect("rev-parse");
    assert!(out.status.success(), "rev-parse {spec}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `git rev-parse <spec>`, or `None` when it doesn't resolve (e.g. a `-N`-deduped ASP
/// branch name that isn't a git ref).
fn rev_parse_opt(bare: &Path, spec: &str) -> Option<String> {
    let out = Command::new("git").arg("--git-dir").arg(bare).args(["rev-parse", "--verify", "--quiet", spec]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// `git ls-tree -r <sha>` → `path -> content bytes` (blobs only).
fn tree_content(bare: &Path, sha: &str) -> BTreeMap<String, Vec<u8>> {
    let out = Command::new("git").arg("--git-dir").arg(bare).args(["ls-tree", "-r", sha]).output().expect("ls-tree");
    assert!(out.status.success(), "ls-tree: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let (meta, path) = line.split_once('\t').expect("ls-tree line");
        let mut parts = meta.split_whitespace();
        let _mode = parts.next().unwrap();
        let typ = parts.next().unwrap();
        let oid = parts.next().unwrap();
        if typ == "commit" {
            continue; // gitlink
        }
        let bytes = Command::new("git").arg("--git-dir").arg(bare).args(["cat-file", "blob", oid]).output().expect("cat-file").stdout;
        map.insert(path.to_string(), bytes);
    }
    map
}

/// The engine's fold of branch `bid` as `path -> bytes` (minus `.aspignore`).
fn fold_branch(engine: &Engine, bid: &str) -> BTreeMap<String, Vec<u8>> {
    engine.checkout(bid).expect("checkout");
    let mut m = BTreeMap::new();
    for f in engine.store.live_files().expect("live_files") {
        if f.deleted || f.path == ".aspignore" {
            continue;
        }
        if let Some(h) = &f.result_hash {
            m.insert(f.path.clone(), engine.store.get_blob(h).unwrap().unwrap_or_default());
        }
    }
    m
}

/// The branch id of the (live) ASP branch named `name`.
fn branch_id_named(engine: &Engine, name: &str) -> Option<String> {
    engine.branches().unwrap().into_iter().find(|b| b.name == name).map(|b| b.branch_id)
}

fn markers_for(rows: &[LogRow], sha: &str) -> Vec<LogRow> {
    rows.iter().filter(|r| r.kind == Kind::GitCommit && r.path.as_deref() == Some(sha)).cloned().collect()
}

/// ASP branch name for a git ref (only `feature-1` is deduped, to `feature-1-2`).
fn asp_name(ref_name: &str) -> String {
    if ref_name == "feature-1" { "feature-1-2".into() } else { ref_name.into() }
}

const LIVE_BRANCHES: &[&str] = &["feat/simple", "feature-1-2", "nested/deep", "orphan", "with-merge"];

// ── 1. checkbox clone ground truth ───────────────────────────────────────────

#[test]
fn checkbox_clone_ground_truth_per_branch() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = open_branches();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let head = rev_parse(&repo.bare, "HEAD");

    let tmp = tempfile::tempdir().unwrap();
    let engine = open_engine(tmp.path(), 1);
    let report = block(clone_from_git(&engine, &https(&url, GitAuth::Anonymous), &all_branches_opts())).expect("clone");

    // 5 live open branches imported; stale-pointer (ancestor of main) skipped.
    assert_eq!(report.open_branches, 5, "5 live open branches");
    assert_eq!(report.refs_skipped, 1, "stale-pointer skipped");

    // The engine's live branch list == the expected set (minus main).
    let mut live: Vec<String> =
        engine.branches().unwrap().into_iter().map(|b| b.name).filter(|n| n != "main").collect();
    live.sort();
    let mut expect: Vec<String> = LIVE_BRANCHES.iter().map(|s| s.to_string()).collect();
    expect.sort();
    assert_eq!(live, expect, "live imported branches");

    // main folds to HEAD's tree.
    assert_eq!(fold_branch(&engine, MAIN_BRANCH_ID), tree_content(&repo.bare, &head), "main fold");

    // Every live branch folds to its git tip tree.
    for ref_name in ["feat/simple", "feature-1", "nested/deep", "orphan", "with-merge"] {
        let tip = rev_parse(&repo.bare, ref_name);
        let asp = asp_name(ref_name);
        let bid = branch_id_named(&engine, &asp).unwrap_or_else(|| panic!("branch {asp}"));
        assert_eq!(
            fold_branch(&engine, &bid),
            tree_content(&repo.bare, &tip),
            "fold({asp}) != git ls-tree -r {tip} ({ref_name})"
        );
    }

    // stale-pointer was skipped → no branch by that name.
    assert!(branch_id_named(&engine, "stale-pointer").is_none(), "stale-pointer not imported");

    // git_remotes still tracks only the default branch.
    let row = engine.store.git_remote_get(&remote_id(&url)).unwrap().expect("remote row");
    assert_eq!(row.default_branch.as_deref(), Some("main"));
    assert_eq!(row.last_ingested_sha.as_deref(), Some(head.as_str()));
    assert_eq!(row.remote_ref.as_deref(), Some("refs/heads/main"));
}

// ── 2. plain clone unchanged + prefix-set property ───────────────────────────

#[test]
fn plain_clone_unchanged_and_is_prefix_set() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = open_branches();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let spec = https(&url, GitAuth::Anonymous);

    // Plain clone (all_branches=false) → today's behavior: no live extra branches.
    let tp = tempfile::tempdir().unwrap();
    let plain = open_engine(tp.path(), 2);
    let rp = block(clone_from_git(&plain, &spec, &CloneOptions::default())).expect("plain clone");
    assert_eq!(rp.open_branches, 0);
    assert_eq!(rp.refs_skipped, 0);
    let plain_live: Vec<String> =
        plain.branches().unwrap().into_iter().map(|b| b.name).filter(|n| n != "main").collect();
    for b in LIVE_BRANCHES {
        assert!(!plain_live.contains(&b.to_string()), "plain clone must not import {b}");
    }

    // Checkbox clone.
    let tc = tempfile::tempdir().unwrap();
    let cb = open_engine(tc.path(), 3);
    block(clone_from_git(&cb, &spec, &all_branches_opts())).expect("checkbox clone");

    // Every plain-clone row id (except the trailing `.aspignore`, whose seq shifts —
    // §2 pinned exception) exists in the checkbox clone: phase-1 dedup at row level.
    let cb_ids: BTreeSet<String> = cb.store.all_rows().unwrap().into_iter().map(|r| r.id).collect();
    for r in plain.store.all_rows().unwrap() {
        if r.path.as_deref() == Some(".aspignore") {
            continue;
        }
        assert!(cb_ids.contains(&r.id), "checkbox clone missing plain row {:?} ({:?})", r.kind, r.path);
    }
    assert_eq!(plain.vault_id(), cb.vault_id(), "same repo → same vault id");
}

// ── 3. THE §4 merge-after-import pull ────────────────────────────────────────

#[test]
fn merge_after_import_folds_onto_existing_branch() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = open_branches();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let rid = remote_id(&url);

    let tmp = tempfile::tempdir().unwrap();
    let engine = open_engine(tmp.path(), 4);
    block(clone_from_git(&engine, &https(&url, GitAuth::Anonymous), &all_branches_opts())).expect("clone");

    // feat/simple's pre-clone commits + branch id (captured before it is tombstoned).
    let s1 = rev_parse(&repo.bare, "feat/simple~1");
    let s2 = rev_parse(&repo.bare, "feat/simple");
    let simple_bid = branch_id_named(&engine, "feat/simple").expect("feat/simple imported");
    let merges_before = engine
        .store
        .all_rows()
        .unwrap()
        .iter()
        .filter(|r| r.kind == Kind::Merge && r.branch_id == MAIN_BRANCH_ID)
        .count();

    // Merge feat/simple upstream WITH a post-clone commit (must import as delta).
    let new_main = merge_branch_upstream(&repo.bare, "feat/simple", true);
    let s3 = rev_parse(&repo.bare, &format!("{new_main}^2")); // the extra pre-merge commit

    let r = block(pull_once(&engine, &rid, None)).expect("pull");
    assert!(matches!(r, PullReport::Updated { .. }), "expected Updated, got {r:?}");

    let rows = engine.store.all_rows().unwrap();

    // Exactly ONE new merge row on main, and its merge_parent is s3's marker row — the
    // post-clone commit chained onto the imported branch lane, whose tip the merge cites.
    let merges_after: Vec<&LogRow> =
        rows.iter().filter(|r| r.kind == Kind::Merge && r.branch_id == MAIN_BRANCH_ID).collect();
    assert_eq!(merges_after.len(), merges_before + 1, "exactly one new merge node on main");

    let s3_marker = markers_for(&rows, &s3);
    assert_eq!(s3_marker.len(), 1, "the extra commit has one marker");
    assert_eq!(s3_marker[0].branch_id, simple_bid, "the extra commit chains onto the EXISTING feat/simple lane");
    let new_merge = merges_after
        .iter()
        .find(|m| m.merge_parent.as_deref() == Some(s3_marker[0].id.as_str()))
        .expect("new merge cites the extended feat/simple tip");
    let _ = new_merge;

    // No duplicate rows for feat/simple's pre-clone commits.
    assert_eq!(markers_for(&rows, &s1).len(), 1, "s1 imported exactly once");
    assert_eq!(markers_for(&rows, &s2).len(), 1, "s2 imported exactly once");

    // feat/simple is now tombstoned (delete-after-merge).
    assert!(branch_id_named(&engine, "feat/simple").is_none(), "feat/simple no longer live");
    assert!(
        engine.store.branches().unwrap().iter().any(|b| b.branch_id == simple_bid && b.deleted),
        "feat/simple has a delete tombstone"
    );

    // fold(main) == the new upstream tip tree.
    assert_eq!(fold_branch(&engine, MAIN_BRANCH_ID), tree_content(&repo.bare, &new_main), "fold(main) == new tip");

    // A second pull is a no-op.
    assert_eq!(block(pull_once(&engine, &rid, None)).unwrap(), PullReport::UpToDate);
}

// ── 4. determinism: two checkbox clones converge ─────────────────────────────

#[test]
fn two_checkbox_clones_converge() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = open_branches();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let spec = https(&url, GitAuth::Anonymous);

    let ta = tempfile::tempdir().unwrap();
    let tb = tempfile::tempdir().unwrap();
    let a = open_engine(ta.path(), 5);
    let b = open_engine(tb.path(), 6);
    let ra = block(clone_from_git(&a, &spec, &all_branches_opts())).expect("clone a");
    let rb = block(clone_from_git(&b, &spec, &all_branches_opts())).expect("clone b");

    assert_eq!(ra.vault_id, rb.vault_id, "same repo → same vault id");

    let ids = |e: &Engine| -> BTreeSet<String> { e.store.all_rows().unwrap().into_iter().map(|r| r.id).collect() };
    assert_eq!(ids(&a), ids(&b), "byte-identical row ids across two checkbox clones");

    let bids = |e: &Engine| -> BTreeSet<String> {
        e.branches().unwrap().into_iter().map(|b| b.branch_id).collect()
    };
    assert_eq!(bids(&a), bids(&b), "identical branch ids");
}

// ── 5. CLI surface: real binary ──────────────────────────────────────────────
//
// The hermetic fixture server is plain `http://` (no TLS), and the CLI front door
// deliberately rejects `http://` git URLs (`parse_git_url`: https/ssh only), so a live
// CLI clone over the fixture isn't possible. Instead: (a) library checkbox-clone into a
// dir, then drive the REAL `asp branch list` binary over that vault — the §5 surface
// ("`asp branch list` shows the live imported branches"); (b) assert the real binary
// exposes the `--all-branches` clone flag. The clone routing + report print are covered
// by the library tests above and `git_clone_cmd`.

#[test]
fn cli_branch_list_shows_live_open_branches() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = open_branches();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let dir = tmp.path().join("vault");
    std::fs::create_dir_all(&home).unwrap();

    // Checkbox-clone via the library (works over http), then drop the engine so the CLI
    // can open the same on-disk vault.
    {
        let engine = open_engine(&dir, 9);
        block(clone_from_git(&engine, &https(&url, GitAuth::Anonymous), &all_branches_opts())).expect("clone");
    }

    let run = |args: &[&str]| -> (bool, String, String) {
        let out = Command::new(asp_e2e::asp_bin())
            .env("ASP_HOME", &home)
            .env("ASP_GIT_DISABLE_KEYRING", "1")
            .env("ASP_NO_RELAY", "1")
            .env("ASP_LOG", "warn")
            .arg("--dir")
            .arg(&dir)
            .args(args)
            .output()
            .expect("spawn asp");
        (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string(), String::from_utf8_lossy(&out.stderr).to_string())
    };

    // The real `asp branch list` binary shows every live imported open branch (§5).
    let (ok, list, stderr) = run(&["branch", "list"]);
    assert!(ok, "branch list failed: {stderr}");
    for b in LIVE_BRANCHES {
        assert!(list.contains(b), "branch list shows {b}:\n{list}");
    }
    assert!(!list.contains("stale-pointer"), "skipped ref must not appear as a branch");

    // The real binary exposes the `--all-branches` clone flag.
    let (_ok, help, _e) = run(&["clone", "--help"]);
    assert!(help.contains("--all-branches"), "clone --help advertises --all-branches:\n{help}");
}

// ── 6. LCG fuzz: random open-branch repos through the driver ──────────────────

#[test]
fn fuzz_random_open_branch_repos_through_driver() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let mut state: u64 = 0xD1CE_5EED_A5F0_0D01;
    let mut next = move || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 33) as u32
    };

    for trial in 0..2u32 {
        let mut r = FixtureRepo::init(&format!("obdrv{trial}"));
        r.commit_file("base.txt", "base\n", "base");
        let mut branches: Vec<String> = vec!["main".into()];
        for step in 0..10 {
            match next() % 4 {
                0 => {
                    let at = branches[(next() as usize) % branches.len()].clone();
                    let name = format!("feat-{trial}-{step}");
                    r.checkout(&at);
                    r.checkout_new(&name, None);
                    branches.push(name);
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
                    r.checkout(&br);
                    r.commit_file(&format!("{br}.txt"), &format!("v{step}\n"), &format!("{br} {step}"));
                }
            }
        }
        // Merge one branch into main so some ref becomes reachable → skipped.
        r.checkout("main");
        if let Some(b) = branches.iter().find(|b| b.as_str() != "main").cloned() {
            r.merge(&b, &format!("Merge branch '{b}'"), true);
        }
        let repo = r.finish();

        let server = GitHttpServer::spawn(repo.repo_root());
        let url = server.repo_url(repo.name());
        let tmp = tempfile::tempdir().unwrap();
        let engine = open_engine(tmp.path(), 100 + trial as u8);
        let report = block(clone_from_git(&engine, &https(&url, GitAuth::Anonymous), &all_branches_opts()))
            .unwrap_or_else(|e| panic!("[obdrv{trial}] clone: {e}"));
        let _ = report;

        // Every live imported branch whose name still resolves to a git ref folds to
        // its git tip tree (a `-N`-deduped name won't resolve — skip it; the model-level
        // fuzz already covers deduped shapes). The load-bearing property here is that the
        // driver never panics on random open-branch topologies.
        for b in engine.branches().unwrap().into_iter().filter(|b| b.name != "main") {
            let Some(tip) = rev_parse_opt(&repo.bare, &b.name) else { continue };
            assert_eq!(
                fold_branch(&engine, &b.branch_id),
                tree_content(&repo.bare, &tip),
                "[obdrv{trial}] fold({}) != tip tree",
                b.name
            );
        }
    }
}
