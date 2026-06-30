//! Branches as **scoped views over the shared log** (§2). asp always converges to
//! one state per branch; a branch is not a fork of the log but a visibility
//! predicate over it. Every [`LogRow`] carries the `branch_id` it was authored on;
//! a fold for branch `B` sees the rows tagged `B` plus its ancestors' rows up to
//! the fork point. Concurrent edits *within* a branch still auto-merge (the CRDT is
//! preserved per branch); rows on *different* branches are simply outside each
//! other's fold scope.
//!
//! This module is pure and wasm-safe: the predicate is a function of the rows and
//! the branch records, so the native engine and the in-memory wasm node compute
//! byte-identical scoped state.

use crate::log::{Kind, LogRow, MAIN_BRANCH_ID};
use crate::order::OrderKey;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// `site_id -> max seq held` at a moment in time (the existing version-vector
/// machinery). A branch's `fork_vv` is the parent branch's vector at the fork.
pub type VersionVector = BTreeMap<String, i64>;

/// A branch record (§2.1). Content-hashed `branch_id` is stable across devices;
/// the root branch is `main` (`parent = None`, `fork_vv = {}`). Records are synced
/// (§7, P4) but for now live in the local `branches` table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branch {
    pub branch_id: String,
    pub name: String,
    pub parent: Option<String>,
    pub fork_vv: VersionVector,
    pub created_lamport: u64,
    pub created_ts: i64,
    #[serde(default)]
    pub deleted: bool,
}

impl Branch {
    /// The root `main` branch every vault starts on.
    pub fn main() -> Branch {
        Branch {
            branch_id: MAIN_BRANCH_ID.to_string(),
            name: "main".to_string(),
            parent: None,
            fork_vv: VersionVector::new(),
            created_lamport: 0,
            created_ts: 0,
            deleted: false,
        }
    }

    /// Stable content-hash id for a new branch, derived from its lineage + fork
    /// point so two devices forking "the same" branch independently get DISTINCT
    /// ids (concurrent creation → two branches, §7), while a deterministic replay
    /// of the same inputs reproduces the id.
    pub fn derive_id(name: &str, parent: &str, fork_vv: &VersionVector, created_lamport: u64, site_id: &str) -> String {
        let mut parts: Vec<Vec<u8>> = vec![
            name.as_bytes().to_vec(),
            parent.as_bytes().to_vec(),
            created_lamport.to_be_bytes().to_vec(),
            site_id.as_bytes().to_vec(),
        ];
        for (s, q) in fork_vv {
            parts.push(s.as_bytes().to_vec());
            parts.push(q.to_be_bytes().to_vec());
        }
        let refs: Vec<&[u8]> = parts.iter().map(|p| p.as_slice()).collect();
        crate::oid::merkle_id(&refs)
    }
}

/// Validate a user-supplied branch name (§4.1). An empty / whitespace-only name
/// would create a branch that `resolve_branch` can never match by name, leaving
/// it addressable only by its raw content-hash id — so reject it at creation.
pub fn validate_branch_name(name: &str) -> crate::error::AspResult<()> {
    if name.trim().is_empty() {
        return Err(crate::error::AspError::Invalid("branch name must not be empty".into()));
    }
    Ok(())
}

/// The branch tree: `branch_id -> Branch`. A predicate built from it answers
/// "is row r visible on branch B?" for the whole log in one pass.
pub struct BranchSet {
    branches: HashMap<String, Branch>,
}

impl BranchSet {
    pub fn new(branches: impl IntoIterator<Item = Branch>) -> BranchSet {
        let mut m: HashMap<String, Branch> = branches.into_iter().map(|b| (b.branch_id.clone(), b)).collect();
        // The root `main` is always present, even before any branch record exists.
        m.entry(MAIN_BRANCH_ID.to_string()).or_insert_with(Branch::main);
        BranchSet { branches: m }
    }

    pub fn get(&self, id: &str) -> Option<&Branch> {
        self.branches.get(id)
    }

