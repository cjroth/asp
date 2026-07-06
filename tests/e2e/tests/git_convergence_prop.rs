//! Property test: a random interleaving of local edits, upstream ingests, and plan
//! authoring on **two** bridge nodes converges — both in the folded content AND in
//! the synthesized git commit chain (git-bridge §10 Determinism (c)). This is the
//! load-bearing "any node may bridge" guarantee: whichever node ends up pushing,
//! every node computes byte-identical commit SHAs and the same object set.
//!
//! Construction (deterministic LCG, no proptest/cargo-fuzz — matches the repo's
//! fuzz style, e.g. `memengine.rs` / `git_push.rs`):
//!
//! * Two nodes clone the same fixture → byte-identical genesis + identical vault_id.
//! * Each iteration picks one op: a **local edit** (`record_write`/`record_remove`)
//!   on a random node, a **plan** (`author_plan`) on a random node, an **upstream
//!   ingest** (`advance_tip` + `pull_once` on *both* nodes so their ingest cursors
//!   stay in lockstep), or a **cross-integrate** (exchange every row both ways).
//! * After the loop, one final plan + a full cross-integration converges the two.
//!
//! Modeling choice (documented): the three edit sources write **disjoint path
//! namespaces** — upstream → `up/*`, node A → `a/*`, node B → `b/*` — so every
//! file's row chain has a single author. This isolates the §10(c) determinism
//! property (identical fold + identical synthesized chain) from merge-order
//! tiebreaks: when two nodes independently ingest the same commit they author
//! same-`(site,seq)`, same-content rows that differ only in `lamport` (the benign
//! §4.3 artifact); on a co-edited file that lamport could reorder a 3-way merge, but
//! single-author chains fold by seq alone, so content — and thus the synthesized
//! tree — is identical on both nodes. (Conflict convergence itself is covered by
//! `git_ingest_race.rs`, `concurrent_merge.rs`, and the fold unit tests.) The
//! upstream advance stays real but tiny (one commit per ingest).
//!
//! Drives real git wire bytes; skips gracefully when system `git` is absent.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use asp_core::gitbridge::{git_oid, remote_id, GitAuth, GitRemoteSpec, RemoteStore};
use asp_core::gitpush::{author_plan, plans_in_order, synthesize_commits, ModeTable, SynthOutput};
use asp_core::gitremote::{clone_from_git, pull_once, CloneOptions, PullReport};
use asp_core::gitwire::GitUrl;
use asp_core::identity::Identity;
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

/// The engine's fold of `main` as `path -> bytes`, minus the clone-seeded `.aspignore`.
fn fold_main(engine: &Engine) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut m = std::collections::BTreeMap::new();
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
/// anti-entropy sync would exchange. `integrate_many` dedups by Merkle id (and, at
/// the same `(site, seq)` slot, by the log's uniqueness constraint).
fn sync_into(dst: &Engine, src: &Engine) {
    let wires: Vec<WireRow> =
        src.store.all_rows().unwrap().into_iter().map(|r| src.wire(r).unwrap()).collect();
    dst.integrate_many(&wires).unwrap();
}

/// Synthesize this node's push chain: `(tip_sha, {object oids})`.
fn synth(engine: &Engine, rid: &str) -> (String, BTreeSet<String>) {
    let store = RemoteStore::open(&engine.asp_dir, rid).unwrap();
    let row = engine.store.git_remote_get(rid).unwrap().unwrap();
    let plans = plans_in_order(engine).unwrap();
    let modes = ModeTable::load(engine).unwrap();
    let out: SynthOutput = synthesize_commits(engine, &store, &row, &plans, &modes).unwrap();
    let oids: BTreeSet<String> = out.objects_to_push.iter().map(|(k, c)| git_oid(*k, c)).collect();
    (out.tip_sha, oids)
}

// ── the property ─────────────────────────────────────────────────────────────

