//! The high-level native engine: capture (FS event → log row), fold →
//! materialize to disk, derived git export, snapshots/restore (PITR), and
//! connection admission against the `authorized_keys` table. Thin over the pure
//! `fold`/`merge`/`store` core — all convergence logic lives there. The native
//! driver (the `asp` CLI) supplies file watching, debounce, and sockets.

use crate::authkeys::{decide_admission, expiry_from_ttl_days, AdmitCtx, AdmitDecision, AuthKey};
use crate::branch::{version_vector_of, Branch, BranchSet};
use crate::config::VaultConfig;
use crate::error::{AspError, AspResult};
use crate::fold::compute_files;
use crate::gitexport;
use crate::identity::Identity;
use crate::log::{Kind, LogRow, MergeClass, MAIN_BRANCH_ID};
use crate::order::{NodeId, OrderKey};
use crate::session::SessionVault;
use crate::sqlite::SqliteStore;
use crate::store::{BlobStore, FileRow};
use crate::wire::{WireBlob, WireRow};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Engine {
    pub root: PathBuf,
    pub asp_dir: PathBuf,
    pub git_dir: PathBuf,
    pub store: SqliteStore,
    pub identity: Identity,
    /// Ignore rules (`.aspignore` + the always-ignored dirs). Reloaded from disk
    /// by `materialize()` whenever `.aspignore` changes — locally or via a peer
    /// push — so the scope never freezes at the value loaded when the engine
    /// opened. Behind a `RefCell` for the same reason `batch` is a `Cell`:
    /// `Engine` is `!Sync` and only ever touched behind a `Mutex`.
    pub scope: std::cell::RefCell<crate::scope::Scope>,
    /// Per-vault authoring `site_id` (distinct from `identity`, the device key).
    pub site: String,
    /// When set, the per-`record_*` `materialize()` is suppressed — capture
    /// authors a whole batch (its diff is computed once up front) then folds
    /// ONCE at the end, turning an N-file capture from O(N²) into O(N).
    /// (`Engine` is `!Sync` and only ever touched behind a `Mutex`, so a `Cell`
    /// is safe here.)
    batch: std::cell::Cell<bool>,
    /// Incremental fold cache: the per-file_id states, so `materialize` re-folds
    /// only the files a change touched instead of the whole log. `None` until the
    /// first materialize builds it (and after anything that can't name what it
    /// touched, forcing a safe full rebuild). Authoritative EXCEPT for file_ids in
    /// `dirty`, which `materialize` re-folds before reading the cache.
    fold: std::cell::RefCell<Option<crate::FoldState>>,
    /// A tiny per-branch `FoldState` LRU (§10). `checkout` parks the outgoing
    /// branch's fold here and restores the target's if present, so switching among
    /// a few branches skips the full O(all-rows) refold and is near-instant. Any
    /// remote integrate clears it (a cached branch may have gained rows). Keyed by
    /// branch_id, most-recently-parked last; capped at `FOLD_CACHE_CAP`.
    fold_cache: std::cell::RefCell<Vec<(String, crate::FoldState)>>,
    /// file_ids whose log changed since the cache was last reconciled — drained
    /// and re-folded by `materialize`. Every row append records its file_id here.
    dirty: std::cell::RefCell<std::collections::HashSet<String>>,
    /// In-memory memo of content_hash → derived-git blob oid. The git oid is a
    /// pure function of the bytes, so this is a safe cache; it spares the git
    /// export a SQLite lookup per file on every settle (the dominant per-op cost
    /// at scale was ~3000 `git_oid_for` queries per export). Backed by the durable
    /// `git_blobs` table for cross-session reuse.
    git_oids: std::cell::RefCell<std::collections::HashMap<String, [u8; 20]>>,
    /// Optional notifier fired after a remote row set integrates (live push /
    /// catch-up). The desktop sets it to emit a Tauri event so the UI refreshes
    /// the instant a peer's change lands — the in-process equivalent of the web
    /// node's `on_change` callback, so the desktop screen isn't stuck waiting for
    /// its periodic re-read. `None` for the CLI / one-shot paths.
    change_listener: std::cell::RefCell<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>,
    /// Whether `materialize` re-exports the derived read-only git tree. It's
    /// O(all files) per call, so a host that never reads `.asp/git` (the desktop
    /// app) turns it off to keep edits O(changed). The CLI leaves it on so
    /// `asp git` stays current. Default on.
    export_git: std::cell::Cell<bool>,
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// How many parked per-branch folds the checkout LRU keeps (§10). Small: a person
/// hops among a couple of branches, and each entry holds a whole fold.
const FOLD_CACHE_CAP: usize = 4;

fn random_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}

/// The per-vault authoring `site_id` (§Security: single-writer protection). It is
/// **distinct per vault**, fresh at `init`/`clone`, so two replicas of one vault
/// on the SAME device (sharing the device key) never share a `site_id` — which
/// would make their concurrent edits collide on `(site_id, seq)` and silently
/// defeat version-vector catch-up. Persisted in the never-synced `.asp/site_id`;
/// the device key remains the connection/admission identity.
fn load_or_create_site_id(asp_dir: &Path) -> AspResult<String> {
    let path = asp_dir.join("site_id");
    if let Ok(s) = fs::read_to_string(&path) {
        let t = s.trim();
        if t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(t.to_string());
        }
    }
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    let id = hex::encode(b);
    fs::write(&path, &id)?;
    Ok(id)
}

pub use crate::log::classify;

/// A file's disk state for the reconcile diff: its content hash (reused from the
/// stat cache when the file is unchanged, else computed from a fresh read) and
/// its bytes — present only when we actually read it this scan (a changed/new
/// file, or a cache miss). Unchanged files carry just the cached hash, no bytes.
pub(crate) struct DiskEntry {
    hash: String,
    size: usize,
    bytes: Option<Vec<u8>>,
}

/// One walked file: its relative path, absolute path, and stat (mtime + size) —
/// the key the reconcile cache is matched on.
struct FileStat {
    rel: String,
    abs: PathBuf,
    mtime_ns: i64,
    size: i64,
}

/// A scan-progress sink: `(done, total, phase)` where `phase` is "scanning" |
/// "hashing" | "saving". Called (possibly from worker threads, so `Sync`) so a
/// host can drive a determinate progress bar instead of an indeterminate spinner
/// on a large vault's startup reconcile. A no-op by default (`|_, _, _| {}`).
pub type ProgressFn<'a> = dyn Fn(u64, u64, &str) + Sync + 'a;

/// Read + content-hash a set of files in parallel (the changed/new subset of a
/// scan). Returns (rel, mtime_ns, size, bytes, hash) per successfully read file;
/// unreadable files are dropped (treated as absent), as before. `on` is called
/// with the running hashed-file count so a large reconcile can show progress.
fn read_and_hash(files: &[FileStat], on: &ProgressFn) -> Vec<(String, i64, i64, Vec<u8>, String)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    let total = files.len() as u64;
    let done = AtomicU64::new(0);
    let read_chunk = |chunk: &[FileStat]| {
        chunk
            .iter()
            .filter_map(|f| {
                let r = fs::read(&f.abs).ok().map(|b| {
                    let h = crate::oid::content_hash(&b);
                    (f.rel.clone(), f.mtime_ns, f.size, b, h)
                });
                // Report every 64 files (and cheaply) so the bar advances without
                // flooding the host with a callback per file on a 28k-file vault.
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if n.is_multiple_of(64) || n == total {
                    on(n, total, "hashing");
                }
                r
            })
            .collect::<Vec<_>>()
    };
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    if workers <= 1 || files.len() < 64 {
        return read_chunk(files);
    }
    let chunk_size = files.len().div_ceil(workers).max(1);
    std::thread::scope(|s| {
        let handles: Vec<_> = files.chunks(chunk_size).map(|chunk| s.spawn(move || read_chunk(chunk))).collect();
        handles.into_iter().filter_map(|h| h.join().ok()).flatten().collect()
    })
}

impl Engine {
    /// Open or create the engine at a vault root, authoring as `identity` (the
    /// device connection key) under a per-vault `site_id`.
    pub fn open(root: &Path, identity: Identity) -> AspResult<Engine> {
        let asp_dir = root.join(".asp");
        fs::create_dir_all(&asp_dir)?;
        let git_dir = asp_dir.join("git");
        let store = SqliteStore::open(&asp_dir.join("asp.db"))?;
        let scope = Self::load_scope(root);
        let site = load_or_create_site_id(&asp_dir)?;
        let eng = Engine {
            root: root.to_path_buf(),
            asp_dir,
            git_dir,
            store,
            identity,
            scope: std::cell::RefCell::new(scope),
            site,
            batch: std::cell::Cell::new(false),
            fold: std::cell::RefCell::new(None),
            fold_cache: std::cell::RefCell::new(Vec::new()),
            dirty: std::cell::RefCell::new(std::collections::HashSet::new()),
            git_oids: std::cell::RefCell::new(std::collections::HashMap::new()),
            change_listener: std::cell::RefCell::new(None),
            export_git: std::cell::Cell::new(true),
        };
        Ok(eng)
    }

    /// `asp init`: create a fresh vault (genesis config + vault id).
    pub fn init(root: &Path, identity: Identity) -> AspResult<Engine> {
        let eng = Engine::open(root, identity)?;
        let cfg = VaultConfig::new(&eng.store);
        cfg.init_genesis(&random_id())?;
        gitexport::init_git_dir(&eng.git_dir)?;
        eng.materialize()?;
        Ok(eng)
    }

    fn load_scope(root: &Path) -> crate::scope::Scope {
        match fs::read_to_string(root.join(".aspignore")) {
            Ok(s) => crate::scope::Scope::parse(&s),
            Err(_) => crate::scope::Scope::default(),
        }
    }

    pub fn reload_scope(&self) {
        *self.scope.borrow_mut() = Self::load_scope(&self.root);
    }

    /// The authoring identity (per-vault, distinct from the device connection key).
    pub fn site_id(&self) -> String {
        self.site.clone()
    }

    // ---------------- branches (§2) ----------------

    /// The checked-out branch (HEAD). New rows are authored on it; the engine
    /// materializes its scoped state to disk.
    pub fn head_branch(&self) -> String {
        self.store.head().unwrap_or_else(|_| MAIN_BRANCH_ID.to_string())
    }

    /// The branch tree (the implicit `main` is always present).
    fn branch_set(&self) -> AspResult<BranchSet> {
        Ok(BranchSet::new(self.store.branches()?))
    }

    /// True for a vault that has never created a branch — HEAD is `main` and there
    /// are no records, so `visible(HEAD)` is every row: the fold/tip fast paths
    /// stay byte-identical to the pre-branching engine (§2.2 back-compat).
    fn single_branch(&self) -> bool {
        self.head_branch() == MAIN_BRANCH_ID && self.store.branches().map(|b| b.is_empty()).unwrap_or(true)
    }

    // ---------------- capture ----------------

    /// The current materialized content hash for a live path, if any. The `files`
    /// table holds HEAD's materialized state, so this is already branch-scoped.
    fn current_for_path(&self, rel: &str) -> AspResult<Option<FileRow>> {
        self.store.live_file_by_path(rel)
    }

    /// Highest-OrderKey row id for a file_id **within the checked-out branch's
    /// visible rows** (§2.4) — the deterministic branch-scoped tip a new row chains
    /// onto. On a single-branch vault this is the whole-log max (fast SQL path).
    fn tip(&self, file_id: &str) -> AspResult<Option<String>> {
        if self.single_branch() {
            return Ok(self
                .store
                .conn()
                .query_row(
                    "SELECT id FROM log WHERE file_id=?1 ORDER BY lamport DESC, site_id DESC, id DESC LIMIT 1",
                    rusqlite::params![file_id],
                    |r| r.get::<_, String>(0),
                )
                .ok());
        }
        let bs = self.branch_set()?;
        let vis = bs.visibility(&self.head_branch());
        let key = |r: &LogRow| OrderKey { lamport: r.lamport, site_id: r.site_id.clone(), id: r.id.clone() };
        Ok(self
            .store
            .rows_for_file(file_id)?
            .into_iter()
            .filter(|r| vis.sees(r))
            .max_by(|a, b| key(a).cmp(&key(b)))
            .map(|r| r.id))
    }

    fn next_counters(&self) -> AspResult<(u64, u64)> {
        let lamport = self.store.next_lamport(0)?;
        let seq = self.store.next_seq(&self.site_id())?;
        Ok((lamport, seq))
    }

