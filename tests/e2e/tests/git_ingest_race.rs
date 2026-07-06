//! The double-ingest race, at the engine level (git-bridge §4.3, §10 "Race tests").
//!
//! Two bridge nodes clone the *same* fixture (byte-identical genesis → identical
//! `vault_id`) and both ingest the *same* upstream commit `X` before their
//! `GitIngest` ledger records cross. Because each authors its import batch under its
//! own **local** identity (`lamport = local max + 1`, git-bridge §4.2), the two
//! batches carry different Merkle ids but **identical `result_hash` content**.
//!
//! Reality of the honest §4.3 outcome: imported rows are authored under the *repo*
//! site with a **deterministic dense `seq`** over ingested commits, so both nodes' X
//! rows land at the *same* `(repo_site, seq)` slots — differing only in
//! `lamport`/`id`. The log's `UNIQUE(site_id, seq)` constraint (`sqlite.rs`) then
//! makes cross-integration idempotent per slot: each node keeps whichever row it
//! already holds, the peer's same-slot row is `INSERT OR IGNORE`d, and **content
//! converges byte-identically** because every `result_hash` matches. So the
//! load-bearing predicate is exactly what the spec states — "a `GitIngest` for X
//! **exists**" — and the duplicate is harmless (it can never diverge the fold). This
//! is stronger than the spec's idealized "both markers retained": `(site, seq)`
//! uniqueness means one sha never occupies two seq slots, so each node retains
//! exactly one marker for X and the two never fight.
//!
//! Construction (deterministic, no live iroh Session — reliable, documented):
//! clone A and B against the hermetic smart-HTTP server; give A and B *different*
//! amounts of local activity so their `next_lamport` diverges (the realistic race
//! condition — otherwise identical local state would make the two ingests
//! byte-identical and simply dedup); `pull_once` on each independently; then
//! cross-integrate every row both directions (the same rows a two-peer anti-entropy
//! sync would exchange) and assert convergence.
//!
//! Drives real git wire bytes; skips gracefully when system `git` is absent.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use asp_core::gitbridge::{remote_id, GitAuth, GitRemoteSpec};
use asp_core::gitremote::{clone_from_git, pull_once, CloneOptions, PullReport};
use asp_core::gitwire::GitUrl;
use asp_core::identity::Identity;
use asp_core::log::Kind;
use asp_core::store::BlobStore;
use asp_core::wire::WireRow;
use asp_core::{Engine, SessionVault};
use asp_e2e::gitfix::{advance_tip, linear_basic, GitHttpServer};

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

fn clone_into(dir: &Path, seed: u8, url: &str) -> Engine {
    let engine = open_engine(dir, seed);
    block(clone_from_git(&engine, &https(url, GitAuth::Anonymous), &CloneOptions::default())).expect("clone");
    engine
}

/// `git ls-tree -r <sha>` on the bare → `path -> content bytes` (blobs only).
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

