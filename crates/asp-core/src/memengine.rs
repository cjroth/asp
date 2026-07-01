//! The wasm-safe in-memory engine (§Implementation: one engine, thin bindings).
//! A complete **thin node**: it holds a full local working copy in memory, authors
//! its own rows offline, integrates rows from a full node, and converges via the
//! *same* `compute_files` fold + `merge3` + `Session` as the native daemon — so a
//! browser/Obsidian node computes byte-identical state. It has no fs, no sockets,
//! and no SQLite; persistence and transport are the host's job (the SDK).
//!
//! It implements [`SessionVault`], so the identical handshake + catch-up runs
//! here as on native. Materialization is to an in-memory `path -> bytes` map
//! (the host renders it to its vault), not to disk.

use crate::authkeys::{decide_admission, expiry_from_ttl_days, AdmitCtx, AdmitDecision, AuthKey};
use crate::error::{AspError, AspResult};
use crate::fold::compute_files;
use crate::identity::Identity;
use crate::log::{classify, Kind, LogRow, MAIN_BRANCH_ID};
use crate::order::NodeId;
use crate::session::SessionVault;
use crate::store::{BlobStore, FileRow, MemBlobStore};
use crate::wire::{WireBlob, WireRow};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet};

fn now_unix() -> i64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
    }
    #[cfg(target_arch = "wasm32")]
    {
        // Browser wall clock (ms → s). A wasm node must stamp real timestamps or the
        // history timeline + point-in-time reads collapse to epoch 0.
        (js_sys::Date::now() / 1000.0) as i64
    }
}

fn random_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}

/// On-disk engine state for thin clients (see [`MemEngine::export_state`]):
/// the row log plus a deduplicated `hash -> bytes` blob table, msgpack-encoded
/// so blob bytes stay binary.
#[derive(serde::Serialize, serde::Deserialize)]
struct StateSnapshot {
    version: u32,
    vault_id: String,
    rows: Vec<LogRow>,
    blobs: BTreeMap<String, serde_bytes::ByteBuf>,
}

/// Bumped on any incompatible change to [`StateSnapshot`] — an old snapshot
/// then fails the version check (the host reconciles fresh) instead of
/// misparsing.
const STATE_SNAPSHOT_VERSION: u32 = 1;

pub struct MemEngine {
    identity: Identity,
    /// Per-vault authoring id, distinct from `identity` (the connection key), so
    /// two in-process vaults that share a device seed never collide on
    /// `(site_id, seq)` and defeat version-vector catch-up (§Security).
    site: String,
    blobs: MemBlobStore,
    rows: RefCell<Vec<LogRow>>,
    /// Every row id we hold — kept in lockstep with `rows` (append-only, so it
    /// only grows). Lets integrate dedup in O(1) instead of rescanning the whole
    /// log per row/page (a paged clone was O(N·pages) just on the dedup).
    row_ids: RefCell<HashSet<String>>,
    files: RefCell<Vec<FileRow>>,
    config: RefCell<BTreeMap<String, String>>,
    authorized: RefCell<Vec<AuthKey>>,
    /// Synced branch records (§7), reconciled LWW from the Kind::Branch rows — the
    /// same set every peer converges to. The implicit `main` is not stored here.
    branches: RefCell<Vec<crate::branch::Branch>>,
    /// The checked-out branch (HEAD) — per-device, never synced (§7).
    head: RefCell<String>,
    /// When set, `integrate_many` appends rows but defers the fold — the clone
    /// driver turns it on across the paged catch-up so the whole history folds
    /// ONCE at the end instead of re-folding the growing log on every page
    /// (O(N·pages) → O(N)). The driver folds + clears it on `Synced`.
    batch: Cell<bool>,
}

impl MemEngine {
    /// Create a fresh in-memory vault authoring as `identity` (the connection key)
    /// under a fresh per-vault `site_id`.
    pub fn create(identity: Identity, vault_id: &str) -> MemEngine {
        let mut cfg = BTreeMap::new();
        cfg.insert("vault_id".to_string(), vault_id.to_string());
        cfg.insert("tiebreak_key".to_string(), "lamport".to_string());
        MemEngine {
            identity,
            site: {
                use rand::RngCore;
                let mut b = [0u8; 32];
                rand::rngs::OsRng.fill_bytes(&mut b);
                hex::encode(b)
            },
            blobs: MemBlobStore::new(),
            rows: RefCell::new(Vec::new()),
            row_ids: RefCell::new(HashSet::new()),
            files: RefCell::new(Vec::new()),
            config: RefCell::new(cfg),
            authorized: RefCell::new(Vec::new()),
            branches: RefCell::new(Vec::new()),
            head: RefCell::new(MAIN_BRANCH_ID.to_string()),
            batch: Cell::new(false),
        }
    }

    /// Defer the fold during a bulk integrate (clone catch-up): append rows
    /// without re-folding; the caller folds once via `materialize` when done.
    pub fn set_batch(&self, on: bool) {
        self.batch.set(on);
    }

    /// The device connection key seed — used to bind this node's iroh endpoint
    /// (the device key *is* the iroh NodeId), distinct from the per-vault `site_id`.
    pub fn device_seed(&self) -> [u8; 32] {
        self.identity.seed()
    }

    /// The authoring identity (per-vault, distinct from the device connection key).
    pub fn site_id(&self) -> String {
        self.site.clone()
    }

    // ----- counters -----

    fn next_lamport(&self) -> u64 {
        self.rows.borrow().iter().map(|r| r.lamport).max().unwrap_or(0) + 1
    }

    fn next_seq(&self) -> u64 {
        let me = self.site_id();
        self.rows.borrow().iter().filter(|r| r.site_id == me).map(|r| r.seq as i64).max().map(|m| (m + 1) as u64).unwrap_or(0)
    }

    fn tip(&self, file_id: &str) -> Option<String> {
        let bs = self.branch_set();
        let vis = bs.visibility(&self.head_branch());
        self.rows
            .borrow()
            .iter()
            .filter(|r| r.file_id == file_id && vis.sees(r))
            .max_by(|a, b| {
                a.lamport.cmp(&b.lamport).then_with(|| a.site_id.cmp(&b.site_id)).then_with(|| a.id.cmp(&b.id))
            })
            .map(|r| r.id.clone())
    }

    // ----- branches (§2, §7) — parity with the native Engine -----

    /// The checked-out branch (HEAD).
    pub fn head_branch(&self) -> String {
        self.head.borrow().clone()
    }

    /// The checked-out branch id (alias used by hosts/bindings).
    pub fn current_branch(&self) -> String {
        self.head_branch()
    }

    fn branch_set(&self) -> crate::branch::BranchSet {
        crate::branch::BranchSet::new(self.branches.borrow().clone())
    }

    /// All live branches, `main` first.
    pub fn branches(&self) -> Vec<crate::branch::Branch> {
        let mut out = vec![crate::branch::Branch::main()];
        for b in self.branches.borrow().iter() {
            if !b.deleted {
                out.push(b.clone());
            }
        }
        out
    }

