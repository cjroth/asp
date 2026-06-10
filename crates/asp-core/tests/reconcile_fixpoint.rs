//! Reconcile/materialize fixpoint property (§Testing). The duplicate-explosion
//! loop was invisible to the engine's convergence tests because those author
//! rows directly — they never model the host round-trip a thin client actually
//! performs: read the MATERIALIZED tree off disk, author it back into a fresh
//! engine, sync with the canonical peer. When two devices touch the same path,
//! the fold disambiguates the loser to `name (1).md`; a fresh node that authors
//! that materialized name as a NEW file id and *then* merges re-collides and
//! re-disambiguates — doubling every reload.
//!
//! The invariant that kills it: a fresh node whose disk mirrors the canonical
//! materialized tree, after adopting the peer's rows and reconciling its disk,
//! converges to EXACTLY the peer's file set — and iterating that is a fixpoint,
//! not a ratchet. We assert it over many randomized vaults that deliberately
//! contain same-path collisions. `bug_shape_*` documents the runaway the fix
//! prevents (reconcile-before-adopt), so the regression can't silently return.

use asp_core::{Identity, MemEngine, WireRow};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::BTreeMap;

fn eng(seed: u8) -> MemEngine {
    MemEngine::create(Identity::from_seed(&[seed; 32]), "v")
}

/// Build a canonical peer whose vault contains genuine same-path collisions:
/// `n_dev` devices each author a random subset of a shared path pool with
/// device-specific content, then everything is merged. Returns (all rows, the
/// canonical materialized tree).
fn canonical(seed: u64) -> (Vec<WireRow>, BTreeMap<String, Vec<u8>>) {
    let mut r = StdRng::seed_from_u64(seed);
    let pool = ["a.md", "b.md", "dir/c.md", "dir/d.md", "e.txt"];
    let n_dev = r.gen_range(2..=3);
    let mut rows: Vec<WireRow> = Vec::new();
    for dev in 0..n_dev {
        let d = eng(50 + dev as u8);
        for p in pool {
            if r.gen_bool(0.6) {
                // Device-specific content → distinct file ids at the same path,
                // i.e. a real collision the fold must disambiguate.
                let body = format!("dev{dev}:{p}:{}\n", r.gen_range(0..4));
                if let Some(wr) = d.record_write(p, body.as_bytes()).expect("write") {
                    rows.push(wr);
                }
            }
        }
    }
    let a = eng(9);
    a.integrate_many(&rows).expect("integrate");
    let goal = a.files_map().expect("files");
    (rows, goal)
}

#[test]
fn fresh_node_adopt_then_reconcile_is_a_fixpoint() {
    let mut saw_collision = false;
    for seed in 0..400u64 {
        let (rows, goal) = canonical(seed);
        // Skip the degenerate empty vault (no device wrote anything this seed).
        if goal.is_empty() {
            continue;
        }
        // Track that the generator actually produced the case that mattered: a
        // same-path collision the fold disambiguated to `name (1).md`. Without
        // this, a collision-free generator would make the fixpoint trivially true.
        saw_collision |= goal.keys().any(|p| p.contains(" (1)"));
        let mut disk = goal.clone();
        // Simulate several "reloads" of a thin client that keeps losing its
        // engine state (the worst case): each reload is a brand-new engine.
        for cycle in 0..4 {
            let b = eng(7);
            b.integrate_many(&rows).expect("adopt"); // adopt peer ids FIRST
            b.record_writes(&disk).expect("reconcile"); // THEN reconcile disk
            let got = b.files_map().expect("files");
            assert_eq!(got, goal, "seed {seed} cycle {cycle}: node diverged from canonical");
            disk = got; // next reload reads what we just materialized
        }
    }
    assert!(saw_collision, "generator never produced a same-path collision — fixpoint test is vacuous");
}

#[test]
fn bug_shape_reconcile_before_adopt_runs_away() {
    // The exact anti-pattern the fix removed: a fresh engine reconciles its disk
    // BEFORE adopting the peer, minting colliding ids. Documents the runaway so
    // nobody "simplifies" syncOnce back into it. Find a seed that actually
    // produced a collision (≥2 files), then show the count strictly grows.
    let mut ran = false;
    for seed in 0..200u64 {
        let (rows, goal) = canonical(seed);
        if goal.len() < 2 {
            continue;
        }
        let mut disk = goal.clone();
        let mut counts = Vec::new();
        for _ in 0..3 {
            let b = eng(7);
            b.record_writes(&disk).expect("reconcile"); // reconcile FIRST (bug)
            b.integrate_many(&rows).expect("merge"); // then merge → collide
            disk = b.files_map().expect("files");
            counts.push(disk.len());
        }
        if counts[0] < counts[1] && counts[1] < counts[2] {
            ran = true;
            break; // demonstrated the strictly-increasing ratchet
        }
    }
    assert!(ran, "expected at least one seed to exhibit the reconcile-before-adopt runaway");
}
