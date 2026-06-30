//! Generative property tests for the deterministic fold (`compute_files`) and,
//! once it lands, the INCREMENTAL fold. A small PRNG builds random concurrent row
//! histories — multiple sites, shared paths (→ collisions), linear + forked
//! (concurrent) edits, renames, deletes, recreates — and we assert the fold's
//! core invariants on each. This is the gate the incremental fold must clear:
//! `compute_files_incremental` must equal `compute_files` for every history and
//! every arrival order.

use asp_core::store::MemBlobStore;
use asp_core::{compute_files, fold_order, BlobStore, FileRow, FoldState, Kind, LogRow, MergeClass};
use std::collections::BTreeMap;

// ---- tiny deterministic PRNG (xorshift) ----
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(0xABCD))
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
}

// A deterministic Fisher-Yates shuffle so we can re-derive an arrival order.
fn shuffle<T>(rng: &mut Rng, xs: &mut [T]) {
    for i in (1..xs.len()).rev() {
        let j = rng.below(i + 1);
        xs.swap(i, j);
    }
}

const PATHS: &[&str] = &["a.md", "b.md", "d/c.md", "d/e.md", "f.txt", "code.rs"];
const SITES: &[&str] = &["s0", "s1", "s2"];

/// Build a random but VALID concurrent history: parents always reference an
/// already-built row of the same file, forks share a parent (→ concurrent rows
/// the fold must merge), and several files may claim the same path (→ collision
/// suffixing). Returns (store, rows). Rows are returned in build order; tests
/// shuffle them to probe order-independence.
fn generate(seed: u64, n_files: usize, ops_per_file: usize) -> (MemBlobStore, Vec<LogRow>) {
    let mut rng = Rng::new(seed);
    let store = MemBlobStore::new();
    let mut rows: Vec<LogRow> = Vec::new();
    let mut lamport: u64 = 1;

    let mut put = |s: &MemBlobStore, r: &mut Rng| -> String {
        // A handful of distinct contents so forks sometimes collide on bytes.
        s.put_blob(format!("content-{}", r.below(8)).as_bytes()).unwrap()
    };

    for fi in 0..n_files {
        let file_id = format!("f{fi}");
        let site = SITES[rng.below(SITES.len())].to_string();
        let create_path = PATHS[rng.below(PATHS.len())].to_string();
        let mut class = if create_path.ends_with(".rs") { MergeClass::Code } else { MergeClass::Text };
        let create = LogRow {
            id: String::new(),
            site_id: site.clone(),
            lamport,
            seq: 0,
            ts: lamport as i64,
            file_id: file_id.clone(),
            kind: Kind::Create,
            merge_class: class,
            parent: None,
            base_hash: None,
            result_hash: Some(put(&store, &mut rng)),
            path: Some(create_path),
            sig: vec![],
        }
        .seal();
        lamport += 1;
        // Tips this file currently has (row id + its result_hash) — forks branch
        // off any tip, linear edits extend the latest.
        let mut tips: Vec<(String, Option<String>)> = vec![(create.id.clone(), create.result_hash.clone())];
        let mut alive = true;
        rows.push(create);

        for _ in 0..ops_per_file {
            if !alive {
                // Possibly recreate at a (maybe different) path under the SAME id.
                if rng.chance(50) {
                    let p = PATHS[rng.below(PATHS.len())].to_string();
                    class = if p.ends_with(".rs") { MergeClass::Code } else { MergeClass::Text };
                    let r = LogRow {
                        id: String::new(),
                        site_id: SITES[rng.below(SITES.len())].into(),
                        lamport,
                        seq: 0,
                        ts: lamport as i64,
                        file_id: file_id.clone(),
                        kind: Kind::Create,
                        merge_class: class,
                        parent: None,
                        base_hash: None,
                        result_hash: Some(put(&store, &mut rng)),
                        path: Some(p),
                        sig: vec![],
                    }
                    .seal();
                    lamport += 1;
                    tips = vec![(r.id.clone(), r.result_hash.clone())];
                    alive = true;
                    rows.push(r);
                }
                continue;
            }
            // Branch off a random tip (usually the latest → linear; sometimes an
            // older tip → a concurrent fork the fold must 3-way merge).
            let (ptip, pbase) = tips[rng.below(tips.len())].clone();
            let site = SITES[rng.below(SITES.len())].to_string();
            let kind_roll = rng.below(10);
            let r = if kind_roll < 5 {
                // edit
                LogRow {
                    id: String::new(),
                    site_id: site,
                    lamport,
                    seq: 0,
                    ts: lamport as i64,
                    file_id: file_id.clone(),
                    kind: Kind::Edit,
                    merge_class: class,
                    parent: Some(ptip),
                    base_hash: pbase,
                    result_hash: Some(put(&store, &mut rng)),
                    path: None,
                    sig: vec![],
                }
                .seal()
            } else if kind_roll < 6 {
                // reclass: change the file's merge class (exercises the fold's
                // Reclass branch + the merge-class routing of later concurrent
                // edits). Content is unchanged, so carry the current hash forward.
                let newc = match rng.below(3) {
                    0 => MergeClass::Text,
                    1 => MergeClass::Code,
                    _ => MergeClass::Binary,
                };
                class = newc;
                LogRow {
                    id: String::new(),
                    site_id: site,
                    lamport,
                    seq: 0,
                    ts: lamport as i64,
                    file_id: file_id.clone(),
                    kind: Kind::Reclass,
                    merge_class: newc,
                    parent: Some(ptip),
                    base_hash: pbase.clone(),
                    result_hash: pbase,
                    path: None,
                    sig: vec![],
                }
                .seal()
            } else if kind_roll < 8 {
                // rename to a random path (also stresses path-collision suffixing)
                LogRow {
                    id: String::new(),
                    site_id: site,
                    lamport,
                    seq: 0,
                    ts: lamport as i64,
                    file_id: file_id.clone(),
                    kind: Kind::Rename,
                    merge_class: class,
                    parent: Some(ptip),
                    base_hash: pbase,
                    result_hash: None,
                    path: Some(PATHS[rng.below(PATHS.len())].to_string()),
                    sig: vec![],
                }
                .seal()
            } else {
                // delete
                alive = false;
                LogRow {
                    id: String::new(),
                    site_id: site,
                    lamport,
                    seq: 0,
                    ts: lamport as i64,
                    file_id: file_id.clone(),
                    kind: Kind::Delete,
                    merge_class: class,
                    parent: Some(ptip),
                    base_hash: pbase,
                    result_hash: None,
                    path: None,
                    sig: vec![],
                }
                .seal()
            };
            lamport += 1;
            if alive {
                // The new row becomes a tip; with low odds keep the old tip too
                // (an explicit fork point for the next op).
                let keep_fork = rng.chance(25);
                let nt = (r.id.clone(), r.result_hash.clone());
                if keep_fork {
                    tips.push(nt);
                } else {
                    tips = vec![nt];
                }
            }
            rows.push(r);
        }
    }
    (store, rows)
}

