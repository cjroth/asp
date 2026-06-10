//! Persistence round-trip property (§Testing). A thin client persists its engine
//! by dumping every row (`rows_after` over an empty vector) and restores by
//! re-integrating that dump. This session a persisted `engine-state.json` grew to
//! 535 MB: the dump was written INTO the synced tree, re-ingested, and each dump
//! then carried the previous dump — an exponential ratchet. The scope fix removed
//! the host-level cause, but the persistence layer itself must also hold its own
//! contract, or a future caller re-opens the same hole.
//!
//! Two invariants, asserted over randomized vaults that carry real edit history:
//!   1. FIDELITY — `restore(dump(e))` materializes byte-identically to `e`, and
//!      re-dumping yields the same row multiset (restore neither drops nor
//!      duplicates rows) and the same blob payload (no blob re-wrapping).
//!   2. SIZE STABILITY — dumping/restoring N times with NO new edits keeps both
//!      the row count and the total blob bytes constant. A restore that
//!      re-appends or re-inflates would ratchet here — the 535 MB class of bug.

use asp_core::{Identity, MemEngine, SessionVault, WireRow};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::BTreeMap;

fn eng(seed: u8) -> MemEngine {
    MemEngine::create(Identity::from_seed(&[seed; 32]), "v")
}

/// The whole op-log as wire rows — exactly what `WasmEngine::rows_after("{}")`
/// (the SDK's `dump()`) produces: version-vector over all sites, from genesis.
fn dump_all(e: &MemEngine) -> Vec<WireRow> {
    let vv = SessionVault::version_vector(e).expect("vv");
    let mut out = Vec::new();
    for site in vv.keys() {
        out.extend(SessionVault::rows_after_wire(e, site, -1).expect("rows"));
    }
    out
}

fn restore(rows: &[WireRow]) -> MemEngine {
    let e = eng(0xD0);
    e.integrate_many(rows).expect("integrate");
    e
}

/// Total content payload carried by a dump — the thing that ballooned to 535 MB.
fn blob_bytes(rows: &[WireRow]) -> usize {
    rows.iter().flat_map(|r| &r.blobs).map(|b| b.bytes.len()).sum()
}

fn row_ids(rows: &[WireRow]) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for r in rows {
        *m.entry(r.row.id.clone()).or_insert(0) += 1; // multiset → catches duplication
    }
    m
}

/// A vault carrying genuine edit HISTORY: a few devices repeatedly rewrite a
/// shared set of paths, so the log holds version chains (multiple blobs per
/// path) — the payload a dump must round-trip without inflating.
fn random_vault(seed: u64) -> MemEngine {
    let mut r = StdRng::seed_from_u64(seed);
    let paths = ["a.md", "b.md", "d/c.md", "d/e.md"];
    let n_dev = r.gen_range(1..=3);
    let mut all: Vec<WireRow> = Vec::new();
    for dev in 0..n_dev {
        let d = eng(60 + dev as u8);
        for _ in 0..r.gen_range(1..=8) {
            let p = paths[r.gen_range(0..paths.len())];
            let body = format!("dev{dev} v{} {}\n", r.gen_range(0..50), "x".repeat(r.gen_range(0..40)));
            if let Some(wr) = d.record_write(p, body.as_bytes()).expect("write") {
                all.push(wr);
            }
        }
    }
    let canon = eng(40);
    canon.integrate_many(&all).expect("merge");
    canon
}

#[test]
fn restore_is_faithful_and_idempotent() {
    for seed in 0..300u64 {
        let e = random_vault(seed);
        let d0 = dump_all(&e);
        let restored = restore(&d0);
        // Fidelity: same materialized tree.
        assert_eq!(restored.files_map().expect("files"), e.files_map().expect("files"), "seed {seed}: restore changed the tree");
        // Idempotency: re-dump is the same multiset of rows + same payload.
        let d1 = dump_all(&restored);
        assert_eq!(row_ids(&d1), row_ids(&d0), "seed {seed}: restore dropped/duplicated rows");
        assert_eq!(blob_bytes(&d1), blob_bytes(&d0), "seed {seed}: restore changed blob payload");
    }
}

#[test]
fn dump_size_is_stable_across_reload_cycles() {
    // The literal 535 MB shape: persist → restore → persist, repeatedly, with NO
    // edits. Neither the row count nor the blob payload may grow.
    let mut saw_history = false;
    for seed in 0..120u64 {
        let e = random_vault(seed);
        let mut rows = dump_all(&e);
        if rows.is_empty() {
            continue;
        }
        // A vault with >files worth of rows is carrying version history — the
        // case that makes "size stable" non-trivial.
        saw_history |= rows.len() > e.files_map().expect("files").len();
        let base_rows = rows.len();
        let base_bytes = blob_bytes(&rows);
        for cycle in 0..8 {
            let r = restore(&rows);
            rows = dump_all(&r);
            assert_eq!(rows.len(), base_rows, "seed {seed} cycle {cycle}: row count grew {base_rows}→{}", rows.len());
            assert_eq!(blob_bytes(&rows), base_bytes, "seed {seed} cycle {cycle}: blob payload grew");
        }
    }
    assert!(saw_history, "generator never produced a vault with edit history — size test is weak");
}

#[test]
fn reintegrating_own_rows_is_a_noop() {
    // The idempotency guard that actually exercises dedup-vs-existing: feed an
    // engine its OWN dump back. It already holds every row, so nothing may be
    // added — row count and blob payload stay put. (A restore loop that wrote
    // the dump back into the synced tree relied on exactly this no-op; if
    // integrate stopped deduping by Merkle id, the log would double each pass.)
    for seed in 0..200u64 {
        let e = random_vault(seed);
        let d = dump_all(&e);
        if d.is_empty() {
            continue;
        }
        let rows_before = e.row_count();
        let bytes_before = blob_bytes(&dump_all(&e));
        // Re-apply the whole dump several times into the SAME engine.
        for _ in 0..3 {
            let flags = e.integrate_many(&d).expect("integrate");
            assert!(flags.iter().all(|&added| !added), "seed {seed}: re-integrating own rows reported new rows");
        }
        assert_eq!(e.row_count(), rows_before, "seed {seed}: re-integration grew the log");
        assert_eq!(blob_bytes(&dump_all(&e)), bytes_before, "seed {seed}: re-integration grew the payload");
    }
}

#[test]
fn restore_is_order_independent() {
    // A dump is delivered/stored in arbitrary order; restoring a shuffled dump
    // must still reproduce the exact tree (the fold is order-independent, and
    // restore must not rely on dump order).
    for seed in 0..150u64 {
        let e = random_vault(seed);
        let d = dump_all(&e);
        let mut shuffled = d.clone();
        let mut r = StdRng::seed_from_u64(seed ^ 0xABCD);
        for i in (1..shuffled.len()).rev() {
            shuffled.swap(i, r.gen_range(0..=i));
        }
        assert_eq!(restore(&shuffled).files_map().expect("f"), e.files_map().expect("f"), "seed {seed}: shuffled restore diverged");
    }
}