    /// Record a create/edit for `rel` with new `bytes`. Returns the authored row
    /// (with blobs to ship) or None if the content is unchanged (self-write echo).
    pub fn record_write(&self, rel: &str, bytes: &[u8]) -> AspResult<Option<WireRow>> {
        if self.scope.borrow().ignored(rel) {
            return Ok(None);
        }
        let result_hash = self.store.put_blob(bytes)?;
        let (lamport, seq) = self.next_counters()?;
        let ts = now_unix() as i64;
        let cur_opt = self.current_for_path(rel)?;
        let row = match &cur_opt {
            Some(cur) => {
                if cur.result_hash.as_deref() == Some(result_hash.as_str()) {
                    return Ok(None); // no net change
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
                    parent: self.tip(&cur.file_id)?,
                    base_hash: cur.result_hash.clone(),
                    result_hash: Some(result_hash.clone()),
                    path: None,
                    branch_id: self.head_branch(),
                    merge_parent: None,
                    sig: vec![],
                }
                .seal()
            }
            None => {
                let merge_class = classify(rel, bytes);
                LogRow {
                    id: String::new(),
                    site_id: self.site_id(),
                    lamport,
                    seq,
                    ts,
                    file_id: random_id(),
                    kind: Kind::Create,
                    merge_class,
                    parent: None,
                    base_hash: None,
                    result_hash: Some(result_hash.clone()),
                    path: Some(rel.to_string()),
                    branch_id: self.head_branch(),
                    merge_parent: None,
                    sig: vec![],
                }
                .seal()
            }
        };
        self.store.append_row(&row)?;
        self.note_dirty(&row.file_id);
        // Fast path: a local linear edit on the tip changed exactly one file's
        // content (no merge, no path change). Reflect it incrementally instead of
        // re-folding the whole log. Skipped in a capture batch (one flush at the
        // end) and for Create / anything structural, which take the full fold.
        // (note_dirty above still records it, so a later full materialize re-folds
        // it into the cache — the fast path's files-table write is idempotent.)
        if !self.batch.get() && matches!(row.kind, Kind::Edit) {
            self.materialize_local_edit(&row.file_id, rel, &result_hash, bytes, row.lamport, &row.site_id)?;
        } else {
            self.materialize_unless_batched()?;
        }
        Ok(Some(self.wire(row)?))
    }

    /// Record a delete for `rel`. Returns the row, or None if no such live file.
    pub fn record_remove(&self, rel: &str) -> AspResult<Option<WireRow>> {
        let Some(cur) = self.current_for_path(rel)? else { return Ok(None) };
        let (lamport, seq) = self.next_counters()?;
        let row = LogRow {
            id: String::new(),
            site_id: self.site_id(),
            lamport,
            seq,
            ts: now_unix() as i64,
            file_id: cur.file_id.clone(),
            kind: Kind::Delete,
            merge_class: cur.merge_class,
            parent: self.tip(&cur.file_id)?,
            base_hash: cur.result_hash.clone(),
            result_hash: None,
            path: None,
            branch_id: self.head_branch(),
            merge_parent: None,
            sig: vec![],
        }
        .seal();
        self.store.append_row(&row)?;
        self.note_dirty(&row.file_id);
        self.materialize_unless_batched()?;
        Ok(Some(self.wire(row)?))
    }

    /// Record a rename `old` → `new` (path attribute change; content preserved).
    pub fn record_rename(&self, old: &str, new: &str) -> AspResult<Option<WireRow>> {
        let Some(cur) = self.current_for_path(old)? else { return Ok(None) };
        let (lamport, seq) = self.next_counters()?;
        let row = LogRow {
            id: String::new(),
            site_id: self.site_id(),
            lamport,
            seq,
            ts: now_unix() as i64,
            file_id: cur.file_id.clone(),
            kind: Kind::Rename,
            merge_class: cur.merge_class,
            parent: self.tip(&cur.file_id)?,
            base_hash: cur.result_hash.clone(),
            result_hash: cur.result_hash.clone(),
            path: Some(new.to_string()),
            branch_id: self.head_branch(),
            merge_parent: None,
            sig: vec![],
        }
        .seal();
        self.store.append_row(&row)?;
        self.note_dirty(&row.file_id);
        self.materialize_unless_batched()?;
        Ok(Some(self.wire(row)?))
    }

    /// Bundle a row with the blobs it references (base + result).
    pub fn wire(&self, row: LogRow) -> AspResult<WireRow> {
        let mut blobs = Vec::new();
        for h in [row.base_hash.clone(), row.result_hash.clone()].into_iter().flatten() {
            if let Some(bytes) = self.store.get_blob(&h)? {
                if !blobs.iter().any(|b: &WireBlob| b.hash == h) {
                    blobs.push(WireBlob { hash: h, bytes });
                }
            }
        }
        Ok(WireRow { row, blobs })
    }

    // ---------------- integrate ----------------

    /// Integrate a received row + its blobs. Returns true if newly added.
    /// Register a notifier fired whenever a remote row set integrates (see the
    /// `change_listener` field). Idempotent — the last one wins.
    pub fn set_change_listener(&self, cb: std::sync::Arc<dyn Fn() + Send + Sync>) {
        *self.change_listener.borrow_mut() = Some(cb);
    }

    fn notify_change(&self) {
        if let Some(cb) = self.change_listener.borrow().as_ref() {
            cb();
        }
    }

    pub fn integrate(&self, wr: &WireRow) -> AspResult<bool> {
        if !wr.row.id_valid() {
            return Err(AspError::Protocol("row id does not match its contents".into()));
        }
        for b in &wr.blobs {
            let h = self.store.put_blob(&b.bytes)?;
            if h != b.hash {
                return Err(AspError::Protocol("blob hash mismatch".into()));
            }
        }
        let added = self.store.append_row(&wr.row)?;
        if added {
            // A synced branch record (§7) updates the branch set before the fold, so
            // a freshly-arrived branch's fork_vv is in scope when HEAD folds.
            if wr.row.kind == Kind::Branch {
                self.reconcile_branches()?;
            }
            self.note_dirty(&wr.row.file_id);
            self.materialize()?;
            self.notify_change();
        }
        Ok(added)
    }

    /// Integrate a batch of rows, materializing (fold + write to disk + git
    /// export) **once** at the end. Per-row `integrate` re-materializes on every
    /// row — O(n²) over a large catch-up (the native daemon / hub serving a big
    /// clone). Validates every row up front. Returns a per-row flag (true = new).
    pub fn integrate_many(&self, wrs: &[WireRow]) -> AspResult<Vec<bool>> {
        for wr in wrs {
            if !wr.row.id_valid() {
                return Err(AspError::Protocol("row id does not match its contents".into()));
            }
        }
        // Persist bundled blobs (content-hash verified) then append rows, each in
        // ONE transaction — without it every INSERT auto-commits its own WAL
        // transaction, and at ~41k rows + ~41k blobs those ~82k fsync'd commits
        // dominated a large git clone's "saving" phase (see SqliteStore::append_rows
        // / put_blobs, and replace_files for the same pattern).
        self.store
            .put_blobs(wrs.iter().flat_map(|wr| wr.blobs.iter().map(|b| (b.hash.as_str(), b.bytes.as_slice()))))?;
        let flags = self.store.append_rows(wrs.iter().map(|wr| &wr.row))?;
        let mut any = false;
        let mut any_branch = false;
        for (wr, &added) in wrs.iter().zip(flags.iter()) {
            if added {
                any = true;
                any_branch |= wr.row.kind == Kind::Branch;
                self.note_dirty(&wr.row.file_id);
            }
        }
        if any_branch {
            self.reconcile_branches()?;
        }
        if any {
            // A remote row may land on (or before the fork of) a branch whose fold
            // we parked — invalidate the whole checkout LRU so the next switch to
            // any parked branch rebuilds from the log. Cheap: the cache is tiny and
            // integrate isn't the branch-switch hot path.
            self.fold_cache.borrow_mut().clear();
            // In batch mode (a paged clone/ingest: `set_batch(true)`) defer the
            // fold+materialize+notify so a large multi-page catch-up folds ONCE; the
            // caller materializes after `set_batch(false)`. Not batched → behave as
            // before (materialize + notify every call).
            if !self.batch.get() {
                self.materialize()?;
                self.notify_change();
            }
        }
        Ok(flags)
    }

    /// Enable/disable batch mode. While enabled, [`integrate_many`](Self::integrate_many)
    /// defers its fold + materialize + change-notify so a large **paged** catch-up
    /// (a git clone/ingest streamed page-by-page) folds only once. The caller MUST
    /// disable it and call [`materialize`](Self::materialize) afterwards. Mirrors the
    /// `batch` guard `capture_rescan` already uses for a whole-disk diff.
    pub fn set_batch(&self, on: bool) {
        self.batch.set(on);
    }

    /// One page of a site's rows (as wire rows, blobs bundled) after `after`,
    /// capped at `limit` — the streaming-catch-up cursor (see net.rs / Step::CatchUp).
    pub fn rows_after_wire_page(&self, site: &str, after: i64, limit: i64) -> AspResult<Vec<WireRow>> {
        self.store.rows_after_page(site, after, limit)?.into_iter().map(|r| self.wire(r)).collect()
    }

    // ---------------- fold → materialize ----------------

    /// Fold the log, write the materialized `files` table, render changed files
    /// to disk (atomic, self-write-suppressed), and export the derived git repo.
    /// Materialize unless we're mid-batch (see `batch`). Per-`record_*` callers
    /// use this so a capture of N files folds once, not once per file.
    fn materialize_unless_batched(&self) -> AspResult<()> {
        if self.batch.get() {
            return Ok(());
        }
        self.materialize().map(|_| ())
    }

    /// Mark a file_id's log as changed since the fold cache was last reconciled.
    /// Called at every row append; `materialize` drains this and re-folds only
    /// these files.
    fn note_dirty(&self, file_id: &str) {
        self.dirty.borrow_mut().insert(file_id.to_string());
    }

    /// Returns the materialized path → content-hash map (for echo suppression).
    ///
    /// Reconciles disk + the derived git store to the folded log. Every stage is
    /// kept O(changed) rather than O(vault): the fold itself re-folds only the
    /// file_ids touched since the last reconcile (the incremental `FoldState`
    /// cache); the files table is written by delta (`sync_files`); a content file
    /// is only (re)written when its hash changed (or it's new / went missing from
    /// disk); and the git export resolves blob oids through a `content_hash →
    /// git_oid` memo so an unchanged file is never re-read/re-hashed. A single edit
    /// on a 50k-file vault therefore touches one file's bytes, not all of them. (On
    /// the first materialize the cache + previous table are empty, so everything is
    /// "changed" and the full initial reconcile still happens.) Each file we write
    /// is stat'd into the mtime+size reconcile cache so the next reopen skips
    /// re-reading it; the git export is skipped entirely on a host that turned it
    /// off (the desktop app never reads `.asp/git`).
    pub fn materialize(&self) -> AspResult<BTreeMap<String, String>> {
        self.materialize_with_progress(None)
    }

