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
use crate::log::{classify, Kind, LogRow};
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
        0
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
        self.rows
            .borrow()
            .iter()
            .filter(|r| r.file_id == file_id)
            .max_by(|a, b| {
                a.lamport.cmp(&b.lamport).then_with(|| a.site_id.cmp(&b.site_id)).then_with(|| a.id.cmp(&b.id))
            })
            .map(|r| r.id.clone())
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
        {
            let mut ids = self.row_ids.borrow_mut();
            let mut store = self.rows.borrow_mut();
            for wr in wrs {
                let is_new = ids.insert(wr.row.id.clone());
                if is_new {
                    store.push(wr.row.clone());
                    added += 1;
                }
                flags.push(is_new);
            }
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
            self.materialize()?;
        }
        Ok(added)
    }

    pub fn materialize(&self) -> AspResult<()> {
        // Fold directly off the borrowed log — cloning the whole Vec<LogRow> on
        // every write/integrate was pure waste (this is the wasm/Obsidian path).
        let files = compute_files(&self.blobs, &self.rows.borrow())?;
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
}
