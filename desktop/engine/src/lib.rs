//! Context Desktop engine (§Surfaces: Context Desktop). A normal background
//! process running **one `asp-core` engine instance per enabled folder** — the
//! in-process equivalent of one `asp watch [--listen]` per folder. It links
//! `asp-core` directly at the full-node profile (merge engine + on-disk SQLite +
//! the tokio WebSocket driver), **not** a consumer of the wasm SDK and **not** an
//! `asp` subprocess.
//!
//! **HARD INVARIANT — no protocol logic here.** Every sync/merge/identity/auth/
//! history behavior is a call into `asp-core`; this crate contributes process
//! lifecycle, per-folder listen/connect orchestration, and small app config. Any
//! behavioral difference from the `asp` CLI is a bug. The crate has **no Tauri
//! dependency**, so it builds and is tested on plain Linux.

use anyhow::{anyhow, Context, Result};
use asp_core::iroh_net;
use asp_core::log::LogRow;
use asp_core::net::{AuthOpts, EngineRef};
use asp_core::{compute_files, BlobStore, Engine, Identity, VaultConfig};
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as AsyncMutex;

/// Public, serializable view of a managed folder (what the UI renders).
#[derive(Clone, Serialize)]
pub struct VaultInfo {
    pub id: String,
    pub path: String,
    pub vault_id: String,
    pub enabled: bool,
    pub listening_ticket: Option<String>,
}

/// Live sync state for a folder.
#[derive(Clone, Serialize)]
pub struct VaultStatus {
    pub id: String,
    pub vault_id: String,
    pub rows: u64,
    pub files: usize,
    pub head: String,
    pub listening_ticket: Option<String>,
    pub peers: Vec<String>,
}

/// One node in the file tree (a file or a directory). The frontend renders this
/// as an expandable sidebar; children are present only for directories.
#[derive(Clone, Serialize)]
pub struct TreeNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<TreeNode>>,
}

/// One log row surfaced to the history timeline (point-in-time-travel UI).
#[derive(Clone, Serialize)]
pub struct HistoryEvent {
    pub id: String,
    pub ts: i64,
    pub lamport: u64,
    pub kind: String, // create | edit | rename | delete | reclass
    pub path: Option<String>,
}

/// Result of a point-in-time read: the file's content at `ts` (or `gone` when it
/// didn't exist yet / was deleted by then).
#[derive(Clone, Serialize)]
pub struct FileAtTime {
    pub exists: bool,
    pub content: Option<String>,
    pub key: String,
}

type Conns = Arc<AsyncMutex<HashMap<u64, tokio::sync::mpsc::UnboundedSender<asp_core::Msg>>>>;

struct Folder {
    id: String,
    path: PathBuf,
    engine: EngineRef,
    conns: Conns,
    enabled: bool,
    listening_ticket: Option<String>,
    listener: Option<tokio::task::JoinHandle<()>>,
}

pub struct DesktopEngine {
    rt: tokio::runtime::Runtime,
    identity: Identity,
    folders: Mutex<HashMap<String, Folder>>,
}

fn random_id() -> String {
    use asp_core::oid::content_hash;
    content_hash(format!("{:?}{}", std::time::Instant::now(), std::process::id()).as_bytes())[..12].to_string()
}

impl DesktopEngine {
    /// Create the engine with a device identity (one identity per device, as in
    /// the CLI's `~/.asp/id_ed25519`).
    pub fn new(identity: Identity) -> Result<DesktopEngine> {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().context("tokio runtime")?;
        Ok(DesktopEngine { rt, identity, folders: Mutex::new(HashMap::new()) })
    }

    pub fn identity_ssh(&self) -> String {
        self.identity.to_ssh_string()
    }

    fn auth_opts(&self, auth_keys: Vec<String>) -> AuthOpts {
        AuthOpts { auth_keys, no_tofu: false, default_ttl_days: 90 }
    }

    /// Use the public n0 relays unless `ASP_NO_RELAY` opts into direct/LAN-only
    /// (hermetic tests, trusted LANs) — mirrors the CLI's `--no-relay`.
    fn use_relays() -> bool {
        !matches!(std::env::var("ASP_NO_RELAY").as_deref(), Ok("1") | Ok("true"))
    }

    fn handle(&self, eng: Engine) -> EngineRef {
        Arc::new(Mutex::new(eng))
    }

