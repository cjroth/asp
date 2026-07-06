//! git-bridge spec §12 **R3 — imported-branch-volume perf guardrail**.
//!
//! A mature repo can carry tens of thousands of merged PRs. Delete-after-merge
//! (§3.1) authors, per merged PR, a `Kind::Branch` *create* record and a
//! `Kind::Branch` *delete* tombstone — so the log accrues ~2 branch records per
//! imported branch, i.e. tens of thousands of them. R3 requires proving the
//! load-bearing branch machinery stays fast at that cardinality *before* full-DAG
//! import (M2) ships:
//!   * `BranchSet::new` (construction over the whole reconciled set),
//!   * `BranchSet::visibility(target)` + `Visibility::sees` (the fold hot path),
//!   * `build_graph` (branch.rs:255 — the network-graph renderer),
//!   * `reconcile_branches` (the create+delete → converged-set fold).
//!
//! The synthesis mirrors `gitgenesis::create_side_lane` / `emit_branch_delete`:
//! ids via `Branch::derive_id(name, "main", &fork_vv, lamport, site)`, PR-style
//! distinct names, monotonic `created_lamport`, a create record then a
//! `deleted = true` tombstone at a higher lamport, plus a couple of ordinary
//! content rows authored on the branch so `build_graph` has something to coarsen.
//!
//! ## Why a *ratio* check and not just a wall-clock bound
//! Wall-clock bounds are machine-dependent (this runs on a memory-throttled VM and
//! on shared CI). The real guardrail is the **N vs 2N scaling ratio**: an honest
//! O(n) / O(n log n) op stays near-linear (ratio ~2; we allow ≤ 3.0 for noise),
//! whereas an accidental O(n²) roughly *quadruples* (ratio ~4) — which is the
//! regression R3 exists to catch. Generous absolute ceilings back the ratio up so
//! a pathological blow-up trips loudly instead of hanging.
//!
//! ## Measurement hygiene
//! Each op is timed on its **own freshly-built corpus** and only one corpus is ever
//! alive at a time (this VM OOM-kills under load, and a heavy op left a large,
//! fragmented heap that inflated a following op's ratio — see the R3 finding note
//! on `build_graph` below). This isolates each op's true scaling.
//!
//! Run: `cargo test -p asp-core --test branch_scale -- --nocapture`
//! (asp-core is opt-level 3 in dev per the root Cargo.toml, so debug is fast
//! enough — no `--release` needed).

use asp_core::branch::{
    build_graph, encode_branch_record, reconcile_branches, Branch, BranchSet, VersionVector,
};
use asp_core::log::{Kind, LogRow, MergeClass, MAIN_BRANCH_ID};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const SITE: &str = "0011223344556677889900aabbccddeeff00112233445566778899aabbccddee";

/// One synthesized corpus at a given branch cardinality.
struct Corpus {
    /// The full log: 2 branch records (create + delete tombstone) per branch, plus
    /// two content rows per branch. Order is emit-order, as genesis pushes.
    rows: Vec<LogRow>,
    /// The reconciled branch set (post-LWW) — what a real vault feeds
    /// `BranchSet::new`. Under delete-after-merge every entry is `deleted = true`.
    reconciled: Vec<Branch>,
    /// The same branches presented as *live* lanes (`deleted = false`). This is the
    /// worst case the graph renderer must survive — a repo asked to draw N lanes.
    /// `build_graph` filters `deleted` out of lanes, so to exercise lane
    /// cardinality at all (the O(lanes) fork-edge pass) it must be handed live lanes.
    live: Vec<Branch>,
    /// blob hash -> encoded branch record, so `reconcile_branches` can decode.
    blobs: HashMap<String, Vec<u8>>,
    /// A target branch id deep in the set (the last-created branch).
    deep_target: String,
}