    /// Like [`materialize`](Self::materialize), but reports `(files_written, total)`
    /// to `progress` as content files stream to disk — drives the clone "materialize"
    /// phase (writing a big working tree is otherwise ~20s of invisible time). `total`
    /// is the number of files that need (re)writing this pass; on a fresh clone that's
    /// every file. All other callers pass `None` and pay nothing.
    pub fn materialize_with_progress(
        &self,
        progress: Option<&dyn Fn(u64, u64)>,
    ) -> AspResult<BTreeMap<String, String>> {
        // Previous materialized live state: path -> content_hash, and the set of
        // all previously-live paths (content + dir) for stale removal.
        let prev = self.store.live_files()?;
        let mut prev_hash: std::collections::HashMap<String, Option<String>> = std::collections::HashMap::new();
        let mut old_live: Vec<String> = Vec::with_capacity(prev.len());
        for f in &prev {
            if f.deleted {
                continue;
            }
            old_live.push(f.path.clone());
            prev_hash.insert(f.path.clone(), f.result_hash.clone());
        }

        // Fold incrementally: re-fold only the file_ids touched since the cache was
        // last reconciled (`dirty`), reusing every other file's cached state, then
        // resolve paths across all of them. Falls back to a full fold the first
        // time (or whenever the cache is absent). `resolve_paths` runs over ALL
        // files, so a path-collision side effect (e.g. a delete promoting a
        // suffixed file) is handled even though only the deleted file was dirty —
        // the differential test pins this equal to a from-scratch fold.
        // Fold is scoped to the checked-out branch's visible rows (§2.3). On a
        // single-branch vault `visible(main)` is every row, so this is a no-op
        // filter — byte-identical to the pre-branching fold. The incremental cache
        // is per-checked-out-branch; `checkout` rebuilds it (clears `self.fold`).
        let head = self.head_branch();
        let bs = self.branch_set()?;
        let vis = bs.visibility(&head);
        let files = {
            let mut fg = self.fold.borrow_mut();
            let dirty: Vec<String> = self.dirty.borrow_mut().drain().collect();
            match fg.as_mut() {
                Some(fs) => {
                    fs.refold_files(&self.store, &dirty, |fid| {
                        Ok(self.store.rows_for_file(fid)?.into_iter().filter(|r| vis.sees(r)).collect())
                    })?;
                    fs.files()
                }
                None => {
                    let scoped: Vec<LogRow> = self.store.all_rows()?.into_iter().filter(|r| vis.sees(r)).collect();
                    let fs = crate::FoldState::from_rows(&self.store, &scoped)?;
                    let f = fs.files();
                    *fg = Some(fs);
                    f
                }
            }
        };
        self.store.sync_files(&files)?;

        // New desired set, built WITHOUT reading blobs: content files (path ->
        // content_hash, also the returned echo-suppression map) and dir entities.
        let mut hashes: BTreeMap<String, String> = BTreeMap::new();
        let mut desired_dirs: Vec<String> = Vec::new();
        for f in &files {
            if f.deleted {
                continue;
            }
            if f.merge_class == MergeClass::Dir {
                desired_dirs.push(f.path.clone());
            } else if let Some(h) = &f.result_hash {
                hashes.insert(f.path.clone(), h.clone());
            }
        }

        // Write content files whose hash CHANGED vs the previous materialize (or
        // are new / went missing from disk). An unchanged file present on disk is
        // skipped with a cheap `exists` stat — no content read, no write. (External
        // edits are reconciled through `rescan`/`capture_rescan`, not here; this
        // pass reflects the log to disk.) `.aspignore` (re)writes/removals still
        // trigger a scope reload below. Every file we actually write is stat'd into
        // `fs_updates` so the mtime+size reconcile cache stays warm and the next
        // reopen skips re-hashing it.
        let mut aspignore_changed = false;
        let mut fs_updates: Vec<(String, i64, i64, String)> = Vec::new();
        // Decide which files even need consideration on this thread (cheap `exists`
        // stat skips unchanged files without touching sqlite), and ensure every
        // parent dir up front — deduped, so `create_dir_all`'s stat/mkdir syscalls
        // aren't repeated per sibling on a big initial materialize (a 41k-file clone).
        // A dir could be removed externally between materializes, so this dedup is
        // per-call only and never persisted.
        let mut to_process: Vec<(String, PathBuf, String)> = Vec::with_capacity(hashes.len());
        {
            let mut made_dirs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
            for (path, h) in &hashes {
                let abs = self.root.join(path);
                let unchanged = prev_hash.get(path).map(|ph| ph.as_deref() == Some(h.as_str())).unwrap_or(false);
                if unchanged && abs.exists() {
                    continue;
                }
                if let Some(parent) = abs.parent() {
                    if !made_dirs.contains(parent) {
                        fs::create_dir_all(parent)?;
                        made_dirs.insert(parent.to_path_buf());
                    }
                }
                to_process.push((path.clone(), abs, h.clone()));
            }
        }

        // Overlap sqlite blob reads with disk writes: this (the sqlite-owning) thread
        // streams (path, abs, bytes, hash) tuples through a bounded channel to a pool
        // of writer threads that do the differ-check + atomic tmp-write/rename + stat.
        // Deterministic OUTCOME (same files, same contents, same skips); only the write
        // ORDER varies. The engine's sqlite handle is `!Sync`, so reads stay on this
        // thread; the bound gives backpressure so we never buffer the whole tree.
        if !to_process.is_empty() {
            use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
            use std::sync::mpsc::sync_channel;
            use std::sync::Mutex;

            const WRITER_THREADS: usize = 4;
            let (tx, rx) = sync_channel::<(String, PathBuf, Vec<u8>, String)>(WRITER_THREADS * 4);
            let rx = Mutex::new(rx);
            let fs_updates_out: Mutex<Vec<(String, i64, i64, String)>> = Mutex::new(Vec::new());
            let aspignore_hit = AtomicBool::new(false);
            let writer_err: Mutex<Option<AspError>> = Mutex::new(None);
            // Unique tmp suffix per write, so two siblings whose `with_extension` tmp
            // names would otherwise collide (same second) can't clobber each other.
            let tmp_seq = AtomicU64::new(0);

            let read_err: AspResult<()> = std::thread::scope(|scope| {
                for _ in 0..WRITER_THREADS {
                    scope.spawn(|| {
                        loop {
                            let item = { rx.lock().expect("rx poisoned").recv() };
                            let Ok((path, abs, bytes, h)) = item else { break };
                            let differs = match fs::read(&abs) {
                                Ok(cur) => cur != bytes,
                                Err(_) => true,
                            };
                            if !differs {
                                continue;
                            }
                            if path == ".aspignore" {
                                aspignore_hit.store(true, Ordering::Relaxed);
                            }
                            let seq = tmp_seq.fetch_add(1, Ordering::Relaxed);
                            let tmp = abs.with_extension(format!("asp-tmp-{}-{seq}", now_unix()));
                            let write_res = fs::write(&tmp, &bytes).and_then(|_| fs::rename(&tmp, &abs));
                            if let Err(e) = write_res {
                                let mut slot = writer_err.lock().expect("err poisoned");
                                if slot.is_none() {
                                    *slot = Some(AspError::from(e));
                                }
                                continue;
                            }
                            if let Ok(md) = fs::metadata(&abs) {
                                let mtime_ns = md
                                    .modified()
                                    .ok()
                                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                                    .map(|d| d.as_nanos() as i64)
                                    .unwrap_or(0);
                                fs_updates_out
                                    .lock()
                                    .expect("out poisoned")
                                    .push((path, mtime_ns, md.len() as i64, h));
                            }
                        }
                    });
                }
                // Producer: read blobs on this thread and feed the writers. Always drop
                // `tx` before returning so the writers' `recv()` unblocks (else the
                // scope join deadlocks). Progress is emitted here (single-threaded, in
                // stable order) as each blob is enqueued — the sqlite reads are the
                // bottleneck, so "enqueued" tracks real work closely enough for a bar.
                let mut err: AspResult<()> = Ok(());
                let total = to_process.len() as u64;
                if let Some(p) = progress {
                    p(0, total);
                }
                let mut done: u64 = 0;
                for (path, abs, h) in &to_process {
                    match self.store.get_blob(h) {
                        Ok(b) => {
                            if tx.send((path.clone(), abs.clone(), b.unwrap_or_default(), h.clone())).is_err() {
                                break; // all writers gone (they only exit on error)
                            }
                            done += 1;
                            if let Some(p) = progress {
                                p(done, total);
                            }
                        }
                        Err(e) => {
                            err = Err(e);
                            break;
                        }
                    }
                }
                drop(tx);
                err
            });

            read_err?;
            if let Some(e) = writer_err.into_inner().expect("err poisoned") {
                return Err(e);
            }
            if aspignore_hit.load(Ordering::Relaxed) {
                aspignore_changed = true;
            }
            fs_updates = fs_updates_out.into_inner().expect("out poisoned");
        }
        self.store.upsert_fs_stat(&fs_updates)?;

        // Materialize empty directories (the content-free dir entities).
        for path in &desired_dirs {
            let _ = fs::create_dir_all(self.root.join(path));
        }

        // Remove files/dirs that were live before but no longer are.
        let desired_dir_set: std::collections::HashSet<&String> = desired_dirs.iter().collect();
        for path in old_live {
            if !hashes.contains_key(&path) && !desired_dir_set.contains(&path) {
                if path == ".aspignore" {
                    aspignore_changed = true; // an ignore file was removed
                }
                let abs = self.root.join(&path);
                let _ = fs::remove_file(&abs); // no-op if it was a directory
                self.prune_empty_dirs(Some(abs.as_path()));
                self.prune_empty_dirs(abs.parent());
            }
        }

        // `.aspignore` changed on disk this pass (local edit or a peer push):
        // refresh the live ignore rules so newly-ignored paths stop syncing and
        // un-ignored ones resume, instead of the scope staying frozen at open.
        if aspignore_changed {
            self.reload_scope();
        }

        // Derived read-only git export at the settle boundary — O(all files) even
        // through the oid memo, so skipped on hosts that never read `.asp/git` (the
        // desktop app turns it off; the CLI keeps it on so `asp git` stays current).
        if self.export_git.get() {
            let derived_time = self.store.max_lamport()?;
            self.export_git_tree(&hashes, derived_time);
        }

        Ok(hashes)
    }

    /// Turn the derived git export off (a host that never reads `.asp/git`). With
    /// it off, `materialize` and the local-edit fast path skip the export entirely,
    /// keeping an edit on a big vault O(changed) instead of O(all files).
    pub fn set_git_export(&self, on: bool) {
        self.export_git.set(on);
    }

    /// Export the derived git tree from `entries` (path → content_hash for every
    /// live content file). Blob oids resolve through the content_hash → git_oid
    /// cache, so an unchanged file is never re-read/re-hashed into the git store.
    /// Best-effort: a git-store hiccup never fails a write.
    fn export_git_tree(&self, entries: &BTreeMap<String, String>, derived_time: u64) {
        let git_dir = &self.git_dir;
        let store = &self.store;
        let cache = &self.git_oids;
        let _ = gitexport::export(git_dir, entries, derived_time, |content_hash: &str| -> AspResult<[u8; 20]> {
            // In-memory memo first (deterministic fn of content), so a settle does
            // not issue a SQLite lookup per file.
            if let Some(oid) = cache.borrow().get(content_hash).copied() {
                return Ok(oid);
            }
            // Then the durable git_blobs cache (cross-session), else compute + persist.
            let oid = match store.git_oid_for(content_hash)? {
                Some(oid_hex) => match hex::decode(&oid_hex) {
                    Ok(b) if b.len() == 20 => {
                        let mut o = [0u8; 20];
                        o.copy_from_slice(&b);
                        o
                    }
                    _ => {
                        let bytes = store.get_blob(content_hash)?.unwrap_or_default();
                        gitexport::write_blob_object(git_dir, &bytes)?
                    }
                },
                None => {
                    let bytes = store.get_blob(content_hash)?.unwrap_or_default();
                    let o = gitexport::write_blob_object(git_dir, &bytes)?;
                    store.put_git_oid(content_hash, &hex::encode(o))?;
                    o
                }
            };
            cache.borrow_mut().insert(content_hash.to_string(), oid);
            Ok(oid)
        });
    }

    /// Incremental-materialize fast path for a LOCAL LINEAR EDIT: an `Edit` row
    /// authored on the current tip (so `result_hash` is the new content outright —
    /// no merge — and the path/class/liveness are unchanged). Such a write changes
    /// exactly one file, so we reflect it in place (one files-table row, one disk
    /// write) and refresh the derived git tree, instead of re-folding the whole log
    /// like `materialize`. The full fold stays the source of truth and the path for
    /// everything else (create/rename/delete, peer pushes, conflicts, capture
    /// batches); a later full materialize re-derives the identical state, so the
    /// two never disagree (the sync fuzzer asserts this every round).
    fn materialize_local_edit(&self, file_id: &str, rel: &str, result_hash: &str, bytes: &[u8], lamport: u64, site: &str) -> AspResult<()> {
        self.store.update_file_hash(file_id, result_hash, lamport, site)?;

        let abs = self.root.join(rel);
        let differs = match fs::read(&abs) {
            Ok(cur) => cur != bytes,
            Err(_) => true,
        };
        if differs {
            if let Some(parent) = abs.parent() {
                fs::create_dir_all(parent)?;
            }
            let tmp = abs.with_extension(format!("asp-tmp-{}", now_unix()));
            fs::write(&tmp, bytes)?;
            fs::rename(&tmp, &abs)?;
            // Keep the mtime+size reconcile cache warm for this one file so a
            // reopen doesn't re-hash it.
            if let Ok(md) = fs::metadata(&abs) {
                let mtime_ns = md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0);
                self.store
                    .upsert_fs_stat(&[(rel.to_string(), mtime_ns, md.len() as i64, result_hash.to_string())])?;
            }
        }
        if rel == ".aspignore" {
            self.reload_scope();
        }