/// Canonical, comparable view of a fold result (ignores incidental fields like
/// the authoring clock that don't affect the converged tree).
fn norm(files: &[FileRow]) -> Vec<(String, Option<String>, bool, String)> {
    let mut v: Vec<_> = files
        .iter()
        .map(|f| (f.path.clone(), f.result_hash.clone(), f.deleted, f.merge_class.as_str().to_string()))
        .collect();
    v.sort();
    v
}

#[test]
fn fold_is_permutation_invariant_over_random_histories() {
    // The fold is a CRDT: the converged tree must not depend on the order rows
    // arrived in. Across many random concurrent histories, every shuffle of the
    // same rows must yield the identical materialized file set.
    for seed in 0..400u64 {
        let (store, rows) = generate(seed, 6, 8);
        let base = compute_files(&store, &rows).unwrap();
        for k in 0..3 {
            let mut shuffled = rows.clone();
            let mut rng = Rng::new(seed.wrapping_mul(1000) + k);
            shuffle(&mut rng, &mut shuffled);
            let got = compute_files(&store, &shuffled).unwrap();
            assert_eq!(norm(&base), norm(&got), "seed {seed} shuffle {k}: fold not order-invariant");
        }
    }
}

#[test]
fn incremental_foldstate_matches_full_fold_after_every_row() {
    // THE gate for the incremental fold: feed a random concurrent history to a
    // FoldState ONE ROW AT A TIME in a random arrival order (re-folding only the
    // touched file each step), and assert it equals compute_files over the rows
    // seen so far — after EVERY row. Out-of-order arrival, concurrent forks,
    // renames creating/breaking path collisions, delete+recreate: all must keep
    // the incremental state identical to a from-scratch fold.
    for seed in 0..300u64 {
        let (store, rows) = generate(seed, 6, 8);
        let mut arrival = rows.clone();
        let mut rng = Rng::new(seed.wrapping_mul(7919) + 1);
        shuffle(&mut rng, &mut arrival);

        // rows seen so far, indexed by file_id, so refold_files can hand the
        // incremental fold ALL rows for a touched file.
        let mut by_file: BTreeMap<String, Vec<LogRow>> = BTreeMap::new();
        let mut seen: Vec<LogRow> = Vec::new();
        let mut fold = FoldState::from_rows(&store, &[]).unwrap();

        for r in arrival {
            by_file.entry(r.file_id.clone()).or_default().push(r.clone());
            seen.push(r.clone());
            let fid = r.file_id.clone();
            fold
                .refold_files(&store, std::slice::from_ref(&fid), |f| Ok(by_file.get(f).cloned().unwrap_or_default()))
                .unwrap();

            let incremental = norm(&fold.files());
            let full = norm(&compute_files(&store, &seen).unwrap());
            assert_eq!(incremental, full, "seed {seed}: incremental fold diverged after a row for {fid}");
        }
    }
}

#[test]
fn fold_order_total_and_stable_over_random_histories() {
    // fold_order must produce the same total order for any input permutation
    // (the backbone of the fold's determinism).
    for seed in 0..200u64 {
        let (_store, rows) = generate(seed, 5, 7);
        let ord1: Vec<String> = fold_order(&rows).into_iter().map(|r| r.id).collect();
        let mut shuffled = rows.clone();
        let mut rng = Rng::new(seed + 7);
        shuffle(&mut rng, &mut shuffled);
        let ord2: Vec<String> = fold_order(&shuffled).into_iter().map(|r| r.id).collect();
        assert_eq!(ord1, ord2, "seed {seed}: fold_order not permutation-invariant");
    }
}
