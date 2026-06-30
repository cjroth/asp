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

use crate::log::{LogRow, MAIN_BRANCH_ID};
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
}
