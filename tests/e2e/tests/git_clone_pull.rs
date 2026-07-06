//! End-to-end tests for the native git-bridge **orchestration** (`asp_core::gitremote`,
//! git-bridge §3/§4/§8): `clone_from_git` / `pull_once` / `rebaseline` / `git_status`
//! driving a real on-disk [`Engine`] against the hermetic smart-HTTP fixture server.
//!
//! Everything drives real git wire bytes (the CGI shim CGI-execs `git http-backend`),
//! so clone/pull speak protocol v2 exactly as they would to GitHub. Tests skip
//! gracefully when system `git` is absent.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use asp_core::gitbridge::{remote_id, GitAuth, GitRemoteSpec};
use asp_core::gitremote::{clone_from_git, git_status, pull_once, rebaseline, CloneOptions, PullReport};
use asp_core::gitwire::GitUrl;
use asp_core::identity::Identity;
use asp_core::store::BlobStore;
use asp_core::{Engine, SessionVault};
use asp_e2e::gitfix::{advance_tip, force_rewrite_tip, linear_basic, merged_prs, GitHttpServer};

// ── harness ─────────────────────────────────────────────────────────────────

fn git_available() -> bool {
    Command::new("git")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

/// Never touch the OS keychain in tests (git-bridge §8 guard).
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

/// `git ls-tree -r <sha>` on the bare → `path -> content bytes` (blobs only).
fn tree_content(bare: &Path, sha: &str) -> BTreeMap<String, Vec<u8>> {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(bare)
        .args(["ls-tree", "-r", sha])
        .output()
        .expect("ls-tree");
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
        let bytes = Command::new("git")
            .arg("--git-dir")
            .arg(bare)
            .args(["cat-file", "blob", oid])
            .output()
            .expect("cat-file")
            .stdout;
        map.insert(path.to_string(), bytes);
    }
    map
}

/// The engine's fold of the checked-out branch (`main`) as `path -> bytes`, minus the
/// clone-seeded `.aspignore`.
fn fold_main(engine: &Engine) -> BTreeMap<String, Vec<u8>> {
    let mut m = BTreeMap::new();
    for f in engine.store.live_files().expect("live_files") {
        if f.deleted || f.path == ".aspignore" {
            continue;
        }
        if let Some(h) = &f.result_hash {
            let bytes = engine.store.get_blob(h).unwrap().unwrap_or_default();
            m.insert(f.path.clone(), bytes);
        }
    }
    m
}

fn rev_parse(bare: &Path, spec: &str) -> String {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(bare)
        .args(["rev-parse", spec])
        .output()
        .expect("rev-parse");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// ── clone: linear_basic ──────────────────────────────────────────────────────

#[test]
fn clone_linear_basic_folds_to_tip_and_persists_config() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = linear_basic();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let tip = rev_parse(&repo.bare, "HEAD");

    let tmp = tempfile::tempdir().unwrap();
    let engine = open_engine(tmp.path(), 1);
    let spec = https(&url, GitAuth::Anonymous);
    let report = block(clone_from_git(&engine, &spec, &CloneOptions::default())).expect("clone");

    // fold(main) == git ls-tree -r tip.
    assert_eq!(fold_main(&engine), tree_content(&repo.bare, &tip), "fold(main) == tip tree");
    assert_eq!(report.tip_sha, tip);
    assert!(report.branches.is_empty(), "linear repo has no side branches");

    // vault_id is the repo-derived one.
    assert_eq!(engine.vault_id(), report.vault_id);
    assert_eq!(report.vault_id, asp_core::gitgenesis::git_vault_id(&rev_parse(&repo.bare, "HEAD~4")));

    // git_remotes row persisted + .aspignore on disk.
    let rid = remote_id(&url);
    let row = engine.store.git_remote_get(&rid).unwrap().expect("remote row");
    assert_eq!(row.last_ingested_sha.as_deref(), Some(tip.as_str()));
    assert_eq!(row.default_branch.as_deref(), Some("main"));
    assert!(tmp.path().join(".aspignore").exists(), ".aspignore materialized to disk");
}

// ── clone: merged_prs → pull an upstream advance ─────────────────────────────

#[test]
fn clone_merged_prs_then_pull_advance() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = merged_prs();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());

    let tmp = tempfile::tempdir().unwrap();
    let engine = open_engine(tmp.path(), 2);
    let spec = https(&url, GitAuth::Anonymous);
    let report = block(clone_from_git(&engine, &spec, &CloneOptions::default())).expect("clone");

    // Side branches present in the report.
    assert!(report.branches.iter().any(|b| b.contains("feature-1")), "feature-1 branch: {:?}", report.branches);
    assert!(report.branches.iter().any(|b| b.contains("feature-2")), "feature-2 branch: {:?}", report.branches);

    // Simulate an upstream advance, then pull.
    let new_tip = advance_tip(&repo.bare, "advance.txt", "hi from upstream\n", "upstream advance");
    let rid = remote_id(&url);
    let r = block(pull_once(&engine, &rid, None)).expect("pull");
    match r {
        PullReport::Updated { new_commits, .. } => assert!(new_commits >= 1, "at least one new commit"),
        other => panic!("expected Updated, got {other:?}"),
    }
    assert_eq!(fold_main(&engine), tree_content(&repo.bare, &new_tip), "fold == new tip tree");
    assert_eq!(
        engine.store.git_remote_get(&rid).unwrap().unwrap().last_ingested_sha.as_deref(),
        Some(new_tip.as_str())
    );

    // A GitIngest row for the new commit exists.
    let ingest_present = engine
        .store
        .all_rows()
        .unwrap()
        .iter()
        .any(|row| row.kind == asp_core::log::Kind::GitIngest && row.path.as_deref() == Some(new_tip.as_str()));
    assert!(ingest_present, "GitIngest ledger row for the advance");

    // Second pull is a no-op (up to date).
    assert_eq!(block(pull_once(&engine, &rid, None)).unwrap(), PullReport::UpToDate);
}

