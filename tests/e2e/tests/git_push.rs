//! End-to-end tests for the native git-bridge **push** slice (`asp_core::gitpush`,
//! git-bridge §5, M4): deterministic commit synthesis, pack assembly, and the push
//! driver, verified against the hermetic smart-HTTP fixture server (its bare mirrors
//! have `http.receivepack=true`) with system `git` inspecting the result.
//!
//! Everything drives real git wire bytes end-to-end, so a push here is byte-for-byte
//! what would go to GitHub. Tests skip gracefully when system `git` is absent.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use asp_core::gitbridge::{remote_id, GitAuth, GitObjectKind, GitRemoteSpec, RemoteStore};
use asp_core::gitpush::{author_plan, pending_git_diff, plans_in_order, push, synthesize_commits, ModeTable, PushReport};
use asp_core::gitremote::{clone_from_git, CloneOptions};
use asp_core::gitwire::GitUrl;
use asp_core::identity::Identity;
use asp_core::store::BlobStore;
use asp_core::Engine;
use asp_e2e::gitfix::{advance_tip, linear_basic, modes_and_symlinks, GitHttpServer};

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

fn git(bare: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("--git-dir").arg(bare).args(args).output().expect("git");
    assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn rev_parse(bare: &Path, spec: &str) -> String {
    git(bare, &["rev-parse", spec])
}

/// `git rev-list --parents -n1 <sha>` → parent shas.
fn parents(bare: &Path, sha: &str) -> Vec<String> {
    git(bare, &["rev-list", "--parents", "-n", "1", sha])
        .split_whitespace()
        .skip(1)
        .map(str::to_string)
        .collect()
}

fn is_ancestor(bare: &Path, ancestor: &str, descendant: &str) -> bool {
    Command::new("git")
        .arg("--git-dir")
        .arg(bare)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .status()
        .expect("git merge-base")
        .success()
}

fn commit_message(bare: &Path, sha: &str) -> String {
    git(bare, &["log", "-1", "--format=%B", sha]).trim().to_string()
}

/// `git ls-tree -r <sha>` → `path -> (mode, oid)` (blobs + gitlinks).
fn ls_tree(bare: &Path, sha: &str) -> BTreeMap<String, (String, String)> {
    let text = git(bare, &["ls-tree", "-r", sha]);
    let mut m = BTreeMap::new();
    for line in text.lines() {
        let (meta, path) = line.split_once('\t').expect("ls-tree line");
        let mut parts = meta.split_whitespace();
        let mode = parts.next().unwrap().to_string();
        let _typ = parts.next().unwrap();
        let oid = parts.next().unwrap().to_string();
        m.insert(path.to_string(), (mode, oid));
    }
    m
}

/// `git ls-tree -r <sha>` → `path -> content bytes` (blobs only).
fn tree_content(bare: &Path, sha: &str) -> BTreeMap<String, Vec<u8>> {
    let mut map = BTreeMap::new();
    for (path, (_mode, oid)) in ls_tree(bare, sha) {
        let bytes = Command::new("git").arg("--git-dir").arg(bare).args(["cat-file", "blob", &oid]).output().expect("cat-file").stdout;
        map.insert(path, bytes);
    }
    map
}

fn blob_at(bare: &Path, sha: &str, path: &str) -> Vec<u8> {
    Command::new("git").arg("--git-dir").arg(bare).args(["cat-file", "blob", &format!("{sha}:{path}")]).output().expect("cat-file").stdout
}

/// The engine's fold of `main` as `path -> bytes`, minus the clone-seeded `.aspignore`
/// (ASP-local, never pushed — git-bridge §3.3), so it compares against a remote tree.
fn fold_main(engine: &Engine) -> BTreeMap<String, Vec<u8>> {
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

fn clone_into(dir: &Path, seed: u8, url: &str) -> Engine {
    let engine = open_engine(dir, seed);
    block(clone_from_git(&engine, &https(url, GitAuth::Anonymous), &CloneOptions::default())).expect("clone");
    engine
}

// ── round-trip: clone → edit → plan → push (twice) ───────────────────────────

#[test]
fn round_trip_edit_and_push_linear() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = linear_basic();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let old_tip = rev_parse(&repo.bare, "main");

    let tmp = tempfile::tempdir().unwrap();
    let engine = clone_into(tmp.path(), 1, &url);
    let rid = remote_id(&url);

    // Edit an existing file, author a plan, push.
    engine.record_write("a2.txt", b"alpha\nalpha2\nalpha3\nedited\n").unwrap();
    author_plan(&engine, &rid, "vault edit one", Some("Tester <t@x>")).unwrap();
    let report = block(push(&engine, &rid, |_| {})).expect("push");
    let tip1 = match report {
        PushReport::Pushed { pushed_sha, commits_pushed, plans_pushed } => {
            assert_eq!(commits_pushed, 1);
            assert_eq!(plans_pushed, 1);
            pushed_sha
        }
        other => panic!("expected Pushed, got {other:?}"),
    };

    // Remote advanced to our commit, a child of the old tip, with our message + content.
    assert_eq!(rev_parse(&repo.bare, "main"), tip1);
    assert_eq!(parents(&repo.bare, &tip1), vec![old_tip.clone()], "new commit is a child of the old tip");
    assert!(is_ancestor(&repo.bare, &old_tip, &tip1));
    assert_eq!(commit_message(&repo.bare, &tip1), "vault edit one");
    assert_eq!(blob_at(&repo.bare, &tip1, "a2.txt"), b"alpha\nalpha2\nalpha3\nedited\n");
    // Ordinary file mode preserved.
    assert_eq!(ls_tree(&repo.bare, &tip1).get("a2.txt").unwrap().0, "100644");
    // The clone-seeded root `.aspignore` is ASP-local and never pushed (git-bridge §3.3).
    assert!(!ls_tree(&repo.bare, &tip1).contains_key(".aspignore"), "clone-seeded .aspignore is never pushed");

    // A second edit + push → linear history (two new commits over the old tip).
    engine.record_write("dir/c.txt", b"charlie edited\n").unwrap();
    author_plan(&engine, &rid, "vault edit two", None).unwrap();
    let tip2 = match block(push(&engine, &rid, |_| {})).expect("push2") {
        PushReport::Pushed { pushed_sha, .. } => pushed_sha,
        other => panic!("expected Pushed, got {other:?}"),
    };
    assert_eq!(rev_parse(&repo.bare, "main"), tip2);
    assert_eq!(parents(&repo.bare, &tip2), vec![tip1.clone()], "linear: tip2 child of tip1");
    // Exactly two commits from old tip to tip2.
    let count = git(&repo.bare, &["rev-list", "--count", &format!("{old_tip}..{tip2}")]);
    assert_eq!(count, "2", "linear history of two synthesized commits");
    assert_eq!(blob_at(&repo.bare, &tip2, "dir/c.txt"), b"charlie edited\n");
}

// ── R4: executable bit + symlink fidelity across a vault edit ─────────────────

#[test]
fn modes_and_symlinks_survive_push() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = modes_and_symlinks();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());

    let tmp = tempfile::tempdir().unwrap();
    let engine = clone_into(tmp.path(), 2, &url);
    let rid = remote_id(&url);

    // Edit the executable file's content → pushed tree keeps 100755.
    engine.record_write("script.sh", b"#!/bin/sh\necho changed\n").unwrap();
    author_plan(&engine, &rid, "edit script content", None).unwrap();
    let tip1 = match block(push(&engine, &rid, |_| {})).expect("push") {
        PushReport::Pushed { pushed_sha, .. } => pushed_sha,
        other => panic!("expected Pushed, got {other:?}"),
    };
    let t1 = ls_tree(&repo.bare, &tip1);
    assert_eq!(t1.get("script.sh").unwrap().0, "100755", "executable bit preserved after a content edit");
    assert_eq!(blob_at(&repo.bare, &tip1, "script.sh"), b"#!/bin/sh\necho changed\n");
    // The symlink is untouched by this push and stays a symlink.
    assert_eq!(t1.get("link").unwrap().0, "120000");

    // Edit the symlink-backed file's content (retarget) → still 120000 with the new
    // target text, consulting the ledger, not the materialized form (git-bridge R4).
    engine.record_write("link", b"targetA.txt").unwrap();
    author_plan(&engine, &rid, "retarget symlink", None).unwrap();
    let tip2 = match block(push(&engine, &rid, |_| {})).expect("push2") {
        PushReport::Pushed { pushed_sha, .. } => pushed_sha,
        other => panic!("expected Pushed, got {other:?}"),
    };
    let t2 = ls_tree(&repo.bare, &tip2);
    assert_eq!(t2.get("link").unwrap().0, "120000", "symlink mode preserved across a vault edit");
    assert_eq!(blob_at(&repo.bare, &tip2, "link"), b"targetA.txt", "symlink target text updated");
    assert_eq!(t2.get("script.sh").unwrap().0, "100755", "still executable");
}