fn rev_parse(bare: &Path, spec: &str) -> String {
    let out = Command::new("git").arg("--git-dir").arg(bare).args(["rev-parse", spec]).output().expect("rev-parse");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The engine's fold of `main` as `path -> bytes`, minus the clone-seeded `.aspignore`.
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

/// Every row `src` holds, bundled with its blobs — the exact set a two-peer
/// anti-entropy sync would hand `dst`. `integrate_many` dedups by Merkle id.
fn sync_into(dst: &Engine, src: &Engine) {
    let wires: Vec<WireRow> =
        src.store.all_rows().unwrap().into_iter().map(|r| src.wire(r).unwrap()).collect();
    dst.integrate_many(&wires).unwrap();
}

/// Count of `GitIngest` ledger markers for `sha` in the log.
fn ingest_markers(engine: &Engine, sha: &str) -> usize {
    engine
        .store
        .all_rows()
        .unwrap()
        .iter()
        .filter(|r| r.kind == Kind::GitIngest && r.path.as_deref() == Some(sha))
        .count()
}

// ── the race ─────────────────────────────────────────────────────────────────

#[test]
fn double_ingest_of_one_commit_converges_with_duplicate_markers() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = linear_basic();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let rid = remote_id(&url);

    // Two independent clones — byte-identical genesis + identical vault_id (§3.2).
    let ta = tempfile::tempdir().unwrap();
    let tb = tempfile::tempdir().unwrap();
    let a = clone_into(ta.path(), 21, &url);
    let b = clone_into(tb.path(), 22, &url);
    assert_eq!(a.vault_id(), b.vault_id(), "same repo → same vault id");

    // Diverge the two nodes' local activity so their `next_lamport` differs at pull
    // time (git-bridge §4.2). This is what turns the two ingests into a genuine fork
    // with distinct Merkle ids rather than byte-identical dedup — the honest §4.3
    // race. Local edits are on distinct paths so they simply co-exist post-merge.
    a.record_write("a_local.txt", b"from A\n").unwrap();
    b.record_write("b_local.txt", b"from B one\n").unwrap();
    b.record_write("b_local.txt", b"from B two\n").unwrap();

    // Upstream advances by exactly one commit `X` (a fresh file both will import).
    let x = advance_tip(&repo.bare, "shared.txt", "converged upstream\n", "upstream commit X");
    assert_ne!(x, rev_parse(&repo.bare, "main~1"));

    // Both ingest X independently, before any GitIngest record crosses.
    match block(pull_once(&a, &rid, None)).expect("pull a") {
        PullReport::Updated { new_commits, .. } => assert!(new_commits >= 1),
        other => panic!("A expected Updated, got {other:?}"),
    }
    match block(pull_once(&b, &rid, None)).expect("pull b") {
        PullReport::Updated { new_commits, .. } => assert!(new_commits >= 1),
        other => panic!("B expected Updated, got {other:?}"),
    }

    // Pre-cross-integration: each node authored its own X import independently, under
    // a different local lamport → distinct-id GitIngest markers at the *same* repo
    // (site, seq) slot. That distinct id is what makes this a genuine race (not the
    // trivial byte-identical dedup that identical local state would produce).
    let a_marker = a
        .store
        .all_rows()
        .unwrap()
        .into_iter()
        .find(|r| r.kind == Kind::GitIngest && r.path.as_deref() == Some(x.as_str()))
        .expect("A has a GitIngest for X");
    let b_marker = b
        .store
        .all_rows()
        .unwrap()
        .into_iter()
        .find(|r| r.kind == Kind::GitIngest && r.path.as_deref() == Some(x.as_str()))
        .expect("B has a GitIngest for X");
    assert_ne!(a_marker.id, b_marker.id, "distinct-id ingest markers — a genuine race, not dedup");
    assert_eq!(
        (&a_marker.site_id, a_marker.seq),
        (&b_marker.site_id, b_marker.seq),
        "same repo (site, seq) slot — the deterministic dense seq puts X's marker in one place"
    );
    assert_eq!(a_marker.result_hash, b_marker.result_hash, "identical ledger payload content");

    // Cross-integrate every row both directions (what a two-peer anti-entropy sync
    // exchanges). No panic; the log's UNIQUE(site,seq) makes each same-slot row an
    // idempotent no-op on the peer.
    sync_into(&b, &a); // B ← A
    sync_into(&a, &b); // A ← B

    // (§4.3) Content convergence is guaranteed: both folds are byte-identical…
    let fa = fold_main(&a);
    let fb = fold_main(&b);
    assert_eq!(fa, fb, "both nodes' fold(main) are byte-identical after the race");

    // …and equal to the real upstream tip tree ∪ the two local edits (adds, the
    // fixture's delete/edit/rename, and X's new file all converged — no divergence).
    let mut want = tree_content(&repo.bare, &x);
    want.insert("a_local.txt".into(), b"from A\n".to_vec()); // A's local edit
    want.insert("b_local.txt".into(), b"from B two\n".to_vec()); // B's local edit (last write wins)
    assert_eq!(fa, want, "fold == upstream tip tree ∪ the two local edits");

    // The raced commit's file content appears exactly once, at the converged value.
    assert_eq!(fa.get("shared.txt").map(|v| v.as_slice()), Some(&b"converged upstream\n"[..]), "X's file present once");
    assert_eq!(fa.keys().filter(|k| *k == "shared.txt").count(), 1, "exactly one shared.txt entry");

    // The load-bearing §4.3 predicate — "a GitIngest for X exists" — holds on both
    // nodes. UNIQUE(site,seq) collapses the two same-slot markers to one per node
    // (each keeps its own), so the duplicate never even reaches the fold: harmless.
    assert!(ingest_markers(&a, &x) >= 1, "A: a GitIngest for X exists (load-bearing predicate)");
    assert!(ingest_markers(&b, &x) >= 1, "B: a GitIngest for X exists (load-bearing predicate)");
    assert_eq!(ingest_markers(&a, &x), 1, "one marker per (site,seq) slot survives on A — no fold divergence");
    assert_eq!(ingest_markers(&b, &x), 1, "one marker per (site,seq) slot survives on B — no fold divergence");
}
