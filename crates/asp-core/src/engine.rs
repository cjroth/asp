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
    pub scope: crate::scope::Scope,
    /// Per-vault authoring `site_id` (distinct from `identity`, the device key).
    pub site: String,
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
        let eng = Engine { root: root.to_path_buf(), asp_dir, git_dir, store, identity, scope, site };
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

    pub fn reload_scope(&mut self) {
        self.scope = Self::load_scope(&self.root);
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
        if self.scope.ignored(rel) {
            return Ok(None);
        }
        let result_hash = self.store.put_blob(bytes)?;
        let (lamport, seq) = self.next_counters()?;
        let ts = now_unix() as i64;
        let row = match self.current_for_path(rel)? {
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
        self.materialize()?;
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
            sig: vec![],
        }
        .seal();
        self.store.append_row(&row)?;
        self.materialize()?;
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
        self.materialize()?;
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
        }
        Ok(added)
    }

    // ---------------- fold → materialize ----------------

    /// Fold the log, write the materialized `files` table, render changed files
    /// to disk (atomic, self-write-suppressed), and export the derived git repo.
    /// Returns the materialized path → content-hash map (for echo suppression).
    pub fn materialize(&self) -> AspResult<BTreeMap<String, String>> {
        let rows = self.store.all_rows()?;
        let old_live: Vec<String> = self.store.live_files()?.into_iter().map(|f| f.path).collect();
        let files = compute_files(&self.store, &rows)?;
        self.store.replace_files(&files)?;

        // Desired on-disk set: content files (path -> bytes) and directory
        // entities (paths to `mkdir`).
        let mut desired: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut desired_dirs: Vec<String> = Vec::new();
        let mut hashes: BTreeMap<String, String> = BTreeMap::new();
        for f in &files {
            if f.deleted {
                continue;
            }
            if f.merge_class == MergeClass::Dir {
                desired_dirs.push(f.path.clone());
            } else if let Some(h) = &f.result_hash {
                let bytes = self.store.get_blob(h)?.unwrap_or_default();
                desired.insert(f.path.clone(), bytes);
                hashes.insert(f.path.clone(), h.clone());
            }
        }

        // Write/overwrite desired files atomically (only when content differs).
        for (path, bytes) in &desired {
            let abs = self.root.join(path);
            let differs = match fs::read(&abs) {
                Ok(cur) => &cur != bytes,
                Err(_) => true,
            };
            if differs {
                if let Some(parent) = abs.parent() {
                    fs::create_dir_all(parent)?;
                }
                let tmp = abs.with_extension(format!(
                    "asp-tmp-{}",
                    now_unix()
                ));
                fs::write(&tmp, bytes)?;
                fs::rename(&tmp, &abs)?;
            }
        }

        // Materialize empty directories (the content-free dir entities).
        for path in &desired_dirs {
            let _ = fs::create_dir_all(self.root.join(path));
        }

        // Remove files/dirs that were live before but no longer are.
        let desired_dir_set: std::collections::HashSet<&String> = desired_dirs.iter().collect();
        for path in old_live {
            if !desired.contains_key(&path) && !desired_dir_set.contains(&path) {
                let abs = self.root.join(&path);
                let _ = fs::remove_file(&abs); // no-op if it was a directory
                self.prune_empty_dirs(Some(abs.as_path()));
                self.prune_empty_dirs(abs.parent());
            }
        }

        // Derived git export at the settle boundary.
        let derived_time = rows.iter().map(|r| r.lamport).max().unwrap_or(0);
        let _ = gitexport::export(&self.git_dir, &desired, derived_time);

        Ok(hashes)
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
        let on_disk = self.scan_disk()?;
        // Content files vs directory entities are tracked separately: directories
        // are first-class, content-free entities (§Capture: empty directories).
        let (live_files, live_dirs): (Vec<FileRow>, Vec<FileRow>) =
            self.store.live_files()?.into_iter().partition(|f| f.merge_class != MergeClass::Dir);
        let live: BTreeMap<String, FileRow> = live_files.into_iter().map(|f| (f.path.clone(), f)).collect();

        let mut disappeared: Vec<String> = Vec::new();
        let mut changed: Vec<String> = Vec::new();
        for (path, f) in &live {
            match on_disk.get(path) {
                None => disappeared.push(path.clone()),
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
            if self.scope.ignored(&rel) {
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
        self.materialize()?;
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
        self.materialize()?;
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
            if self.scope.ignored(&rel) {
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
    fn sign(&self, msg: &[u8]) -> Vec<u8> {
        self.identity.sign(msg)
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