// ── determinism: two engines with the same rows synth identical commits ───────

#[test]
fn synthesis_is_deterministic_across_engines() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = linear_basic();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let rid = remote_id(&url);

    // Two independent clones (byte-identical genesis, proven elsewhere).
    let ta = tempfile::tempdir().unwrap();
    let tb = tempfile::tempdir().unwrap();
    let a = clone_into(ta.path(), 3, &url);
    let b = clone_into(tb.path(), 4, &url);

    // A makes a local edit + a plan; those two rows are synced to B verbatim, so both
    // engines hold an identical row set (the property synthesis determinism needs).
    let edit = a.record_write("a2.txt", b"alpha\nalpha2\ndet\n").unwrap().unwrap();
    let plan_row = author_plan(&a, &rid, "deterministic plan", Some("Det <d@x>")).unwrap();
    b.integrate(&edit).unwrap();
    b.integrate(&a.wire(plan_row).unwrap()).unwrap();

    let synth = |e: &Engine, dir: &Path| {
        let store = RemoteStore::open(&e.asp_dir, &rid).unwrap();
        let row = e.store.git_remote_get(&rid).unwrap().unwrap();
        let plans = plans_in_order(e).unwrap();
        let modes = ModeTable::load(e).unwrap();
        let _ = dir;
        synthesize_commits(e, &store, &row, &plans, &modes).unwrap()
    };
    let sa = synth(&a, ta.path());
    let sb = synth(&b, tb.path());

    assert_eq!(sa.tip_sha, sb.tip_sha, "identical synthesized tip sha");
    assert!(!sa.tip_sha.is_empty());
    let oids = |s: &asp_core::gitpush::SynthOutput| -> BTreeSet<String> {
        s.objects_to_push
            .iter()
            .map(|(k, c)| asp_core::gitbridge::git_oid(*k, c))
            .collect()
    };
    assert_eq!(oids(&sa), oids(&sb), "identical object oid set");
    // Sanity: the object set contains the tip commit.
    assert!(oids(&sa).contains(&sa.tip_sha));
    let _ = GitObjectKind::Commit;
}