    /// The GitHub-network-style branch/commit DAG (§4.5), bounded to `cap` commits
    /// per lane — same builder as native, so web and desktop render identically.
    pub fn graph(&self, cap: usize) -> crate::branch::Graph {
        let live: Vec<crate::branch::Branch> = self.branches.borrow().iter().filter(|b| !b.deleted).cloned().collect();
        let mut g = crate::branch::build_graph(&self.rows.borrow(), &live, &self.head_branch(), cap);
        let lane_of: std::collections::HashMap<String, usize> =
            g.branches.iter().map(|b| (b.id.clone(), b.lane)).collect();
        g.tags = self
            .tags()
            .into_iter()
            .map(|t| crate::branch::GraphTag {
                lane: *lane_of.get(&t.branch_id).unwrap_or(&0),
                tag_id: t.tag_id,
                name: t.name,
                at_ts: t.at_ts,
                branch_id: t.branch_id,
            })
            .collect();
        g
    }

    /// Live (non-deleted) tags, reconciled LWW from the synced `Kind::Tag` records.
    pub fn tags(&self) -> Vec<crate::tag::Tag> {
        crate::tag::reconcile_tags(&self.rows.borrow(), |h| self.blobs.get_blob(h).ok().flatten())
            .into_iter()
            .filter(|t| !t.deleted)
            .collect()
    }

    /// Author a synced tag record — the wasm-side mirror of the native engine.
    pub fn author_tag_record(&self, t: &crate::tag::Tag) -> AspResult<WireRow> {
        let blob = crate::tag::encode_tag_record(t);
        let h = self.blobs.put_blob(&blob)?;
        let row = LogRow {
            site_id: self.site_id(),
            lamport: self.next_lamport(),
            seq: self.next_seq(),
            ts: now_unix(),
            file_id: t.tag_id.clone(),
            kind: Kind::Tag,
            result_hash: Some(h),
            path: Some(t.name.clone()),
            ..LogRow::default()
        }
        .seal();
        self.row_ids.borrow_mut().insert(row.id.clone());
        self.rows.borrow_mut().push(row.clone());
        self.wire(row)
    }

    /// Tag the point at wall-clock `at_ts` on the current branch with `name`.
    pub fn create_tag(&self, name: &str, at_ts: i64) -> AspResult<String> {
        crate::tag::validate_tag_name(name)?;
        let head = self.head_branch();
        let at_lamport = {
            let bs = self.branch_set();
            let vis = bs.visibility(&head);
            self.rows.borrow().iter().filter(|r| vis.sees(r) && r.ts <= at_ts).map(|r| r.lamport).max().unwrap_or(0)
        };
        let created_lamport = self.next_lamport();
        let tag_id = crate::tag::Tag::derive_id(name, at_ts, &head, created_lamport, &self.site_id());
        let t = crate::tag::Tag {
            tag_id: tag_id.clone(),
            name: name.to_string(),
            at_ts,
            at_lamport,
            branch_id: head,
            created_lamport,
            created_ts: now_unix(),
            deleted: false,
        };
        self.author_tag_record(&t)?;
        Ok(tag_id)
    }

    /// Soft-delete a tag (its rows remain for history).
    pub fn delete_tag(&self, tag_id: &str) -> AspResult<()> {
        let existing = crate::tag::reconcile_tags(&self.rows.borrow(), |h| self.blobs.get_blob(h).ok().flatten())
            .into_iter()
            .find(|t| t.tag_id == tag_id)
            .ok_or_else(|| AspError::NotFound(format!("no such tag: {tag_id}")))?;
        let tomb = crate::tag::Tag { deleted: true, ..existing };
        self.author_tag_record(&tomb)?;
        Ok(())
    }

    /// The version vector visible on `branch` right now — the fork point a child
    /// branch captures when forking "from here" (§2.1).
    pub fn visible_version_vector(&self, branch: &str) -> crate::branch::VersionVector {
        let bs = self.branch_set();
        let vis = bs.visibility(branch);
        let rows = self.rows.borrow();
        let scoped: Vec<LogRow> = rows.iter().filter(|r| vis.sees(r)).cloned().collect();
        crate::branch::version_vector_of(&scoped)
    }

    /// Rebuild the branch set from the synced Kind::Branch records (LWW).
    fn reconcile_branches(&self) {
        let recs = crate::branch::reconcile_branches(&self.rows.borrow(), |h| self.blobs.get_blob(h).ok().flatten());
        *self.branches.borrow_mut() = recs;
    }

    /// Author a synced branch record (§7) — the wasm-side mirror of the native
    /// engine: a Kind::Branch row whose result blob is the JSON-encoded Branch.
    pub fn author_branch_record(&self, b: &crate::branch::Branch) -> AspResult<WireRow> {
        let blob = crate::branch::encode_branch_record(b);
        let h = self.blobs.put_blob(&blob)?;
        let row = LogRow {
            site_id: self.site_id(),
            lamport: self.next_lamport(),
            seq: self.next_seq(),
            ts: now_unix(),
            file_id: b.branch_id.clone(),
            kind: Kind::Branch,
            result_hash: Some(h),
            path: Some(b.name.clone()),
            ..LogRow::default()
        }
        .seal();
        self.row_ids.borrow_mut().insert(row.id.clone());
        self.rows.borrow_mut().push(row.clone());
        self.reconcile_branches();
        self.wire(row)
    }

    /// Create a branch off `parent` capturing `fork_vv`. Returns its id.
    pub fn create_branch(&self, name: &str, parent: &str, fork_vv: crate::branch::VersionVector) -> AspResult<String> {
        crate::branch::validate_branch_name(name)?;
        if self.branch_set().get(parent).is_none() {
            return Err(AspError::NotFound(format!("no such parent branch: {parent}")));
        }
        let created_lamport = self.next_lamport();
        let branch_id = crate::branch::Branch::derive_id(name, parent, &fork_vv, created_lamport, &self.site_id());
        let b = crate::branch::Branch {
            branch_id: branch_id.clone(),
            name: name.to_string(),
            parent: Some(parent.to_string()),
            fork_vv,
            created_lamport,
            created_ts: now_unix(),
            deleted: false,
        };
        self.author_branch_record(&b)?;
        Ok(branch_id)
    }

    /// Switch HEAD and re-materialize the branch's scoped state.
    pub fn checkout(&self, branch_id: &str) -> AspResult<()> {
        if self.branch_set().get(branch_id).is_none() {
            return Err(AspError::NotFound(format!("no such branch: {branch_id}")));
        }
        *self.head.borrow_mut() = branch_id.to_string();
        self.materialize()
    }

    /// Edit-in-the-past ⇒ branch (§2.5): fork HEAD at wall-clock `t`, switch to it.
    pub fn fork_from_time(&self, name: &str, t: i64) -> AspResult<String> {
        let fork_vv = {
            let bs = self.branch_set();
            let vis = bs.visibility(&self.head_branch());
            let rows = self.rows.borrow();
            let scoped: Vec<LogRow> = rows.iter().filter(|r| vis.sees(r) && r.ts <= t).cloned().collect();
            crate::branch::version_vector_of(&scoped)
        };
        let head = self.head_branch();
        let id = self.create_branch(name, &head, fork_vv)?;
        self.checkout(&id)?;
        Ok(id)
    }

