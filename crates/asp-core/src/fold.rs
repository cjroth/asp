//! The deterministic fold (§Core model, §Clocks & ordering, §Renames). State is
//! the fold of the log in a **canonical order with two layers**:
//!
//! 1. **Causal layer (always on):** a row is folded only after the rows its
//!    `parent` depends on — a topological sort of the per-`file_id` DAG, so a
//!    diff's base is always present and nothing reorders across a real dependency.
//! 2. **Concurrent tiebreak:** among rows with no causal relation, order by
//!    `(lamport, site_id, id)` (v1 `tiebreak_key = lamport`).
//!
//! Implemented as Kahn's algorithm with a min-heap on the tiebreak key: a single
//! total order, identical on every node holding the same rows. Folding left-to-
//! right, each `file_id` runs a small state machine (3-way merge against the LCA
//! by `merge_class`); a later-in-order row is "theirs" and wins a same-region
//! contention. Live-path collisions are resolved deterministically with a ` (n)`
//! suffix (the identity-convergence gate, §Renames & file identity).

use crate::log::{Kind, LogRow, MergeClass};
use crate::merge::merge3;
use crate::order::OrderKey;
use crate::store::{BlobStore, FileRow};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

/// Pure canonical fold order: causal topological sort, concurrent ties by
/// `(lamport, site_id, id)`. Deterministic for any input permutation.
pub fn fold_order(rows: &[LogRow]) -> Vec<LogRow> {
    let present: HashSet<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    let index: HashMap<&str, usize> = rows.iter().enumerate().map(|(i, r)| (r.id.as_str(), i)).collect();

    // children[parent_idx] = [child_idx...]; indeg[idx] = unplaced parent count.
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); rows.len()];
    let mut indeg: Vec<usize> = vec![0; rows.len()];
    for (i, r) in rows.iter().enumerate() {
        if let Some(p) = &r.parent {
            if present.contains(p.as_str()) {
                let pi = index[p.as_str()];
                children[pi].push(i);
                indeg[i] += 1;
            }
        }
    }

    let key = |i: usize| {
        let r = &rows[i];
        OrderKey { lamport: r.lamport, site_id: r.site_id.clone(), id: r.id.clone() }
    };

    // Min-heap (Reverse) of ready rows by tiebreak key.
    let mut ready: BinaryHeap<Reverse<(OrderKey, usize)>> = BinaryHeap::new();
    for (i, &d) in indeg.iter().enumerate() {
        if d == 0 {
            ready.push(Reverse((key(i), i)));
        }
    }

    let mut out = Vec::with_capacity(rows.len());
    while let Some(Reverse((_, i))) = ready.pop() {
        out.push(rows[i].clone());
        for &c in &children[i] {
            indeg[c] -= 1;
            if indeg[c] == 0 {
                ready.push(Reverse((key(c), c)));
            }
        }
    }
    // Any rows left out (cycle — impossible with Merkle parents) appended stably.
    if out.len() != rows.len() {
        for (i, r) in rows.iter().enumerate() {
            if !out.iter().any(|o| o.id == r.id) {
                let _ = i;
                out.push(r.clone());
            }
        }
    }
    out
}

/// Per-`file_id` fold state.
struct FileState {
    content: Option<Vec<u8>>,
    content_hash: Option<String>,
    path: Option<String>,
    merge_class: MergeClass,
    deleted: bool,
    lamport: u64,
    site_id: String,
    conflict: bool,
    /// Fold-order index of the row that last set this file's path (create/rename)
    /// — the deterministic key for resolving live-path collisions.
    path_claim: usize,
    created: bool,
}