// ── idempotent race: second push sees the ref already at our tip ──────────────

#[test]
fn idempotent_race_second_push_is_noop() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = linear_basic();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let rid = remote_id(&url);

    // A and B both clone the T0 tip.
    let ta = tempfile::tempdir().unwrap();
    let tb = tempfile::tempdir().unwrap();
    let a = clone_into(ta.path(), 5, &url);
    let b = clone_into(tb.path(), 6, &url);

    // A edits, plans, pushes → ref advances to T1.
    let edit = a.record_write("a2.txt", b"alpha\nalpha2\nrace\n").unwrap().unwrap();
    let plan_row = author_plan(&a, &rid, "race plan", Some("R <r@x>")).unwrap();
    let tip = match block(push(&a, &rid, |_| {})).expect("push a") {
        PushReport::Pushed { pushed_sha, .. } => pushed_sha,
        other => panic!("{other:?}"),
    };
    assert_eq!(rev_parse(&repo.bare, "main"), tip);

    // B receives the same rows (still thinks base is T0) and pushes the same tip: the
    // remote is already at T1, so the non-FF is recognized as an idempotent success.
    b.integrate(&edit).unwrap();
    b.integrate(&a.wire(plan_row).unwrap()).unwrap();
    let report = block(push(&b, &rid, |_| {})).expect("push b");
    match report {
        PushReport::Pushed { pushed_sha, .. } => assert_eq!(pushed_sha, tip, "same tip, no-op success"),
        other => panic!("expected idempotent Pushed, got {other:?}"),
    }
    assert_eq!(rev_parse(&repo.bare, "main"), tip, "ref unchanged by the second push");
    // B's cursor is stable at the tip.
    assert_eq!(b.store.git_remote_get(&rid).unwrap().unwrap().last_pushed_sha.as_deref(), Some(tip.as_str()));
}

// ── non-FF recovery: a human pushed mid-cycle → pull → re-synthesize → retry ──

