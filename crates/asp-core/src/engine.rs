//! The high-level native engine: capture (FS event → log row), fold →
//! materialize to disk, derived git export, snapshots/restore (PITR), and
//! connection admission against the `authorized_keys` table. Thin over the pure
//! `fold`/`merge`/`store` core — all convergence logic lives there. The native
//! driver (the `asp` CLI) supplies file watching, debounce, and sockets.

use crate::authkeys::{expiry_from_ttl_days, AuthKey};
use crate::config::VaultConfig;
use crate::error::{AspError, AspResult};
use crate::fold::compute_files;
use crate::gitexport;
use crate::identity::Identity;
use crate::log::{Kind, LogRow, MergeClass};
use crate::order::NodeId;
use crate::store::{FileRow, Store};
use crate::wire::{WireBlob, WireRow};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Per-connection context the listener uses to decide admission.
#[derive(Clone)]
pub struct AdmitCtx {
    pub no_tofu: bool,
    /// A valid auth key was presented at the WebSocket upgrade.
    pub auth_key_ok: bool,
    /// An auth key is configured on this listener (implicitly disables TOFU).
    pub auth_key_configured: bool,
    pub default_ttl_days: u64,
    pub now_unix: u64,
}

pub struct Engine {
    pub root: PathBuf,
    pub asp_dir: PathBuf,
    pub git_dir: PathBuf,
    pub store: Store,
    pub identity: Identity,
    pub scope: crate::scope::Scope,
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

/// Classify a path's merge behavior (§The merge model). Constant per `file_id`
/// from creation; changes only via an explicit `reclass`.
pub fn classify(path: &str, bytes: &[u8]) -> MergeClass {
    if std::str::from_utf8(bytes).is_err() || bytes.contains(&0) {
        return MergeClass::Binary;
    }
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    const CODE: &[&str] = &[
        "rs", "py", "js", "ts", "tsx", "jsx", "go", "c", "h", "cpp", "cc", "hpp", "java", "rb",
        "sh", "bash", "zsh", "php", "swift", "kt", "scala", "lua", "pl", "r", "sql", "toml", "yaml",
        "yml", "json", "xml", "html", "css", "scss", "vue", "ex", "exs", "erl", "hs", "ml", "fs",
        "cs", "dart", "zig", "nim",
    ];
    if CODE.contains(&ext.as_str()) {
        MergeClass::Code
    } else {
        MergeClass::Text
    }
}

impl Engine {
    /// Open or create the engine at a vault root, authoring as `identity`.
    pub fn open(root: &Path, identity: Identity) -> AspResult<Engine> {
        let asp_dir = root.join(".asp");
        fs::create_dir_all(&asp_dir)?;
        let git_dir = asp_dir.join("git");
        let store = Store::open(&asp_dir.join("asp.db"))?;
        let scope = Self::load_scope(root);
        let eng = Engine { root: root.to_path_buf(), asp_dir, git_dir, store, identity, scope };
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

    pub fn site_id(&self) -> String {
        self.identity.node_id().to_hex()
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

        // Desired on-disk set.
        let mut desired: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut hashes: BTreeMap<String, String> = BTreeMap::new();
        for f in &files {
            if f.deleted {
                continue;
            }
            if let Some(h) = &f.result_hash {
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

        // Remove files that were live before but no longer are (delete/rename-away).
        for path in old_live {
            if !desired.contains_key(&path) {
                let abs = self.root.join(&path);
                let _ = fs::remove_file(&abs);
                // prune now-empty parent dirs up to root
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
        let mut authored = Vec::new();
        let on_disk = self.scan_disk()?;
        let live: BTreeMap<String, FileRow> =
            self.store.live_files()?.into_iter().map(|f| (f.path.clone(), f)).collect();

        // New or changed files on disk.
        for (rel, bytes) in &on_disk {
            let changed = match live.get(rel) {
                Some(f) => {
                    let h = crate::oid::content_hash(bytes);
                    f.result_hash.as_deref() != Some(h.as_str())
                }
                None => true,
            };
            if changed {
                if let Some(wr) = self.record_write(rel, bytes)? {
                    authored.push(wr);
                }
            }
        }
        // Files removed from disk while we were off.
        for (rel, _f) in &live {
            if !on_disk.contains_key(rel) {
                if let Some(wr) = self.record_remove(rel)? {
                    authored.push(wr);
                }
            }
        }
        Ok(authored)
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

    /// Decide whether to admit `peer` and (if enrolling/TOFU) persist the row.
    /// Returns Ok(()) on admit, Err(AuthDenied) otherwise.
    pub fn admit(&self, peer: &NodeId, ctx: &AdmitCtx) -> AspResult<()> {
        let peer_hex = peer.to_hex();
        // Already enrolled and currently valid → admit.
        if let Some(k) = self.store.authkey_by_node(&peer_hex)? {
            if k.admissible(ctx.now_unix) {
                return Ok(());
            }
            // Expired: only an auth-key re-enrollment refreshes the TTL.
            if ctx.auth_key_ok {
                let exp = expiry_from_ttl_days(ctx.now_unix, ctx.default_ttl_days);
                self.store.set_authkey_expiry(&peer_hex, Some(exp), false)?;
                return Ok(());
            }
            return Err(AspError::AuthDenied(format!("key expired: {}", &peer_hex[..12])));
        }
        // Not enrolled. Auth-key enrollment is the front door for fresh peers.
        if ctx.auth_key_ok {
            let exp = expiry_from_ttl_days(ctx.now_unix, ctx.default_ttl_days);
            let line = crate::identity::ssh_pubkey_string(peer, "enrolled");
            let k = AuthKey::from_ssh(&line, Some(exp), false, ctx.now_unix, "enroll").unwrap();
            self.store.insert_authkey(&k)?;
            return Ok(());
        }
        // TOFU — only while the set is empty, no auth key configured, not disabled.
        if !ctx.no_tofu && !ctx.auth_key_configured && self.store.authkeys_empty()? {
            let line = crate::identity::ssh_pubkey_string(peer, "tofu");
            let exp = expiry_from_ttl_days(ctx.now_unix, ctx.default_ttl_days);
            let k = AuthKey::from_ssh(&line, Some(exp), false, ctx.now_unix, "tofu").unwrap();
            self.store.insert_authkey(&k)?;
            return Ok(());
        }
        Err(AspError::AuthDenied(format!("not authorized: {}", &peer_hex[..12])))
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
}