// ── determinism: two independent clones converge ─────────────────────────────

#[test]
fn two_independent_clones_converge() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = merged_prs();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let spec = https(&url, GitAuth::Anonymous);

    let ta = tempfile::tempdir().unwrap();
    let tb = tempfile::tempdir().unwrap();
    let a = open_engine(ta.path(), 3);
    let b = open_engine(tb.path(), 4);
    let ra = block(clone_from_git(&a, &spec, &CloneOptions::default())).expect("clone a");
    let rb = block(clone_from_git(&b, &spec, &CloneOptions::default())).expect("clone b");

    assert_eq!(ra.vault_id, rb.vault_id, "same repo → same vault id");

    // Identical set of row ids across the two independent clones (§3.2 convergence).
    let ids = |e: &Engine| -> std::collections::BTreeSet<String> {
        e.store.all_rows().unwrap().into_iter().map(|r| r.id).collect()
    };
    assert_eq!(ids(&a), ids(&b), "byte-identical genesis rows on both nodes");
}

// ── force-push freeze + rebaseline ───────────────────────────────────────────

#[test]
fn force_push_freezes_then_rebaseline_recovers() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = linear_basic();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());

    let tmp = tempfile::tempdir().unwrap();
    let engine = open_engine(tmp.path(), 5);
    let spec = https(&url, GitAuth::Anonymous);
    block(clone_from_git(&engine, &spec, &CloneOptions::default())).expect("clone");
    let rid = remote_id(&url);

    // Rewrite upstream history → pull must freeze (git-bridge §4.4).
    let new_tip = force_rewrite_tip(&repo.bare);
    assert_eq!(block(pull_once(&engine, &rid, None)).unwrap(), PullReport::Frozen);
    assert!(engine.store.git_remote_get(&rid).unwrap().unwrap().frozen, "remote frozen");
    // A frozen remote refuses further pulls.
    assert_eq!(block(pull_once(&engine, &rid, None)).unwrap(), PullReport::Frozen);
    assert!(git_status(&engine, &rid).unwrap().frozen);

    // Rebaseline recovers: unfrozen + fold matches the rewritten tip.
    match block(rebaseline(&engine, &rid)).expect("rebaseline") {
        PullReport::Updated { .. } => {}
        other => panic!("expected Updated, got {other:?}"),
    }
    assert!(!engine.store.git_remote_get(&rid).unwrap().unwrap().frozen, "unfrozen after rebaseline");
    assert_eq!(fold_main(&engine), tree_content(&repo.bare, &new_tip), "fold == rewritten tip tree");
}

// ── auth: token gate ─────────────────────────────────────────────────────────

#[test]
fn token_auth_gate() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = linear_basic();
    let server = GitHttpServer::spawn_with_token(repo.repo_root(), "s3cr3t-pat");
    let url = server.repo_url(repo.name());

    // Anonymous (no token) → typed auth error, no vault.
    let ta = tempfile::tempdir().unwrap();
    let anon = open_engine(ta.path(), 6);
    let err = block(clone_from_git(&anon, &https(&url, GitAuth::Anonymous), &CloneOptions::default()))
        .expect_err("anonymous clone must fail");
    assert!(
        format!("{err}").to_lowercase().contains("credential") || format!("{err}").contains("401") || format!("{err}").contains("403"),
        "auth error surfaced: {err}"
    );

    // With the right token → success.
    let tb = tempfile::tempdir().unwrap();
    let authed = open_engine(tb.path(), 7);
    let report = block(clone_from_git(
        &authed,
        &https(&url, GitAuth::Token("s3cr3t-pat".into())),
        &CloneOptions::default(),
    ))
    .expect("token clone succeeds");
    assert!(report.commits >= 1);
}

// ── depth: shallow-ish clone still folds to the tip tree ─────────────────────

#[test]
fn depth_clone_folds_to_tip() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = linear_basic();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let tip = rev_parse(&repo.bare, "HEAD");

    let tmp = tempfile::tempdir().unwrap();
    let engine = open_engine(tmp.path(), 8);
    let opts = CloneOptions { depth: Some(2), new_identity: false, on_progress: None };
    let report = block(clone_from_git(&engine, &https(&url, GitAuth::Anonymous), &opts)).expect("depth clone");
    assert!(report.commits >= 1, "depth clone imports the recent window + a snapshot");

    assert_eq!(fold_main(&engine), tree_content(&repo.bare, &tip), "depth fold(main) == tip tree");
    let row = engine.store.git_remote_get(&remote_id(&url)).unwrap().expect("remote row");
    assert_eq!(row.last_ingested_sha.as_deref(), Some(tip.as_str()));
}