#[test]
fn non_fast_forward_recovers_by_pull_and_resynthesize() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = linear_basic();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let rid = remote_id(&url);

    let tmp = tempfile::tempdir().unwrap();
    let engine = clone_into(tmp.path(), 7, &url);
    let old_tip = rev_parse(&repo.bare, "main");

    // Someone pushes upstream between our clone and our push.
    let upstream = advance_tip(&repo.bare, "human.txt", "from a human\n", "human upstream commit");
    assert_ne!(upstream, old_tip);

    // Our push: base is stale (T0) → non-FF → pull ingests the human commit →
    // re-synthesize onto the new tip → succeeds.
    engine.record_write("a2.txt", b"alpha\nalpha2\nours\n").unwrap();
    author_plan(&engine, &rid, "our concurrent edit", Some("Us <u@x>")).unwrap();
    let tip = match block(push(&engine, &rid, |_| {})).expect("push recovers") {
        PushReport::Pushed { pushed_sha, .. } => pushed_sha,
        other => panic!("expected Pushed after recovery, got {other:?}"),
    };

    assert_eq!(rev_parse(&repo.bare, "main"), tip);
    // Linear: T0 <- human <- ours. Both the human commit and our edit are present.
    assert!(is_ancestor(&repo.bare, &upstream, &tip), "our commit builds on the human's");
    assert_eq!(parents(&repo.bare, &tip), vec![upstream.clone()]);
    assert_eq!(blob_at(&repo.bare, &tip, "human.txt"), b"from a human\n", "upstream change retained");
    assert_eq!(blob_at(&repo.bare, &tip, "a2.txt"), b"alpha\nalpha2\nours\n", "our edit present");
}

// ── pending diff ─────────────────────────────────────────────────────────────

#[test]
fn pending_diff_reports_changed_files() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = linear_basic();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let rid = remote_id(&url);

    let tmp = tempfile::tempdir().unwrap();
    let engine = clone_into(tmp.path(), 8, &url);

    // A baseline plan captures the clone state (incl. the seeded .aspignore), so the
    // diff below reflects exactly the two new edits.
    author_plan(&engine, &rid, "baseline", None).unwrap();
    engine.record_write("new1.txt", b"one\n").unwrap();
    engine.record_write("new2.txt", b"two\n").unwrap();

    let pd = pending_git_diff(&engine, &rid).unwrap();
    assert_eq!(pd.files_changed, 2, "two changed files: {:?}", pd.paths);
    assert!(pd.paths.contains(&"new1.txt".to_string()));
    assert!(pd.paths.contains(&"new2.txt".to_string()));
    assert!(pd.unified.contains("new1.txt") && pd.unified.contains("one"), "non-empty unified diff");
}

// ── LCG fuzz: random (edit, plan) then synthesize; tree round-trips ───────────

#[test]
fn fuzz_edit_plan_then_push_round_trips() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = linear_basic();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let rid = remote_id(&url);

    let tmp = tempfile::tempdir().unwrap();
    let engine = clone_into(tmp.path(), 9, &url);

    // Deterministic LCG (no rng dep, matching the repo's fuzz style).
    let mut state: u64 = 0xA5A5_1234_DEAD_0001;
    let mut next = || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        state >> 33
    };
    let paths = ["a2.txt", "dir/c.txt", "f1.txt", "nested/deep/g.txt"];

    for _ in 0..18u32 {
        if next() % 3 == 0 {
            // Author a plan (a commit boundary).
            author_plan(&engine, &rid, &format!("fuzz plan {}", next() % 1000), None).unwrap();
        } else {
            let p = paths[(next() as usize) % paths.len()];
            let content = format!("content-{}-{}\n", next() % 97, next() % 97);
            engine.record_write(p, content.as_bytes()).unwrap();
        }
    }
    // A final plan so the tip commit captures the full current main state.
    author_plan(&engine, &rid, "fuzz final", None).unwrap();

    let report = block(push(&engine, &rid, |_| {})).expect("fuzz push");
    let tip = match report {
        PushReport::Pushed { pushed_sha, .. } => pushed_sha,
        PushReport::Nothing => return, // no net change is still valid
    };
    // The synthesized tip is a valid commit whose tree folds back to the current main
    // state. The clone-seeded root `.aspignore` is ASP-local and never pushed (§3.3).
    let remote_tree = tree_content(&repo.bare, &tip);
    assert!(!remote_tree.contains_key(".aspignore"), "clone-seeded .aspignore is never pushed (§3.3)");
    assert_eq!(remote_tree, fold_main(&engine), "synthesized tip tree == fold(main)");
}