        // Refresh the derived git tree from the now-updated files table. The new
        // local row carries the highest lamport, so it is the derived time. Skipped
        // entirely when git export is off (the desktop app), which is what keeps a
        // local edit O(1) — no O(all-files) scan/export per keystroke.
        if self.export_git.get() {
            let mut entries: BTreeMap<String, String> = BTreeMap::new();
            for f in self.store.live_files()? {
                if f.deleted || f.merge_class == MergeClass::Dir {
                    continue;
                }
                if let Some(h) = f.result_hash {
                    entries.insert(f.path, h);
                }
            }
            self.export_git_tree(&entries, lamport);
        }
        Ok(())
    }

    fn prune_empty_dirs(&self, mut dir: Option<&Path>) {
        while let Some(d) = dir {
            if d == self.root || !d.starts_with(&self.root) {
                break;
            }
            if fs::read_dir(d).map(|mut it| it.next().is_none()).unwrap_or(false) {
                let _ = fs::remove_dir(d);
                dir = d.parent();
            } else {
                break;
            }
        }
    }

    /// On launch, diff actual files on disk against `files` and emit changes for
    /// any divergence (§Capture: startup reconciliation). Disk is ground truth at
    /// boot. Returns authored rows to push.
    pub fn reconcile_startup(&self) -> AspResult<Vec<WireRow>> {
        self.capture_rescan()
    }

    /// Capture the current on-disk state against the materialized `files`,
    /// authoring create/edit/delete rows — and inferring **renames** by pairing a
    /// disappeared path with an appeared path of identical, non-trivial content
    /// (host-signal-free fallback; conservative to avoid empty/template matches,
    /// §Renames). Returns authored rows to push. Used by the `watch` debounce
    /// flush and by startup reconciliation.
    pub fn capture_rescan(&self) -> AspResult<Vec<WireRow>> {
        self.capture_rescan_progress(&|_, _, _| {})
    }

    /// Like [`capture_rescan`] but reports scan/hash/save progress to `on` so a host
    /// can show a determinate progress bar during a large vault's startup reconcile.
    pub fn capture_rescan_progress(&self, on: &ProgressFn) -> AspResult<Vec<WireRow>> {
        // Author the whole diff with per-row materialize deferred, then fold ONCE.
        // The diff below is computed from a single pre-pass snapshot, so deferring
        // is sound: each path is touched at most once, and `current_for_path`
        // inside `record_*` keeps reading that same pre-capture state. Reset the
        // flag even on error so a failed scan can't wedge the engine into batch
        // mode forever.
        // Disk is the input to a capture, and `.aspignore` lives on disk — so
        // refresh the scope from disk first. This is what makes an `.aspignore`
        // edited externally (another editor, a `git pull`, a script) take effect
        // on the next rescan instead of staying frozen at the open-time value.
        // (`materialize()` separately reloads it for the API/peer-push paths, where
        // the change arrives as a written row rather than a pre-existing disk file.)
        self.reload_scope();
        self.batch.set(true);
        let scanned = self.scan_disk_progress(on)?;
        let result = self.capture_rescan_inner(&scanned);
        self.batch.set(false);
        let authored = result?;
        // Nothing changed on disk → no new rows → the materialized files table and
        // working tree already match the log. Skip the (full-log) fold + disk diff
        // that `materialize` would otherwise redo every reopen — the dominant cost
        // of the startup reconcile on a big, unchanged vault.
        if !authored.is_empty() {
            on(0, authored.len() as u64, "saving");
            self.materialize()?;
            on(authored.len() as u64, authored.len() as u64, "saving");
        }
        Ok(authored)
    }

    fn capture_rescan_inner(&self, on_disk: &BTreeMap<String, DiskEntry>) -> AspResult<Vec<WireRow>> {
        // Content files vs directory entities are tracked separately: directories
        // are first-class, content-free entities (§Capture: empty directories).
        let (live_files, live_dirs): (Vec<FileRow>, Vec<FileRow>) =
            self.store.live_files()?.into_iter().partition(|f| f.merge_class != MergeClass::Dir);
        let live: BTreeMap<String, FileRow> = live_files.into_iter().map(|f| (f.path.clone(), f)).collect();

        let mut disappeared: Vec<String> = Vec::new();
        let mut changed: Vec<String> = Vec::new();
        for (path, f) in &live {
            match on_disk.get(path) {
                None => {
                    // `scan_disk` omits ignored paths, so a file that just became
                    // ignored (e.g. a new `.aspignore` rule) looks "disappeared".
                    // Don't tombstone one that still physically exists — that would
                    // delete it on every peer the moment it's ignored. Drop it from
                    // management instead; only a genuinely-removed file is deleted.
                    let still_on_disk =
                        self.scope.borrow().ignored(path) && self.root.join(path).exists();
                    if !still_on_disk {
                        disappeared.push(path.clone());
                    }
                }
                Some(de) => {
                    // Compare against the (cached or freshly computed) disk hash —
                    // no re-hashing here; the scan already did it for read files and
                    // skipped it for unchanged ones.
                    if f.result_hash.as_deref() != Some(de.hash.as_str()) {
                        changed.push(path.clone());
                    }
                }
            }
        }
        let mut appeared: Vec<String> =
            on_disk.keys().filter(|p| !live.contains_key(*p)).cloned().collect();

        // Index appeared paths by content hash for rename inference (unique,
        // non-trivial content only).
        let mut by_hash: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for a in &appeared {
            if let Some(de) = on_disk.get(a) {
                if de.size > 8 {
                    by_hash.entry(de.hash.clone()).or_default().push(a.clone());
                }
            }
        }

        // Folders that lost files this pass (renamed-away or deleted). Such a
        // folder is a rename/delete leftover, NOT an intentionally-empty folder —
        // so we must not preserve it as a `dir` entity (else a folder rename would
        // leave the old, now-empty tree behind on every node). Materialize prunes
        // it once its files are gone.
        let mut vacated: std::collections::HashSet<String> = std::collections::HashSet::new();
        for d in &disappeared {
            let mut p: &str = d;
            while let Some(i) = p.rfind('/') {
                p = &p[..i];
                vacated.insert(p.to_string());
            }
        }

        let mut authored = Vec::new();
        let mut consumed_appeared: std::collections::HashSet<String> = Default::default();
        let mut still_disappeared = Vec::new();
        for d in disappeared {
            let dh = live.get(&d).and_then(|f| f.result_hash.clone());
            let matched = dh
                .as_ref()
                .and_then(|h| by_hash.get(h))
                .and_then(|cands| cands.iter().find(|c| !consumed_appeared.contains(*c)).cloned());
            match matched {
                Some(a) => {
                    consumed_appeared.insert(a.clone());
                    if let Some(wr) = self.record_rename(&d, &a)? {
                        authored.push(wr);
                    }
                }
                None => still_disappeared.push(d),
            }
        }
        appeared.retain(|a| !consumed_appeared.contains(a));

        for path in changed.iter().chain(appeared.iter()) {
            // Authoring needs the bytes. A changed/new file was read this scan, so
            // they're in hand; the rare exception (an on-disk file the cache let us
            // skip yet the log doesn't have) reads on demand so we never miss it.
            let bytes: std::borrow::Cow<[u8]> = match on_disk.get(path).and_then(|d| d.bytes.as_ref()) {
                Some(b) => std::borrow::Cow::Borrowed(b.as_slice()),
                None => match fs::read(self.root.join(path)) {
                    Ok(b) => std::borrow::Cow::Owned(b),
                    Err(_) => continue,
                },
            };
            if let Some(wr) = self.record_write(path, &bytes)? {
                authored.push(wr);
            }
        }
        for d in still_disappeared {
            if let Some(wr) = self.record_remove(&d)? {
                authored.push(wr);
            }
        }

        // Directory entities: a physically-empty in-scope directory is a
        // first-class, content-free entity so the folder replicates without a
        // marker file (§Capture). Create one for each empty dir not yet tracked;
        // delete a tracked dir entity once the folder is gone or holds a real file
        // (no longer physically empty).
        let empty_dirs = self.empty_in_scope_dirs();
        let tracked: std::collections::HashSet<String> = live_dirs.iter().map(|f| f.path.clone()).collect();
        for path in &empty_dirs {
            if !tracked.contains(path) && !vacated.contains(path) {
                if let Some(wr) = self.record_dir_create(path)? {
                    authored.push(wr);
                }
            }
        }
        let empty_set: std::collections::HashSet<&String> = empty_dirs.iter().collect();
        for f in &live_dirs {
            // Delete a tracked dir entity when its folder is no longer empty / is
            // gone, OR when it was vacated this pass (files renamed/deleted away).
            // The `vacated` clause also cleans up a *stale* entity — e.g. one a
            // prior build wrongly authored for a rename leftover — propagating the
            // removal so the old folder disappears on every node.
            if !empty_set.contains(&f.path) || vacated.contains(&f.path) {
                if let Some(wr) = self.record_dir_delete(&f.file_id)? {
                    authored.push(wr);
                }
            }
        }
        Ok(authored)
    }

    /// Physically-empty in-scope directories under the root (leaf empties).
    fn empty_in_scope_dirs(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_empty_dirs(&self.root, &mut out);
        out
    }

    fn collect_empty_dirs(&self, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let rel = match path.strip_prefix(&self.root) {
                Ok(r) => r.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            if self.scope.borrow().ignored(&rel) {
                continue;
            }
            if fs::read_dir(&path).map(|mut it| it.next().is_none()).unwrap_or(false) {
                out.push(rel);
            } else {
                self.collect_empty_dirs(&path, out);
            }
        }
    }

    /// Author a `dir` create for an empty directory (content-free, random id so a
    /// recreated-after-delete folder is a fresh entity; same-path dirs dedupe in
    /// the fold by path — directories are identity-by-path, unlike files).
    fn record_dir_create(&self, rel: &str) -> AspResult<Option<WireRow>> {
        let (lamport, seq) = self.next_counters()?;
        let row = LogRow {
            id: String::new(),
            site_id: self.site_id(),
            lamport,
            seq,
            ts: now_unix() as i64,
            file_id: random_id(),
            kind: Kind::Create,
            merge_class: MergeClass::Dir,
            parent: None,
            base_hash: None,
            result_hash: None,
            path: Some(rel.to_string()),
            branch_id: self.head_branch(),
            merge_parent: None,
            sig: vec![],
        }
        .seal();
        self.store.append_row(&row)?;
        self.note_dirty(&row.file_id);
        self.materialize_unless_batched()?;
        Ok(Some(self.wire(row)?))
    }

    fn record_dir_delete(&self, file_id: &str) -> AspResult<Option<WireRow>> {
        let (lamport, seq) = self.next_counters()?;
        let row = LogRow {
            id: String::new(),
            site_id: self.site_id(),
            lamport,
            seq,
            ts: now_unix() as i64,
            file_id: file_id.to_string(),
            kind: Kind::Delete,
            merge_class: MergeClass::Dir,
            parent: self.tip(file_id)?,
            base_hash: None,
            result_hash: None,
            path: None,
            branch_id: self.head_branch(),
            merge_parent: None,
            sig: vec![],
        }
        .seal();
        self.store.append_row(&row)?;
        self.note_dirty(&row.file_id);
        self.materialize_unless_batched()?;
        Ok(Some(self.wire(row)?))
    }

    /// Walk the in-scope working tree into a `rel_path -> bytes` map.
    pub(crate) fn scan_disk_progress(&self, on: &ProgressFn) -> AspResult<BTreeMap<String, DiskEntry>> {
        // Walk + stat every file (cheap), then read the body ONLY of files whose
        // (mtime, size) changed since we last hashed them. On a big working tree
        // that the user hasn't touched externally, the startup reconcile becomes
        // ~one stat per file instead of reading + SHA-256-hashing every byte
        // (careerbot: 481MB/29k files → tens of seconds → ~one walk). The cache is
        // a pure local accelerator: a miss just re-reads, so it never affects
        // correctness or convergence.
        let mut files: Vec<FileStat> = Vec::new();
        self.collect_files(&self.root, &mut files)?;
        on(files.len() as u64, files.len() as u64, "scanning");

        let cache = self.store.load_fs_stat()?;
        let mut out: BTreeMap<String, DiskEntry> = BTreeMap::new();
        let mut to_read: Vec<FileStat> = Vec::new();
        for fstat in files {
            match cache.get(&fstat.rel) {
                // Unchanged stat → trust the cached content hash, skip the read.
                Some((m, sz, h)) if *m == fstat.mtime_ns && *sz == fstat.size => {
                    out.insert(fstat.rel, DiskEntry { hash: h.clone(), size: fstat.size.max(0) as usize, bytes: None });
                }
                _ => to_read.push(fstat),
            }
        }

        // Read (+ hash) the changed/new files in parallel — reads dominate.
        on(0, to_read.len() as u64, "hashing");
        let read = read_and_hash(&to_read, on);
        let mut updates: Vec<(String, i64, i64, String)> = Vec::with_capacity(read.len());
        for (rel, mtime_ns, size, bytes, hash) in read {
            updates.push((rel.clone(), mtime_ns, size, hash.clone()));
            out.insert(rel, DiskEntry { hash, size: bytes.len(), bytes: Some(bytes) });
        }

        // Keep the cache in sync: record what we just read, drop vanished paths.
        self.store.upsert_fs_stat(&updates)?;
        let vanished: Vec<String> = cache.keys().filter(|p| !out.contains_key(*p)).cloned().collect();
        self.store.delete_fs_stat(&vanished)?;
        Ok(out)
    }

    /// Walk the working tree collecting each non-ignored file's relative + absolute
    /// path and its stat (mtime, size). One `metadata()` per entry (it follows
    /// symlinks, exactly as the old `is_dir`/`is_file` did) yields the kind and the
    /// stat together. No file bodies are read here, so it stays cheap.
    fn collect_files(&self, dir: &Path, out: &mut Vec<FileStat>) -> AspResult<()> {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = match path.strip_prefix(&self.root) {
                Ok(r) => r.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            if self.scope.borrow().ignored(&rel) {
                continue;
            }
            let md = match path.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if md.is_dir() {
                self.collect_files(&path, out)?;
            } else if md.is_file() {
                let mtime_ns = md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(0);
                out.push(FileStat { rel, abs: path, mtime_ns, size: md.len() as i64 });
            }
        }
        Ok(())
    }

    // ---------------- snapshots / PITR ----------------

    /// Pin the current `result_hash`es as an immutable, content-addressed
    /// snapshot (§History). The snapshot is a GC root.
    pub fn snapshot(&self, label: &str) -> AspResult<String> {
        let files = self.store.live_files()?;
        let mut manifest: Vec<(String, String, String)> = files
            .iter()
            .filter(|f| !f.deleted)
            .filter_map(|f| f.result_hash.clone().map(|h| (f.file_id.clone(), f.path.clone(), h)))
            .collect();
        manifest.sort();
        let mut tree_input = String::new();
        for (fid, path, h) in &manifest {
            tree_input.push_str(&format!("{fid}\0{path}\0{h}\n"));
        }
        let tree_hash = crate::oid::content_hash(tree_input.as_bytes());
        let snapshot_id = random_id();
        let created_lamport = self.store.next_lamport(0)? - 1;
        let manifest_json = serde_json::to_string(&manifest).unwrap_or_default();
        self.store
            .insert_snapshot(&snapshot_id, created_lamport, label, &tree_hash, now_unix() as i64, &manifest_json)?;
        Ok(snapshot_id)
    }

    /// Restore the working tree to a named snapshot (exact) or "as of T" wall
    /// time (best-effort), recording the resulting edits so the log stays the
    /// append-only source of truth and converges.
    pub fn restore(&self, target: &str) -> AspResult<Vec<WireRow>> {
        let desired: BTreeMap<String, Vec<u8>> = if let Some((_, _, manifest)) =
            self.store.snapshot_by_label(target)?
        {
            let entries: Vec<(String, String, String)> = serde_json::from_str(&manifest)
                .map_err(|e| AspError::Invalid(format!("bad snapshot manifest: {e}")))?;
            let mut m = BTreeMap::new();
            for (_fid, path, h) in entries {
                let bytes = self.store.get_blob(&h)?.unwrap_or_default();
                m.insert(path, bytes);
            }
            m
        } else if let Some(t) = parse_time_arg(target) {
            self.state_as_of(t)?
        } else {
            return Err(AspError::NotFound(format!("no snapshot or time: {target}")));
        };
        self.apply_target(&desired)
    }

    /// Materialized state at wall-clock T (best-effort): fold the checked-out
    /// branch's visible rows with ts ≤ T (§4.6 — time-travel is branch-scoped, so a
    /// scrub on a branch never mixes in sibling/parent-after-fork history).
    pub fn state_as_of(&self, t: i64) -> AspResult<BTreeMap<String, Vec<u8>>> {
        let bs = self.branch_set()?;
        let vis = bs.visibility(&self.head_branch());
        let rows: Vec<LogRow> = self.store.all_rows()?.into_iter().filter(|r| vis.sees(r) && r.ts <= t).collect();
        let files = compute_files(&self.store, &rows)?;
        let mut m = BTreeMap::new();
        for f in files {
            if f.deleted {
                continue;
            }
            if let Some(h) = f.result_hash {
                m.insert(f.path, self.store.get_blob(&h)?.unwrap_or_default());
            }
        }
        Ok(m)
    }

    /// Content of a single `path` as the vault was at wall-clock `t` — `None` if
    /// it didn't exist (non-deleted) then. The history-slider read: it still
    /// folds the log as-of `t` for correct path resolution (renames / ` (n)`
    /// collisions), but reads exactly **one** blob (the target), not every live
    /// file's blob like `state_as_of`. On a large vault that is the difference
    /// between a snappy scrub and reading the whole vault on every tick.
    pub fn file_at(&self, path: &str, t: i64) -> AspResult<Option<Vec<u8>>> {
        let bs = self.branch_set()?;
        let vis = bs.visibility(&self.head_branch());
        let rows: Vec<LogRow> = self.store.all_rows()?.into_iter().filter(|r| vis.sees(r) && r.ts <= t).collect();
        let files = compute_files(&self.store, &rows)?;
        for f in files {
            if !f.deleted && f.path == path {
                let bytes = match f.result_hash {
                    Some(h) => self.store.get_blob(&h)?.unwrap_or_default(),
                    None => Vec::new(),
                };
                return Ok(Some(bytes));
            }
        }
        Ok(None)
    }

    /// Bring the working set to `desired` by recording the necessary edits.
    fn apply_target(&self, desired: &BTreeMap<String, Vec<u8>>) -> AspResult<Vec<WireRow>> {
        let mut authored = Vec::new();
        let current: BTreeMap<String, FileRow> =
            self.store.live_files()?.into_iter().map(|f| (f.path.clone(), f)).collect();
        for (path, bytes) in desired {
            let changed = match current.get(path) {
                Some(f) => f.result_hash.as_deref() != Some(crate::oid::content_hash(bytes).as_str()),
                None => true,
            };
            if changed {
                if let Some(wr) = self.record_write(path, bytes)? {
                    authored.push(wr);
                }
            }
        }
        for path in current.keys() {
            if !desired.contains_key(path) {
                if let Some(wr) = self.record_remove(path)? {
                    authored.push(wr);
                }
            }
        }
        Ok(authored)
    }

    // ---------------- branch ops (§4.2) ----------------

    /// All live branches, `main` first — the switcher/list source.
    pub fn branches(&self) -> AspResult<Vec<Branch>> {
        let mut out = vec![Branch::main()];
        for b in self.store.branches()? {
            if !b.deleted {
                out.push(b);
            }
        }
        Ok(out)
    }

    /// The checked-out branch id.
    pub fn current_branch(&self) -> String {
        self.head_branch()
    }

    /// The latest `Kind::Branch` record row for `branch_id`, as a wire row — for a
    /// live driver to push after a fork (where the create path doesn't hand back
    /// the row directly). None if the branch has no record.
    pub fn branch_record_wire(&self, branch_id: &str) -> Option<WireRow> {
        let rows = self.store.rows_for_file(branch_id).ok()?;
        let key = |r: &LogRow| OrderKey { lamport: r.lamport, site_id: r.site_id.clone(), id: r.id.clone() };
        let latest = rows.into_iter().filter(|r| r.kind == Kind::Branch).max_by(|a, b| key(a).cmp(&key(b)))?;
        self.wire(latest).ok()
    }

    /// The GitHub-network-style branch/commit DAG (§4.5) — lanes per branch + a
    /// coarsened commit graph, bounded to `cap` commits per lane.
    pub fn graph(&self, cap: usize) -> AspResult<crate::branch::Graph> {
        let rows = self.store.all_rows()?;
        let live: Vec<Branch> = self.store.branches()?.into_iter().filter(|b| !b.deleted).collect();
        let mut g = crate::branch::build_graph(&rows, &live, &self.head_branch(), cap);
        // Place live tags on their branch's lane (unknown/deleted-branch tags land on
        // lane 0 so they still show). Tag metadata rides the same log, reconciled LWW.
        let lane_of: std::collections::HashMap<String, usize> =
            g.branches.iter().map(|b| (b.id.clone(), b.lane)).collect();
        g.tags = self
            .tags()?
            .into_iter()
            .map(|t| crate::branch::GraphTag {
                lane: *lane_of.get(&t.branch_id).unwrap_or(&0),
                tag_id: t.tag_id,
                name: t.name,
                at_ts: t.at_ts,
                branch_id: t.branch_id,
            })
            .collect();
        Ok(g)
    }

    /// Live (non-deleted) tags, reconciled LWW from the synced `Kind::Tag` records.
    pub fn tags(&self) -> AspResult<Vec<crate::tag::Tag>> {
        let rows = self.store.tag_rows()?;
        Ok(crate::tag::reconcile_tags(&rows, |h| self.store.get_blob(h).ok().flatten())
            .into_iter()
            .filter(|t| !t.deleted)
            .collect())
    }

    /// Tag the point in history at wall-clock `at_ts` on the current branch with
    /// `name`. Authored as a synced `Kind::Tag` row (like a branch record), so the
    /// tag propagates to every peer. Returns the tag id + wire row for live push.
    pub fn create_tag(&self, name: &str, at_ts: i64) -> AspResult<(String, WireRow)> {
        crate::tag::validate_tag_name(name)?;
        let head = self.head_branch();
        // Lamport of the tagged instant: the max lamport of head-visible rows at or
        // before `at_ts` (0 if none) — a stable ordering hint for the graph.
        let bs = self.branch_set()?;
        let vis = bs.visibility(&head);
        let at_lamport = self
            .store
            .all_rows()?
            .iter()
            .filter(|r| vis.sees(r) && r.ts <= at_ts)
            .map(|r| r.lamport)
            .max()
            .unwrap_or(0);
        let created_lamport = self.store.next_lamport(0)?;
        let created_ts = now_unix() as i64;
        let tag_id = crate::tag::Tag::derive_id(name, at_ts, &head, created_lamport, &self.site_id());
        let t = crate::tag::Tag {
            tag_id: tag_id.clone(),
            name: name.to_string(),
            at_ts,
            at_lamport,
            branch_id: head,
            created_lamport,
            created_ts,
            deleted: false,
        };
        let wr = self.author_tag_record(&t)?;
        Ok((tag_id, wr))
    }

    /// Soft-delete a tag (its rows remain for history). Returns the tombstone wire
    /// row for live push; it also converges via catch-up.
    pub fn delete_tag(&self, tag_id: &str) -> AspResult<WireRow> {
        let rows = self.store.tag_rows()?;
        let existing = crate::tag::reconcile_tags(&rows, |h| self.store.get_blob(h).ok().flatten())
            .into_iter()
            .find(|t| t.tag_id == tag_id)
            .ok_or_else(|| AspError::NotFound(format!("no such tag: {tag_id}")))?;
        let tomb = crate::tag::Tag { deleted: true, ..existing };
        self.author_tag_record(&tomb)
    }

    /// Author a synced tag record — a `Kind::Tag` row whose result blob is the
    /// JSON-encoded `Tag`, keyed by `file_id = tag_id` so create → rename → delete
    /// converge LWW. Mirrors `author_branch_record`.
    fn author_tag_record(&self, t: &crate::tag::Tag) -> AspResult<WireRow> {
        let blob = crate::tag::encode_tag_record(t);
        let result_hash = self.store.put_blob(&blob)?;
        let (lamport, seq) = self.next_counters()?;
        let row = LogRow {
            id: String::new(),
            site_id: self.site_id(),
            lamport,
            seq,
            ts: now_unix() as i64,
            file_id: t.tag_id.clone(),
            kind: Kind::Tag,
            merge_class: MergeClass::Text,
            parent: None,
            base_hash: None,
            result_hash: Some(result_hash),
            path: Some(t.name.clone()),
            branch_id: MAIN_BRANCH_ID.to_string(),
            merge_parent: None,
            sig: vec![],
        }
        .seal();
        self.store.append_row(&row)?;
        self.wire(row)
    }

    /// The latest `Kind::Tag` record row for `tag_id`, as a wire row — for a live
    /// driver to push after a fork-time tag where the create path handed back the id.
    pub fn tag_record_wire(&self, tag_id: &str) -> Option<WireRow> {
        let rows = self.store.rows_for_file(tag_id).ok()?;
        let key = |r: &LogRow| OrderKey { lamport: r.lamport, site_id: r.site_id.clone(), id: r.id.clone() };
        let latest = rows.into_iter().filter(|r| r.kind == Kind::Tag).max_by(|a, b| key(a).cmp(&key(b)))?;
        self.wire(latest).ok()
    }

    /// The version vector visible on `branch` right now — the fork point a child
    /// captures when forking "from here" (§2.1).
    pub fn visible_version_vector(&self, branch: &str) -> AspResult<crate::branch::VersionVector> {
        let bs = self.branch_set()?;
        let vis = bs.visibility(branch);
        let rows: Vec<LogRow> = self.store.all_rows()?.into_iter().filter(|r| vis.sees(r)).collect();
        Ok(version_vector_of(&rows))
    }

    /// Create a branch off HEAD at the current point and return its id (the CLI's
    /// `branch create`). Does not switch HEAD.
    pub fn create_branch_here(&self, name: &str) -> AspResult<String> {
        Ok(self.create_branch_here_wire(name)?.0)
    }

    /// Like [`create_branch_here`] but also returns the authored branch-record wire
    /// row, so a live driver (desktop) can push it to peers immediately.
    pub fn create_branch_here_wire(&self, name: &str) -> AspResult<(String, WireRow)> {
        crate::branch::validate_branch_name(name)?;
        let head = self.head_branch();
        let fork_vv = self.visible_version_vector(&head)?;
        let created_lamport = self.store.next_lamport(0)?;
        let created_ts = now_unix() as i64;
        let branch_id = Branch::derive_id(name, &head, &fork_vv, created_lamport, &self.site_id());
        let b = Branch {
            branch_id: branch_id.clone(),
            name: name.to_string(),
            parent: Some(head),
            fork_vv,
            created_lamport,
            created_ts,
            deleted: false,
        };
        let wr = self.author_branch_record(&b)?;
        Ok((branch_id, wr))
    }

    /// Create a branch off `parent` capturing `fork_vv` (the parent's version
    /// vector at the fork). Returns the new content-hashed branch id. Does **not**
    /// switch HEAD. The record is authored as a synced `Kind::Branch` row (§7), so
    /// the branch propagates to every peer — not just the device that made it.
    pub fn create_branch(&self, name: &str, parent: &str, fork_vv: crate::branch::VersionVector) -> AspResult<String> {
        crate::branch::validate_branch_name(name)?;
        if self.branch_set()?.get(parent).is_none() {
            return Err(AspError::NotFound(format!("no such parent branch: {parent}")));
        }
        let created_lamport = self.store.next_lamport(0)?;
        let created_ts = now_unix() as i64;
        let branch_id = Branch::derive_id(name, parent, &fork_vv, created_lamport, &self.site_id());
        let b = Branch {
            branch_id: branch_id.clone(),
            name: name.to_string(),
            parent: Some(parent.to_string()),
            fork_vv,
            created_lamport,
            created_ts,
            deleted: false,
        };
        self.author_branch_record(&b)?;
        Ok(branch_id)
    }

    /// Author a synced branch record (§7): a `Kind::Branch` row whose result blob
    /// is the JSON-encoded `Branch`, keyed by `file_id = branch_id` so create →
    /// rename → delete (and concurrent variants) converge last-writer-wins. Returns
    /// the wire row so a live driver can push it immediately; either way the record
    /// also ships via ordinary version-vector catch-up.
    pub fn author_branch_record(&self, b: &Branch) -> AspResult<WireRow> {
        let blob = crate::branch::encode_branch_record(b);
        let result_hash = self.store.put_blob(&blob)?;
        let (lamport, seq) = self.next_counters()?;
        let row = LogRow {
            id: String::new(),
            site_id: self.site_id(),
            lamport,
            seq,
            ts: now_unix() as i64,
            file_id: b.branch_id.clone(),
            kind: Kind::Branch,
            merge_class: MergeClass::Text,
            parent: None,
            base_hash: None,
            result_hash: Some(result_hash),
            path: Some(b.name.clone()),
            branch_id: MAIN_BRANCH_ID.to_string(),
            merge_parent: None,
            sig: vec![],
        }
        .seal();
        self.store.append_row(&row)?;
        self.reconcile_branches()?;
        self.wire(row)
    }

    /// Rebuild the local `branches` table from the synced `Kind::Branch` records
    /// (LWW). Idempotent; run after authoring or integrating a branch record so a
    /// peer holds the same branch set as everyone else.
    fn reconcile_branches(&self) -> AspResult<()> {
        let rows = self.store.branch_rows()?;
        for b in crate::branch::reconcile_branches(&rows, |h| self.store.get_blob(h).ok().flatten()) {
            self.store.put_branch(&b)?;
        }
        Ok(())
    }

    /// Switch HEAD to `branch_id` and re-materialize its scoped state to disk +
    /// `files` table (§3.3). Uses a per-branch `FoldState` LRU: the outgoing
    /// branch's fold is parked and the target's restored if cached, so switching
    /// among a few branches skips the full O(all-rows) refold and is near-instant.
    /// A cold target still rebuilds from `visible(new HEAD)` exactly as before.
    pub fn checkout(&self, branch_id: &str) -> AspResult<()> {
        if self.store.branch(branch_id)?.is_none() {
            return Err(AspError::NotFound(format!("no such branch: {branch_id}")));
        }
        let old = self.head_branch();
        if old == branch_id {
            return Ok(());
        }
        // Bring the current branch's fold fully up to date (drains `dirty`), then
        // park it so a later switch back is a cache hit. Cheap when already clean.
        self.materialize()?;
        if let Some(fs) = self.fold.borrow_mut().take() {
            let mut cache = self.fold_cache.borrow_mut();
            cache.retain(|(b, _)| b != &old);
            cache.push((old, fs));
            while cache.len() > FOLD_CACHE_CAP {
                cache.remove(0);
            }
        }
        self.store.set_head(branch_id)?;
        // Restore the target's parked fold if we have it; else force a full rebuild.
        let restored = {
            let mut cache = self.fold_cache.borrow_mut();
            cache.iter().position(|(b, _)| b == branch_id).map(|pos| cache.remove(pos).1)
        };
        *self.fold.borrow_mut() = restored;
        self.dirty.borrow_mut().clear();
        self.materialize()?;
        Ok(())
    }

    /// Edit-in-the-past ⇒ branch (§2.5): fork the current branch at wall-clock `t`
    /// (`fork_vv` = version vector of HEAD's rows with `ts ≤ t`), switch HEAD to the
    /// new branch, and re-materialize the historical state. A subsequent
    /// `record_write` then authors on the new branch, chaining on the historical
    /// tip — main is left untouched. Returns the new branch id.
    pub fn fork_from_time(&self, name: &str, t: i64) -> AspResult<String> {
        let head = self.head_branch();
        let bs = self.branch_set()?;
        let vis = bs.visibility(&head);
        let rows: Vec<LogRow> =
            self.store.all_rows()?.into_iter().filter(|r| vis.sees(r) && r.ts <= t).collect();
        let fork_vv = version_vector_of(&rows);
        let id = self.create_branch(name, &head, fork_vv)?;
        self.checkout(&id)?;
        Ok(id)
    }

    /// Soft-delete a branch (§4.2): its rows remain for sync/history. The root
    /// `main` cannot be deleted; deleting the checked-out branch auto-checks-out its
    /// parent (or `main`).
    /// Soft-delete a branch (§4.2). Returns the authored tombstone wire row so a
    /// live driver can push it to peers immediately (it also converges via
    /// catch-up). Root `main` cannot be deleted; deleting HEAD auto-checks-out the
    /// parent.
    pub fn delete_branch(&self, branch_id: &str) -> AspResult<WireRow> {
        if branch_id == MAIN_BRANCH_ID {
            return Err(AspError::Invalid("cannot delete the main branch".into()));
        }
        let Some(mut b) = self.store.branch(branch_id)? else {
            return Err(AspError::NotFound(format!("no such branch: {branch_id}")));
        };
        if self.head_branch() == branch_id {
            // Land on the nearest *live* ancestor — never another tombstone, which
            // would strand HEAD on a branch absent from the switcher.
            let target = self.nearest_live_ancestor(&b)?;
            self.checkout(&target)?;
        }
        b.deleted = true;
        // Author a synced deletion record so the tombstone converges everywhere.
        self.author_branch_record(&b)
    }

    /// The nearest ancestor of `start` that is still live (not tombstoned),
    /// defaulting to `main`. Walks the parent chain; cycle- and dangling-safe.
    fn nearest_live_ancestor(&self, start: &Branch) -> AspResult<String> {
        let mut seen = std::collections::HashSet::new();
        seen.insert(start.branch_id.clone());
        let mut cur = start.parent.clone();
        while let Some(id) = cur {
            if !seen.insert(id.clone()) {
                break; // cycle
            }
            if id == MAIN_BRANCH_ID {
                return Ok(id); // main is always live
            }
            match self.store.branch(&id)? {
                Some(p) if !p.deleted => return Ok(p.branch_id),
                Some(p) => cur = p.parent.clone(),
                None => break, // dangling parent
            }
        }
        Ok(MAIN_BRANCH_ID.to_string())
    }

    // ---------------- admission (§Security) ----------------

    /// Seed the admission set at `init`/`authorize`/env.
    pub fn authorize(&self, ssh_line: &str, expires_at: Option<u64>, never: bool, source: &str) -> AspResult<NodeId> {
        let k = AuthKey::from_ssh(ssh_line, expires_at, never, now_unix(), source)
            .ok_or_else(|| AspError::Invalid("not an ssh-ed25519 key line".into()))?;
        let node = k.node().ok_or_else(|| AspError::Invalid("bad node id".into()))?;
        self.store.insert_authkey(&k)?;
        Ok(node)
    }

    pub fn revoke(&self, node_hex: &str) -> AspResult<bool> {
        self.store.delete_authkey_by_node(node_hex)
    }

    /// Listen-start migration: fill `expires_at` on unset rows with
    /// `today + default_ttl`. Idempotent. Returns rows filled.
    pub fn migrate_keys(&self, default_ttl_days: u64) -> AspResult<usize> {
        let exp = expiry_from_ttl_days(now_unix(), default_ttl_days);
        self.store.migrate_fill_expiry(exp)
    }

    /// Decide whether to admit `peer` and (if enrolling/TOFU) persist the row,
    /// via the shared `decide_admission` logic (§Security).
    pub fn admit(&self, peer: &NodeId, ctx: &AdmitCtx) -> AspResult<()> {
        let peer_hex = peer.to_hex();
        let existing = self.store.authkey_by_node(&peer_hex)?;
        match decide_admission(existing.as_ref(), self.store.authkeys_empty()?, ctx) {
            AdmitDecision::Admit => Ok(()),
            AdmitDecision::Insert(source) => {
                let exp = expiry_from_ttl_days(ctx.now_unix, ctx.default_ttl_days);
                let line = crate::identity::ssh_pubkey_string(peer, source);
                let k = AuthKey::from_ssh(&line, Some(exp), false, ctx.now_unix, source).unwrap();
                self.store.insert_authkey(&k)?;
                Ok(())
            }
            AdmitDecision::Deny(why) => Err(AspError::AuthDenied(format!("{why}: {}", &peer_hex[..12]))),
        }
    }
}