#[test]
fn random_interleaving_converges_fold_and_synthesized_chain() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();

    // Several deterministic seeds; each is an independent random interleaving.
    for (run, seed) in [0x00C0_FFEE_1234_5678u64, 0x9E37_79B9_7F4A_7C15, 0xDEAD_BEEF_0BAD_F00D].into_iter().enumerate() {
        let repo = linear_basic();
        let server = GitHttpServer::spawn(repo.repo_root());
        let url = server.repo_url(repo.name());
        let rid = remote_id(&url);

        let ta = tempfile::tempdir().unwrap();
        let tb = tempfile::tempdir().unwrap();
        let a = clone_into(ta.path(), 40 + run as u8, &url);
        let b = clone_into(tb.path(), 80 + run as u8, &url);
        assert_eq!(a.vault_id(), b.vault_id(), "[run {run}] same repo → same vault id");

        // Deterministic LCG (no rng dep, matches the repo's fuzz style).
        let mut state = seed;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            state >> 33
        };

        // Per-node written paths (so record_remove targets something that exists).
        let a_paths = ["a/p.txt", "a/q.txt", "a/r.txt"];
        let b_paths = ["b/p.txt", "b/q.txt", "b/r.txt"];
        let mut a_live: BTreeSet<&str> = BTreeSet::new();
        let mut b_live: BTreeSet<&str> = BTreeSet::new();
        let mut upstream_n = 0u32;
        let mut plans_authored = 0u32;

        for _ in 0..18u32 {
            match next() % 6 {
                // Local edit on node A (its own `a/*` namespace — single-author chain).
                0 | 1 => {
                    let p = a_paths[(next() as usize) % a_paths.len()];
                    if next() % 4 == 0 && a_live.contains(p) {
                        a.record_remove(p).unwrap();
                        a_live.remove(p);
                    } else {
                        a.record_write(p, format!("A-{}-{}\n", next() % 97, next() % 97).as_bytes()).unwrap();
                        a_live.insert(p);
                    }
                }
                // Local edit on node B (its own `b/*` namespace).
                2 => {
                    let p = b_paths[(next() as usize) % b_paths.len()];
                    if next() % 4 == 0 && b_live.contains(p) {
                        b.record_remove(p).unwrap();
                        b_live.remove(p);
                    } else {
                        b.record_write(p, format!("B-{}-{}\n", next() % 97, next() % 97).as_bytes()).unwrap();
                        b_live.insert(p);
                    }
                }
                // Plan authoring on a random node.
                3 => {
                    let msg = format!("plan {} ({})", plans_authored, next() % 1000);
                    if next() % 2 == 0 {
                        author_plan(&a, &rid, &msg, Some("Prop A <a@x>")).unwrap();
                    } else {
                        author_plan(&b, &rid, &msg, Some("Prop B <b@x>")).unwrap();
                    }
                    plans_authored += 1;
                }
                // Upstream ingest: one real commit, pulled by BOTH nodes (cursors stay
                // in lockstep at the new tip; each authors its own same-content batch).
                4 => {
                    upstream_n += 1;
                    let content = format!("upstream v{upstream_n}\n");
                    advance_tip(&repo.bare, "up/shared.txt", &content, &format!("upstream {upstream_n}"));
                    for e in [&a, &b] {
                        match block(pull_once(e, &rid, None)).expect("pull") {
                            PullReport::Updated { .. } | PullReport::UpToDate => {}
                            PullReport::Frozen => panic!("[run {run}] unexpected freeze"),
                        }
                    }
                }
                // Cross-integrate at a random point (both directions).
                _ => {
                    sync_into(&b, &a);
                    sync_into(&a, &b);
                }
            }
        }

        // A final plan guarantees a non-trivial synthesized chain to compare, then a
        // full cross-integration converges the two nodes' logs.
        author_plan(&a, &rid, "final plan", Some("Prop A <a@x>")).unwrap();
        sync_into(&b, &a);
        sync_into(&a, &b);
        // A second round settles any branch/tag reconciliation ordering.
        sync_into(&b, &a);
        sync_into(&a, &b);

        // Both nodes reached the same upstream ingest cursor (every ingest hit both).
        assert_eq!(
            a.store.git_remote_get(&rid).unwrap().unwrap().last_ingested_sha,
            b.store.git_remote_get(&rid).unwrap().unwrap().last_ingested_sha,
            "[run {run}] identical ingest cursor"
        );

        // (a) Converged fold: both nodes' fold(main) are byte-identical.
        let fa = fold_main(&a);
        let fb = fold_main(&b);
        assert_eq!(fa, fb, "[run {run}] fold(main) converged");

        // (b) Converged synthesized chain: identical tip sha AND identical object set
        // — the "any node may bridge" determinism property (§10c).
        let (tip_a, oids_a) = synth(&a, &rid);
        let (tip_b, oids_b) = synth(&b, &rid);
        assert!(!tip_a.is_empty(), "[run {run}] a real synthesized tip");
        assert_eq!(tip_a, tip_b, "[run {run}] identical synthesized tip sha");
        assert_eq!(oids_a, oids_b, "[run {run}] identical synthesized object set");
        assert!(oids_a.contains(&tip_a), "[run {run}] object set contains the tip commit");
    }
}