    fn info_of(f: &Folder) -> VaultInfo {
        let eng = f.engine.lock().unwrap();
        let vault_id = VaultConfig::new(&eng.store).vault_id().ok().flatten().unwrap_or_default();
        VaultInfo {
            id: f.id.clone(),
            path: f.path.to_string_lossy().to_string(),
            vault_id,
            enabled: f.enabled,
            listening_ticket: f.listening_ticket.clone(),
        }
    }

    /// Add (and initialize/open) a local folder as a vault. Captures current disk
    /// contents into the log.
    pub fn add_local_folder(&self, path: &Path) -> Result<VaultInfo> {
        let eng = if path.join(".asp/asp.db").exists() {
            Engine::open(path, self.identity.clone())?
        } else {
            Engine::init(path, self.identity.clone())?
        };
        eng.capture_rescan()?;
        let id = random_id();
        let folder = Folder {
            id: id.clone(),
            path: path.to_path_buf(),
            engine: self.handle(eng),
            conns: Arc::new(AsyncMutex::new(HashMap::new())),
            enabled: false,
            listening_ticket: None,
            listener: None,
        };
        let info = Self::info_of(&folder);
        self.folders.lock().unwrap().insert(id, folder);
        Ok(info)
    }

    /// Bootstrap a new folder by cloning from a listening peer (by iroh ticket /
    /// node id).
    pub fn clone_remote(&self, dest: &Path, ticket: &str, auth_key: Option<&str>) -> Result<VaultInfo> {
        let eng = Engine::open(dest, self.identity.clone())?;
        let engine = self.handle(eng);
        let auth = self.auth_opts(auth_key.map(|s| vec![s.to_string()]).unwrap_or_default());
        let addr = iroh_net::parse_peer(ticket)?;
        let seed = self.identity.seed();
        let ce = engine.clone();
        self.rt.block_on(async move {
            let ep = iroh_net::bind_endpoint(&seed, Self::use_relays()).await?;
            let r = iroh_net::clone_bootstrap(ce, &ep, addr, &auth).await;
            ep.close().await;
            r.map(|_| ())
        })?;
        let id = random_id();
        let folder = Folder {
            id: id.clone(),
            path: dest.to_path_buf(),
            engine,
            conns: Arc::new(AsyncMutex::new(HashMap::new())),
            enabled: false,
            listening_ticket: None,
            listener: None,
        };
        let info = Self::info_of(&folder);
        self.folders.lock().unwrap().insert(id, folder);
        Ok(info)
    }

    /// Toggle "allow connections": bind (or tear down) a per-folder iroh listener
    /// — the literal `asp watch --listen` mapping. Returns the shareable ticket.
    pub fn set_allow_connections(&self, id: &str, on: bool, auth_key: Option<&str>) -> Result<Option<String>> {
        let mut folders = self.folders.lock().unwrap();
        let f = folders.get_mut(id).ok_or_else(|| anyhow!("no such folder"))?;
        if on {
            if let Some(t) = &f.listening_ticket {
                return Ok(Some(t.clone()));
            }
            let auth = self.auth_opts(auth_key.map(|s| vec![s.to_string()]).unwrap_or_default());
            let (engine, conns) = (f.engine.clone(), f.conns.clone());
            let seed = self.identity.seed();
            let relays = Self::use_relays();
            let ep = self.rt.block_on(iroh_net::bind_endpoint(&seed, relays)).context("listener bind")?;
            let ticket = self.rt.block_on(iroh_net::ticket(&ep, relays)).context("ticket")?;
            let handle = self.rt.spawn(async move {
                let _ = iroh_net::serve(engine, ep, auth, conns).await;
            });
            f.listening_ticket = Some(ticket.clone());
            f.listener = Some(handle);
            Ok(Some(ticket))
        } else {
            if let Some(h) = f.listener.take() {
                h.abort();
            }
            f.listening_ticket = None;
            Ok(None)
        }
    }

    /// One-shot sync of a folder against a peer (used for catch-up + the UI's
    /// "sync now"; the same `Session` as the CLI).
    pub fn sync(&self, id: &str, ticket: &str, auth_key: Option<&str>) -> Result<()> {
        let engine = {
            let folders = self.folders.lock().unwrap();
            folders.get(id).ok_or_else(|| anyhow!("no such folder"))?.engine.clone()
        };
        let auth = self.auth_opts(auth_key.map(|s| vec![s.to_string()]).unwrap_or_default());
        let addr = iroh_net::parse_peer(ticket)?;
        let seed = self.identity.seed();
        self.rt.block_on(async move {
            let ep = iroh_net::bind_endpoint(&seed, Self::use_relays()).await?;
            let r = iroh_net::sync_oneshot(engine, &ep, addr, &auth).await;
            ep.close().await;
            r
        })
    }