    /// The ancestor path from `branch_id` up to a root (or to a cycle/dangling
    /// break), nearest first: `[B, parent(B), …]`. Cycles are broken
    /// deterministically by stopping at the first already-seen branch (mirrors
    /// `fold_order`'s cycle handling — never loops, never panics on adversarial
    /// lineage, §8.3).
    fn ancestry(&self, branch_id: &str) -> Vec<&Branch> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut cur = self.branches.get(branch_id);
        while let Some(b) = cur {
            if !seen.insert(b.branch_id.as_str()) {
                break; // cycle — stop deterministically
            }
            out.push(b);
            cur = match &b.parent {
                Some(p) => self.branches.get(p.as_str()),
                None => None,
            };
        }
        out
    }

    /// A reusable visibility test for one target branch. Precomputes the ancestry
    /// path and, per branch on it, the position so a row's visibility is O(depth).
    pub fn visibility<'a>(&'a self, target: &str) -> Visibility<'a> {
        let path = self.ancestry(target);
        // branch_id -> index on the path (0 = target itself).
        let index: HashMap<&str, usize> = path.iter().enumerate().map(|(i, b)| (b.branch_id.as_str(), i)).collect();
        Visibility { path, index }
    }
}

/// Visibility predicate for a single target branch (§2.2). A row authored on a
/// branch at path index `k` is visible iff it passes the `fork_vv` gate of every
/// branch *above* it on the path (indices `0..k`).
pub struct Visibility<'a> {
    path: Vec<&'a Branch>,
    index: HashMap<&'a str, usize>,
}

impl Visibility<'_> {
    /// Is `r` visible on the target branch?
    pub fn sees(&self, r: &LogRow) -> bool {
        let Some(&k) = self.index.get(r.branch_id.as_str()) else {
            return false; // authored on a branch not in this lineage (sibling/unknown)
        };
        // r authored on path[k]; visible on the target (path[0]) iff for every
        // branch path[i], i in 0..k, the row is within that branch's fork point:
        // r.seq <= fork_vv[r.site_id] (a site absent at the fork → not an ancestor).
        for b in &self.path[..k] {
            let cap = b.fork_vv.get(&r.site_id).copied().unwrap_or(-1);
            if (r.seq as i64) > cap {
                return false;
            }
        }
        true
    }
}

/// Filter `rows` to those visible on `target` given `branches` (§2.3). Order
/// preserved. The fold then runs over exactly this scoped set.
pub fn visible_rows(rows: &[LogRow], branches: &BranchSet, target: &str) -> Vec<LogRow> {
    let vis = branches.visibility(target);
    rows.iter().filter(|r| vis.sees(r)).cloned().collect()
}

/// JSON-encode a branch record into the bytes carried by its `Kind::Branch` row's
/// result blob (§7). The row's `file_id` is the `branch_id`; multiple records for
/// one branch (create → rename → delete, possibly concurrent) converge by fold
/// order key. Same surface on native + wasm, so the synced branch set converges.
pub fn encode_branch_record(b: &Branch) -> Vec<u8> {
    serde_json::to_vec(b).unwrap_or_default()
}

/// Reconcile the synced branch set from the `Kind::Branch` records among `rows`
/// (§7): group by `file_id` (= `branch_id`), keep the highest-order-key record
/// (last-writer-wins on name/parent/deleted), and decode its blob via `blob`.
/// Tombstoned (`deleted`) branches are included — their rows persist for history;
/// callers filter for the live set. Deterministic on any arrival order.
pub fn reconcile_branches(rows: &[LogRow], blob: impl Fn(&str) -> Option<Vec<u8>>) -> Vec<Branch> {
    let mut best: HashMap<String, (OrderKey, Branch)> = HashMap::new();
    for r in rows {
        if r.kind != Kind::Branch {
            continue;
        }
        let Some(h) = &r.result_hash else { continue };
        let Some(bytes) = blob(h) else { continue };
        let Ok(b) = serde_json::from_slice::<Branch>(&bytes) else { continue };
        let key = OrderKey { lamport: r.lamport, site_id: r.site_id.clone(), id: r.id.clone() };
        match best.get(&r.file_id) {
            Some((k, _)) if *k >= key => {}
            _ => {
                best.insert(r.file_id.clone(), (key, b));
            }
        }
    }
    let mut out: Vec<Branch> = best.into_values().map(|(_, b)| b).collect();
    out.sort_by(|a, b| a.created_lamport.cmp(&b.created_lamport).then_with(|| a.branch_id.cmp(&b.branch_id)));
    out
}