/// The native engine drives the *same* sans-IO `Session` as the wasm node.
impl SessionVault for Engine {
    fn node_id(&self) -> NodeId {
        self.identity.node_id()
    }
    fn vault_id(&self) -> String {
        self.store.get_config("vault_id").ok().flatten().unwrap_or_default()
    }
    fn adopt_vault_id(&self, vault_id: &str) -> AspResult<()> {
        self.store.set_config("vault_id", vault_id)
    }
    fn version_vector(&self) -> AspResult<BTreeMap<String, i64>> {
        self.store.version_vector()
    }
    fn rows_after_wire(&self, site: &str, after: i64) -> AspResult<Vec<WireRow>> {
        self.store.rows_after(site, after)?.into_iter().map(|r| self.wire(r)).collect()
    }
    fn integrate(&self, wr: &WireRow) -> AspResult<bool> {
        Engine::integrate(self, wr)
    }
    fn integrate_many(&self, rows: &[WireRow]) -> AspResult<Vec<bool>> {
        Engine::integrate_many(self, rows)
    }
    fn admit(&self, peer: &NodeId, ctx: &AdmitCtx) -> AspResult<()> {
        Engine::admit(self, peer, ctx)
    }
    fn is_pristine(&self) -> bool {
        self.store.row_count().map(|c| c == 0).unwrap_or(false)
    }
}