    /// Soft-delete a branch (§4.2); main cannot be deleted, deleting HEAD checks
    /// out the parent.
    pub fn delete_branch(&self, branch_id: &str) -> AspResult<()> {
        if branch_id == MAIN_BRANCH_ID {
            return Err(AspError::Invalid("cannot delete the main branch".into()));
        }
        let Some(mut b) = self.branch_set().get(branch_id).cloned() else {
            return Err(AspError::NotFound(format!("no such branch: {branch_id}")));
        };
        if self.head_branch() == branch_id {
            // Land on the nearest *live* ancestor — never another tombstone (parity
            // with the native engine, §7).
            let target = self.nearest_live_ancestor(&b);
            self.checkout(&target)?;
        }
        b.deleted = true;
        self.author_branch_record(&b)?;
        Ok(())
    }

    /// The nearest ancestor of `start` that is still live (not tombstoned),
    /// defaulting to `main`. Cycle- and dangling-safe.
    fn nearest_live_ancestor(&self, start: &crate::branch::Branch) -> String {
        let bs = self.branch_set();
        let mut seen = std::collections::HashSet::new();
        seen.insert(start.branch_id.clone());
        let mut cur = start.parent.clone();
        while let Some(id) = cur {
            if !seen.insert(id.clone()) {
                break; // cycle
            }
            if id == MAIN_BRANCH_ID {
                return id; // main is always live
            }
            match bs.get(&id) {
                Some(p) if !p.deleted => return p.branch_id.clone(),
                Some(p) => cur = p.parent.clone(),
                None => break, // dangling parent
            }
        }
        MAIN_BRANCH_ID.to_string()
    }

    fn current_for_path(&self, rel: &str) -> Option<FileRow> {
        self.files.borrow().iter().find(|f| !f.deleted && f.path == rel).cloned()
    }

    // ----- capture -----

    /// Author a create/edit for `rel`. Returns the row (with blobs), or None if
    /// the content is unchanged.
    // Author a write row but DON'T materialize — so a batch (commit_files /
    // record_writes) can fold once at the end instead of per file. Returns the
    // pushed row, or None if the bytes already match the current content.
    // (lamport/seq/parent read the log, which is updated here; only the
    // materialized `files` table is deferred, and each path is touched once per
    // batch so its base is the pre-batch state — correct.)
    fn write_row(&self, rel: &str, bytes: &[u8]) -> AspResult<Option<LogRow>> {
        let result_hash = self.blobs.put_blob(bytes)?;
        let (lamport, seq, ts) = (self.next_lamport(), self.next_seq(), now_unix());
        let row = match self.current_for_path(rel) {
            Some(cur) => {
                if cur.result_hash.as_deref() == Some(result_hash.as_str()) {
                    return Ok(None);
                }
                LogRow {
                    id: String::new(),
                    site_id: self.site_id(),
                    lamport,
                    seq,
                    ts,
                    file_id: cur.file_id.clone(),
                    kind: Kind::Edit,
                    merge_class: cur.merge_class,
                    parent: self.tip(&cur.file_id),
                    base_hash: cur.result_hash.clone(),
                    result_hash: Some(result_hash),
                    path: None,
                    branch_id: self.head_branch(),
                    merge_parent: None,
                    sig: vec![],
                }
                .seal()
            }
            None => LogRow {
                id: String::new(),
                site_id: self.site_id(),
                lamport,
                seq,
                ts,
                file_id: random_id(),
                kind: Kind::Create,
                merge_class: classify(rel, bytes),
                parent: None,
                base_hash: None,
                result_hash: Some(result_hash),
                path: Some(rel.to_string()),
                branch_id: self.head_branch(),
                merge_parent: None,
                sig: vec![],
            }
            .seal(),
        };
        self.rows.borrow_mut().push(row.clone());
        self.row_ids.borrow_mut().insert(row.id.clone());
        Ok(Some(row))
    }

    // Author a delete row without materializing (batch helper, see write_row).
    fn remove_row(&self, rel: &str) -> AspResult<Option<LogRow>> {
        let Some(cur) = self.current_for_path(rel) else { return Ok(None) };
        let (lamport, seq, ts) = (self.next_lamport(), self.next_seq(), now_unix());
        let row = LogRow {
            id: String::new(),
            site_id: self.site_id(),
            lamport,
            seq,
            ts,
            file_id: cur.file_id.clone(),
            kind: Kind::Delete,
            merge_class: cur.merge_class,
            parent: self.tip(&cur.file_id),
            base_hash: cur.result_hash.clone(),
            result_hash: None,
            path: None,
            branch_id: self.head_branch(),
            merge_parent: None,
            sig: vec![],
        }
        .seal();
        self.rows.borrow_mut().push(row.clone());
        self.row_ids.borrow_mut().insert(row.id.clone());
        Ok(Some(row))
    }

    pub fn record_write(&self, rel: &str, bytes: &[u8]) -> AspResult<Option<WireRow>> {
        match self.write_row(rel, bytes)? {
            Some(row) => {
                // Fast path: a linear local edit folds to exactly the new bytes for
                // this one file_id with no cross-file effect, so update just its
                // FileRow instead of re-folding the whole log. (Creates fall to the
                // full fold below, which resolves any path collision.)
                if row.kind == Kind::Edit {
                    self.apply_one_edit(&row);
                } else {
                    self.materialize()?;
                }
                Ok(Some(self.wire(row)?))
            }
            None => Ok(None),
        }
    }

    /// Update a single file's materialized row for a linear edit — byte-identical
    /// to what a full fold would produce for it (the edit was authored on the tip,
    /// so it's a linear apply: new hash + lamport + author, nothing else changes).
    fn apply_one_edit(&self, row: &LogRow) {
        if let Some(f) = self.files.borrow_mut().iter_mut().find(|f| f.file_id == row.file_id) {
            f.result_hash = row.result_hash.clone();
            f.lamport = row.lamport;
            f.site_id = row.site_id.clone();
            f.deleted = false;
        }
    }

    pub fn record_remove(&self, rel: &str) -> AspResult<Option<WireRow>> {
        match self.remove_row(rel)? {
            Some(row) => {
                self.materialize()?;
                Ok(Some(self.wire(row)?))
            }
            None => Ok(None),
        }
    }

    /// Author writes for a batch of files (create/edit), materializing the fold
    /// **once**. Write-only (no deletes) — the seam the host's startup reconcile
    /// uses to stage a whole vault without re-folding per file (O(n²) → O(n)).
    pub fn record_writes(&self, files: &BTreeMap<String, Vec<u8>>) -> AspResult<Vec<WireRow>> {
        let mut rows = Vec::new();
        for (path, bytes) in files {
            if let Some(row) = self.write_row(path, bytes)? {
                rows.push(row);
            }
        }
        if !rows.is_empty() {
            self.materialize()?;
        }
        rows.into_iter().map(|r| self.wire(r)).collect()
    }

