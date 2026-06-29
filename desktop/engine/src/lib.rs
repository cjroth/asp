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
use asp_core::{Engine, Identity, Msg, VaultConfig, WireRow};
use serde::{Deserialize, Serialize};
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
    /// Wall-clock unix seconds of the most recent log row (for "last synced"
    /// labels), or `None` for an empty vault.
    pub last_ts: Option<i64>,
}

/// One live (non-deleted) file in a vault — what the file tree renders. A flat
/// list of slash-separated paths; the UI assembles the tree.
#[derive(Clone, Serialize)]
pub struct FileEntry {
    pub path: String,
    pub file_id: String,
    pub is_dir: bool,
    pub merge_class: String,
}

/// One entry in a vault's append-only history, for the time-travel scrubber.
/// A thin projection of an `asp-core` `LogRow` (no protocol logic added here).
#[derive(Clone, Serialize)]
pub struct HistEvent {
    pub id: String,
    pub ts: i64,
    pub lamport: u64,
    /// "create" | "edit" | "rename" | "delete" | "reclass" (from `LogRow.kind`).
    pub kind: String,
    /// Path the row applies to (resolved from the file_id's latest path for
    /// rows that don't carry one, e.g. edits/deletes).
    pub path: String,
}

/// Content of a file as of a point in time (for read-only time travel).
#[derive(Clone, Serialize)]
pub struct FileAt {
    /// Whether the file existed (non-deleted) at the requested instant.
    pub exists: bool,
    pub content: String,
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
    /// The folder's single long-lived iroh endpoint (one device key, one socket),
    /// shared by the listener (`serve`) and the connector (`connect`) — exactly
    /// like the CLI's one `ep` per `asp watch`. `None` until the folder first
    /// needs networking (share or upstream peer).
    endpoint: Option<iroh_net::Endpoint>,
    /// Persistent connector to an upstream peer (the literal `asp watch --peer`
    /// dial loop), kept open so live edits push both ways without an explicit sync.
    connector: Option<tokio::task::JoinHandle<()>>,
    /// The upstream peer ticket this folder stays connected to, if any.
    #[allow(dead_code)]
    peer: Option<String>,
}

/// Persisted record of a managed folder (small app config — not protocol state).
#[derive(Clone, Serialize, Deserialize)]
struct FolderCfg {
    path: String,
    #[serde(default)]
    peer: Option<String>,
}

type ChangeCb = Arc<dyn Fn(String) + Send + Sync>;

