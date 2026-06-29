//! The high-level native engine: capture (FS event → log row), fold →
//! materialize to disk, derived git export, snapshots/restore (PITR), and
//! connection admission against the `authorized_keys` table. Thin over the pure
//! `fold`/`merge`/`store` core — all convergence logic lives there. The native
//! driver (the `asp` CLI) supplies file watching, debounce, and sockets.

use crate::authkeys::{decide_admission, expiry_from_ttl_days, AdmitCtx, AdmitDecision, AuthKey};
use crate::config::VaultConfig;
use crate::error::{AspError, AspResult};
use crate::fold::compute_files;
use crate::gitexport;
use crate::identity::Identity;
use crate::log::{Kind, LogRow, MergeClass};
use crate::order::NodeId;
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

    // ---------------- capture ----------------

    /// The current materialized content hash for a live path, if any.
    fn current_for_path(&self, rel: &str) -> AspResult<Option<FileRow>> {
        let files = self.store.live_files()?;
        Ok(files.into_iter().find(|f| f.path == rel))
    }

    /// Highest-OrderKey row id for a file_id (its deterministic local tip).
    fn tip(&self, file_id: &str) -> AspResult<Option<String>> {
        Ok(self
            .store
            .conn()
            .query_row(
                "SELECT id FROM log WHERE file_id=?1 ORDER BY lamport DESC, site_id DESC, id DESC LIMIT 1",
                rusqlite::params![file_id],
                |r| r.get::<_, String>(0),
            )
            .ok())
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
                    sig: vec![],
                }
                .seal()
            }
        };
        self.store.append_row(&row)?;
        // Fast path: a linear local edit on an existing file. Its folded result is
        // exactly `bytes` for this one file_id — no path change, no cross-file
        // effect — so update just this file instead of re-folding + rewriting the
        // whole vault. (Skipped when batching, or when git export is on, since the
        // git tree needs the full file set; the CLI takes the full path instead.)
        if let Some(cur) = cur_opt.filter(|_| !self.batch.get() && !self.export_git.get()) {
            self.apply_one_edit(&cur.file_id, &cur.path, result_hash, bytes, row.lamport, cur.merge_class, cur.conflict)?;
        } else {
            self.materialize_unless_batched()?;
        }
        Ok(Some(self.wire(row)?))
    }

    /// Persist a single linear content edit: update just this file's row + bytes
    /// on disk. Byte-identical to what a full `materialize()` would write for it.
    fn apply_one_edit(&self, file_id: &str, path: &str, result_hash: String, bytes: &[u8], lamport: u64, merge_class: MergeClass, conflict: bool) -> AspResult<()> {
        self.store.upsert_files(&[FileRow {
            file_id: file_id.to_string(),
            path: path.to_string(),
            result_hash: Some(result_hash),
            merge_class,
            deleted: false,
            lamport,
            site_id: self.site_id(),
            conflict,
        }])?;
        let abs = self.root.join(path);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = abs.with_extension(format!("asp-tmp-{}", now_unix()));
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, &abs)?;
        if path == ".aspignore" {
            self.reload_scope();
        }
        Ok(())
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
            sig: vec![],
        }
        .seal();
        self.store.append_row(&row)?;
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
            sig: vec![],
        }
        .seal();
        self.store.append_row(&row)?;
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
            for b in &wr.blobs {
                let h = self.store.put_blob(&b.bytes)?;
                if h != b.hash {
                    return Err(AspError::Protocol("blob hash mismatch".into()));
                }
            }
        }
        let mut flags = Vec::with_capacity(wrs.len());
        let mut any = false;
        for wr in wrs {
            let added = self.store.append_row(&wr.row)?;
            if added {
                any = true;
            }
            flags.push(added);
        }
        if any {
            self.materialize()?;
            self.notify_change();
        }
        Ok(flags)
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

    /// Returns the materialized path → content-hash map (for echo suppression).
    ///
    /// Diffs the fresh fold against the previously-persisted file rows and only
    /// touches what changed: upsert the changed rows, write/remove the changed
    /// files on disk, prune emptied dirs. The fold itself is canonical and
    /// unchanged — the persisted state is byte-identical to a full rewrite — but
    /// an edit to one file now does O(changed) I/O instead of rewriting all N
    /// rows + re-reading all N files from disk (which was seconds on a big vault).
    pub fn materialize(&self) -> AspResult<BTreeMap<String, String>> {
        let rows = self.store.all_rows()?;
        let files = compute_files(&self.store, &rows)?;
        self.apply_files(&files, rows.iter().map(|r| r.lamport).max().unwrap_or(0))
    }

    fn apply_files(&self, files: &[FileRow], derived_time: u64) -> AspResult<BTreeMap<String, String>> {
        let old: std::collections::HashMap<String, FileRow> =
            self.store.all_files()?.into_iter().map(|f| (f.file_id.clone(), f)).collect();

        // Only the rows whose folded value changed need persisting / disk work.
        let changed: Vec<FileRow> = files.iter().filter(|f| old.get(&f.file_id) != Some(*f)).cloned().collect();
        self.store.upsert_files(&changed)?;

        // Rows the fold no longer emits at all (a deleted dir entity has no
        // tombstone) must be removed — `replace_files` did this implicitly.
        let new_ids: std::collections::HashSet<&str> = files.iter().map(|f| f.file_id.as_str()).collect();
        let mut removed_ids: Vec<String> = Vec::new();
        let mut aspignore_changed = false;
        for of in old.values() {
            if new_ids.contains(of.file_id.as_str()) {
                continue;
            }
            if !of.deleted {
                let abs = self.root.join(&of.path);
                if of.merge_class == MergeClass::Dir {
                    self.prune_empty_dirs(Some(abs.as_path()));
                } else {
                    if of.path == ".aspignore" {
                        aspignore_changed = true;
                    }
                    let _ = fs::remove_file(&abs);
                    self.prune_empty_dirs(abs.parent());
                }
            }
            removed_ids.push(of.file_id.clone());
        }
        if !removed_ids.is_empty() {
            self.store.delete_file_rows(&removed_ids)?;
        }

        for f in &changed {
            let prev = old.get(&f.file_id);
            // A file/dir whose live path went away (rename or delete) is removed
            // from its OLD location on disk.
            if let Some(p) = prev {
                let path_gone = f.deleted || f.path != p.path;
                if !p.deleted && path_gone {
                    if p.path == ".aspignore" {
                        aspignore_changed = true;
                    }
                    let abs = self.root.join(&p.path);
                    if p.merge_class == MergeClass::Dir {
                        self.prune_empty_dirs(Some(abs.as_path()));
                    } else {
                        let _ = fs::remove_file(&abs);
                        self.prune_empty_dirs(abs.parent());
                    }
                }
            }
            if f.deleted {
                continue;
            }
            if f.merge_class == MergeClass::Dir {
                let _ = fs::create_dir_all(self.root.join(&f.path));
                continue;
            }
            let Some(h) = &f.result_hash else { continue };
            // Write only when the on-disk content would actually differ — known
            // from the OLD row (same path + same hash ⇒ already correct), no
            // 28k-file stat/read sweep.
            let already_ok = matches!(prev, Some(p) if !p.deleted && p.path == f.path && p.result_hash.as_deref() == Some(h.as_str()));
            if !already_ok {
                if f.path == ".aspignore" {
                    aspignore_changed = true;
                }
                let abs = self.root.join(&f.path);
                if let Some(parent) = abs.parent() {
                    fs::create_dir_all(parent)?;
                }
                let bytes = self.store.get_blob(h)?.unwrap_or_default();
                let tmp = abs.with_extension(format!("asp-tmp-{}", now_unix()));
                fs::write(&tmp, &bytes)?;
                fs::rename(&tmp, &abs)?;
            }
        }

        if aspignore_changed {
            self.reload_scope();
        }

        // Derived read-only git export — O(all files), so skipped on hosts that
        // never read it (the desktop app turns it off; the CLI keeps it on).
        if self.export_git.get() {
            let mut desired: BTreeMap<String, Vec<u8>> = BTreeMap::new();
            for f in files {
                if f.deleted || f.merge_class == MergeClass::Dir {
                    continue;
                }
                if let Some(h) = &f.result_hash {
                    desired.insert(f.path.clone(), self.store.get_blob(h)?.unwrap_or_default());
                }
            }
            let _ = gitexport::export(&self.git_dir, &desired, derived_time);
        }

        // path -> hash for the live content files (echo suppression on capture).
        Ok(files
            .iter()
            .filter(|f| !f.deleted && f.merge_class != MergeClass::Dir)
            .filter_map(|f| f.result_hash.clone().map(|h| (f.path.clone(), h)))
            .collect())
    }

    /// Turn the O(all-files) git export off (a host that never reads `.asp/git`).
    pub fn set_git_export(&self, on: bool) {
        self.export_git.set(on);
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
        let result = self.capture_rescan_inner(&self.scan_disk()?);
        self.batch.set(false);
        let authored = result?;
        self.materialize()?;
        Ok(authored)
    }

    fn capture_rescan_inner(&self, on_disk: &BTreeMap<String, Vec<u8>>) -> AspResult<Vec<WireRow>> {
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
                Some(bytes) => {
                    if f.result_hash.as_deref() != Some(crate::oid::content_hash(bytes).as_str()) {
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
            if let Some(bytes) = on_disk.get(a) {
                if bytes.len() > 8 {
                    by_hash.entry(crate::oid::content_hash(bytes)).or_default().push(a.clone());
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
            if let Some(bytes) = on_disk.get(path) {
                if let Some(wr) = self.record_write(path, bytes)? {
                    authored.push(wr);
                }
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
            sig: vec![],
        }
        .seal();
        self.store.append_row(&row)?;
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
            sig: vec![],
        }
        .seal();
        self.store.append_row(&row)?;
        self.materialize_unless_batched()?;
        Ok(Some(self.wire(row)?))
    }

    /// Walk the in-scope working tree into a `rel_path -> bytes` map.
    pub fn scan_disk(&self) -> AspResult<BTreeMap<String, Vec<u8>>> {
        let mut out = BTreeMap::new();
        self.scan_dir(&self.root, &mut out)?;
        Ok(out)
    }

    fn scan_dir(&self, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) -> AspResult<()> {
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
            if path.is_dir() {
                self.scan_dir(&path, out)?;
            } else if path.is_file() {
                if let Ok(bytes) = fs::read(&path) {
                    out.insert(rel, bytes);
                }
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

    /// Materialized state at wall-clock T (best-effort): fold rows with ts ≤ T.
    pub fn state_as_of(&self, t: i64) -> AspResult<BTreeMap<String, Vec<u8>>> {
        let rows: Vec<LogRow> = self.store.all_rows()?.into_iter().filter(|r| r.ts <= t).collect();
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