    /// Author deletes for a batch of paths, materializing the fold **once** —
    /// the seam a host's startup reconcile uses to capture files deleted while
    /// the host app was closed (no events fire for those, and a write-only
    /// reconcile would let the peer's copy resurrect them). Unknown paths are
    /// skipped, like `record_remove`.
    pub fn record_removes(&self, paths: &[String]) -> AspResult<Vec<WireRow>> {
        let mut rows = Vec::new();
        for path in paths {
            if let Some(row) = self.remove_row(path)? {
                rows.push(row);
            }
        }
        if !rows.is_empty() {
            self.materialize()?;
        }
        rows.into_iter().map(|r| self.wire(r)).collect()
    }

    pub fn record_rename(&self, old: &str, new: &str) -> AspResult<Option<WireRow>> {
        let Some(cur) = self.current_for_path(old) else { return Ok(None) };
        let (lamport, seq, ts) = (self.next_lamport(), self.next_seq(), now_unix());
        let row = LogRow {
            id: String::new(),
            site_id: self.site_id(),
            lamport,
            seq,
            ts,
            file_id: cur.file_id.clone(),
            kind: Kind::Rename,
            merge_class: cur.merge_class,
            parent: self.tip(&cur.file_id),
            base_hash: cur.result_hash.clone(),
            result_hash: cur.result_hash.clone(),
            path: Some(new.to_string()),
            branch_id: self.head_branch(),
            merge_parent: None,
            sig: vec![],
        }
        .seal();
        self.rows.borrow_mut().push(row.clone());
        self.row_ids.borrow_mut().insert(row.id.clone());
        self.materialize()?;
        Ok(Some(self.wire(row)?))
    }

    /// Bring the working set to `desired` (whole-set commit) — used to seed from a
    /// host's current vault contents. Authors the necessary create/edit/delete
    /// rows. Returns authored rows.
    pub fn commit_files(&self, desired: &BTreeMap<String, Vec<u8>>) -> AspResult<Vec<WireRow>> {
        let mut rows = Vec::new();
        let current: Vec<String> = self.files.borrow().iter().filter(|f| !f.deleted).map(|f| f.path.clone()).collect();
        // Author every change against the pre-batch state, then fold ONCE — the
        // old loop re-folded the whole vault on every file (O(n²)).
        for (path, bytes) in desired {
            if let Some(row) = self.write_row(path, bytes)? {
                rows.push(row);
            }
        }
        for path in current {
            if !desired.contains_key(&path) {
                if let Some(row) = self.remove_row(&path)? {
                    rows.push(row);
                }
            }
        }
        if !rows.is_empty() {
            self.materialize()?;
        }
        rows.into_iter().map(|r| self.wire(r)).collect()
    }

    pub fn wire(&self, row: LogRow) -> AspResult<WireRow> {
        let mut blobs = Vec::new();
        for h in [row.base_hash.clone(), row.result_hash.clone()].into_iter().flatten() {
            if let Some(bytes) = self.blobs.get_blob(&h)? {
                if !blobs.iter().any(|b: &WireBlob| b.hash == h) {
                    blobs.push(WireBlob { hash: h, bytes });
                }
            }
        }
        Ok(WireRow { row, blobs })
    }

    // ----- integrate / fold -----

    pub fn integrate(&self, wr: &WireRow) -> AspResult<bool> {
        if !wr.row.id_valid() {
            return Err(AspError::Protocol("row id does not match its contents".into()));
        }
        for b in &wr.blobs {
            let h = self.blobs.put_blob(&b.bytes)?;
            if h != b.hash {
                return Err(AspError::Protocol("blob hash mismatch".into()));
            }
        }
        if !self.row_ids.borrow_mut().insert(wr.row.id.clone()) {
            return Ok(false); // already held
        }
        self.rows.borrow_mut().push(wr.row.clone());
        if wr.row.kind == Kind::Branch {
            self.reconcile_branches();
        }
        self.materialize()?;
        Ok(true)
    }

    /// Integrate a batch of wire rows, materializing the fold **once** at the
    /// end rather than per row. Integrating one-by-one re-folds the whole log on
    /// every row — O(n²) over a batch (a 3000-row catch-up / restore is ~10s);
    /// folding once is ~O(n log n). Validates every row up front so a bad row
    /// can't leave a half-integrated log. Returns a per-row flag: true where the
    /// row was newly added (the caller forwards those to other peers).
    pub fn integrate_many(&self, wrs: &[WireRow]) -> AspResult<Vec<bool>> {
        for wr in wrs {
            if !wr.row.id_valid() {
                return Err(AspError::Protocol("row id does not match its contents".into()));
            }
            for b in &wr.blobs {
                let h = self.blobs.put_blob(&b.bytes)?;
                if h != b.hash {
                    return Err(AspError::Protocol("blob hash mismatch".into()));
                }
            }
        }
        // Dedup against the maintained id-set (and repeats within the batch) in
        // O(1) per row — the old code rebuilt the set from the whole log on every
        // call, which over a paged catch-up was O(N·pages).
        let mut flags = Vec::with_capacity(wrs.len());
        let mut added = 0usize;
        let mut any_branch = false;
        {
            let mut ids = self.row_ids.borrow_mut();
            let mut store = self.rows.borrow_mut();
            for wr in wrs {
                let is_new = ids.insert(wr.row.id.clone());
                if is_new {
                    store.push(wr.row.clone());
                    added += 1;
                    any_branch |= wr.row.kind == Kind::Branch;
                }
                flags.push(is_new);
            }
        }
        if any_branch {
            self.reconcile_branches();
        }
        if added > 0 && !self.batch.get() {
            self.materialize()?;
        }
        Ok(flags)
    }

    // ----- persistence (thin-client state snapshot) -----

    /// Serialize the full engine state — every log row plus each referenced
    /// content blob stored **once** — as compact msgpack bytes. This is the
    /// persistable form for thin clients (the Obsidian plugin) that rebuild the
    /// engine each launch. The wire form (`rows_after` over an empty vector) is
    /// the wrong shape for persistence: it bundles base+result blobs *per row*,
    /// so an edit history duplicates content, and a JSON dump inflates every
    /// content byte to ~4 characters — both of which OOM a mobile WebView on a
    /// large vault.
    pub fn export_state(&self) -> AspResult<Vec<u8>> {
        let rows = self.rows.borrow().clone();
        let mut blobs: BTreeMap<String, serde_bytes::ByteBuf> = BTreeMap::new();
        for r in &rows {
            for h in [r.base_hash.as_ref(), r.result_hash.as_ref()].into_iter().flatten() {
                if !blobs.contains_key(h) {
                    if let Some(bytes) = self.blobs.get_blob(h)? {
                        blobs.insert(h.clone(), serde_bytes::ByteBuf::from(bytes));
                    }
                }
            }
        }
        let snap = StateSnapshot {
            version: STATE_SNAPSHOT_VERSION,
            vault_id: SessionVault::vault_id(self),
            rows,
            blobs,
        };
        rmp_serde::to_vec_named(&snap).map_err(|e| AspError::Protocol(e.to_string()))
    }