/// Build a corpus of `n` imported branches the way genesis does.
fn synthesize(n: usize) -> Corpus {
    let mut rows: Vec<LogRow> = Vec::with_capacity(n * 4);
    let mut reconciled: Vec<Branch> = Vec::with_capacity(n);
    let mut live: Vec<Branch> = Vec::with_capacity(n);
    let mut blobs: HashMap<String, Vec<u8>> = HashMap::with_capacity(n * 2);
    let mut deep_target = MAIN_BRANCH_ID.to_string();

    // A monotonic lamport/seq clock, exactly like GenesisBuilder::tick.
    let mut clock: u64 = 1;
    let mut tick = || {
        let c = clock;
        clock += 1;
        c
    };

    // A single content-blob hash reused by every content row (the fold isn't under
    // test here — branch cardinality is).
    let content_hash = "content".to_string();

    for i in 0..n {
        // fork_vv = {site: imported frontier at the fork} — a growing cap, mirroring
        // create_side_lane's `vv.insert(site, cap)`.
        let cap = (i as i64) + 1;
        let mut fork_vv = VersionVector::new();
        fork_vv.insert(SITE.to_string(), cap);

        let name = format!("git/pr-{i}");
        let create_lamport = tick();
        let branch_id = Branch::derive_id(&name, MAIN_BRANCH_ID, &fork_vv, create_lamport, SITE);

        // --- Branch CREATE record (authored on main, file_id = branch_id). ---
        let create_rec = Branch {
            branch_id: branch_id.clone(),
            name: name.clone(),
            parent: Some(MAIN_BRANCH_ID.to_string()),
            fork_vv: fork_vv.clone(),
            created_lamport: create_lamport,
            created_ts: 0,
            deleted: false,
        };
        let create_h = format!("bc-{i}"); // stand-in blob hash (put_blob would return a real one)
        blobs.insert(create_h.clone(), encode_branch_record(&create_rec));
        rows.push(
            LogRow {
                site_id: SITE.to_string(),
                lamport: create_lamport,
                seq: create_lamport,
                file_id: branch_id.clone(),
                kind: Kind::Branch,
                merge_class: MergeClass::Text,
                result_hash: Some(create_h),
                path: Some(name.clone()),
                branch_id: MAIN_BRANCH_ID.to_string(),
                ..LogRow::default()
            }
            .seal(),
        );

        // --- Two ordinary content rows authored ON the branch (coarsen into a
        //     settle-commit per lane for build_graph). ---
        for j in 0..2u64 {
            let lam = tick();
            rows.push(
                LogRow {
                    site_id: SITE.to_string(),
                    lamport: lam,
                    seq: lam,
                    file_id: format!("f-{i}-{j}"),
                    kind: if j == 0 { Kind::Create } else { Kind::Edit },
                    merge_class: MergeClass::Text,
                    base_hash: if j == 0 { None } else { Some(content_hash.clone()) },
                    result_hash: Some(content_hash.clone()),
                    path: Some(format!("pr-{i}/file.md")),
                    branch_id: branch_id.clone(),
                    ..LogRow::default()
                }
                .seal(),
            );
        }

        // --- Branch DELETE tombstone (higher lamport wins the LWW reconcile). ---
        let del_lamport = tick();
        let del_rec = Branch { deleted: true, ..create_rec.clone() };
        let del_h = format!("bd-{i}");
        blobs.insert(del_h.clone(), encode_branch_record(&del_rec));
        rows.push(
            LogRow {
                site_id: SITE.to_string(),
                lamport: del_lamport,
                seq: del_lamport,
                file_id: branch_id.clone(),
                kind: Kind::Branch,
                merge_class: MergeClass::Text,
                result_hash: Some(del_h),
                path: Some(name.clone()),
                branch_id: MAIN_BRANCH_ID.to_string(),
                ..LogRow::default()
            }
            .seal(),
        );

        // Converged (post-LWW) record is the tombstone; the live-lane variant is the
        // same branch presented as un-deleted for the graph stress.
        reconciled.push(del_rec.clone());
        live.push(Branch { deleted: false, ..del_rec });
        deep_target = branch_id;
    }

    Corpus { rows, reconciled, live, blobs, deep_target }
}

/// The four ops under guard.
#[derive(Clone, Copy, PartialEq)]
enum Op {
    BranchSetNew,
    VisibilityAndSees,
    BuildGraph,
    Reconcile,
}

impl Op {
    fn label(self) -> &'static str {
        match self {
            Op::BranchSetNew => "BranchSet::new",
            Op::VisibilityAndSees => "visibility + sees",
            Op::BuildGraph => "build_graph",
            Op::Reconcile => "reconcile_branches",
        }
    }
}

/// Reps per measurement; we keep the **minimum** (the least scheduler-/cache-
/// perturbed sample), the standard estimator for noisy wall-clock microbenchmarks
/// on a loaded box.
const REPS: usize = 3;

/// Time a single op on its OWN fresh corpus (corpus dropped before returning, so
/// only one N-sized structure is ever resident). The op runs `REPS` times and the
/// minimum wall time is returned. Care is taken to keep *setup* (clones, corpus
/// construction) OUT of the timed region — real callers hand `BranchSet::new` an
/// already-owned `Vec<Branch>` (from `reconcile_branches`), so cloning it would
/// time an allocation the production path never does.
fn time_op(op: Op, n: usize) -> Duration {
    let c = synthesize(n);
    let mut best = Duration::MAX;
    for _ in 0..REPS {
        let dt = match op {
            Op::BranchSetNew => {
                // Clone OUTSIDE the timer so we measure only BranchSet construction.
                let owned = c.reconciled.clone();
                let t = Instant::now();
                let bs = BranchSet::new(owned);
                let dt = t.elapsed();
                std::hint::black_box(bs.get(&c.deep_target).is_some());
                dt
            }
            Op::VisibilityAndSees => {
                let bs = BranchSet::new(c.reconciled.clone());
                let t = Instant::now();
                let vis = bs.visibility(&c.deep_target);
                // Scan the whole log, exactly like the fold's `visible_rows` filter —
                // one `sees` per row (O(depth) each). This is the true fold hot path.
                let mut visible = 0usize;
                for r in &c.rows {
                    if vis.sees(r) {
                        visible += 1;
                    }
                }
                let dt = t.elapsed();
                std::hint::black_box(visible);
                dt
            }
            Op::BuildGraph => {
                let t = Instant::now();
                let g = build_graph(&c.rows, &c.live, &c.deep_target, 200);
                let dt = t.elapsed();
                std::hint::black_box(g.nodes.len());
                dt
            }
            Op::Reconcile => {
                let blobs = &c.blobs;
                let t = Instant::now();
                let recs = reconcile_branches(&c.rows, |h| blobs.get(h).cloned());
                let dt = t.elapsed();
                std::hint::black_box(recs.len());
                dt
            }
        };
        best = best.min(dt);
    }
    best
}

