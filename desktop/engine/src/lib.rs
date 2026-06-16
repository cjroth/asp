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
use asp_core::net::{AuthOpts, EngineRef};
use asp_core::{Engine, Identity, VaultConfig};
use serde::Serialize;
use std::collections::HashMap;
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
}