    /// Re-integrate a snapshot produced by [`export_state`]. Validates like
    /// `integrate` does — Merkle row ids and blob hashes — because the snapshot
    /// is host-supplied input (a corrupt or tampered state file must fail
    /// loudly, not poison the log). Adopts the snapshot's vault id when this
    /// engine has none; refuses a snapshot for a *different* vault. Returns the
    /// number of rows newly added (idempotent: re-importing yields 0).
    pub fn import_state(&self, bytes: &[u8]) -> AspResult<usize> {
        let snap: StateSnapshot =
            rmp_serde::from_slice(bytes).map_err(|e| AspError::Protocol(e.to_string()))?;
        if snap.version != STATE_SNAPSHOT_VERSION {
            return Err(AspError::Protocol(format!(
                "unsupported engine state snapshot version {}",
                snap.version
            )));
        }
        for r in &snap.rows {
            if !r.id_valid() {
                return Err(AspError::Protocol("state row id does not match its contents".into()));
            }
        }
        for (hash, b) in &snap.blobs {
            let h = self.blobs.put_blob(b)?;
            if &h != hash {
                return Err(AspError::Protocol("state blob hash mismatch".into()));
            }
        }
        let mine = SessionVault::vault_id(self);
        if !snap.vault_id.is_empty() {
            if mine.is_empty() {
                self.adopt_vault_id(&snap.vault_id)?;
            } else if mine != snap.vault_id {
                return Err(AspError::Protocol("state snapshot is for a different vault".into()));
            }
        }
        let mut added = 0usize;
        {
            let mut ids = self.row_ids.borrow_mut();
            let mut store = self.rows.borrow_mut();
            for r in snap.rows {
                if ids.insert(r.id.clone()) {
                    store.push(r);
                    added += 1;
                }
            }
        }
        if added > 0 {
            self.reconcile_branches();
            self.materialize()?;
        }
        Ok(added)
    }

    pub fn materialize(&self) -> AspResult<()> {
        // Fold the checked-out branch's visible rows (§2.3). On a single-branch
        // vault visible(main) is every row — byte-identical to before.
        let bs = self.branch_set();
        let vis = bs.visibility(&self.head_branch());
        let rows = self.rows.borrow();
        let scoped: Vec<LogRow> = rows.iter().filter(|r| vis.sees(r)).cloned().collect();
        let files = compute_files(&self.blobs, &scoped)?;
        *self.files.borrow_mut() = files;
        Ok(())
    }

    /// The materialized file rows (the fold's output) — surface-independent
    /// metadata for hosts that render more than `path -> bytes`: `merge_class`,
    /// `result_hash`, the stable `file_id`, and the `conflict` flag. Callers
    /// filter `deleted` as needed.
    pub fn files_detail(&self) -> Vec<FileRow> {
        self.files.borrow().clone()
    }

    /// The materialized working tree as `path -> bytes` (what the host renders).
    pub fn files_map(&self) -> AspResult<BTreeMap<String, Vec<u8>>> {
        let mut m = BTreeMap::new();
        for f in self.files.borrow().iter() {
            if f.deleted {
                continue;
            }
            if let Some(h) = &f.result_hash {
                m.insert(f.path.clone(), self.blobs.get_blob(h)?.unwrap_or_default());
            }
        }
        Ok(m)
    }

    pub fn read_file(&self, rel: &str) -> AspResult<Option<Vec<u8>>> {
        match self.current_for_path(rel).and_then(|f| f.result_hash) {
            Some(h) => self.blobs.get_blob(&h),
            None => Ok(None),
        }
    }

    // ----- time travel (branch-scoped PITR) — parity with the native engine so a
    // web node can scrub history + fork-on-edit-in-the-past exactly like desktop -----

    /// Content of `path` as the vault was at wall-clock `t` on the checked-out
    /// branch (`None` if it didn't exist then). Folds visible rows with `ts <= t`.
    pub fn file_at(&self, path: &str, t: i64) -> AspResult<Option<Vec<u8>>> {
        let bs = self.branch_set();
        let vis = bs.visibility(&self.head_branch());
        let rows: Vec<LogRow> = self.rows.borrow().iter().filter(|r| vis.sees(r) && r.ts <= t).cloned().collect();
        let files = crate::fold::compute_files(&self.blobs, &rows)?;
        for f in files {
            if !f.deleted && f.path == path {
                let bytes = match f.result_hash {
                    Some(h) => self.blobs.get_blob(&h)?.unwrap_or_default(),
                    None => Vec::new(),
                };
                return Ok(Some(bytes));
            }
        }
        Ok(None)
    }

    /// Restore `path` to its content as of `t` by recording it as a new edit on the
    /// current branch (the log stays append-only). No-op if it didn't exist then.
    pub fn restore_file_at(&self, path: &str, t: i64) -> AspResult<Option<WireRow>> {
        match self.file_at(path, t)? {
            Some(bytes) => self.record_write(path, &bytes),
            None => Ok(None),
        }
    }

    /// The append-only history as `(id, ts, lamport, kind, path, branch_id)`, path
    /// resolved from each file_id's latest path (edits/deletes carry none). Branch
    /// and tag records are metadata, not file-history events, so they're skipped.
    pub fn history(&self) -> Vec<(String, i64, u64, String, String, String)> {
        let mut latest: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut rows: Vec<LogRow> = self.rows.borrow().clone();
        rows.sort_by(|a, b| a.lamport.cmp(&b.lamport).then_with(|| a.site_id.cmp(&b.site_id)).then_with(|| a.id.cmp(&b.id)));
        let mut out = Vec::new();
        for r in rows {
            if let Some(p) = &r.path {
                latest.insert(r.file_id.clone(), p.clone());
            }
            if matches!(r.kind, Kind::Branch | Kind::Tag) {
                continue;
            }
            let path = r.path.clone().or_else(|| latest.get(&r.file_id).cloned()).unwrap_or_default();
            out.push((r.id, r.ts, r.lamport, r.kind.as_str().to_string(), path, r.branch_id));
        }
        out
    }

    pub fn row_count(&self) -> usize {
        self.rows.borrow().len()
    }

    // ----- auth -----

    pub fn authorize(&self, ssh_line: &str, expires_at: Option<u64>, never: bool, source: &str) -> AspResult<()> {
        let k = AuthKey::from_ssh(ssh_line, expires_at, never, now_unix() as u64, source)
            .ok_or_else(|| AspError::Invalid("not an ssh-ed25519 key line".into()))?;
        let mut set = self.authorized.borrow_mut();
        set.retain(|x| x.node_id != k.node_id);
        set.push(k);
        Ok(())
    }
}