/// N — the imported-branch cardinality under test. R3 names ~50k; this VM
/// OOM-kills under load, so we default to 20k (still 80k log rows + 40k branch
/// records — solidly in the tens-of-thousands R3 cares about) and lean on the
/// N-vs-2N *ratio* to catch super-linear complexity independent of absolute size.
/// Override with `BRANCH_SCALE_N` to push toward 50k on a bigger box.
fn base_n() -> usize {
    std::env::var("BRANCH_SCALE_N").ok().and_then(|s| s.parse().ok()).unwrap_or(20_000)
}

/// The scaling multiplier we permit going N -> 2N. A linear/n-log-n op ~doubles
/// (ratio ~2); we allow up to 3.0 for timer/allocator noise on a loaded box. A
/// genuine O(n²) ~quadruples (ratio ~4) and blows past this — the R3 regression.
const MAX_RATIO: f64 = 3.0;

/// Below this baseline the op is dominated by timer noise and a ratio is
/// meaningless; we still print it but don't assert on it.
const SIGNAL: Duration = Duration::from_millis(2);

fn ratio(a: Duration, b: Duration) -> f64 {
    b.as_secs_f64() / a.as_secs_f64().max(1e-6)
}

/// Measure an op at N and 2N, print, and return (t_n, t_2n, ratio).
fn scale(op: Op, n: usize) -> (Duration, Duration, f64) {
    let a = time_op(op, n);
    let b = time_op(op, 2 * n);
    let r = ratio(a, b);
    eprintln!(
        "  {:<20} N={:>6}: {:>12?}   2N={:>6}: {:>12?}   ratio {:.2}x",
        op.label(),
        n,
        a,
        2 * n,
        b,
        r
    );
    (a, b, r)
}

#[test]
fn r3_core_branch_ops_stay_sub_quadratic() {
    let n = base_n();
    eprintln!("\n=== git-bridge R3 imported-branch perf guardrail ===");
    eprintln!("N = {n}  (log rows ~= {}, branch records ~= {})", n * 4, n * 2);
    eprintln!("linear ~2.0x, quadratic ~4.0x, cap {MAX_RATIO}x, signal floor {SIGNAL:?}\n");

    // The three ops that are (and must stay) linear: reconcile the branch set,
    // build the BranchSet, and run the visibility fold hot path over the log.
    for op in [Op::BranchSetNew, Op::VisibilityAndSees, Op::Reconcile] {
        let (a, b, r) = scale(op, n);
        assert!(b < Duration::from_secs(10), "{} too slow at 2N: {:?}", op.label(), b);
        if a > SIGNAL {
            assert!(r < MAX_RATIO, "{} scales super-linearly ({r:.2}x > {MAX_RATIO}x)", op.label());
        }
    }

    eprintln!("\n=== R3 core ops PASS: BranchSet::new / visibility+sees / reconcile all sub-quadratic ===\n");
}

/// R3 guardrail: `build_graph` (branch.rs:255) must stay **sub-quadratic** in live
/// lane count. This test originally SURFACED an O(lanes²) fork-edge loop — a per-lane
/// linear `nodes.iter_mut().find(...)` (branch.rs ~324-340) that measured a ~3.7x
/// N->2N ratio (clean quadratic) and dominated every other op by ~10x at 16k+ lanes.
/// Fixed by indexing each branch's first empty-parent node in a
/// `HashMap<branch_id, node_idx>` during the per-lane build (O(1) fork-edge lookup);
/// the ratio dropped to ~2.2x (linear). This test now LOCKS that in — it fails again
/// if the fork-edge loop (or anything else in `build_graph`) regresses to super-linear.
/// Kept at a bounded N so the guardrail stays cheap and memory-safe.
#[test]
fn r3_build_graph_scaling_is_sub_quadratic() {
    let n = std::env::var("BRANCH_SCALE_GRAPH_N").ok().and_then(|s| s.parse().ok()).unwrap_or(4_000);
    eprintln!("\n=== R3 build_graph scaling (branch.rs:255) ===");
    let (a, b, r) = scale(Op::BuildGraph, n);
    eprintln!(
        "build_graph N->2N ratio = {r:.2}x  (linear ~2.0, quadratic ~4.0)\n\
         If this asserts, the O(lanes^2) fork-edge loop is still present."
    );
    assert!(b < Duration::from_secs(30), "build_graph absurdly slow at 2N: {:?}", b);
    if a > SIGNAL {
        assert!(
            r < MAX_RATIO,
            "build_graph is super-linear ({r:.2}x > {MAX_RATIO}x) — R3 quadratic in the \
             fork-edge loop (branch.rs ~324-340) still present; see the test doc-comment"
        );
    }
}