/// One node in the branch/commit DAG (§4.5) — a coarsened "settle commit": a run
/// of rows authored close together on one branch, so the graph isn't one node per
/// keystroke. `parents` are commit ids (the prior commit on the lane, plus a fork
/// edge into the parent lane for a branch's first commit).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GraphNode {
    pub commit_id: String,
    pub branch_id: String,
    pub parents: Vec<String>,
    pub ts: i64,
    pub lamport: u64,
    pub label: String,
    /// Horizontal lane (0 = main); the UI draws one column per branch.
    pub lane: usize,
}

/// A branch lane in the graph.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GraphBranch {
    pub id: String,
    pub name: String,
    pub parent: Option<String>,
    pub head_commit: Option<String>,
    pub lane: usize,
    pub current: bool,
}

/// The full network graph: lanes (branches) + the commit DAG over them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: Vec<GraphNode>,
    pub branches: Vec<GraphBranch>,
}

/// Build the GitHub-network-style branch/commit DAG from the log + branch set
/// (§4.5, §6). Pure + wasm-safe so native and web render identically. Rows are
/// coarsened per branch into settle-commits (a new commit starts when the author
/// changes or the Lamport gap exceeds `COARSEN`), bounded to `cap` nodes per
/// branch (keeping the most recent) so it stays fast at thousands of commits.
/// `head` marks the checked-out lane.
pub fn build_graph(rows: &[LogRow], live_branches: &[Branch], head: &str, cap: usize) -> Graph {
    const COARSEN: u64 = 3; // Lamport gap that starts a new settle-commit.

    // Lanes: main first, then live branches by creation order. Deterministic.
    let mut lanes: Vec<Branch> = vec![Branch::main()];
    let mut ordered: Vec<Branch> = live_branches.iter().filter(|b| b.branch_id != MAIN_BRANCH_ID && !b.deleted).cloned().collect();
    ordered.sort_by(|a, b| a.created_lamport.cmp(&b.created_lamport).then_with(|| a.branch_id.cmp(&b.branch_id)));
    lanes.extend(ordered);
    let lane_of: HashMap<String, usize> = lanes.iter().enumerate().map(|(i, b)| (b.branch_id.clone(), i)).collect();

    // Rows authored on each branch, in fold-order key, coarsened into commits.
    let mut nodes: Vec<GraphNode> = Vec::new();
    // branch_id -> its commit ids in order (for parent chaining + fork lookup).
    let mut chain: HashMap<String, Vec<(u64, String)>> = HashMap::new();

    // Group rows by branch in a single pass (was a full-log filter per lane,
    // i.e. O(lanes × rows); now O(rows)). Lanes are distinct branch ids, so each
    // group is consumed by exactly one lane.
    let mut by_branch: HashMap<&str, Vec<&LogRow>> = HashMap::new();
    for r in rows.iter().filter(|r| r.kind != Kind::Branch) {
        by_branch.entry(r.branch_id.as_str()).or_default().push(r);
    }

    for (lane, b) in lanes.iter().enumerate() {
        let mut mine: Vec<&LogRow> = by_branch.remove(b.branch_id.as_str()).unwrap_or_default();
        mine.sort_by(|x, y| {
            x.lamport.cmp(&y.lamport).then_with(|| x.site_id.cmp(&y.site_id)).then_with(|| x.id.cmp(&y.id))
        });
        // Coarsen into commit buckets.
        let mut commits: Vec<GraphNode> = Vec::new();
        let mut i = 0;
        while i < mine.len() {
            let start = i;
            let mut last = mine[i];
            i += 1;
            while i < mine.len()
                && mine[i].site_id == last.site_id
                && mine[i].lamport.saturating_sub(last.lamport) <= COARSEN
            {
                last = mine[i];
                i += 1;
            }
            let bucket = &mine[start..i];
            let label = commit_label(bucket);
            commits.push(GraphNode {
                commit_id: last.id.clone(),
                branch_id: b.branch_id.clone(),
                parents: Vec::new(), // filled below
                ts: last.ts,
                lamport: last.lamport,
                label,
                lane,
            });
        }
        // Cap: keep the most recent `cap` commits on this lane.
        if commits.len() > cap {
            commits = commits.split_off(commits.len() - cap);
        }
        // Chain parents within the lane.
        for w in 1..commits.len() {
            let prev = commits[w - 1].commit_id.clone();
            commits[w].parents.push(prev);
        }
        chain.insert(b.branch_id.clone(), commits.iter().map(|c| (c.lamport, c.commit_id.clone())).collect());
        nodes.extend(commits);
    }

    // Fork edges: a branch's FIRST commit's parent is the parent branch's commit
    // at the fork point (the highest-lamport parent commit at or before the fork).
    for b in lanes.iter() {
        if b.branch_id == MAIN_BRANCH_ID {
            continue;
        }
        let Some(parent_id) = &b.parent else { continue };
        let fork_at = b.fork_vv.values().copied().max().unwrap_or(0).max(0) as u64;
        let fork_lamport = b.created_lamport.max(fork_at);
        let parent_commit = chain.get(parent_id).and_then(|cs| {
            cs.iter().rfind(|(lam, _)| *lam <= fork_lamport).or_else(|| cs.last()).map(|(_, id)| id.clone())
        });
        if let (Some(first), Some(pc)) = (
            nodes.iter_mut().find(|n| n.branch_id == b.branch_id && n.parents.is_empty()),
            parent_commit,
        ) {
            first.parents.push(pc);
        }
    }

    let branches: Vec<GraphBranch> = lanes
        .iter()
        .map(|b| GraphBranch {
            id: b.branch_id.clone(),
            name: b.name.clone(),
            parent: b.parent.clone(),
            head_commit: chain.get(&b.branch_id).and_then(|cs| cs.last().map(|(_, id)| id.clone())),
            lane: *lane_of.get(&b.branch_id).unwrap_or(&0),
            current: b.branch_id == head,
        })
        .collect();

    Graph { nodes, branches }
}