impl SessionVault for MemEngine {
    fn node_id(&self) -> NodeId {
        self.identity.node_id()
    }
    fn vault_id(&self) -> String {
        self.config.borrow().get("vault_id").cloned().unwrap_or_default()
    }
    fn adopt_vault_id(&self, vault_id: &str) -> AspResult<()> {
        self.config.borrow_mut().insert("vault_id".to_string(), vault_id.to_string());
        Ok(())
    }
    fn version_vector(&self) -> AspResult<BTreeMap<String, i64>> {
        let mut vv = BTreeMap::new();
        for r in self.rows.borrow().iter() {
            let e = vv.entry(r.site_id.clone()).or_insert(-1i64);
            if (r.seq as i64) > *e {
                *e = r.seq as i64;
            }
        }
        Ok(vv)
    }
    fn rows_after_wire(&self, site: &str, after: i64) -> AspResult<Vec<WireRow>> {
        let rows: Vec<LogRow> = self
            .rows
            .borrow()
            .iter()
            .filter(|r| r.site_id == site && (r.seq as i64) > after)
            .cloned()
            .collect();
        let mut sorted = rows;
        sorted.sort_by_key(|r| r.seq);
        sorted.into_iter().map(|r| self.wire(r)).collect()
    }
    fn integrate(&self, wr: &WireRow) -> AspResult<bool> {
        MemEngine::integrate(self, wr)
    }
    fn integrate_many(&self, rows: &[WireRow]) -> AspResult<Vec<bool>> {
        MemEngine::integrate_many(self, rows)
    }
    fn is_pristine(&self) -> bool {
        self.rows.borrow().is_empty()
    }
    fn admit(&self, peer: &NodeId, ctx: &AdmitCtx) -> AspResult<()> {
        let peer_hex = peer.to_hex();
        let set = self.authorized.borrow();
        let existing = set.iter().find(|k| k.node_id == peer_hex).cloned();
        let empty = set.is_empty();
        drop(set);
        match decide_admission(existing.as_ref(), empty, ctx) {
            AdmitDecision::Admit => Ok(()),
            AdmitDecision::Insert(source) => {
                let exp = expiry_from_ttl_days(ctx.now_unix, ctx.default_ttl_days);
                let line = crate::identity::ssh_pubkey_string(peer, source);
                self.authorize(&line, Some(exp), false, source)?;
                Ok(())
            }
            AdmitDecision::Deny(why) => Err(AspError::AuthDenied(format!("{why}: {}", &peer_hex[..12.min(peer_hex.len())]))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_engine_branch_ops_and_isolation() {
        // SDK-surface parity: the wasm node creates/forks/checks-out/deletes
        // branches with the same isolation guarantees as the native engine.
        let e = MemEngine::create(Identity::from_seed(&[3; 32]), "v");
        e.record_write("a.md", b"m1\n").unwrap().unwrap();
        assert_eq!(e.current_branch(), MAIN_BRANCH_ID);
        let b = e.fork_from_time("feature", i64::MAX).unwrap();
        assert_eq!(e.current_branch(), b);
        assert_eq!(e.read_file("a.md").unwrap().as_deref(), Some(&b"m1\n"[..]));
        e.record_write("a.md", b"b2\n").unwrap().unwrap();
        e.record_write("only-branch.md", b"x\n").unwrap().unwrap();

        // main is isolated.
        e.checkout(MAIN_BRANCH_ID).unwrap();
        assert_eq!(e.read_file("a.md").unwrap().as_deref(), Some(&b"m1\n"[..]));
        assert!(e.read_file("only-branch.md").unwrap().is_none());
        // back to the branch.
        e.checkout(&b).unwrap();
        assert_eq!(e.read_file("a.md").unwrap().as_deref(), Some(&b"b2\n"[..]));

        // delete rules: main protected; deleting HEAD auto-checks-out main.
        assert!(e.delete_branch(MAIN_BRANCH_ID).is_err());
        e.delete_branch(&b).unwrap();
        assert_eq!(e.current_branch(), MAIN_BRANCH_ID);
        assert!(e.branches().iter().all(|x| x.branch_id != b));
    }

    #[test]
    fn mem_tags_history_and_time_travel_parity() {
        // Web parity: the wasm node tags moments, lists history, folds as-of a time,
        // and forks-on-edit-in-the-past exactly like the native engine.
        let e = MemEngine::create(Identity::from_seed(&[11; 32]), "v");
        e.record_write("a.md", b"v1\n").unwrap().unwrap();
        let t1 = e.history().last().unwrap().1; // ts of the create
        e.record_write("a.md", b"v2\n").unwrap().unwrap();

        // history() lists file events (no branch/tag rows), newest last.
        let h = e.history();
        assert!(h.iter().all(|(_, _, _, kind, _, _)| kind != "branch" && kind != "tag"));
        assert!(h.iter().any(|(_, _, _, _, p, _)| p == "a.md"));

        // Tag a moment; it's listed and does NOT appear as a history event.
        let (tid, _) = { let id = e.create_tag("v1-point", t1).unwrap(); (id, ()) };
        assert_eq!(e.tags().len(), 1);
        assert!(e.history().iter().all(|(_, _, _, kind, _, _)| kind != "tag"));

        // file_at folds as-of the timestamp. (Both writes land in the same wall-clock
        // second in-test, so we assert existence boundaries, not sub-second ordering.)
        assert!(e.file_at("a.md", t1 - 1).unwrap().is_none(), "file didn't exist before its create");
        assert!(e.file_at("a.md", i64::MAX).unwrap().is_some(), "file exists at/after its history");
        assert_eq!(e.file_at("a.md", i64::MAX).unwrap().as_deref(), Some(&b"v2\n"[..]));

        // Fork-on-edit-in-the-past: fork at the tagged instant; edits on the branch
        // don't touch main (the core isolation the auto-branch UX relies on).
        let b = e.fork_from_time("from-v1", i64::MAX).unwrap();
        e.record_write("a.md", b"branch-edit\n").unwrap().unwrap();
        e.checkout(MAIN_BRANCH_ID).unwrap();
        assert_eq!(e.read_file("a.md").unwrap().as_deref(), Some(&b"v2\n"[..]), "main untouched by the past-fork edit");
        e.checkout(&b).unwrap();
        assert_eq!(e.read_file("a.md").unwrap().as_deref(), Some(&b"branch-edit\n"[..]));

        e.delete_tag(&tid).unwrap();
        assert!(e.tags().is_empty());
    }

    #[test]
    fn mem_delete_head_lands_on_nearest_live_ancestor() {
        // Parity with the native engine: main <- a <- b on b; delete a then b →
        // HEAD skips the tombstoned a and lands on main, not on a deleted branch.
        let e = MemEngine::create(Identity::from_seed(&[7; 32]), "v");
        e.record_write("f.md", b"m\n").unwrap().unwrap();
        let a = e.fork_from_time("a", i64::MAX).unwrap();
        let b = e.fork_from_time("b", i64::MAX).unwrap();
        assert_eq!(e.current_branch(), b);
        e.delete_branch(&a).unwrap();
        assert_eq!(e.current_branch(), b, "deleting a non-HEAD branch must not move HEAD");
        e.delete_branch(&b).unwrap();
        assert_eq!(e.current_branch(), MAIN_BRANCH_ID, "must not be stranded on the deleted ancestor a");
    }

    #[test]
    fn mem_create_branch_validates_name_and_parent() {
        // Parity with the native engine: reject empty/whitespace names and unknown
        // parents instead of creating unaddressable / orphan branches.
        let e = MemEngine::create(Identity::from_seed(&[9; 32]), "v");
        e.record_write("a.md", b"v1\n").unwrap().unwrap();
        let head = e.head_branch();
        assert!(e.create_branch("", &head, Default::default()).is_err(), "empty name rejected");
        assert!(e.create_branch("  ", &head, Default::default()).is_err(), "whitespace name rejected");
        assert!(e.create_branch("x", "no-such-parent", Default::default()).is_err(), "unknown parent rejected");
        assert!(e.create_branch("ok", &head, Default::default()).is_ok());
    }

    #[test]
    fn mem_engine_authors_folds_and_reads() {
        let e = MemEngine::create(Identity::from_seed(&[1; 32]), "v1");
        e.record_write("a.md", b"hello\n").unwrap().unwrap();
        e.record_write("a.md", b"hello world\n").unwrap().unwrap();
        assert_eq!(e.read_file("a.md").unwrap().as_deref(), Some(&b"hello world\n"[..]));
        assert!(e.record_write("a.md", b"hello world\n").unwrap().is_none());
        e.record_remove("a.md").unwrap();
        assert!(e.read_file("a.md").unwrap().is_none());
    }

    /// The persistence snapshot round-trips the full engine state: a fresh
    /// engine that imports it folds to byte-identical files, holds the same
    /// log, and keeps the vault id. Re-import is idempotent (0 rows added).
    #[test]
    fn state_snapshot_roundtrips_and_is_idempotent() {
        let a = MemEngine::create(Identity::from_seed(&[1; 32]), "v1");
        a.record_write("a.md", b"one\n").unwrap();
        a.record_write("dir/b.md", b"two\n").unwrap();
        a.record_write("a.md", b"one edited\n").unwrap();
        a.record_rename("dir/b.md", "dir/c.md").unwrap();
        a.record_write("gone.md", b"bye\n").unwrap();
        a.record_remove("gone.md").unwrap();

        let snap = a.export_state().unwrap();
        let b = MemEngine::create(Identity::from_seed(&[2; 32]), "");
        let added = b.import_state(&snap).unwrap();
        assert_eq!(added, a.row_count(), "every row lands");
        assert_eq!(b.files_map().unwrap(), a.files_map().unwrap(), "byte-identical fold");
        assert_eq!(SessionVault::vault_id(&b), "v1", "vault id adopted from the snapshot");
        assert_eq!(b.import_state(&snap).unwrap(), 0, "re-import is a no-op");
    }

    /// The snapshot stores each blob ONCE. The wire form (`rows_after({})`)
    /// bundles base+result blobs per row, so an edit history duplicates large
    /// content — the snapshot must not (that duplication is what OOM'd mobile).
    #[test]
    fn state_snapshot_dedups_blobs_across_history() {
        let e = MemEngine::create(Identity::from_seed(&[1; 32]), "v1");
        let big_a = vec![b'a'; 200_000];
        let big_b = vec![b'b'; 200_000];
        // Edit back and forth: 5 rows, but only TWO distinct blobs.
        e.record_write("big.bin", &big_a).unwrap();
        e.record_write("big.bin", &big_b).unwrap();
        e.record_write("big.bin", &big_a).unwrap();
        e.record_write("big.bin", &big_b).unwrap();
        e.record_write("big.bin", &big_a).unwrap();

        let snap = e.export_state().unwrap();
        // Two blobs + rows + framing — comfortably under three blobs' worth.
        assert!(
            snap.len() < 3 * 200_000,
            "snapshot must hold each blob once (got {} bytes)",
            snap.len()
        );
        // And it still restores the full history.
        let b = MemEngine::create(Identity::from_seed(&[2; 32]), "");
        assert_eq!(b.import_state(&snap).unwrap(), 5);
        assert_eq!(b.read_file("big.bin").unwrap().unwrap(), big_a);
    }

    /// A snapshot is host-supplied input: corrupt bytes, a tampered row, and a
    /// snapshot from another vault must all fail loudly (not poison the log).
    #[test]
    fn state_snapshot_rejects_corrupt_or_foreign_state() {
        let e = MemEngine::create(Identity::from_seed(&[1; 32]), "v1");
        e.record_write("a.md", b"hello\n").unwrap();
        let snap = e.export_state().unwrap();

        // Truncated / garbage bytes.
        let b = MemEngine::create(Identity::from_seed(&[2; 32]), "");
        assert!(b.import_state(&snap[..snap.len() / 2]).is_err());
        assert!(b.import_state(b"not a snapshot").is_err());
        assert_eq!(b.row_count(), 0, "failed import leaves the log untouched");

        // A tampered row: re-encode with one row's site_id flipped → Merkle id
        // no longer matches its contents.
        let mut parsed: StateSnapshot = rmp_serde::from_slice(&snap).unwrap();
        parsed.rows[0].site_id = "ff".repeat(32);
        let tampered = rmp_serde::to_vec_named(&parsed).unwrap();
        assert!(b.import_state(&tampered).is_err());

        // A tampered blob: bytes no longer hash to their table key.
        let mut parsed: StateSnapshot = rmp_serde::from_slice(&snap).unwrap();
        let key = parsed.blobs.keys().next().unwrap().clone();
        parsed.blobs.insert(key, serde_bytes::ByteBuf::from(&b"swapped"[..]));
        let bad_blob = rmp_serde::to_vec_named(&parsed).unwrap();
        assert!(b.import_state(&bad_blob).is_err());

        // A future snapshot version (incompatible format change).
        let mut parsed: StateSnapshot = rmp_serde::from_slice(&snap).unwrap();
        parsed.version = STATE_SNAPSHOT_VERSION + 1;
        let future = rmp_serde::to_vec_named(&parsed).unwrap();
        assert!(b.import_state(&future).is_err());

        // A snapshot for a different vault.
        let other = MemEngine::create(Identity::from_seed(&[3; 32]), "v2");
        assert!(other.import_state(&snap).is_err(), "refuses a snapshot for another vault");
    }

    /// Batch deletes: one fold, unknown paths skipped, and the authored rows
    /// carry across the wire (a peer integrating them drops the files too).
    #[test]
    fn record_removes_batch_deletes_and_propagates() {
        let a = MemEngine::create(Identity::from_seed(&[1; 32]), "v1");
        a.record_write("keep.md", b"k\n").unwrap();
        a.record_write("x.md", b"x\n").unwrap();
        a.record_write("dir/y.md", b"y\n").unwrap();
        let b = MemEngine::create(Identity::from_seed(&[2; 32]), "v1");
        b.import_state(&a.export_state().unwrap()).unwrap();

        let rows = a
            .record_removes(&["x.md".into(), "dir/y.md".into(), "never-existed.md".into()])
            .unwrap();
        assert_eq!(rows.len(), 2, "unknown path authors nothing");
        assert!(a.read_file("x.md").unwrap().is_none());
        assert!(a.read_file("dir/y.md").unwrap().is_none());
        assert_eq!(a.read_file("keep.md").unwrap().as_deref(), Some(&b"k\n"[..]));

        for r in &rows {
            b.integrate(r).unwrap();
        }
        assert_eq!(a.files_map().unwrap(), b.files_map().unwrap(), "deletes propagate");
    }

    /// The wasm-safe MemEngine and a fresh MemEngine converge by exchanging wire
    /// rows — the in-process analogue of the cross-surface gate.
    #[test]
    fn two_mem_engines_converge() {
        let a = MemEngine::create(Identity::from_seed(&[1; 32]), "v1");
        let b = MemEngine::create(Identity::from_seed(&[2; 32]), "v1");
        let r1 = a.record_write("doc.md", b"l1\nl2\nl3\n").unwrap().unwrap();
        b.integrate(&r1).unwrap();
        // concurrent disjoint edits
        let ra = a.record_write("doc.md", b"L1\nl2\nl3\n").unwrap().unwrap();
        let rb = b.record_write("doc.md", b"l1\nl2\nL3\n").unwrap().unwrap();
        b.integrate(&ra).unwrap();
        a.integrate(&rb).unwrap();
        assert_eq!(a.files_map().unwrap(), b.files_map().unwrap(), "mem engines converge");
        assert_eq!(a.read_file("doc.md").unwrap().as_deref(), Some(&b"L1\nl2\nL3\n"[..]));
    }

    /// Guards the perf rewrite: integrating a catch-up in PAGES under batch mode
    /// (fold once at the end) must produce byte-identical state to a single
    /// integrate, to the source's own fold, and be fully idempotent — and the
    /// linear-edit fast-path must match a full re-fold.
    #[test]
    fn paged_batch_equals_single_fold_and_dedups() {
        // A source with creates, a linear edit (exercises the fast-path), a rename
        // and a delete — then collect every authored row in order.
        let src = MemEngine::create(Identity::from_seed(&[9; 32]), "v1");
        let mut all: Vec<WireRow> = Vec::new();
        for i in 0..40 {
            all.push(src.record_write(&format!("dir{}/f{i}.md", i % 5), format!("body {i}\n").as_bytes()).unwrap().unwrap());
        }
        all.push(src.record_write("dir0/f0.md", b"edited via fast-path\n").unwrap().unwrap());
        all.push(src.record_rename("dir1/f1.md", "dir1/renamed.md").unwrap().unwrap());
        all.push(src.record_remove("dir2/f2.md").unwrap().unwrap());

        // A: one batch.
        let a = MemEngine::create(Identity::from_seed(&[1; 32]), "v1");
        assert_eq!(a.integrate_many(&all).unwrap().iter().filter(|f| **f).count(), all.len());

        // B: paged under batch mode, fold once at the end (the clone path).
        let b = MemEngine::create(Identity::from_seed(&[2; 32]), "v1");
        b.set_batch(true);
        for page in all.chunks(7) {
            b.integrate_many(page).unwrap();
        }
        b.set_batch(false);
        b.materialize().unwrap();

        assert_eq!(a.files_map().unwrap(), b.files_map().unwrap(), "paged batch == single batch");
        assert_eq!(a.files_map().unwrap(), src.files_map().unwrap(), "== source's own fold (fast-path correct)");
        assert!(a.read_file("dir0/f0.md").unwrap().as_deref() == Some(&b"edited via fast-path\n"[..]));
        assert!(a.read_file("dir1/renamed.md").unwrap().is_some() && a.read_file("dir1/f1.md").unwrap().is_none());
        assert!(a.read_file("dir2/f2.md").unwrap().is_none());

        // Idempotency: re-integrating everything is a no-op (dedup via the id-set),
        // single + batch, in or out of batch mode.
        assert!(a.integrate_many(&all).unwrap().iter().all(|f| !*f), "batch re-integration adds nothing");
        assert!(!a.integrate(&all[0]).unwrap(), "single re-integration dedups");
        b.set_batch(true);
        assert!(b.integrate_many(&all).unwrap().iter().all(|f| !*f), "batch dedup holds in batch mode");
        b.set_batch(false);
        assert_eq!(a.files_map().unwrap(), src.files_map().unwrap(), "state unchanged after redundant integrates");
    }

    /// Fuzz the perf paths: random op sequences (create/edit/delete/rename, hitting
    /// the linear-edit fast-path) integrated in random-sized pages under batch mode
    /// must converge byte-identically to a single integrate and to the source's own
    /// fold, and stay idempotent. Seeded → deterministic.
    #[test]
    fn fuzz_random_ops_paged_batch_converges() {
        use rand::{Rng, SeedableRng};
        for seed in 0..16u64 {
            let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
            let src = MemEngine::create(Identity::from_seed(&[7; 32]), "v1");
            let mut paths: Vec<String> = Vec::new();
            let mut all: Vec<WireRow> = Vec::new();
            for _ in 0..70 {
                match rng.gen_range(0..5) {
                    // create-or-edit a (maybe new) path
                    0 | 1 => {
                        let p = if !paths.is_empty() && rng.gen_bool(0.5) {
                            paths[rng.gen_range(0..paths.len())].clone()
                        } else {
                            format!("d{}/f{}.md", rng.gen_range(0..4), rng.gen_range(0..40))
                        };
                        if let Some(r) = src.record_write(&p, format!("v{}", rng.gen_range(0..10_000)).as_bytes()).unwrap() {
                            all.push(r);
                            if !paths.contains(&p) {
                                paths.push(p);
                            }
                        }
                    }
                    // edit an existing path (linear fast-path)
                    2 => {
                        if !paths.is_empty() {
                            let p = paths[rng.gen_range(0..paths.len())].clone();
                            if let Some(r) = src.record_write(&p, format!("e{}", rng.gen_range(0..10_000)).as_bytes()).unwrap() {
                                all.push(r);
                            }
                        }
                    }
                    // delete
                    3 => {
                        if !paths.is_empty() {
                            let p = paths.remove(rng.gen_range(0..paths.len()));
                            if let Some(r) = src.record_remove(&p).unwrap() {
                                all.push(r);
                            }
                        }
                    }
                    // rename
                    _ => {
                        if !paths.is_empty() {
                            let i = rng.gen_range(0..paths.len());
                            let old = paths[i].clone();
                            let new = format!("d{}/r{}.md", rng.gen_range(0..4), rng.gen_range(0..40));
                            if !paths.contains(&new) {
                                if let Some(r) = src.record_rename(&old, &new).unwrap() {
                                    all.push(r);
                                    paths[i] = new;
                                }
                            }
                        }
                    }
                }
            }

            let single = MemEngine::create(Identity::from_seed(&[1; 32]), "v1");
            single.integrate_many(&all).unwrap();

            let paged = MemEngine::create(Identity::from_seed(&[2; 32]), "v1");
            paged.set_batch(true);
            let mut i = 0;
            while i < all.len() {
                let n = rng.gen_range(1..9).min(all.len() - i);
                paged.integrate_many(&all[i..i + n]).unwrap();
                i += n;
            }
            paged.set_batch(false);
            paged.materialize().unwrap();

            let want = src.files_map().unwrap();
            assert_eq!(single.files_map().unwrap(), want, "seed {seed}: single integrate == source fold");
            assert_eq!(paged.files_map().unwrap(), want, "seed {seed}: paged batch == source fold");
            assert!(single.integrate_many(&all).unwrap().iter().all(|f| !*f), "seed {seed}: idempotent");
            assert_eq!(single.files_map().unwrap(), want, "seed {seed}: unchanged after redundant integrate");
        }
    }
}