/// Parse a restore time argument: a unix-seconds integer or `YYYY-MM-DD`.
fn parse_time_arg(s: &str) -> Option<i64> {
    if let Ok(n) = s.parse::<i64>() {
        return Some(n);
    }
    crate::authkeys::parse_date_ymd_utc(s).map(|t| t as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn eng(dir: &Path, seed: u8) -> Engine {
        Engine::init(dir, Identity::from_seed(&[seed; 32])).unwrap()
    }

    #[test]
    fn create_edit_delete_roundtrip_on_disk() {
        let d = tempdir().unwrap();
        let e = eng(d.path(), 1);
        e.record_write("notes/a.md", b"hello\n").unwrap().unwrap();
        assert_eq!(fs::read(d.path().join("notes/a.md")).unwrap(), b"hello\n");

        e.record_write("notes/a.md", b"hello world\n").unwrap().unwrap();
        assert_eq!(fs::read(d.path().join("notes/a.md")).unwrap(), b"hello world\n");

        // No-op write returns None.
        assert!(e.record_write("notes/a.md", b"hello world\n").unwrap().is_none());

        e.record_remove("notes/a.md").unwrap().unwrap();
        assert!(!d.path().join("notes/a.md").exists());
    }

    #[test]
    fn two_engines_converge_via_integrate() {
        let da = tempdir().unwrap();
        let db = tempdir().unwrap();
        let a = eng(da.path(), 1);
        let b = eng(db.path(), 2);
        let wr = a.record_write("x.md", b"from A\n").unwrap().unwrap();
        b.integrate(&wr).unwrap();
        assert_eq!(fs::read(db.path().join("x.md")).unwrap(), b"from A\n");
    }

    #[test]
    fn integrates_git_kind_rows_without_touching_files() {
        // git-bridge §6.1: the native Engine must accept rows of the three new
        // kinds through both integrate paths, treat them as fold no-ops (real files
        // unchanged, none created for the markers), and dedup on replay.
        use crate::gitrecord::{
            build_commit_marker_row, build_ingest_row, build_plan_row, GitCommitMarker, GitIngestRecord,
            GitPlanRecord, GitRowIdentity,
        };
        use crate::store::{BlobStore, MemBlobStore};

        let scratch = MemBlobStore::new();
        // (site_id, seq) is UNIQUE + dense per site — each row needs its own seq.
        let ident_at = |seq: u64| GitRowIdentity {
            site_id: "repo-site".into(),
            lamport: 100 + seq,
            seq,
            ts: 9,
            parent: None,
        };
        let marker = GitCommitMarker {
            sha: "abc123".into(),
            author_name: "Ada".into(),
            author_email: "ada@x".into(),
            committer_ts: 9,
            message: "import".into(),
            parents: vec![],
            branch_id: MAIN_BRANCH_ID.to_string(),
        };
        let ingest = GitIngestRecord {
            commit_sha: "abc123".into(),
            upto_site: "repo-site".into(),
            upto_seq: 0,
            modes: vec![],
            symlinks: vec![],
            gitlinks: vec![],
            remote_ref: "refs/heads/main".into(),
            rebaselined: false,
        };
        let plan = GitPlanRecord {
            frontier: std::collections::BTreeMap::new(),
            message: "asp: 0 files".into(),
            author: "Bot <b@x>".into(),
            planned_ts: 9,
        };
        let to_wire = |row: LogRow| {
            let hash = row.result_hash.clone().unwrap();
            let bytes = scratch.get_blob(&hash).unwrap().unwrap();
            WireRow { row, blobs: vec![WireBlob { hash, bytes }] }
        };
        let git_rows = vec![
            to_wire(build_commit_marker_row(&scratch, &ident_at(0), &marker).unwrap()),
            to_wire(build_ingest_row(&scratch, &ident_at(1), &ingest).unwrap()),
            to_wire(build_plan_row(&scratch, &ident_at(2), &plan).unwrap()),
        ];

        // Single-integrate path.
        let da = tempdir().unwrap();
        let a = eng(da.path(), 1);
        a.record_write("real.md", b"content\n").unwrap().unwrap();
        for wr in &git_rows {
            assert!(a.integrate(wr).unwrap(), "git row integrates as new");
        }
        assert_eq!(fs::read(da.path().join("real.md")).unwrap(), b"content\n", "unchanged");
        // No marker leaked onto disk (path=sha is metadata, not a file).
        assert!(!da.path().join("abc123").exists());
        for wr in &git_rows {
            assert!(!a.integrate(wr).unwrap(), "re-integration dedups");
        }

        // Batch-integrate path.
        let db = tempdir().unwrap();
        let b = eng(db.path(), 2);
        b.record_write("real.md", b"content\n").unwrap().unwrap();
        assert!(b.integrate_many(&git_rows).unwrap().iter().all(|added| *added));
        assert_eq!(fs::read(db.path().join("real.md")).unwrap(), b"content\n");
        assert!(b.integrate_many(&git_rows).unwrap().iter().all(|added| !*added), "batch dedup");
    }

    #[test]
    fn rename_preserves_content_and_concurrent_edit() {
        let da = tempdir().unwrap();
        let db = tempdir().unwrap();
        let a = eng(da.path(), 1);
        let b = eng(db.path(), 2);
        let c = a.record_write("f.md", b"line1\nline2\n").unwrap().unwrap();
        b.integrate(&c).unwrap();
        // A renames, B edits concurrently.
        let rn = a.record_rename("f.md", "g.md").unwrap().unwrap();
        let ed = b.record_write("f.md", b"line1\nline2\nline3\n").unwrap().unwrap();
        a.integrate(&ed).unwrap();
        b.integrate(&rn).unwrap();
        // Converge: file at new path g.md with B's edit intact.
        assert!(da.path().join("g.md").exists());
        assert!(db.path().join("g.md").exists());
        assert_eq!(fs::read(da.path().join("g.md")).unwrap(), b"line1\nline2\nline3\n");
        assert!(!db.path().join("f.md").exists());
    }

    #[test]
    fn edit_in_past_forks_a_branch_and_isolates_from_main() {
        // §2.5 + §2.2 isolation: fork the current state onto a new branch, edit on
        // it, and main must be untouched; switching back and forth shows each
        // branch's own state.
        let d = tempdir().unwrap();
        let e = eng(d.path(), 1);
        e.record_write("a.md", b"main-v1\n").unwrap().unwrap();
        assert_eq!(e.current_branch(), MAIN_BRANCH_ID);

        // Fork capturing everything up to now (t = MAX → fork_vv = full main vv).
        let b = e.fork_from_time("feature", i64::MAX).unwrap();
        assert_eq!(e.current_branch(), b, "HEAD followed the new branch");
        assert_eq!(fs::read(d.path().join("a.md")).unwrap(), b"main-v1\n", "branch starts from the forked state");

        // Edit on the branch + add a branch-only file.
        e.record_write("a.md", b"branch-v2\n").unwrap().unwrap();
        e.record_write("only-on-branch.md", b"x\n").unwrap().unwrap();
        assert_eq!(fs::read(d.path().join("a.md")).unwrap(), b"branch-v2\n");

        // Back to main: original content, and the branch-only file is gone.
        e.checkout(MAIN_BRANCH_ID).unwrap();
        assert_eq!(fs::read(d.path().join("a.md")).unwrap(), b"main-v1\n", "main is isolated from branch edits");
        assert!(!d.path().join("only-on-branch.md").exists(), "branch-only file not visible on main");

        // Back to the branch: its edits are intact.
        e.checkout(&b).unwrap();
        assert_eq!(fs::read(d.path().join("a.md")).unwrap(), b"branch-v2\n");
        assert!(d.path().join("only-on-branch.md").exists());
    }

    #[test]
    fn time_travel_is_branch_scoped() {
        // §4.6 regression: state_as_of / file_at must fold only the checked-out
        // branch's visible rows. With a divergent post-fork edit on main, a scrub
        // on the branch must show the BRANCH's content — not a merge of the two.
        let d = tempdir().unwrap();
        let e = eng(d.path(), 1);
        e.record_write("a.md", b"m1\n").unwrap().unwrap();
        let b = e.fork_from_time("feature", i64::MAX).unwrap();
        e.record_write("a.md", b"b2\n").unwrap().unwrap(); // edit on the branch
        e.checkout(MAIN_BRANCH_ID).unwrap();
        e.record_write("a.md", b"m2\n").unwrap().unwrap(); // divergent edit on main

        // On main, the slider sees main's line.
        assert_eq!(e.file_at("a.md", i64::MAX).unwrap().as_deref(), Some(&b"m2\n"[..]));
        // On the branch, it sees ONLY the branch's line — main's post-fork edit is
        // invisible (pre-fix this folded both → a 3-way merge, not "b2").
        e.checkout(&b).unwrap();
        assert_eq!(e.file_at("a.md", i64::MAX).unwrap().as_deref(), Some(&b"b2\n"[..]));
        let st = e.state_as_of(i64::MAX).unwrap();
        assert_eq!(st.get("a.md").map(|v| v.as_slice()), Some(&b"b2\n"[..]));
    }

    #[test]
    fn delete_branch_rules() {
        let d = tempdir().unwrap();
        let e = eng(d.path(), 1);
        e.record_write("a.md", b"v1\n").unwrap().unwrap();
        let b = e.fork_from_time("feature", i64::MAX).unwrap();
        assert_eq!(e.current_branch(), b);
        // Deleting the checked-out branch auto-checks-out main.
        e.delete_branch(&b).unwrap();
        assert_eq!(e.current_branch(), MAIN_BRANCH_ID);
        assert!(e.branches().unwrap().iter().all(|x| x.branch_id != b), "deleted branch dropped from the live list");
        // main can never be deleted.
        assert!(e.delete_branch(MAIN_BRANCH_ID).is_err());
        // Checking out a non-existent branch errors.
        assert!(e.checkout("nope").is_err());
    }

    #[test]
    fn checkout_lru_roundtrip_matches_a_cold_fold() {
        // Switching A→B→A must land byte-identical state whether the target's fold
        // is restored from the LRU (warm) or rebuilt from scratch (cold) — the
        // per-branch fold cache must never diverge from a full fold.
        let d = tempdir().unwrap();
        let e = eng(d.path(), 1);
        e.record_write("shared.md", b"base\n").unwrap().unwrap();
        let b = e.fork_from_time("feature", i64::MAX).unwrap(); // checks out feature
        e.record_write("only-b.md", b"b\n").unwrap().unwrap();
        e.record_write("shared.md", b"b-edit\n").unwrap().unwrap();

        // main: only shared.md=base, no only-b.md.
        e.checkout(MAIN_BRANCH_ID).unwrap();
        assert_eq!(e.file_at("shared.md", i64::MAX).unwrap().as_deref(), Some(&b"base\n"[..]));
        assert!(e.file_at("only-b.md", i64::MAX).unwrap().is_none());

        // Back to feature via the LRU (warm): must equal the branch's real state.
        e.checkout(&b).unwrap();
        assert_eq!(e.file_at("shared.md", i64::MAX).unwrap().as_deref(), Some(&b"b-edit\n"[..]));
        assert_eq!(e.file_at("only-b.md", i64::MAX).unwrap().as_deref(), Some(&b"b\n"[..]));

        // Cold reference: a freshly-opened engine (empty LRU) checked out to feature.
        let cold = Engine::open(d.path(), Identity::from_seed(&[1; 32])).unwrap();
        cold.checkout(&b).unwrap();
        let norm = |e: &Engine| {
            let mut v: Vec<(String, Option<String>, bool)> =
                e.store.live_files().unwrap().into_iter().map(|f| (f.path, f.result_hash, f.deleted)).collect();
            v.sort();
            v
        };
        assert_eq!(norm(&e), norm(&cold), "warm (LRU) checkout diverged from a cold full fold");
    }

    #[test]
    fn tags_create_list_delete_and_survive_reopen() {
        let d = tempdir().unwrap();
        {
            let e = eng(d.path(), 1);
            e.record_write("a.md", b"v1\n").unwrap().unwrap();
            let (tid, _wr) = e.create_tag("release-1", 1_700_000_000).unwrap();
            let tags = e.tags().unwrap();
            assert_eq!(tags.len(), 1);
            assert_eq!(tags[0].name, "release-1");
            assert_eq!(tags[0].at_ts, 1_700_000_000);
            assert_eq!(tags[0].branch_id, MAIN_BRANCH_ID, "tag records the branch it was taken on");
            // Empty name rejected.
            assert!(e.create_tag("  ", 1).is_err());
            // Tags show up on the graph, placed on their branch lane.
            let g = e.graph(100).unwrap();
            assert_eq!(g.tags.len(), 1);
            assert_eq!(g.tags[0].lane, 0);
            // Soft-delete removes it from the live set.
            e.delete_tag(&tid).unwrap();
            assert!(e.tags().unwrap().is_empty());
        }
        // A fresh tag persists across reopen (it's a synced log row, not just memory).
        {
            let e = eng(d.path(), 1);
            e.create_tag("keep", 1_700_000_500).unwrap();
        }
        let e = eng(d.path(), 1);
        assert_eq!(e.tags().unwrap().iter().map(|t| t.name.clone()).collect::<Vec<_>>(), vec!["keep".to_string()]);
    }

    #[test]
    fn capture_rescan_progress_reports_scan_and_hash() {
        use std::sync::Mutex;
        let d = tempdir().unwrap();
        std::fs::write(d.path().join("a.md"), b"aaaa").unwrap();
        std::fs::write(d.path().join("b.md"), b"bbbb").unwrap();
        let e = eng(d.path(), 1);
        let seen: Mutex<Vec<(u64, u64, String)>> = Mutex::new(Vec::new());
        e.capture_rescan_progress(&|done, total, phase| {
            seen.lock().unwrap().push((done, total, phase.to_string()));
        })
        .unwrap();
        let seen = seen.into_inner().unwrap();
        // We must have seen a "scanning" phase reporting the two files present.
        assert!(seen.iter().any(|(_, t, p)| p == "scanning" && *t >= 2), "scanning phase reports the file total: {seen:?}");
        // And a "hashing" phase (the two new files were read + hashed).
        assert!(seen.iter().any(|(_, _, p)| p == "hashing"), "hashing phase reported: {seen:?}");
    }

    #[test]
    fn delete_head_lands_on_nearest_live_ancestor_not_a_tombstone() {
        // main <- a <- b, checked out on b. Delete a (not HEAD), then delete b
        // (HEAD): HEAD must skip the tombstoned parent a and land on a LIVE branch
        // (main). Landing on deleted a would strand the user on a branch absent
        // from the switcher.
        let d = tempdir().unwrap();
        let e = eng(d.path(), 1);
        e.record_write("f.md", b"m\n").unwrap().unwrap();
        let a = e.fork_from_time("a", i64::MAX).unwrap(); // forks main, checks out a
        assert_eq!(e.current_branch(), a);
        let b = e.fork_from_time("b", i64::MAX).unwrap(); // forks a, checks out b
        assert_eq!(e.current_branch(), b);
        // Delete the mid branch while sitting on b (a is not HEAD → HEAD unmoved).
        e.delete_branch(&a).unwrap();
        assert_eq!(e.current_branch(), b, "deleting a non-HEAD branch must not move HEAD");
        // Delete HEAD: must skip tombstoned parent a and land on main.
        e.delete_branch(&b).unwrap();
        assert_eq!(e.current_branch(), MAIN_BRANCH_ID, "must not be stranded on the deleted ancestor a");
    }

    #[test]
    fn create_branch_validates_name_and_parent() {
        let d = tempdir().unwrap();
        let e = eng(d.path(), 1);
        e.record_write("a.md", b"v1\n").unwrap().unwrap();
        let head = e.head_branch();
        let vv = crate::branch::version_vector_of(&e.store.all_rows().unwrap());
        // Empty / whitespace-only names are rejected (would be unaddressable by name).
        assert!(e.create_branch_here("").is_err(), "empty name rejected");
        assert!(e.create_branch_here("   ").is_err(), "whitespace-only name rejected");
        assert!(e.create_branch("", &head, vv.clone()).is_err());
        // Forking off a non-existent parent is rejected (no orphan branch).
        assert!(e.create_branch("x", "no-such-parent", vv.clone()).is_err(), "unknown parent rejected");
        // A valid create still works, and the count reflects exactly one new branch.
        assert!(e.create_branch("ok", &head, vv).is_ok());
        assert_eq!(e.branches().unwrap().len(), 2, "only the valid branch was created");
    }

    #[test]
    fn create_branch_checkout_and_scoped_tip() {
        // create_branch (explicit fork_vv) + checkout + authoring on a non-main
        // branch exercises the branch-scoped tip path and isolation.
        let d = tempdir().unwrap();
        let e = eng(d.path(), 1);
        e.record_write("a.md", b"m1\n").unwrap().unwrap();
        let head = e.head_branch();
        let vv = crate::branch::version_vector_of(&e.store.all_rows().unwrap());
        let bid = e.create_branch("topic", &head, vv).unwrap();
        assert_ne!(bid, MAIN_BRANCH_ID);
        assert_eq!(e.branches().unwrap().len(), 2, "main + topic");
        e.checkout(&bid).unwrap();
        assert_eq!(e.current_branch(), bid);
        // Two edits on the branch chain through the branch-scoped tip.
        e.record_write("a.md", b"m1\nb2\n").unwrap().unwrap();
        e.record_write("a.md", b"m1\nb2\nb3\n").unwrap().unwrap();
        assert_eq!(fs::read(d.path().join("a.md")).unwrap(), b"m1\nb2\nb3\n");
        e.checkout(MAIN_BRANCH_ID).unwrap();
        assert_eq!(fs::read(d.path().join("a.md")).unwrap(), b"m1\n", "main untouched by branch edits");
        // Deleting a non-existent branch is an error (not a panic).
        assert!(e.delete_branch("does-not-exist").is_err());
    }

    #[test]
    fn branches_sync_to_peers_not_just_the_checked_out_one() {
        // The user's requirement: every branch syncs, not only HEAD. A forks a
        // branch, edits on it, then edits main; B catches up ALL of A's rows and
        // must (1) learn the branch record, (2) hold main's state, and (3) be able
        // to check out the branch and see its isolated state.
        let da = tempdir().unwrap();
        let db = tempdir().unwrap();
        let a = eng(da.path(), 1);
        let b = eng(db.path(), 2);

        a.record_write("a.md", b"m1\n").unwrap().unwrap();
        let bid = a.fork_from_time("feature", i64::MAX).unwrap();
        a.record_write("a.md", b"branch2\n").unwrap().unwrap();
        a.record_write("feat-only.md", b"x\n").unwrap().unwrap();
        a.checkout(MAIN_BRANCH_ID).unwrap();
        a.record_write("a.md", b"m2\n").unwrap().unwrap(); // divergent edit on main

        // Full catch-up: ship every one of A's rows (content + branch records) to B.
        let all: Vec<WireRow> = a.store.all_rows().unwrap().into_iter().map(|r| a.wire(r).unwrap()).collect();
        b.integrate_many(&all).unwrap();

        // B is on main and converges to main's state.
        assert_eq!(fs::read(db.path().join("a.md")).unwrap(), b"m2\n");
        assert!(!db.path().join("feat-only.md").exists(), "branch-only file not on main");

        // B learned the branch record purely from sync.
        assert!(
            b.branches().unwrap().iter().any(|x| x.branch_id == bid && x.name == "feature" && !x.deleted),
            "the feature branch synced to B"
        );

        // B can check out the synced branch and see its isolated state.
        b.checkout(&bid).unwrap();
        assert_eq!(fs::read(db.path().join("a.md")).unwrap(), b"branch2\n", "branch state on B");
        assert!(db.path().join("feat-only.md").exists());

        // A delete on A also converges (tombstone syncs).
        a.delete_branch(&bid).unwrap();
        let more: Vec<WireRow> = a.store.all_rows().unwrap().into_iter().map(|r| a.wire(r).unwrap()).collect();
        // B is currently on the branch being deleted on A; integrate the tombstone.
        b.checkout(MAIN_BRANCH_ID).unwrap();
        b.integrate_many(&more).unwrap();
        assert!(b.branches().unwrap().iter().all(|x| x.branch_id != bid), "branch deletion synced to B");
    }

    #[test]
    fn branch_records_sync_native_to_wasm_memengine() {
        // Cross-surface: a native (CLI/desktop) node's branches must converge on a
        // wasm (web/Obsidian) MemEngine node — same branch set, same per-branch
        // scoped state — purely from the synced rows.
        use crate::memengine::MemEngine;
        let da = tempdir().unwrap();
        let a = eng(da.path(), 1);
        a.record_write("a.md", b"m1\n").unwrap().unwrap();
        let bid = a.fork_from_time("feature", i64::MAX).unwrap();
        a.record_write("a.md", b"branch2\n").unwrap().unwrap();
        a.checkout(MAIN_BRANCH_ID).unwrap();
        a.record_write("a.md", b"m2\n").unwrap().unwrap();

        let mem = MemEngine::create(Identity::from_seed(&[5; 32]), "v");
        let all: Vec<WireRow> = a.store.all_rows().unwrap().into_iter().map(|r| a.wire(r).unwrap()).collect();
        mem.integrate_many(&all).unwrap();

        // wasm node converged main and learned the branch from sync alone.
        assert_eq!(mem.read_file("a.md").unwrap().as_deref(), Some(&b"m2\n"[..]));
        assert!(mem.branches().iter().any(|x| x.branch_id == bid && x.name == "feature"));
        // and can check it out to its isolated state.
        mem.checkout(&bid).unwrap();
        assert_eq!(mem.read_file("a.md").unwrap().as_deref(), Some(&b"branch2\n"[..]));
    }

    #[test]
    fn single_branch_vault_is_byte_identical_back_compat() {
        // §2.2 back-compat: with no branch ever created, HEAD=main and every row is
        // visible, so the materialized tree matches a plain whole-log fold.
        let d = tempdir().unwrap();
        let e = eng(d.path(), 1);
        e.record_write("a.md", b"hello\n").unwrap().unwrap();
        e.record_write("dir/b.md", b"world\n").unwrap().unwrap();
        let scoped = e.store.live_files().unwrap();
        let full = compute_files(&e.store, &e.store.all_rows().unwrap()).unwrap();
        let live: Vec<_> = full.into_iter().filter(|f| !f.deleted).collect();
        assert_eq!(scoped.len(), live.len());
    }

    #[test]
    fn admission_tofu_then_authoritative() {
        let d = tempdir().unwrap();
        let e = eng(d.path(), 1);
        let peer = Identity::from_seed(&[9; 32]).node_id();
        let ctx = AdmitCtx { no_tofu: false, auth_key_ok: false, auth_key_configured: false, default_ttl_days: 90, now_unix: now_unix() };
        e.admit(&peer, &ctx).unwrap(); // empty set → TOFU
        // Now non-empty; a different peer is denied.
        let other = Identity::from_seed(&[10; 32]).node_id();
        assert!(e.admit(&other, &ctx).is_err());
    }

    #[test]
    fn expired_key_denied_then_refreshed() {
        let d = tempdir().unwrap();
        let e = eng(d.path(), 1);
        let peer = Identity::from_seed(&[9; 32]);
        // A key that expired in 1970.
        e.authorize(&peer.to_ssh_string(), Some(1000), false, "test").unwrap();
        let ctx = AdmitCtx { no_tofu: true, auth_key_ok: false, auth_key_configured: true, default_ttl_days: 90, now_unix: 2000 };
        assert!(e.admit(&peer.node_id(), &ctx).is_err(), "expired key past its time is refused");
        // Re-authorized with a future expiry → admitted.
        e.authorize(&peer.to_ssh_string(), Some(10_000), false, "cli").unwrap();
        let ctx2 = AdmitCtx { now_unix: 5000, ..ctx };
        e.admit(&peer.node_id(), &ctx2).unwrap();
    }

    #[test]
    fn listen_start_migration_is_idempotent() {
        let d = tempdir().unwrap();
        let e = eng(d.path(), 1);
        // A hand-seeded key with unset expiry.
        e.authorize(&Identity::from_seed(&[9; 32]).to_ssh_string(), None, false, "env").unwrap();
        assert_eq!(e.migrate_keys(90).unwrap(), 1, "unset row gets a default expiry");
        assert_eq!(e.migrate_keys(90).unwrap(), 0, "second run is a no-op (idempotent)");
        // A `never` key is never rewritten by migration.
        e.authorize(&Identity::from_seed(&[7; 32]).to_ssh_string(), None, true, "cli").unwrap();
        assert_eq!(e.migrate_keys(90).unwrap(), 0, "never=1 rows are left untouched");
    }
}