pub struct DesktopEngine {
    rt: tokio::runtime::Runtime,
    identity: Identity,
    folders: Mutex<HashMap<String, Folder>>,
    /// Fired with a vault_id whenever a peer's change integrates into any folder's
    /// engine — the Tauri shell sets this to emit a `vault-changed` event so the UI
    /// updates the instant a change lands (no waiting on a poll). Shared (Arc) so
    /// per-engine notifiers read it at fire time even if it's set after open.
    change_listener: Arc<Mutex<Option<ChangeCb>>>,
    /// When set, a co-hosted relay's URL — endpoints bind through it (and tickets
    /// advertise it) so same-machine/LAN peers route locally instead of via the
    /// public n0 relays. Backs the "faster local syncing" toggle.
    relay_override: Mutex<Option<String>>,
    /// The co-hosted relay task (abort to stop it).
    relay_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
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
        let de = DesktopEngine {
            rt,
            identity,
            folders: Mutex::new(HashMap::new()),
            change_listener: Arc::new(Mutex::new(None)),
            relay_override: Mutex::new(None),
            relay_task: Mutex::new(None),
        };
        // Restore the persisted "faster local syncing" preference: co-host the
        // relay up front (before any folder binds) so reopened vaults' tickets
        // advertise it. A bind failure just falls back to the default relays.
        if Self::read_local_relay_setting() {
            let _ = de.set_local_relay(true);
        }
        Ok(de)
    }

    /// Whether the co-hosted local relay is currently on.
    pub fn local_relay_on(&self) -> bool {
        self.relay_override.lock().unwrap().is_some()
    }

    /// The co-hosted relay's URL when on (the relay peers route through).
    pub fn local_relay_url(&self) -> Option<String> {
        self.relay_override.lock().unwrap().clone()
    }

    /// Toggle a co-hosted local relay ("faster local syncing"). When on, spin up a
    /// relay on a free localhost port, pin it as the relay for endpoint binds, and
    /// re-establish every active share/connector through it (re-minting tickets).
    /// When off, stop the relay and re-establish back onto the default (n0) relays.
    pub fn set_local_relay(&self, on: bool) -> Result<bool> {
        if on == self.local_relay_on() {
            return Ok(on); // already in the requested state
        }
        if on {
            let bind: std::net::SocketAddr = "127.0.0.1:0".parse().expect("static addr");
            let (url, task) = self.block(async move { iroh_net::spawn_relay(bind).await })?;
            *self.relay_override.lock().unwrap() = Some(url);
            *self.relay_task.lock().unwrap() = Some(task);
        } else {
            if let Some(task) = self.relay_task.lock().unwrap().take() {
                task.abort();
            }
            *self.relay_override.lock().unwrap() = None;
        }
        Self::write_local_relay_setting(on); // persist across restarts
        self.reestablish_all()?;
        Ok(on)
    }

    /// Tear down and re-bind every folder's endpoint so active shares/connectors
    /// pick up the current relay choice (a new endpoint = a new ticket; the device
    /// NodeId and per-vault admission set are unchanged).
    fn reestablish_all(&self) -> Result<()> {
        let mut folders = self.folders.lock().unwrap();
        for f in folders.values_mut() {
            if let Some(h) = f.listener.take() {
                h.abort();
            }
            if let Some(h) = f.connector.take() {
                h.abort();
            }
            let was_sharing = f.listening_ticket.take().is_some();
            let peer = f.peer.clone();
            f.endpoint = None;
            if !was_sharing && peer.is_none() {
                continue; // a plain local folder binds lazily on first share
            }
            let ep = self.bind_ep().context("re-bind endpoint")?;
            f.endpoint = Some(ep.clone());
            if let Some(ticket) = &peer {
                f.connector = Some(self.spawn_connector(f.engine.clone(), f.conns.clone(), ep.clone(), ticket.clone()));
            }
            if was_sharing {
                let relays = Self::use_relays();
                let relay_url = self.relay_url();
                let ep_t = ep.clone();
                let ticket = self
                    .block(async move { iroh_net::ticket_with_relay(&ep_t, relays, relay_url.as_deref()).await })
                    .context("re-mint ticket")?;
                let (engine, conns, auth) = (f.engine.clone(), f.conns.clone(), self.auth_opts(Vec::new()));
                let ep_s = ep.clone();
                f.listener = Some(self.rt.spawn(async move {
                    let _ = iroh_net::serve(engine, ep_s, auth, conns).await;
                }));
                f.listening_ticket = Some(ticket);
            }
        }
        Ok(())
    }

    /// Register a callback fired (with the vault_id) when a peer's change lands in
    /// any managed folder. The Tauri shell uses it to emit a realtime UI event.
    pub fn set_change_listener(&self, cb: impl Fn(String) + Send + Sync + 'static) {
        *self.change_listener.lock().unwrap() = Some(Arc::new(cb));
    }

    pub fn identity_ssh(&self) -> String {
        self.identity.to_ssh_string()
    }

    /// Drive a future to completion on the engine's runtime from *any* calling
    /// context — crucially, including from inside another tokio runtime.
    ///
    /// We must NOT use `self.rt.block_on()` directly. Tauri dispatches our
    /// `#[command(async)]` handlers via `async_runtime::spawn`, so command code
    /// runs *inside Tauri's own tokio runtime*. Calling a second runtime's
    /// `block_on` from there panics ("Cannot start a runtime from within a
    /// runtime"); that panic aborts the command task before it can reply, so the
    /// webview's `invoke` promise hangs forever (e.g. Share stuck on
    /// "Generating…", an edit's write never resolving). Spawning the future onto
    /// our runtime and parking the caller on a channel is safe from any context:
    /// our runtime's own worker threads drive the future to completion.
    fn block<F>(&self, fut: F) -> F::Output
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.rt.spawn(async move {
            let _ = tx.send(fut.await);
        });
        rx.recv().expect("engine runtime task panicked or runtime shut down")
    }

    fn auth_opts(&self, auth_keys: Vec<String>) -> AuthOpts {
        AuthOpts { auth_keys, no_tofu: false, default_ttl_days: 90 }
    }

    /// Use the public n0 relays unless `ASP_NO_RELAY` opts into direct/LAN-only
    /// (hermetic tests, trusted LANs) — mirrors the CLI's `--no-relay`.
    fn use_relays() -> bool {
        !matches!(std::env::var("ASP_NO_RELAY").as_deref(), Ok("1") | Ok("true"))
    }

    /// Pin a specific self-hosted relay (`asp relay`) via `ASP_RELAY_URL`, mirroring
    /// the CLI's `--relay-url`. When set it takes precedence over the public n0
    /// relays (and over `ASP_NO_RELAY`), so a NAT'd desktop user can route through
    /// their own relay; without it the endpoint uses n0 / direct per `use_relays`.
    fn relay_url(&self) -> Option<String> {
        // The co-hosted local relay takes precedence over the env-pinned one.
        if let Some(u) = self.relay_override.lock().unwrap().clone() {
            return Some(u);
        }
        match std::env::var("ASP_RELAY_URL") {
            Ok(u) if !u.trim().is_empty() => Some(u.trim().to_string()),
            _ => None,
        }
    }

    fn handle(&self, eng: Engine) -> EngineRef {
        // Bridge this engine's integrate-notifier to the manager's change listener,
        // tagged with the folder's vault_id so the UI knows which vault moved.
        let vault_id = VaultConfig::new(&eng.store).vault_id().ok().flatten().unwrap_or_default();
        let slot = self.change_listener.clone();
        eng.set_change_listener(Arc::new(move || {
            if let Some(cb) = slot.lock().unwrap().as_ref() {
                cb(vault_id.clone());
            }
        }));
        // The desktop never reads the derived `.asp/git` tree, so skip the
        // O(all-files) git export on every edit (it dominated materialize on a
        // large vault). The CLI keeps it on.
        eng.set_git_export(false);
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
    /// contents into the log, and remembers the path so it reopens next launch.
    pub fn add_local_folder(&self, path: &Path) -> Result<VaultInfo> {
        let info = self.add_folder_inner(path, None)?;
        self.remember_folder(path, None);
        Ok(info)
    }

    /// Open/init a folder and register it (with its live services), without
    /// touching the persisted list. Shared by `add_local_folder`/`reopen_saved`.
    /// `peer` is an optional upstream ticket to stay connected to.
    fn add_folder_inner(&self, path: &Path, peer: Option<String>) -> Result<VaultInfo> {
        let eng = if path.join(".asp/asp.db").exists() {
            Engine::open(path, self.identity.clone())?
        } else {
            Engine::init(path, self.identity.clone())?
        };
        eng.capture_rescan()?;
        let id = random_id();
        let engine = self.handle(eng);
        let conns: Conns = Arc::new(AsyncMutex::new(HashMap::new()));
        // A folder that follows an upstream peer needs its (single, shared)
        // endpoint up front for the connector; a plain local folder binds lazily
        // when first shared.
        let endpoint = match &peer {
            Some(_) => Some(self.bind_ep().context("folder endpoint")?),
            None => None,
        };
        let connector = self.maybe_connector(&engine, &conns, endpoint.as_ref(), peer.as_deref());
        let folder = Folder {
            id: id.clone(),
            path: path.to_path_buf(),
            engine,
            conns,
            enabled: false,
            listening_ticket: None,
            listener: None,
            endpoint,
            connector,
            peer,
        };
        let info = Self::info_of(&folder);
        self.folders.lock().unwrap().insert(id, folder);
        Ok(info)
    }

    /// Bind a fresh long-lived iroh endpoint for this device key.
    fn bind_ep(&self) -> Result<iroh_net::Endpoint> {
        let seed = self.identity.seed();
        let relays = Self::use_relays();
        let relay_url = self.relay_url();
        self.block(async move {
            iroh_net::bind_endpoint_relay(&seed, relays, relay_url.as_deref()).await
        })
    }

    /// If `peer`+`ep` are set, start a persistent reconnecting connector to that
    /// upstream (reusing the folder's single shared endpoint). Note: in-app edits
    /// capture via `record_*` and push via `broadcast`, so there is deliberately
    /// no per-folder fs watcher — that would re-hash the whole folder on every
    /// save. External on-disk edits are picked up on reopen or an explicit
    /// `rescan`, not live.
    fn maybe_connector(&self, engine: &EngineRef, conns: &Conns, ep: Option<&iroh_net::Endpoint>, peer: Option<&str>) -> Option<tokio::task::JoinHandle<()>> {
        match (peer, ep) {
            (Some(ticket), Some(ep)) => Some(self.spawn_connector(engine.clone(), conns.clone(), ep.clone(), ticket.to_string())),
            _ => None,
        }
    }

    /// Persistent connector loop: dial the upstream and run a live (`oneshot=false`)
    /// session on the folder's **shared** endpoint, reconnecting with a short
    /// backoff if it drops — the desktop equivalent of `asp watch --peer <ticket>`.
    /// Never closes the endpoint (the listener shares it).
    fn spawn_connector(&self, engine: EngineRef, conns: Conns, ep: iroh_net::Endpoint, ticket: String) -> tokio::task::JoinHandle<()> {
        // Reconnect needs no enrollment secret — the key is authorized after the
        // first successful connect/clone.
        let auth = self.auth_opts(Vec::new());
        self.rt.spawn(async move {
            let addr = match iroh_net::parse_peer(&ticket) {
                Ok(a) => a,
                Err(_) => return,
            };
            loop {
                let _ = iroh_net::connect(engine.clone(), &ep, addr.clone(), &auth, false, conns.clone(), None).await;
                tokio::time::sleep(std::time::Duration::from_millis(700)).await;
            }
        })
    }

    /// Fan a freshly-authored row out to every live peer of a folder (the
    /// real-time push the `asp watch` watcher does for disk edits — here for the
    /// app's own `record_*` edits, which materialize before the watcher sees them).
    fn broadcast(&self, conns: &Conns, wr: WireRow) {
        let conns = conns.clone();
        self.block(async move {
            let map = conns.lock().await;
            for tx in map.values() {
                let _ = tx.send(Msg::Push { row: Box::new(wr.clone()) });
            }
        });
    }

    /// Path of the small app-config file listing managed folders (allowed
    /// non-protocol app state; shares `~/.asp` with the CLI identity).
    fn folders_config_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".asp").join("desktop_folders.json")
    }

    fn settings_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".asp").join("desktop_settings.json")
    }

    /// The persisted "faster local syncing" preference (default off).
    fn read_local_relay_setting() -> bool {
        std::fs::read_to_string(Self::settings_path())
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("local_relay").and_then(|b| b.as_bool()))
            .unwrap_or(false)
    }

    fn write_local_relay_setting(on: bool) {
        let p = Self::settings_path();
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&p, serde_json::json!({ "local_relay": on }).to_string());
    }

    fn saved_folders() -> Vec<FolderCfg> {
        std::fs::read_to_string(Self::folders_config_path())
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<FolderCfg>>(&s).ok())
            .unwrap_or_default()
    }

    fn write_saved_folders(list: &[FolderCfg]) {
        let p = Self::folders_config_path();
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_string_pretty(list) {
            let _ = std::fs::write(&p, s);
        }
    }

    fn remember_folder(&self, path: &Path, peer: Option<String>) {
        let p = path.to_string_lossy().to_string();
        let mut list = Self::saved_folders();
        match list.iter_mut().find(|c| c.path == p) {
            Some(c) => c.peer = peer,
            None => list.push(FolderCfg { path: p, peer }),
        }
        Self::write_saved_folders(&list);
    }

    fn forget_folder(&self, path: &Path) {
        let p = path.to_string_lossy().to_string();
        let mut list = Self::saved_folders();
        if let Some(i) = list.iter().position(|c| c.path == p) {
            list.remove(i);
            Self::write_saved_folders(&list);
        }
    }

    /// Re-open every folder remembered from a previous session (reconnecting any
    /// persisted upstream peers). Call once at startup. Folders that no longer
    /// exist on disk are skipped (and pruned).
    pub fn reopen_saved(&self) -> Result<Vec<VaultInfo>> {
        Ok(self.reopen_saved_streaming(|_| {}))
    }

    /// Reopen saved folders **concurrently**, invoking `on_each` the moment each
    /// one is ready. Each folder's open includes a startup reconcile that reads
    /// every file on disk (the only way external-while-closed edits are caught —
    /// there's no fs watcher), so a 28k-file vault takes ~tens of seconds. Doing
    /// them in parallel and streaming each as it lands means a big vault never
    /// blocks the small ones, and the shell can surface vaults the instant they're
    /// ready (a realtime `vaults-changed` event) rather than after the slowest.
    pub fn reopen_saved_streaming(&self, on_each: impl Fn(&VaultInfo) + Send + Sync) -> Vec<VaultInfo> {
        let cfgs: Vec<FolderCfg> = Self::saved_folders()
            .into_iter()
            .filter(|c| PathBuf::from(&c.path).join(".asp/asp.db").exists())
            .collect();
        let opened: Vec<(FolderCfg, VaultInfo)> = std::thread::scope(|s| {
            let handles: Vec<_> = cfgs
                .iter()
                .map(|cfg| {
                    let on_each = &on_each;
                    s.spawn(move || {
                        let info = self.add_folder_inner(&PathBuf::from(&cfg.path), cfg.peer.clone()).ok()?;
                        on_each(&info);
                        Some((cfg.clone(), info))
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
        });
        let keep: Vec<FolderCfg> = opened.iter().map(|(c, _)| c.clone()).collect();
        Self::write_saved_folders(&keep);
        opened.into_iter().map(|(_, i)| i).collect()
    }

    /// Bootstrap a new folder by cloning from a listening peer (by iroh ticket /
    /// node id).
    pub fn clone_remote(&self, dest: &Path, ticket: &str, auth_key: Option<&str>) -> Result<VaultInfo> {
        let eng = Engine::open(dest, self.identity.clone())?;
        let engine = self.handle(eng);
        let auth = self.auth_opts(auth_key.map(|s| vec![s.to_string()]).unwrap_or_default());
        let addr = iroh_net::parse_peer(ticket)?;
        // Bind the folder's single shared endpoint once, bootstrap the clone on
        // it, then keep it for the persistent connector (don't close it).
        let ep = self.bind_ep().context("clone endpoint")?;
        let ce = engine.clone();
        let (bep, baddr) = (ep.clone(), addr.clone());
        self.block(async move { iroh_net::clone_bootstrap(ce, &bep, baddr, &auth).await.map(|_| ()) })?;
        let id = random_id();
        let conns: Conns = Arc::new(AsyncMutex::new(HashMap::new()));
        // Stay connected to the source so edits sync live both ways (not one-shot).
        let peer = Some(ticket.to_string());
        let endpoint = Some(ep);
        let connector = self.maybe_connector(&engine, &conns, endpoint.as_ref(), peer.as_deref());
        let folder = Folder {
            id: id.clone(),
            path: dest.to_path_buf(),
            engine,
            conns,
            enabled: false,
            listening_ticket: None,
            listener: None,
            endpoint,
            connector,
            peer,
        };
        let info = Self::info_of(&folder);
        self.folders.lock().unwrap().insert(id, folder);
        self.remember_folder(dest, Some(ticket.to_string()));
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
            let relays = Self::use_relays();
            // Reuse the folder's single shared endpoint (the connector may already
            // hold it); bind it lazily on first share otherwise.
            let ep = match &f.endpoint {
                Some(ep) => ep.clone(),
                None => {
                    let ep = self.bind_ep().context("listener bind")?;
                    f.endpoint = Some(ep.clone());
                    ep
                }
            };
            let ep_t = ep.clone();
            let relay_url = self.relay_url();
            let ticket = self
                .block(async move { iroh_net::ticket_with_relay(&ep_t, relays, relay_url.as_deref()).await })
                .context("ticket")?;
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
        let (engine, shared_ep) = {
            let folders = self.folders.lock().unwrap();
            let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
            (f.engine.clone(), f.endpoint.clone())
        };
        let auth = self.auth_opts(auth_key.map(|s| vec![s.to_string()]).unwrap_or_default());
        let addr = iroh_net::parse_peer(ticket)?;
        let seed = self.identity.seed();
        let relay_url = self.relay_url();
        self.block(async move {
            match shared_ep {
                // Reuse the folder's standing endpoint (one device key, one socket).
                Some(ep) => iroh_net::sync_oneshot(engine, &ep, addr, &auth).await,
                // No standing endpoint (a plain local folder): a throwaway is fine.
                None => {
                    let ep =
                        iroh_net::bind_endpoint_relay(&seed, Self::use_relays(), relay_url.as_deref()).await?;
                    let r = iroh_net::sync_oneshot(engine, &ep, addr, &auth).await;
                    ep.close().await;
                    r
                }
            }
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
        // `restore` authors the rows that revert the vault to the target state;
        // push them live to peers (as write_file/create_dir do) so a connected
        // peer converges instead of silently keeping the pre-restore content.
        let (conns, rows) = {
            let folders = self.folders.lock().unwrap();
            let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
            let eng = f.engine.lock().unwrap();
            let rows = eng.restore(target)?;
            (f.conns.clone(), rows)
        };
        for wr in rows {
            self.broadcast(&conns, wr);
        }
        Ok(())
    }

    pub fn status(&self, id: &str) -> Result<VaultStatus> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        let vault_id = VaultConfig::new(&eng.store).vault_id().ok().flatten().unwrap_or_default();
        let files = eng.store.live_file_count()?;
        let head = std::fs::read_to_string(eng.git_dir.join("refs/heads/main")).map(|s| s.trim().to_string()).unwrap_or_default();
        let peers = eng.store.peers()?.into_iter().map(|(u, _)| u).collect();
        let last_ts = eng.store.max_ts()?;
        Ok(VaultStatus {
            id: id.to_string(),
            vault_id,
            rows: eng.store.row_count()?,
            files,
            head,
            listening_ticket: f.listening_ticket.clone(),
            peers,
            last_ts,
        })
    }

    // ---- File surface: thin forwarders to `asp-core` (no protocol logic) ----

    /// List the vault's live files (flat; the UI builds the tree).
    pub fn list_files(&self, id: &str) -> Result<Vec<FileEntry>> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        Ok(eng
            .store
            .live_files()?
            .into_iter()
            .filter(|fr| !fr.deleted)
            .map(|fr| FileEntry {
                is_dir: fr.merge_class == asp_core::MergeClass::Dir,
                merge_class: fr.merge_class.as_str().to_string(),
                path: fr.path,
                file_id: fr.file_id,
            })
            .collect())
    }

    /// Read a live file's current content (the materialized file on disk is the
    /// ground truth `asp-core` renders to).
    pub fn read_file(&self, id: &str, path: &str) -> Result<String> {
        let full = {
            let folders = self.folders.lock().unwrap();
            let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
            f.path.join(path)
        };
        let bytes = std::fs::read(&full).with_context(|| format!("read {}", full.display()))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Create or update a file by recording an edit (persists to the log + disk
    /// via `asp-core` materialize). New paths author a `Create`. The authored row
    /// is pushed live to every connected peer.
    pub fn write_file(&self, id: &str, path: &str, content: &str) -> Result<()> {
        let (conns, wr) = {
            let folders = self.folders.lock().unwrap();
            let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
            let eng = f.engine.lock().unwrap();
            let wr = eng.record_write(path, content.as_bytes())?;
            (f.conns.clone(), wr)
        };
        if let Some(wr) = wr {
            self.broadcast(&conns, wr);
        }
        Ok(())
    }

    /// Rename/move a file (preserves its stable `file_id`); pushed live to peers.
    pub fn rename_file(&self, id: &str, old: &str, new: &str) -> Result<()> {
        let (conns, wr) = {
            let folders = self.folders.lock().unwrap();
            let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
            let eng = f.engine.lock().unwrap();
            let wr = eng.record_rename(old, new)?;
            (f.conns.clone(), wr)
        };
        if let Some(wr) = wr {
            self.broadcast(&conns, wr);
        }
        Ok(())
    }

    /// Delete a file (authors a tombstone; removes it from disk on materialize);
    /// pushed live to peers.
    pub fn delete_file(&self, id: &str, path: &str) -> Result<()> {
        let (conns, wr) = {
            let folders = self.folders.lock().unwrap();
            let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
            let eng = f.engine.lock().unwrap();
            let wr = eng.record_remove(path)?;
            (f.conns.clone(), wr)
        };
        if let Some(wr) = wr {
            self.broadcast(&conns, wr);
        }
        Ok(())
    }

    /// Create an empty directory. A physically-empty in-scope directory is a
    /// first-class, content-free entity in `asp-core` (materialized via real
    /// `mkdir`), so we `mkdir` then `capture_rescan`, which authors the `Dir`
    /// row(s); each is pushed live to every connected peer.
    pub fn create_dir(&self, id: &str, path: &str) -> Result<()> {
        let (conns, rows) = {
            let folders = self.folders.lock().unwrap();
            let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
            std::fs::create_dir_all(f.path.join(path)).with_context(|| format!("mkdir {}", path))?;
            let eng = f.engine.lock().unwrap();
            let rows = eng.capture_rescan()?;
            (f.conns.clone(), rows)
        };
        for wr in rows {
            self.broadcast(&conns, wr);
        }
        Ok(())
    }

    /// Project the append-only log into wall-clock history events for the
    /// time-travel scrubber. Resolves a path for every row (edits/deletes carry
    /// none, so we track each `file_id`'s latest path in fold order).
    pub fn history(&self, id: &str) -> Result<Vec<HistEvent>> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        let mut latest: HashMap<String, String> = HashMap::new();
        let mut out = Vec::new();
        for r in eng.store.all_rows()? {
            if let Some(p) = &r.path {
                latest.insert(r.file_id.clone(), p.clone());
            }
            let path = r.path.clone().or_else(|| latest.get(&r.file_id).cloned()).unwrap_or_default();
            out.push(HistEvent {
                id: r.id,
                ts: r.ts,
                lamport: r.lamport,
                kind: r.kind.as_str().to_string(),
                path,
            });
        }
        Ok(out)
    }

    /// Content of a file as the vault was at wall-clock `ts` (read-only; folds
    /// rows with `ts <= ts` via `asp-core::state_as_of`).
    pub fn read_file_at(&self, id: &str, path: &str, ts: i64) -> Result<FileAt> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        match eng.state_as_of(ts)?.get(path) {
            Some(bytes) => Ok(FileAt { exists: true, content: String::from_utf8_lossy(bytes).into_owned() }),
            None => Ok(FileAt { exists: false, content: String::new() }),
        }
    }

    /// Restore one file to its content as of `ts` (records the historical bytes
    /// as a new edit — the log stays append-only). No-op if it didn't exist then.
    pub fn restore_file_at(&self, id: &str, path: &str, ts: i64) -> Result<()> {
        let (conns, wr) = {
            let folders = self.folders.lock().unwrap();
            let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
            let eng = f.engine.lock().unwrap();
            let wr = match eng.state_as_of(ts)?.get(path) {
                Some(bytes) => eng.record_write(path, bytes)?,
                None => None,
            };
            (f.conns.clone(), wr)
        };
        if let Some(wr) = wr {
            self.broadcast(&conns, wr);
        }
        Ok(())
    }

    /// Re-capture on-disk changes into the log (manual refresh after external
    /// edits). Mirrors the CLI's rescan.
    pub fn rescan(&self, id: &str) -> Result<()> {
        // capture_rescan authors rows for on-disk changes made behind the engine
        // (external editors, git pulls, scripts). Broadcast them live to peers —
        // exactly as create_dir does with its capture_rescan rows — so an external
        // edit + refresh propagates instead of leaving connected peers stale.
        let (conns, rows) = {
            let folders = self.folders.lock().unwrap();
            let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
            let eng = f.engine.lock().unwrap();
            let rows = eng.capture_rescan()?;
            (f.conns.clone(), rows)
        };
        for wr in rows {
            self.broadcast(&conns, wr);
        }
        Ok(())
    }

    /// Stop managing a vault: tear down its listener and forget it (so it does
    /// not reopen next launch). `trash` is accepted for the UI's "move folder to
    /// Trash" toggle but OS-trash deletion is deferred — we never destroy data
    /// here; the folder and its `.asp` history stay on disk.
    pub fn remove_vault(&self, id: &str, _trash: bool) -> Result<()> {
        let folder = {
            let mut folders = self.folders.lock().unwrap();
            folders.remove(id)
        };
        if let Some(mut f) = folder {
            if let Some(h) = f.listener.take() {
                h.abort();
            }
            if let Some(h) = f.connector.take() {
                h.abort();
            }
            self.forget_folder(&f.path);
        }
        Ok(())
    }
}