/// A short human label for a coarsened commit: the file it touched (or "N changes").
fn commit_label(bucket: &[&LogRow]) -> String {
    let path = bucket.iter().rev().find_map(|r| r.path.clone());
    match (path, bucket.len()) {
        (Some(p), 1) => p,
        (Some(p), n) => format!("{p} +{}", n - 1),
        (None, 1) => "1 change".to_string(),
        (None, n) => format!("{n} changes"),
    }
}

/// The version vector of a row set: `site_id -> max seq`. A branch's `fork_vv` is
/// this over the parent branch's visible rows at the fork instant (§2.1).
pub fn version_vector_of(rows: &[LogRow]) -> VersionVector {
    let mut vv = VersionVector::new();
    for r in rows {
        let e = vv.entry(r.site_id.clone()).or_insert(-1);
        if (r.seq as i64) > *e {
            *e = r.seq as i64;
        }
    }
    vv
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{Kind, MergeClass};

    fn row(site: &str, seq: u64, branch: &str) -> LogRow {
        LogRow {
            site_id: site.into(),
            lamport: seq + 1,
            seq,
            file_id: format!("{site}-{seq}"),
            kind: Kind::Create,
            merge_class: MergeClass::Text,
            result_hash: Some("h".into()),
            path: Some(format!("{site}{seq}.md")),
            branch_id: branch.into(),
            ..LogRow::default()
        }
        .seal()
    }

    fn vv(pairs: &[(&str, i64)]) -> VersionVector {
        pairs.iter().map(|(s, q)| (s.to_string(), *q)).collect()
    }

    #[test]
    fn root_sees_only_main_rows() {
        let bs = BranchSet::new([]);
        let m = row("aa", 0, MAIN_BRANCH_ID);
        let b = row("aa", 1, "feature");
        assert!(bs.visibility(MAIN_BRANCH_ID).sees(&m));
        assert!(!bs.visibility(MAIN_BRANCH_ID).sees(&b), "main never sees a branch row");
    }

    #[test]
    fn ancestor_rows_visible_only_up_to_fork() {
        // feature forked from main at fork_vv {aa:1} (main rows seq 0,1 included).
        let feature = Branch {
            branch_id: "feature".into(),
            name: "feature".into(),
            parent: Some(MAIN_BRANCH_ID.into()),
            fork_vv: vv(&[("aa", 1)]),
            created_lamport: 3,
            created_ts: 0,
            deleted: false,
        };
        let bs = BranchSet::new([feature]);
        let vis = bs.visibility("feature");
        assert!(vis.sees(&row("aa", 0, MAIN_BRANCH_ID)), "pre-fork ancestor visible");
        assert!(vis.sees(&row("aa", 1, MAIN_BRANCH_ID)), "ancestor at the fork point visible");
        assert!(!vis.sees(&row("aa", 2, MAIN_BRANCH_ID)), "main row authored AFTER the fork is hidden");
        assert!(vis.sees(&row("aa", 5, "feature")), "own-branch row visible regardless of seq");
    }

    #[test]
    fn siblings_are_isolated() {
        let mk = |id: &str| Branch {
            branch_id: id.into(),
            name: id.into(),
            parent: Some(MAIN_BRANCH_ID.into()),
            fork_vv: vv(&[("aa", 0)]),
            created_lamport: 2,
            created_ts: 0,
            deleted: false,
        };
        let bs = BranchSet::new([mk("a"), mk("b")]);
        let on_a = row("zz", 9, "a");
        assert!(bs.visibility("a").sees(&on_a));
        assert!(!bs.visibility("b").sees(&on_a), "branch b never sees branch a's rows");
        assert!(!bs.visibility(MAIN_BRANCH_ID).sees(&on_a), "main never sees a child's rows");
    }

    #[test]
    fn multi_level_lineage_gates_at_every_fork() {
        // main <- mid (fork {aa:2}) <- leaf (fork {aa:5, bb:0})
        let mid = Branch {
            branch_id: "mid".into(),
            name: "mid".into(),
            parent: Some(MAIN_BRANCH_ID.into()),
            fork_vv: vv(&[("aa", 2)]),
            created_lamport: 4,
            created_ts: 0,
            deleted: false,
        };
        let leaf = Branch {
            branch_id: "leaf".into(),
            name: "leaf".into(),
            parent: Some("mid".into()),
            fork_vv: vv(&[("aa", 5), ("bb", 0)]),
            created_lamport: 8,
            created_ts: 0,
            deleted: false,
        };
        let bs = BranchSet::new([mid, leaf]);
        let vis = bs.visibility("leaf");
        // A main row at seq 2 passes mid's gate (<=2) — visible on leaf.
        assert!(vis.sees(&row("aa", 2, MAIN_BRANCH_ID)));
        // A main row at seq 3 fails mid's gate (3 > 2) even though it'd pass leaf's.
        assert!(!vis.sees(&row("aa", 3, MAIN_BRANCH_ID)));
        // A mid row is gated only by leaf's fork_vv (aa:5): seq 5 passes, seq 6 is hidden.
        assert!(vis.sees(&row("aa", 5, "mid")));
        assert!(!vis.sees(&row("aa", 6, "mid")));
    }

    #[test]
    fn cyclic_lineage_does_not_loop() {
        let a = Branch { branch_id: "a".into(), name: "a".into(), parent: Some("b".into()), fork_vv: vv(&[]), created_lamport: 1, created_ts: 0, deleted: false };
        let b = Branch { branch_id: "b".into(), name: "b".into(), parent: Some("a".into()), fork_vv: vv(&[]), created_lamport: 1, created_ts: 0, deleted: false };
        let bs = BranchSet::new([a, b]);
        // Must terminate (cycle broken) and never panic.
        let _ = bs.visibility("a").sees(&row("aa", 0, "a"));
    }

    #[test]
    fn unknown_branch_id_row_is_invisible_not_a_panic() {
        let bs = BranchSet::new([]);
        let ghost = row("aa", 0, "no-such-branch");
        assert!(!bs.visibility(MAIN_BRANCH_ID).sees(&ghost));
    }

    #[test]
    fn build_graph_lanes_chain_and_fork_edges() {
        // main has two commits; a feature branch forks after the first and adds a
        // commit. The graph must place main on lane 0, feature on lane 1, chain
        // feature's commit to its predecessor, and draw a fork edge into main.
        let m0 = row("aa", 0, MAIN_BRANCH_ID); // lamport 1
        let mut m1 = row("aa", 1, MAIN_BRANCH_ID);
        m1.lamport = 10; // gap > COARSEN → a second commit
        let m1 = m1.clone().seal();
        let feature = Branch {
            branch_id: "feat".into(),
            name: "feature".into(),
            parent: Some(MAIN_BRANCH_ID.into()),
            fork_vv: vv(&[("aa", 0)]),
            created_lamport: 2,
            created_ts: 0,
            deleted: false,
        };
        let mut fb = row("aa", 0, "feat");
        fb.lamport = 5;
        let fb = fb.clone().seal();
        let g = build_graph(&[m0.clone(), m1.clone(), fb.clone()], &[feature], "feat", 100);

        // Two lanes: main (0) + feature (1).
        assert_eq!(g.branches.len(), 2);
        assert_eq!(g.branches.iter().find(|b| b.id == MAIN_BRANCH_ID).unwrap().lane, 0);
        let feat_lane = g.branches.iter().find(|b| b.id == "feat").unwrap();
        assert_eq!(feat_lane.lane, 1);
        assert!(feat_lane.current, "HEAD lane marked current");

        // main has two coarsened commits (the lamport gap split them).
        let main_nodes: Vec<_> = g.nodes.iter().filter(|n| n.branch_id == MAIN_BRANCH_ID).collect();
        assert_eq!(main_nodes.len(), 2);
        // feature's single commit forks off a main commit (non-empty parents).
        let feat_nodes: Vec<_> = g.nodes.iter().filter(|n| n.branch_id == "feat").collect();
        assert_eq!(feat_nodes.len(), 1);
        assert!(!feat_nodes[0].parents.is_empty(), "fork edge into the parent lane");
    }

    #[test]
    fn build_graph_fuzz_invariants_hold_and_is_permutation_invariant() {
        // build_graph has the most moving parts (coarsen → cap → chain → fork-edge
        // resolution) yet only one example test. Fuzz it: on random rows over an
        // adversarial branch set (incl. deleted/cyclic/dangling lineage and tiny
        // caps that force split_off), the graph must (a) never panic, (b) be a
        // function of the row *set* not its order, and (c) satisfy structural
        // invariants — every parent id resolves to a node, lanes are consistent,
        // and the per-lane cap is honoured. A regression in capping or fork lookup
        // (e.g. an edge into a commit that capping dropped) trips invariant (c).
        let live = vec![
            Branch { branch_id: "a".into(), name: "a".into(), parent: Some(MAIN_BRANCH_ID.into()), fork_vv: vv(&[("aa", 1)]), created_lamport: 2, created_ts: 0, deleted: false },
            Branch { branch_id: "b".into(), name: "b".into(), parent: Some("a".into()), fork_vv: vv(&[("aa", 4)]), created_lamport: 6, created_ts: 0, deleted: false },
            // A deleted lane (must be filtered out of lanes) and a dangling-parent lane.
            Branch { branch_id: "z".into(), name: "z".into(), parent: Some(MAIN_BRANCH_ID.into()), fork_vv: vv(&[]), created_lamport: 3, created_ts: 0, deleted: true },
            Branch { branch_id: "orphan".into(), name: "orphan".into(), parent: Some("no-such".into()), fork_vv: vv(&[("bb", 2)]), created_lamport: 7, created_ts: 0, deleted: false },
        ];
        let branch_ids = ["main", "a", "b", "z", "orphan", "ghost"];

        // Deterministic LCG (no Math.random / Date in this env).
        let mut state: u64 = 0x1234_5678_9abc_def0;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            state >> 33
        };
        let mut rows = Vec::new();
        for _ in 0..120u64 {
            let mut r = row(&format!("s{}", next() % 3), next() % 10, branch_ids[(next() as usize) % branch_ids.len()]);
            r.lamport = next() % 30; // independent lamports → exercises coarsening gaps
            rows.push(r.seal());
        }

        let norm = |g: &Graph| {
            let mut ns: Vec<(String, String, usize, Vec<String>)> = g
                .nodes
                .iter()
                .map(|n| {
                    let mut ps = n.parents.clone();
                    ps.sort();
                    (n.commit_id.clone(), n.branch_id.clone(), n.lane, ps)
                })
                .collect();
            ns.sort();
            let mut bs: Vec<(String, usize, bool, Option<String>)> =
                g.branches.iter().map(|b| (b.id.clone(), b.lane, b.current, b.head_commit.clone())).collect();
            bs.sort();
            (ns, bs)
        };

        for &head in &["main", "a", "b", "orphan", "deleted-head-z", "ghost"] {
            for &cap in &[1usize, 2, 3, 1000] {
                let g = build_graph(&rows, &live, head, cap);

                // Structural invariants.
                let ids: std::collections::HashSet<&str> = g.nodes.iter().map(|n| n.commit_id.as_str()).collect();
                let lane_of: HashMap<&str, usize> = g.branches.iter().map(|b| (b.id.as_str(), b.lane)).collect();
                let mut per_lane: HashMap<&str, usize> = HashMap::new();
                for n in &g.nodes {
                    *per_lane.entry(n.branch_id.as_str()).or_default() += 1;
                    assert_eq!(Some(&n.lane), lane_of.get(n.branch_id.as_str()), "node lane disagrees with its branch lane");
                    for p in &n.parents {
                        assert!(ids.contains(p.as_str()), "parent {p} of {} references a non-existent node (cap={cap}, head={head})", n.commit_id);
                    }
                }
                for (br, count) in &per_lane {
                    assert!(*count <= cap, "lane {br} has {count} commits > cap {cap}");
                }
                // Deleted branch `z` must never be a lane; main always is.
                assert!(g.branches.iter().all(|b| b.id != "z"), "deleted branch leaked into lanes");
                assert!(g.branches.iter().any(|b| b.id == MAIN_BRANCH_ID), "main lane missing");
                // At most one current lane, and it only exists when head is a real lane.
                assert!(g.branches.iter().filter(|b| b.current).count() <= 1, "more than one current lane");

                // Permutation invariance: shuffle rows, identical graph.
                let mut shuffled = rows.clone();
                shuffled.reverse();
                let rot = (next() as usize) % shuffled.len();
                shuffled.rotate_left(rot);
                let g2 = build_graph(&shuffled, &live, head, cap);
                assert_eq!(norm(&g), norm(&g2), "build_graph not permutation-invariant (cap={cap}, head={head})");
            }
        }
    }

    #[test]
    fn reconcile_branches_is_lww_and_order_invariant() {
        use crate::store::{BlobStore, MemBlobStore};
        let store = MemBlobStore::new();
        let mk_record = |branch_id: &str, name: &str, deleted: bool, lamport: u64, site: &str| {
            let b = Branch {
                branch_id: branch_id.into(),
                name: name.into(),
                parent: Some(MAIN_BRANCH_ID.into()),
                fork_vv: vv(&[("aa", 1)]),
                created_lamport: 5,
                created_ts: 0,
                deleted,
            };
            let h = store.put_blob(&encode_branch_record(&b)).unwrap();
            LogRow {
                site_id: site.into(),
                lamport,
                seq: lamport,
                file_id: branch_id.into(),
                kind: Kind::Branch,
                merge_class: MergeClass::Text,
                result_hash: Some(h),
                path: Some(name.into()),
                ..LogRow::default()
            }
            .seal()
        };
        // Branch x1: created, then renamed (higher lamport wins), then a CONCURRENT
        // rename at the same lamport from a different site (LWW by site_id/id).
        let r_create = mk_record("x1", "feature", false, 5, "aa");
        let r_rename = mk_record("x1", "feature-v2", false, 7, "aa");
        let r_concurrent = mk_record("x1", "feature-zz", false, 7, "zz");
        // Branch x2: a tombstone.
        let r_del = mk_record("x2", "gone", true, 6, "aa");
        let rows = vec![r_create, r_rename.clone(), r_concurrent.clone(), r_del];

        let get = |h: &str| store.get_blob(h).ok().flatten();
        let recs = reconcile_branches(&rows, get);
        let x1 = recs.iter().find(|b| b.branch_id == "x1").unwrap();
        // Highest order key (lamport 7) wins; among the two lamport-7 rows, site "zz"
        // > "aa", so feature-zz is the converged name.
        assert_eq!(x1.name, "feature-zz");
        assert!(recs.iter().find(|b| b.branch_id == "x2").unwrap().deleted);

        // Order-invariant: shuffle the records, same result.
        let mut shuffled = rows.clone();
        shuffled.reverse();
        let recs2 = reconcile_branches(&shuffled, get);
        assert_eq!(recs2.iter().find(|b| b.branch_id == "x1").unwrap().name, "feature-zz");
    }

    #[test]
    fn adversarial_lineage_and_branch_ids_never_panic_and_stay_deterministic() {
        // §8.3: arbitrary/garbage branch_ids, cyclic + dangling branch lineage, and
        // huge/odd fork_vvs must never panic the visibility fold and must stay
        // permutation-invariant (deterministic) — exactly the guarantees fold_order
        // gives for malformed causal DAGs, now for malformed branch DAGs.
        use crate::fold::compute_files;
        use crate::store::{BlobStore, MemBlobStore};
        let store = MemBlobStore::new();
        let h = store.put_blob(b"x\n").unwrap();
        // A few branch_ids: real lineage members + pure garbage.
        let labels = ["main", "a", "b", "cyc1", "cyc2", "dangling", "🗑\0garbage", ""];
        // Adversarial branch set: a<-b (ok), a self-parent, cyc1<->cyc2 cycle,
        // dangling -> unknown, plus huge fork seqs.
        let mut branches = vec![
            Branch { branch_id: "a".into(), name: "a".into(), parent: Some(MAIN_BRANCH_ID.into()), fork_vv: vv(&[("s0", 2)]), created_lamport: 1, created_ts: 0, deleted: false },
            Branch { branch_id: "b".into(), name: "b".into(), parent: Some("a".into()), fork_vv: vv(&[("s0", i64::MAX), ("ghost", 9)]), created_lamport: 2, created_ts: 0, deleted: false },
            Branch { branch_id: "cyc1".into(), name: "c1".into(), parent: Some("cyc2".into()), fork_vv: vv(&[("s1", -5)]), created_lamport: 3, created_ts: 0, deleted: false },
            Branch { branch_id: "cyc2".into(), name: "c2".into(), parent: Some("cyc1".into()), fork_vv: vv(&[]), created_lamport: 3, created_ts: 0, deleted: false },
            Branch { branch_id: "dangling".into(), name: "d".into(), parent: Some("no-such".into()), fork_vv: vv(&[("s2", 0)]), created_lamport: 4, created_ts: 0, deleted: false },
        ];
        branches.push(Branch { branch_id: "selfp".into(), name: "self".into(), parent: Some("selfp".into()), fork_vv: vv(&[]), created_lamport: 5, created_ts: 0, deleted: false });
        let bs = BranchSet::new(branches);

        // Deterministic LCG (no Math.random in this env).
        let mut state: u64 = 0xDEAD_BEEF;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            state >> 33
        };
        let mut rows = Vec::new();
        for i in 0..60u64 {
            let site = format!("s{}", next() % 4);
            let branch = labels[(next() as usize) % labels.len()];
            let seq = next() % 6;
            rows.push(
                LogRow {
                    site_id: site,
                    lamport: i + 1,
                    seq,
                    file_id: format!("f{}", next() % 8),
                    kind: Kind::Create,
                    merge_class: MergeClass::Text,
                    result_hash: Some(h.clone()),
                    path: Some(format!("p{}.md", next() % 5)),
                    branch_id: branch.into(),
                    ..LogRow::default()
                }
                .seal(),
            );
        }

        for target in ["main", "a", "b", "cyc1", "dangling", "selfp", "ghost-target"] {
            // Never panics; permutation-invariant.
            let v1 = compute_files(&store, &visible_rows(&rows, &bs, target)).unwrap();
            let mut shuffled = rows.clone();
            // simple reverse + rotate shuffle
            shuffled.reverse();
            let rot = (next() as usize) % shuffled.len().max(1);
            shuffled.rotate_left(rot);
            let v2 = compute_files(&store, &visible_rows(&shuffled, &bs, target)).unwrap();
            let norm = |fs: &[crate::store::FileRow]| {
                let mut x: Vec<_> = fs.iter().map(|f| (f.path.clone(), f.deleted, f.result_hash.clone())).collect();
                x.sort();
                x
            };
            assert_eq!(norm(&v1), norm(&v2), "target {target}: scoped fold not deterministic on adversarial input");
        }
    }
}