/// Fold the whole log into the materialized `files` set, writing any merged
/// blobs back to the store. Pure function of the rows + blobs.
pub fn compute_files(store: &dyn BlobStore, rows: &[LogRow]) -> crate::error::AspResult<Vec<FileRow>> {
    let ordered = fold_order(rows);
    let mut states: HashMap<String, FileState> = HashMap::new();

    let blob = |h: &Option<String>| -> Vec<u8> {
        match h {
            Some(h) => store.get_blob(h).ok().flatten().unwrap_or_default(),
            None => Vec::new(),
        }
    };

    for (idx, r) in ordered.iter().enumerate() {
        match r.kind {
            Kind::Create => {
                let content = blob(&r.result_hash);
                states.insert(
                    r.file_id.clone(),
                    FileState {
                        content: Some(content),
                        content_hash: r.result_hash.clone(),
                        path: r.path.clone(),
                        merge_class: r.merge_class,
                        deleted: false,
                        lamport: r.lamport,
                        site_id: r.site_id.clone(),
                        conflict: false,
                        path_claim: idx,
                        created: true,
                    },
                );
            }
            Kind::Edit => {
                let Some(st) = states.get_mut(&r.file_id) else { continue };
                if st.deleted {
                    continue; // remove-wins: a concurrent edit does not resurrect
                }
                let theirs = blob(&r.result_hash);
                let ours = st.content.clone().unwrap_or_default();
                if st.content_hash == r.base_hash {
                    // Authored on the current tip — linear apply.
                    st.content = Some(theirs);
                    st.content_hash = r.result_hash.clone();
                } else {
                    let base = blob(&r.base_hash);
                    let m = merge3(st.merge_class, &base, &ours, &theirs);
                    let h = store.put_blob(&m.bytes)?;
                    st.conflict |= m.conflict;
                    st.content = Some(m.bytes);
                    st.content_hash = Some(h);
                }
                st.lamport = r.lamport;
                st.site_id = r.site_id.clone();
            }
            Kind::Rename => {
                let Some(st) = states.get_mut(&r.file_id) else { continue };
                if st.deleted {
                    continue;
                }
                st.path = r.path.clone();
                st.path_claim = idx; // last rename wins by fold order
                st.lamport = r.lamport;
                st.site_id = r.site_id.clone();
            }
            Kind::Delete => {
                if let Some(st) = states.get_mut(&r.file_id) {
                    st.deleted = true;
                    st.content = None;
                    st.lamport = r.lamport;
                    st.site_id = r.site_id.clone();
                }
            }
            Kind::Reclass => {
                if let Some(st) = states.get_mut(&r.file_id) {
                    if !st.deleted {
                        st.merge_class = r.merge_class;
                        st.lamport = r.lamport;
                        st.site_id = r.site_id.clone();
                    }
                }
            }
        }
    }

    Ok(resolve_paths(states))
}

/// Resolve live-path collisions: among files claiming the same path, the lowest
/// fold-order claim keeps it; others get a deterministic ` (n)` suffix. Tombstones
/// are emitted too (deleted=1) so deletes are explicit, ordered rows.
fn resolve_paths(states: HashMap<String, FileState>) -> Vec<FileRow> {
    // Stable ordering of all states by (path_claim, file_id).
    let mut all: Vec<(&String, &FileState)> = states.iter().collect();
    all.sort_by(|a, b| {
        a.1.path_claim
            .cmp(&b.1.path_claim)
            .then_with(|| a.0.cmp(b.0))
    });

    let mut taken: HashSet<String> = HashSet::new();
    let mut out: Vec<FileRow> = Vec::new();

    // Pass 1 — content files. They claim paths (with ` (n)` suffixing among
    // themselves); a real file always wins a path over a directory entity.
    for (file_id, st) in all.iter() {
        if !st.created || st.merge_class == MergeClass::Dir {
            continue;
        }
        let base_path = st.path.clone().unwrap_or_default();
        if st.deleted {
            out.push(FileRow {
                file_id: (*file_id).clone(),
                path: base_path,
                result_hash: None,
                merge_class: st.merge_class,
                deleted: true,
                lamport: st.lamport,
                site_id: st.site_id.clone(),
                conflict: st.conflict,
            });
            continue;
        }
        let path = unique_path(&base_path, &taken);
        taken.insert(path.clone());
        out.push(FileRow {
            file_id: (*file_id).clone(),
            path,
            result_hash: st.content_hash.clone(),
            merge_class: st.merge_class,
            deleted: false,
            lamport: st.lamport,
            site_id: st.site_id.clone(),
            conflict: st.conflict,
        });
    }

    // Pass 2 — directory entities. Identity is by PATH (not file_id): same-path
    // dir entities (concurrent creates / recreate-after-delete) dedupe to one
    // live directory with no suffix; a dir whose path a real file took is dropped
    // (the file implies the folder).
    let mut dir_taken: HashSet<String> = HashSet::new();
    for (file_id, st) in all.iter() {
        if !st.created || st.merge_class != MergeClass::Dir || st.deleted {
            continue;
        }
        let path = st.path.clone().unwrap_or_default();
        if taken.contains(&path) || dir_taken.contains(&path) {
            continue;
        }
        dir_taken.insert(path.clone());
        out.push(FileRow {
            file_id: (*file_id).clone(),
            path,
            result_hash: None,
            merge_class: MergeClass::Dir,
            deleted: false,
            lamport: st.lamport,
            site_id: st.site_id.clone(),
            conflict: false,
        });
    }

    out.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.file_id.cmp(&b.file_id)));
    out
}