    pub fn set_enabled(&self, id: &str, on: bool) -> Result<()> {
        let mut folders = self.folders.lock().unwrap();
        let f = folders.get_mut(id).ok_or_else(|| anyhow!("no such folder"))?;
        f.enabled = on;
        Ok(())
    }

    pub fn list_vaults(&self) -> Vec<VaultInfo> {
        self.folders.lock().unwrap().values().map(Self::info_of).collect()
    }

    pub fn authorize(&self, id: &str, pubkey: &str) -> Result<()> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        eng.authorize(pubkey, None, false, "cli")?;
        Ok(())
    }

    pub fn list_authorized(&self, id: &str) -> Result<Vec<String>> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        Ok(eng.store.authkeys()?.into_iter().map(|k| k.node_id).collect())
    }

    pub fn snapshot(&self, id: &str, name: &str) -> Result<String> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        Ok(eng.snapshot(name)?)
    }

    pub fn restore(&self, id: &str, target: &str) -> Result<()> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        eng.restore(target)?;
        Ok(())
    }

    pub fn status(&self, id: &str) -> Result<VaultStatus> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        let vault_id = VaultConfig::new(&eng.store).vault_id().ok().flatten().unwrap_or_default();
        let files = eng.store.live_files()?.into_iter().filter(|f| !f.deleted).count();
        let head = std::fs::read_to_string(eng.git_dir.join("refs/heads/main")).map(|s| s.trim().to_string()).unwrap_or_default();
        let peers = eng.store.peers()?.into_iter().map(|(u, _)| u).collect();
        Ok(VaultStatus {
            id: id.to_string(),
            vault_id,
            rows: eng.store.row_count()?,
            files,
            head,
            listening_ticket: f.listening_ticket.clone(),
            peers,
        })
    }

    // ---------------- vault file operations (the editor surface) ----------------
    //
    // Every method here is a thin call into `asp-core`'s `Engine` (record_write /
    // record_remove / record_rename / materialize / state_as_of / all_rows). No
    // protocol/merge/fold logic lives in this crate — `Engine` owns it.

    /// The materialized file tree of a vault, as nested directory/file nodes.
    fn build_tree(live: &[asp_core::FileRow]) -> Vec<TreeNode> {
        // Live files are flat `path` strings; build a nested tree by splitting on
        // '/' so the sidebar can render folders expandable. A directory node is
        // synthesized for every interior path segment.
        #[derive(Default)]
        struct Builder {
            name: String,
            path: String,
            children: BTreeMap<String, Builder>,
            is_file: bool,
        }
        let mut root = Builder { name: String::new(), path: String::new(), children: BTreeMap::new(), is_file: false };
        for f in live {
            if f.deleted {
                continue;
            }
            let segs: Vec<&str> = f.path.split('/').filter(|s| !s.is_empty()).collect();
            let mut cur = &mut root;
            let mut acc = String::new();
            for (i, seg) in segs.iter().enumerate() {
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(seg);
                let is_leaf = i == segs.len() - 1;
                let entry = cur.children.entry(seg.to_string()).or_insert_with(|| Builder {
                    name: seg.to_string(),
                    path: acc.clone(),
                    children: BTreeMap::new(),
                    is_file: false,
                });
                if is_leaf {
                    entry.is_file = true;
                }
                cur = entry;
            }
        }
        fn finalize(b: Builder) -> Vec<TreeNode> {
            b.children
                .into_iter()
                .map(|(_, c)| {
                    let is_dir = !c.is_file;
                    let name = c.name.clone();
                    let path = c.path.clone();
                    let children = if is_dir { Some(finalize(c)) } else { None };
                    TreeNode { name, path, is_dir, children }
                })
                .collect()
        }
        finalize(root)
    }

    /// List the file tree of a vault (files + synthesized directory nodes).
    pub fn files_tree(&self, id: &str) -> Result<Vec<TreeNode>> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        let live = eng.store.live_files()?;
        Ok(Self::build_tree(&live))
    }

    /// Read a file's content (UTF-8 text). `None` if the file does not exist.
    /// Authoritative source is the deterministic fold (not the on-disk file,
    /// which may be mid-materialize).
    pub fn read_file(&self, id: &str, path: &str) -> Result<Option<String>> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        let rows = eng.store.all_rows()?;
        let files = compute_files(&eng.store, &rows)?;
        match files.into_iter().find(|x| x.path == path && !x.deleted) {
            Some(f) => match f.result_hash {
                Some(h) => {
                    let bytes = eng.store.get_blob(&h)?.unwrap_or_default();
                    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
                }
                None => Ok(Some(String::new())),
            },
            None => Ok(None),
        }
    }

    /// Author a create/edit for `path` with `content` (UTF-8 text), then
    /// materialize so the file appears on disk and converges to peers.
    pub fn write_file(&self, id: &str, path: &str, content: &str) -> Result<()> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        eng.record_write(path, content.as_bytes())?;
        eng.materialize()?;
        Ok(())
    }

    /// Author a delete for `path`, then materialize so the file disappears on
    /// disk and converges to peers.
    pub fn delete_file(&self, id: &str, path: &str) -> Result<()> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        eng.record_remove(path)?;
        eng.materialize()?;
        Ok(())
    }

    /// Author a rename `from` -> `to` (stable `file_id`), then materialize.
    pub fn rename_file(&self, id: &str, from: &str, to: &str) -> Result<()> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        eng.record_rename(from, to)?;
        eng.materialize()?;
        Ok(())
    }

    /// Create a new untitled file with starter content (avoids a name clash by
    /// suffixing `-N`). Returns the chosen path.
    pub fn new_file(&self, id: &str, name: &str, content: &str) -> Result<String> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        let live: std::collections::HashSet<String> =
            eng.store.live_files()?.into_iter().map(|x| x.path).collect();
        let mut chosen = name.to_string();
        let mut i = 1;
        while live.contains(&chosen) {
            chosen = format!("untitled-{}.md", i);
            i += 1;
        }
        eng.record_write(&chosen, content.as_bytes())?;
        eng.materialize()?;
        Ok(chosen)
    }

    /// Surface the log as a flat list of history events (the timeline data).
    pub fn history(&self, id: &str) -> Result<Vec<HistoryEvent>> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        let mut rows: Vec<LogRow> = eng.store.all_rows()?;
        rows.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.lamport.cmp(&b.lamport)));
        Ok(rows
            .into_iter()
            .map(|r| HistoryEvent {
                id: r.id,
                ts: r.ts,
                lamport: r.lamport,
                kind: r.kind.as_str().to_string(),
                path: r.path,
            })
            .collect())
    }

    /// The byte-exact state of a vault at wall-clock time `ts` (best-effort PITR
    /// fold). Returns the content of `path` if it existed then, else `gone`.
    pub fn file_at_time(&self, id: &str, path: &str, ts: i64) -> Result<FileAtTime> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        let files = eng.state_as_of(ts)?;
        match files.get(path) {
            Some(bytes) => Ok(FileAtTime { exists: true, content: Some(String::from_utf8_lossy(bytes).into_owned()), key: format!("{}:{}", path, ts) }),
            None => Ok(FileAtTime { exists: false, content: None, key: "gone".to_string() }),
        }
    }

    /// Restore a single file to its content at wall-clock `ts`, by authoring a
    /// new write row (a deterministic "restore here" edit). No-op if the file
    /// didn't exist then; clears the playhead state on the caller side.
    pub fn restore_file_at(&self, id: &str, path: &str, ts: i64) -> Result<bool> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        let files = eng.state_as_of(ts)?;
        match files.get(path) {
            Some(bytes) => {
                eng.record_write(path, bytes)?;
                eng.materialize()?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Forget a managed folder. When `trash` is true (and the folder is a real
    /// on-disk vault), move the directory to the OS trash; otherwise leave the
    /// files on disk untouched (the vault is only removed from the app). Returns
    /// the on-disk path that was (or would have been) removed.
    pub fn remove_vault(&self, id: &str, trash: bool) -> Result<String> {
        let mut folders = self.folders.lock().unwrap();
        let f = folders.remove(id).ok_or_else(|| anyhow!("no such folder"))?;
        // Stop the listener if one is bound.
        if let Some(h) = f.listener {
            h.abort();
        }
        let path = f.path.clone();
        if trash {
            // Best-effort move to a sibling `.trash/` dir inside the parent (the
            // std lib has no cross-platform "recycle bin"; the Tauri shell can
            // upgrade this to a real OS-trash call, but the engine stays free
            // of system bindings and testable on plain Linux).
            if let Some(parent) = path.parent() {
                let trash_dir = parent.join(".asp-trash");
                let _ = fs::create_dir_all(&trash_dir);
                let name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "vault".into());
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                let dest = trash_dir.join(format!("{}-{}", name, stamp));
                let _ = fs::rename(&path, &dest);
            }
        }
        Ok(path.to_string_lossy().to_string())
    }
}