/// `todo.md` → `todo (1).md` → `todo (2).md` … until free.
fn unique_path(path: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(path) {
        return path.to_string();
    }
    let (stem, ext) = match path.rfind('.') {
        // keep a real extension, but not a leading dot (".gitignore")
        Some(i) if i > 0 && !path[i + 1..].contains('/') => (&path[..i], &path[i..]),
        _ => (path, ""),
    };
    let mut n = 1;
    loop {
        let candidate = format!("{stem} ({n}){ext}");
        if !taken.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{Kind, MergeClass};

    #[allow(clippy::too_many_arguments)]
    fn mkrow(site: &str, lamport: u64, seq: u64, file_id: &str, kind: Kind, parent: Option<&str>, base: Option<&str>, result: Option<&str>, path: Option<&str>) -> LogRow {
        LogRow {
            id: String::new(),
            site_id: site.into(),
            lamport,
            seq,
            ts: 0,
            file_id: file_id.into(),
            kind,
            merge_class: MergeClass::Text,
            parent: parent.map(|s| s.to_string()),
            base_hash: base.map(|s| s.to_string()),
            result_hash: result.map(|s| s.to_string()),
            path: path.map(|s| s.to_string()),
            sig: vec![],
        }
        .seal()
    }

    #[test]
    fn fold_order_is_permutation_invariant() {
        let s = crate::store::MemBlobStore::new();
        let hb = s.put_blob(b"hello\n").unwrap();
        let r1 = mkrow("aa", 1, 0, "f1", Kind::Create, None, None, Some(&hb), Some("a.md"));
        let r2 = mkrow("aa", 2, 1, "f1", Kind::Edit, Some(&r1.id), Some(&hb), Some(&s.put_blob(b"hello world\n").unwrap()), None);
        let r3 = mkrow("bb", 1, 0, "f2", Kind::Create, None, None, Some(&hb), Some("b.md"));

        let forward = fold_order(&[r1.clone(), r2.clone(), r3.clone()]);
        let shuffled = fold_order(&[r3.clone(), r2.clone(), r1.clone()]);
        let ids_f: Vec<_> = forward.iter().map(|r| r.id.clone()).collect();
        let ids_s: Vec<_> = shuffled.iter().map(|r| r.id.clone()).collect();
        assert_eq!(ids_f, ids_s);
        // r1 (parent) must precede r2.
        let p1 = ids_f.iter().position(|x| *x == r1.id).unwrap();
        let p2 = ids_f.iter().position(|x| *x == r2.id).unwrap();
        assert!(p1 < p2);
    }

    #[test]
    fn concurrent_same_path_create_splits_with_suffix() {
        let s = crate::store::MemBlobStore::new();
        let ha = s.put_blob(b"from A\n").unwrap();
        let hb = s.put_blob(b"from B\n").unwrap();
        let a = mkrow("aa", 1, 0, "fa", Kind::Create, None, None, Some(&ha), Some("todo.md"));
        let b = mkrow("bb", 1, 0, "fb", Kind::Create, None, None, Some(&hb), Some("todo.md"));
        let files = compute_files(&s, &[a, b]).unwrap();
        let paths: Vec<_> = files.iter().filter(|f| !f.deleted).map(|f| f.path.clone()).collect();
        assert!(paths.contains(&"todo.md".to_string()));
        assert!(paths.contains(&"todo (1).md".to_string()));
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn reclass_boundary_changes_merge_class_and_keeps_content() {
        // §The merge model: a `reclass` row seeds the new representation from the
        // file's current content and changes how *later* rows merge, without
        // retro-reinterpreting older rows.
        let s = crate::store::MemBlobStore::new();
        let h0 = s.put_blob(b"line\n").unwrap();
        let create = mkrow("aa", 1, 0, "f1", Kind::Create, None, None, Some(&h0), Some("a.txt"));
        let mut reclass = mkrow("aa", 2, 1, "f1", Kind::Reclass, Some(&create.id), Some(&h0), Some(&h0), None);
        reclass.merge_class = MergeClass::Code;
        let reclass = reclass.seal();
        let files = compute_files(&s, &[create, reclass]).unwrap();
        let f = files.iter().find(|f| f.file_id == "f1").unwrap();
        assert_eq!(f.merge_class, MergeClass::Code, "reclass switched the routing class");
        assert_eq!(f.result_hash.as_deref(), Some(h0.as_str()), "content carried across the boundary");
    }

    #[test]
    fn equal_counter_same_site_is_total_by_id() {
        // Two concurrent edits with the SAME (lamport, site_id) but different ids
        // (a same-site replica race) must still fold to one deterministic result,
        // broken by content-addressed id — permutation-invariant.
        let s = crate::store::MemBlobStore::new();
        let h0 = s.put_blob(b"base\n").unwrap();
        let create = mkrow("aa", 1, 0, "f1", Kind::Create, None, None, Some(&h0), Some("a.md"));
        let h1 = s.put_blob(b"one\n").unwrap();
        let h2 = s.put_blob(b"two\n").unwrap();
        let e1 = mkrow("aa", 2, 1, "f1", Kind::Edit, Some(&create.id), Some(&h0), Some(&h1), None);
        let e2 = mkrow("aa", 2, 2, "f1", Kind::Edit, Some(&create.id), Some(&h0), Some(&h2), None);
        let fwd = compute_files(&s, &[create.clone(), e1.clone(), e2.clone()]).unwrap();
        let rev = compute_files(&s, &[e2, e1, create]).unwrap();
        assert_eq!(fwd, rev, "equal-counter same-site rows fold deterministically");
        assert_eq!(fwd.iter().filter(|f| !f.deleted).count(), 1);
    }

    #[test]
    fn concurrent_dir_entities_dedupe_by_path_no_suffix() {
        let s = crate::store::MemBlobStore::new();
        // Two nodes create the same empty dir (distinct random file_ids).
        let mut a = mkrow("aa", 1, 0, "da", Kind::Create, None, None, None, Some("notes/empty"));
        a.merge_class = MergeClass::Dir;
        let a = a.seal();
        let mut b = mkrow("bb", 1, 0, "db", Kind::Create, None, None, None, Some("notes/empty"));
        b.merge_class = MergeClass::Dir;
        let b = b.seal();
        let files = compute_files(&s, &[a, b]).unwrap();
        let dirs: Vec<_> = files.iter().filter(|f| !f.deleted && f.merge_class == MergeClass::Dir).collect();
        assert_eq!(dirs.len(), 1, "same-path dir entities dedupe to one");
        assert_eq!(dirs[0].path, "notes/empty");
        assert!(!files.iter().any(|f| f.path == "notes/empty (1)"), "no (n) suffix for dirs");
    }

    #[test]
    fn real_file_wins_path_over_dir_entity() {
        let s = crate::store::MemBlobStore::new();
        let hb = s.put_blob(b"i am a file\n").unwrap();
        let file = mkrow("aa", 1, 0, "f1", Kind::Create, None, None, Some(&hb), Some("x"));
        let mut dir = mkrow("bb", 1, 0, "d1", Kind::Create, None, None, None, Some("x"));
        dir.merge_class = MergeClass::Dir;
        let dir = dir.seal();
        let files = compute_files(&s, &[file, dir]).unwrap();
        let live: Vec<_> = files.iter().filter(|f| !f.deleted).collect();
        assert_eq!(live.len(), 1, "the file claims path x; the dir entity is dropped");
        assert_eq!(live[0].merge_class, MergeClass::Text);
    }

    #[test]
    fn delete_remove_wins_over_concurrent_edit() {
        let s = crate::store::MemBlobStore::new();
        let h0 = s.put_blob(b"v0\n").unwrap();
        let create = mkrow("aa", 1, 0, "f1", Kind::Create, None, None, Some(&h0), Some("a.md"));
        // B edits concurrently (parent = create), A deletes concurrently (parent = create).
        let h1 = s.put_blob(b"v1\n").unwrap();
        let edit = mkrow("bb", 2, 0, "f1", Kind::Edit, Some(&create.id), Some(&h0), Some(&h1), None);
        let del = mkrow("aa", 2, 1, "f1", Kind::Delete, Some(&create.id), Some(&h0), None, None);
        let files = compute_files(&s, &[create, edit, del]).unwrap();
        let live: Vec<_> = files.iter().filter(|f| !f.deleted).collect();
        assert!(live.is_empty(), "delete must dominate concurrent edit");
    }
}
