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
use asp_core::gitbridge::GitRemoteSpec;
use asp_core::gitpush::{self, PushReport};
use asp_core::gitremote::{self, CloneOptions, PullReport};
use asp_core::gitwire::parse_git_url;
use asp_core::iroh_net;
use asp_core::net::{AuthOpts, EngineRef};
use asp_core::{Engine, Identity, Msg, VaultConfig, WireRow};
pub use asp_core::Graph;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
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
    /// The branch the row was authored on — lets the timeline place each event on
    /// its branch lane (the network-graph view).
    pub branch_id: String,
}

/// One branch for the switcher UI (a thin projection of an `asp-core` `Branch`).
#[derive(Clone, Serialize)]
pub struct BranchDto {
    pub branch_id: String,
    pub name: String,
    pub parent: Option<String>,
    /// True for the checked-out branch (HEAD).
    pub current: bool,
}

/// One tag for the timeline UI (a thin projection of an `asp-core` `Tag`).
#[derive(Clone, Serialize)]
pub struct TagDto {
    pub tag_id: String,
    pub name: String,
    /// Wall-clock unix seconds the tag marks.
    pub at_ts: i64,
    pub branch_id: String,
}

/// Content of a file as of a point in time (for read-only time travel).
#[derive(Clone, Serialize)]
pub struct FileAt {
    /// Whether the file existed (non-deleted) at the requested instant.
    pub exists: bool,
    pub content: String,
}

/// The git-bridge status chip DTO (git-bridge §7.2). A camelCased projection of
/// [`asp_core::gitremote::GitStatus`] — the core type is snake_case, so we map it
/// here (rather than touching core) to match the shared TS `GitStatus` shape
/// (`{remoteUrl, atSha, frozen, ahead, behind, policy}`) the web slice already uses.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusDto {
    pub remote_url: String,
    pub at_sha: Option<String>,
    pub frozen: bool,
    pub ahead: usize,
    pub behind: usize,
    pub policy: String,
}

impl From<gitremote::GitStatus> for GitStatusDto {
    fn from(s: gitremote::GitStatus) -> Self {
        GitStatusDto {
            remote_url: s.remote_url,
            at_sha: s.at_sha,
            frozen: s.frozen,
            ahead: s.ahead,
            behind: s.behind,
            policy: s.policy,
        }
    }
}

/// A small summary of a [`DesktopEngine::git_pull`] (git-bridge §4). The web
/// `gitPull` binding ignores the payload (`Promise<void>`); we return a shape so
/// the desktop UI can surface "ingested N commits" / "frozen" if it wants to.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPullSummary {
    pub new_commits: usize,
    pub frozen: bool,
    pub up_to_date: bool,
}

/// A small summary of a [`DesktopEngine::git_push`] (git-bridge §7.2). Push is
/// desktop/CLI-only (the web binding rejects), so this shape is only ever produced
/// natively; the UI surfaces "pushed <sha> (N commit(s))" or "nothing to commit".
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPushSummary {
    /// The new remote tip sha, or `None` when nothing was unpushed.
    pub pushed_sha: Option<String>,
    /// Number of commits actually pushed (0 when nothing to commit).
    pub commits: usize,
}

impl From<PushReport> for GitPushSummary {
    fn from(r: PushReport) -> Self {
        match r {
            PushReport::Nothing => GitPushSummary { pushed_sha: None, commits: 0 },
            PushReport::Pushed { pushed_sha, commits_pushed, .. } => {
                GitPushSummary { pushed_sha: Some(pushed_sha), commits: commits_pushed }
            }
        }
    }
}

/// The pending (unpushed) change set for a git-bridge folder (git-bridge §5.3), a
/// camelCased projection of [`asp_core::gitpush::PendingDiff`]. The UI reads it to
/// pre-fill the commit message and show what a push would send before confirming.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingDiffDto {
    pub files_changed: usize,
    pub paths: Vec<String>,
    pub unified: String,
}

impl From<PullReport> for GitPullSummary {
    fn from(r: PullReport) -> Self {
        match r {
            PullReport::UpToDate => GitPullSummary { new_commits: 0, frozen: false, up_to_date: true },
            PullReport::Frozen => GitPullSummary { new_commits: 0, frozen: true, up_to_date: false },
            PullReport::Updated { new_commits, .. } => {
                GitPullSummary { new_commits, frozen: false, up_to_date: false }
            }
        }
    }
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
    /// True when this folder was cloned from a git remote (git-bridge §7.2). Its
    /// configured remote lives in the engine's own `git_remotes` table (not in
    /// `FolderCfg.peer`); this flag just tells us to re-arm the periodic pull tick.
    git: bool,
    /// The periodic `git pull` tick for a git-bridge folder (abort to stop it) —
    /// the desktop analogue of the CLI watch loop's `git_pull_tick`.
    pull_task: Option<tokio::task::JoinHandle<()>>,
}

/// Persisted record of a managed folder (small app config — not protocol state).
#[derive(Clone, Serialize, Deserialize)]
struct FolderCfg {
    path: String,
    #[serde(default)]
    peer: Option<String>,
    /// True for a git-bridge folder (cloned from a git URL) — so reopen knows to
    /// re-arm its periodic pull tick. The remote URL itself is already persisted in
    /// the engine's `git_remotes` table, so a bool is enough here.
    #[serde(default)]
    git: bool,
}

type ChangeCb = Arc<dyn Fn(String) + Send + Sync>;
/// A clone/pull progress sink: `(path, done, total, phase)` — the same shape the
/// startup reconcile emits, so the shell reuses its `vault-scan-progress` event.
type ScanCb = Arc<dyn Fn(&str, u64, u64, &str) + Send + Sync>;

pub struct DesktopEngine {
    rt: tokio::runtime::Runtime,
    identity: Identity,
    folders: Mutex<HashMap<String, Folder>>,
    /// Fired with a vault_id whenever a peer's change integrates into any folder's
    /// engine — the Tauri shell sets this to emit a `vault-changed` event so the UI
    /// updates the instant a change lands (no waiting on a poll). Shared (Arc) so
    /// per-engine notifiers read it at fire time even if it's set after open.
    change_listener: Arc<Mutex<Option<ChangeCb>>>,
    /// Fired with `(path, done, total, phase)` during a `clone_git`/`git_pull` so the
    /// shell can emit a `vault-scan-progress` event (phases `fetching`|`replaying`|
    /// `saving`). Set by the shell after construction, exactly like `change_listener`.
    scan_listener: Arc<Mutex<Option<ScanCb>>>,
    /// When set, a co-hosted relay's URL — endpoints bind through it (and tickets
    /// advertise it) so same-machine/LAN peers route locally instead of via the
    /// public n0 relays. Backs the "faster local syncing" toggle.
    relay_override: Mutex<Option<String>>,
    /// The co-hosted relay task (abort to stop it).
    relay_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Set once `reopen_saved` (the background startup rehydrate) has finished, so
    /// the UI can deterministically clear its "Loading your vaults…" gate by
    /// querying instead of having to catch the one-shot `vaults-ready` event — which
    /// it misses if the webview's listener isn't attached before the (often instant,
    /// e.g. empty-config) reopen emits it.
    ready: AtomicBool,
}

/// Parse an explicit git clone URL. Accepts everything [`parse_git_url`] does
/// (https / ssh / scp-like) plus a **loopback** `http://` URL — a self-hosted git
/// server on `127.0.0.1`/`localhost` served over plain HTTP. The bridge transport
/// uses the URL's base verbatim, so an `http://` loopback base works; and because
/// this is an *explicit* clone target (not the CLI's source auto-detection), there
/// is no risk of mistaking it for a local path or an iroh ticket — the `http://`
/// scheme is unambiguous. Non-loopback `http://` stays rejected (no plaintext creds
/// over the network).
fn git_clone_url(url: &str) -> Option<asp_core::gitwire::GitUrl> {
    use asp_core::gitwire::GitUrl;
    if let Some(g) = parse_git_url(url) {
        return Some(g);
    }
    let s = url.trim();
    if let Some(rest) = s.strip_prefix("http://") {
        let authority = rest.split('/').next().unwrap_or("");
        if authority.contains('@') {
            return None; // no embedded userinfo
        }
        let host = authority.split(':').next().unwrap_or("");
        if host == "localhost" || host == "127.0.0.1" || host == "[::1]" {
            return Some(GitUrl::Https { base: s.trim_end_matches('/').to_string() });
        }
    }
    None
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
            scan_listener: Arc::new(Mutex::new(None)),
            relay_override: Mutex::new(None),
            relay_task: Mutex::new(None),
            ready: AtomicBool::new(false),
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

    /// Register a callback fired with `(path, done, total, phase)` during a
    /// `clone_git`/`git_pull`. The shell wires this to the same `vault-scan-progress`
    /// event the startup reconcile uses, so a git clone shows a determinate bar
    /// (`fetching` → `replaying` → `saving`).
    pub fn set_scan_progress_listener(&self, cb: impl Fn(&str, u64, u64, &str) + Send + Sync + 'static) {
        *self.scan_listener.lock().unwrap() = Some(Arc::new(cb));
    }

    /// Whether `reopen_saved` has completed. The UI polls this once on mount as a
    /// race-proof fallback to the `vaults-ready` event (see the `ready` field).
    pub fn vaults_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
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
        let info = self.add_folder_inner(path, None, false)?;
        self.remember_folder(path, None, false);
        Ok(info)
    }

    /// Open/init a folder and register it (with its live services), without
    /// touching the persisted list. Shared by `add_local_folder`/`reopen_saved`.
    /// `peer` is an optional upstream ticket to stay connected to; `git` marks a
    /// git-bridge folder (so its pull tick is re-armed by the caller).
    fn add_folder_inner(&self, path: &Path, peer: Option<String>, git: bool) -> Result<VaultInfo> {
        self.add_folder_inner_progress(path, peer, git, &|_, _, _| {})
    }

    /// Like [`add_folder_inner`] but reports the startup reconcile's scan/hash/save
    /// progress to `on` — so the shell can show a determinate progress bar during a
    /// large (e.g. 28k-file) vault's open instead of an indeterminate spinner.
    fn add_folder_inner_progress(
        &self,
        path: &Path,
        peer: Option<String>,
        git: bool,
        on: &(dyn Fn(u64, u64, &str) + Sync),
    ) -> Result<VaultInfo> {
        let eng = if path.join(".asp/asp.db").exists() {
            Engine::open(path, self.identity.clone())?
        } else {
            Engine::init(path, self.identity.clone())?
        };
        eng.capture_rescan_progress(on)?;
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
            git,
            pull_task: None,
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

    fn remember_folder(&self, path: &Path, peer: Option<String>, git: bool) {
        let p = path.to_string_lossy().to_string();
        let mut list = Self::saved_folders();
        match list.iter_mut().find(|c| c.path == p) {
            Some(c) => {
                c.peer = peer;
                c.git = c.git || git;
            }
            None => list.push(FolderCfg { path: p, peer, git }),
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
        Ok(self.reopen_saved_streaming(|_| {}, |_, _, _, _| {}))
    }

    /// Reopen saved folders **concurrently**, invoking `on_each` the moment each
    /// one is ready. Each folder's open includes a startup reconcile that reads
    /// every file on disk (the only way external-while-closed edits are caught —
    /// there's no fs watcher), so a 28k-file vault takes ~tens of seconds. Doing
    /// them in parallel and streaming each as it lands means a big vault never
    /// blocks the small ones, and the shell can surface vaults the instant they're
    /// ready (a realtime `vaults-changed` event) rather than after the slowest.
    pub fn reopen_saved_streaming(
        &self,
        on_each: impl Fn(&VaultInfo) + Send + Sync,
        on_progress: impl Fn(&str, u64, u64, &str) + Send + Sync,
    ) -> Vec<VaultInfo> {
        let cfgs: Vec<FolderCfg> = Self::saved_folders()
            .into_iter()
            .filter(|c| PathBuf::from(&c.path).join(".asp/asp.db").exists())
            .collect();
        let opened: Vec<(FolderCfg, VaultInfo)> = std::thread::scope(|s| {
            let handles: Vec<_> = cfgs
                .iter()
                .map(|cfg| {
                    let on_each = &on_each;
                    let on_progress = &on_progress;
                    s.spawn(move || {
                        let prog = |d: u64, t: u64, ph: &str| on_progress(&cfg.path, d, t, ph);
                        let info = self
                            .add_folder_inner_progress(&PathBuf::from(&cfg.path), cfg.peer.clone(), cfg.git, &prog)
                            .ok()?;
                        on_each(&info);
                        Some((cfg.clone(), info))
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
        });
        // Re-arm the periodic git pull for every reopened git-bridge folder (its
        // remote config was rehydrated with the engine; we just restart the tick).
        for (cfg, info) in &opened {
            if cfg.git {
                self.arm_pull_tick(&info.id);
            }
        }
        let keep: Vec<FolderCfg> = opened.iter().map(|(c, _)| c.clone()).collect();
        Self::write_saved_folders(&keep);
        // Startup rehydrate is done — let the UI clear its loading gate (querying
        // `vaults_ready`) even if it missed the one-shot `vaults-ready` event.
        self.ready.store(true, Ordering::SeqCst);
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
            git: false,
            pull_task: None,
        };
        let info = Self::info_of(&folder);
        self.folders.lock().unwrap().insert(id, folder);
        self.remember_folder(dest, Some(ticket.to_string()), false);
        Ok(info)
    }

    /// Bootstrap a new folder by cloning a **git** remote (git-bridge §7.2) — the
    /// native mirror of the web slice's `cloneGit`. Wraps
    /// [`asp_core::gitremote::clone_from_git`] the way [`clone_remote`] wraps the
    /// iroh bootstrap: run the driver, then `handle()` the populated engine, build
    /// the `Folder`, persist it (git-flagged), and arm the periodic pull.
    ///
    /// Concurrency note: `clone_from_git` is `async fn(&Engine, …)` and the on-disk
    /// `Engine` holds a `!Sync` SQLite handle, so the future borrows `&Engine` across
    /// `.await` and is therefore `!Send` — it cannot be driven by [`Self::block`]
    /// (which spawns onto the shared multi-thread runtime). We instead clone into a
    /// **bare, unshared** `Engine` (exactly as the CLI `clone` does) and drive the
    /// future inline on a throwaway current-thread runtime on a fresh OS thread — see
    /// [`Self::run_off_thread`], which is safe from any calling context (incl. inside
    /// Tauri's runtime), just like `block()`. Only after the clone completes do we
    /// share the engine via `handle()`.
    pub fn clone_git(&self, dest: &Path, url: &str, token: Option<&str>, depth: Option<u32>, all_branches: bool) -> Result<VaultInfo> {
        let gurl = git_clone_url(url).ok_or_else(|| anyhow!("not a valid git URL: {url}"))?;
        let auth = gitremote::resolve_git_auth(&gurl, token, None);
        let spec = GitRemoteSpec::new(gurl, auth);
        // Open the (unshared) engine on the caller's thread so open errors surface
        // cleanly; it is pristine, so `clone_from_git` accepts it.
        let eng = Engine::open(dest, self.identity.clone()).context("git clone open")?;
        let scan = self.scan_listener.clone();
        let dest_label = dest.to_string_lossy().to_string();
        let (eng, report) = Self::run_off_thread(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("git bridge current-thread runtime");
            let out = rt.block_on(async {
                let progress = move |phase: &str, done: u64, total: u64| {
                    if let Some(cb) = scan.lock().unwrap().as_ref() {
                        cb(&dest_label, done, total, phase);
                    }
                };
                // `all_branches` is the "also import open branches" checkbox
                // (`specs/git-open-branches.md` §5): import every unmerged `refs/heads/*`
                // as a live ASP branch.
                let opts = CloneOptions { depth, new_identity: false, all_branches, on_progress: Some(&progress) };
                gitremote::clone_from_git(&eng, &spec, &opts).await
            });
            (eng, out)
        });
        // `report` carries best-effort degraded-content notices (submodules/LFS) and
        // the imported commit/tip summary; v1 surfaces vault state via `git_status`,
        // so we just confirm the clone succeeded here.
        let _report = report.map_err(|e| anyhow!("git clone: {e}"))?;
        let engine = self.handle(eng);
        let id = random_id();
        let conns: Conns = Arc::new(AsyncMutex::new(HashMap::new()));
        // A git folder's upstream is the git remote (in the engine's own sqlite), not
        // an ASP peer — so `peer`/`connector` stay `None` (no iroh dial loop).
        let folder = Folder {
            id: id.clone(),
            path: dest.to_path_buf(),
            engine,
            conns,
            enabled: false,
            listening_ticket: None,
            listener: None,
            endpoint: None,
            connector: None,
            peer: None,
            git: true,
            pull_task: None,
        };
        let info = Self::info_of(&folder);
        self.folders.lock().unwrap().insert(id.clone(), folder);
        self.remember_folder(dest, None, true);
        self.arm_pull_tick(&id);
        Ok(info)
    }

    /// Run a (possibly `!Send`) future's driver to completion on a throwaway
    /// current-thread runtime on a fresh OS thread. The git-bridge drivers
    /// (`clone_from_git`/`pull_once`) borrow the `!Sync` on-disk `Engine` across
    /// `.await`, so their futures are `!Send` and cannot be spawned by [`Self::block`].
    /// A fresh thread is never a runtime worker, so `block_on` there is safe from any
    /// calling context — the same guarantee `block()` gives, minus the `Send` bound.
    fn run_off_thread<T, F>(work: F) -> T
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        std::thread::spawn(work).join().expect("git bridge worker thread panicked")
    }

    /// Drive one `pull_once` against a configured remote to completion off-thread
    /// (see [`Self::run_off_thread`]). The engine lock is held across the pull's
    /// awaits — pulls are infrequent (5-min cadence) and short, exactly the tradeoff
    /// the CLI's `git_pull_tick` documents.
    #[allow(clippy::await_holding_lock)]
    fn pull_blocking(engine: EngineRef, remote_id: String) -> Result<PullReport> {
        let out = Self::run_off_thread(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("git bridge current-thread runtime");
            rt.block_on(async move {
                let e = engine.lock().unwrap();
                gitremote::pull_once(&e, &remote_id, None).await
            })
        });
        out.map_err(|e| anyhow!("{e}"))
    }

    /// Author a plan for `message` then drive one `push` to completion off-thread
    /// (see [`Self::run_off_thread`]) — the desktop mapping of the CLI's manual push
    /// policy (`author_plan` + `push`). The engine lock is held across the push's
    /// awaits exactly like [`Self::pull_blocking`]: pushes are user-triggered and
    /// short, and holding the guard keeps synthesis reading a stable log.
    #[allow(clippy::await_holding_lock)]
    fn push_blocking(engine: EngineRef, remote_id: String, message: String) -> Result<PushReport> {
        let out = Self::run_off_thread(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("git bridge current-thread runtime");
            rt.block_on(async move {
                let e = engine.lock().unwrap();
                // Manual policy = author a plan for the current frontier, then push it.
                gitpush::author_plan(&e, &remote_id, &message, None)?;
                gitpush::push(&e, &remote_id, |_phase| {}).await
            })
        });
        out.map_err(|e| anyhow!("{e}"))
    }

    /// Commit the vault's pending changes as a git commit and push it upstream
    /// (git-bridge §7.2, manual policy). Resolves the folder's single configured git
    /// remote (same lookup as [`Self::git_pull`]), then authors a plan for `message`
    /// and drives the push. Surfaces the typed errors verbatim: a frozen remote
    /// (upstream history rewritten) or a non-fast-forward that survived the bounded
    /// retry. `PushReport::Nothing` maps to `commits: 0` (nothing to commit).
    pub fn git_push(&self, id: &str, message: &str) -> Result<GitPushSummary> {
        let (engine, remote_id) = {
            let folders = self.folders.lock().unwrap();
            let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
            let rid = {
                let e = f.engine.lock().unwrap();
                e.store
                    .git_remote_list()
                    .map_err(|e| anyhow!("{e}"))?
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow!("vault has no git remote configured"))?
                    .remote_id
            };
            (f.engine.clone(), rid)
        };
        let report = Self::push_blocking(engine, remote_id, message.to_string())
            .map_err(|e| anyhow!("git push: {e}"))?;
        Ok(GitPushSummary::from(report))
    }

    /// The pending (unpushed) diff for a git-bridge folder (git-bridge §5.3), so the
    /// UI can pre-fill the commit message and show what a push would send. Read-only
    /// — no network, no off-thread drive (mirrors [`Self::git_status`]).
    pub fn git_pending_diff(&self, id: &str) -> Result<PendingDiffDto> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let e = f.engine.lock().unwrap();
        let remote_id = e
            .store
            .git_remote_list()
            .map_err(|e| anyhow!("{e}"))?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("vault has no git remote configured"))?
            .remote_id;
        let d = gitpush::pending_git_diff(&e, &remote_id).map_err(|e| anyhow!("{e}"))?;
        Ok(PendingDiffDto { files_changed: d.files_changed, paths: d.paths, unified: d.unified })
    }

    /// Pull new upstream commits into a git-bridge folder (git-bridge §4). Looks up
    /// the folder's single configured git remote, then drives `pull_once`.
    pub fn git_pull(&self, id: &str) -> Result<GitPullSummary> {
        let (engine, remote_id) = {
            let folders = self.folders.lock().unwrap();
            let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
            let rid = {
                let e = f.engine.lock().unwrap();
                e.store
                    .git_remote_list()
                    .map_err(|e| anyhow!("{e}"))?
                    .into_iter()
                    .next()
                    .ok_or_else(|| anyhow!("vault has no git remote configured"))?
                    .remote_id
            };
            (f.engine.clone(), rid)
        };
        let report = Self::pull_blocking(engine, remote_id).map_err(|e| anyhow!("git pull: {e}"))?;
        Ok(GitPullSummary::from(report))
    }

    /// The git-bridge status chip (git-bridge §7.2), or `None` if the vault has no
    /// git remote configured (so the web `gitStatus` `Promise<GitStatus | null>`
    /// contract holds). Read-only — no network, no off-thread drive.
    pub fn git_status(&self, id: &str) -> Result<Option<GitStatusDto>> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let e = f.engine.lock().unwrap();
        let Some(r) = e.store.git_remote_list().map_err(|e| anyhow!("{e}"))?.into_iter().next() else {
            return Ok(None);
        };
        let st = gitremote::git_status(&e, &r.remote_id).map_err(|e| anyhow!("{e}"))?;
        Ok(Some(GitStatusDto::from(st)))
    }

    /// Arm (once) the periodic `git pull` tick for a git-bridge folder — the desktop
    /// analogue of the CLI watch loop's `git_pull_tick`. Every 5 minutes it offloads
    /// the (blocking, guard-holding) pull to the runtime's blocking pool so the async
    /// worker is never parked. Idempotent: a folder that already has a tick is left
    /// alone (no overlap).
    fn arm_pull_tick(&self, id: &str) {
        let mut folders = self.folders.lock().unwrap();
        let Some(f) = folders.get_mut(id) else { return };
        if !f.git || f.pull_task.is_some() {
            return; // only git folders tick, and never stack two
        }
        let engine = f.engine.clone();
        f.pull_task = Some(self.rt.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(300));
            tick.tick().await; // interval's first tick is immediate — skip it (we just synced)
            loop {
                tick.tick().await;
                let eng = engine.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let remotes = eng.lock().unwrap().store.git_remote_list().unwrap_or_default();
                    for r in remotes {
                        let _ = Self::pull_blocking(eng.clone(), r.remote_id);
                    }
                })
                .await;
            }
        }));
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
        // Cheap aggregates only — the status poll runs periodically on the active
        // vault, so it must never load every row/file (O(N)) just to take a count
        // or a max. `live_file_count` matches the previous `live_files().count()`
        // (both include dir entities; files are stored with deleted=0).
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
            // Branch/Tag records are metadata (they carry no file change), so they
            // aren't history events on the timeline — skip them.
            if matches!(r.kind, asp_core::Kind::Branch | asp_core::Kind::Tag) {
                continue;
            }
            let path = r.path.clone().or_else(|| latest.get(&r.file_id).cloned()).unwrap_or_default();
            out.push(HistEvent {
                id: r.id,
                ts: r.ts,
                lamport: r.lamport,
                kind: r.kind.as_str().to_string(),
                path,
                branch_id: r.branch_id,
            });
        }
        Ok(out)
    }

    // ---- Branches (§2, §7): scoped views over the shared log ----

    /// All live branches for the switcher (`main` first; HEAD flagged).
    pub fn list_branches(&self, id: &str) -> Result<Vec<BranchDto>> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        let head = eng.current_branch();
        Ok(eng
            .branches()?
            .into_iter()
            .map(|b| BranchDto { current: b.branch_id == head, branch_id: b.branch_id, name: b.name, parent: b.parent })
            .collect())
    }

    /// The checked-out branch id (HEAD).
    pub fn current_branch(&self, id: &str) -> Result<String> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let head = f.engine.lock().unwrap().current_branch();
        Ok(head)
    }

    /// The branch/commit DAG (GitHub-network-style), bounded to `cap` per lane.
    pub fn graph(&self, id: &str, cap: usize) -> Result<asp_core::Graph> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let g = f.engine.lock().unwrap().graph(cap)?;
        Ok(g)
    }

    /// Create a branch off HEAD at the current point (does not switch). The branch
    /// record is pushed live to peers so every node learns it.
    pub fn create_branch(&self, id: &str, name: &str) -> Result<String> {
        let (conns, branch_id, wr) = {
            let folders = self.folders.lock().unwrap();
            let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
            let eng = f.engine.lock().unwrap();
            let (bid, wr) = eng.create_branch_here_wire(name)?;
            (f.conns.clone(), bid, wr)
        };
        self.broadcast(&conns, wr);
        Ok(branch_id)
    }

    /// Switch HEAD to a branch and re-materialize its scoped state to disk. HEAD is
    /// per-device (never synced), so nothing is broadcast; the caller re-reads the
    /// now-switched working tree.
    pub fn checkout_branch(&self, id: &str, branch_id: &str) -> Result<()> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        f.engine.lock().unwrap().checkout(branch_id)?;
        Ok(())
    }

    /// Edit-in-the-past ⇒ branch (§2.5): fork HEAD at wall-clock `ts` and switch to
    /// the new branch. The record is pushed live to peers.
    pub fn fork_branch_at(&self, id: &str, name: &str, ts: i64) -> Result<String> {
        let (conns, branch_id, wr) = {
            let folders = self.folders.lock().unwrap();
            let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
            let eng = f.engine.lock().unwrap();
            // Fork at the point in time, then read back the authored branch record to
            // broadcast (it's this site's latest Kind::Branch row).
            let bid = eng.fork_from_time(name, ts)?;
            let wr = eng
                .branch_record_wire(&bid)
                .ok_or_else(|| anyhow!("forked branch record missing"))?;
            (f.conns.clone(), bid, wr)
        };
        self.broadcast(&conns, wr);
        Ok(branch_id)
    }

    /// Soft-delete a branch; the tombstone is pushed live to peers.
    pub fn delete_branch(&self, id: &str, branch_id: &str) -> Result<()> {
        let (conns, wr) = {
            let folders = self.folders.lock().unwrap();
            let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
            let eng = f.engine.lock().unwrap();
            let wr = eng.delete_branch(branch_id)?;
            (f.conns.clone(), wr)
        };
        self.broadcast(&conns, wr);
        Ok(())
    }

    // ---- Tags: named markers at points in history ----

    /// All live tags on the timeline.
    pub fn list_tags(&self, id: &str) -> Result<Vec<TagDto>> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        Ok(eng
            .tags()?
            .into_iter()
            .map(|t| TagDto { tag_id: t.tag_id, name: t.name, at_ts: t.at_ts, branch_id: t.branch_id })
            .collect())
    }

    /// Tag the point at wall-clock `at_ts` on the current branch. The record is
    /// pushed live to peers so every node learns it.
    pub fn create_tag(&self, id: &str, name: &str, at_ts: i64) -> Result<String> {
        let (conns, tag_id, wr) = {
            let folders = self.folders.lock().unwrap();
            let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
            let eng = f.engine.lock().unwrap();
            let (tid, wr) = eng.create_tag(name, at_ts)?;
            (f.conns.clone(), tid, wr)
        };
        self.broadcast(&conns, wr);
        Ok(tag_id)
    }

    /// Soft-delete a tag; the tombstone is pushed live to peers.
    pub fn delete_tag(&self, id: &str, tag_id: &str) -> Result<()> {
        let (conns, wr) = {
            let folders = self.folders.lock().unwrap();
            let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
            let eng = f.engine.lock().unwrap();
            let wr = eng.delete_tag(tag_id)?;
            (f.conns.clone(), wr)
        };
        self.broadcast(&conns, wr);
        Ok(())
    }

    /// Content of a file as the vault was at wall-clock `ts` (read-only; folds
    /// rows with `ts <= ts` via `asp-core::state_as_of`).
    pub fn read_file_at(&self, id: &str, path: &str, ts: i64) -> Result<FileAt> {
        let folders = self.folders.lock().unwrap();
        let f = folders.get(id).ok_or_else(|| anyhow!("no such folder"))?;
        let eng = f.engine.lock().unwrap();
        // Reads exactly the one requested blob (not the whole vault) — see
        // `Engine::file_at`. Keeps the history slider snappy on large vaults.
        match eng.file_at(path, ts)? {
            Some(bytes) => Ok(FileAt { exists: true, content: String::from_utf8_lossy(&bytes).into_owned() }),
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
            let wr = match eng.file_at(path, ts)? {
                Some(bytes) => eng.record_write(path, &bytes)?,
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
            if let Some(h) = f.pull_task.take() {
                h.abort();
            }
            self.forget_folder(&f.path);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The git status chip DTO must serialize to the exact camelCase keys the shared
    /// TS `GitStatus` type expects (`{remoteUrl, atSha, frozen, ahead, behind,
    /// policy}`) — the web slice consumes this shape verbatim.
    #[test]
    fn git_status_dto_serializes_camelcase() {
        let dto = GitStatusDto::from(gitremote::GitStatus {
            remote_url: "https://example.com/r.git".into(),
            at_sha: Some("deadbeef".into()),
            frozen: false,
            ahead: 2,
            behind: 0,
            policy: "manual".into(),
        });
        let v: serde_json::Value = serde_json::to_value(&dto).unwrap();
        let obj = v.as_object().unwrap();
        // Exactly these keys, all camelCase — no snake_case leakage.
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["ahead", "atSha", "behind", "frozen", "policy", "remoteUrl"]);
        assert_eq!(obj["remoteUrl"], "https://example.com/r.git");
        assert_eq!(obj["atSha"], "deadbeef");
        assert_eq!(obj["policy"], "manual");
    }

    /// `atSha` is `null` (not omitted) before the first ingest, matching the TS
    /// `atSha: string | null` contract.
    #[test]
    fn git_status_dto_null_at_sha() {
        let dto = GitStatusDto {
            remote_url: "u".into(),
            at_sha: None,
            frozen: true,
            ahead: 0,
            behind: 0,
            policy: "manual".into(),
        };
        let v = serde_json::to_value(&dto).unwrap();
        assert!(v["atSha"].is_null());
        assert_eq!(v["frozen"], true);
    }

    /// The pull summary maps each `PullReport` variant to a stable camelCase shape.
    #[test]
    fn git_pull_summary_maps_variants() {
        let up = serde_json::to_value(GitPullSummary::from(PullReport::UpToDate)).unwrap();
        assert_eq!(up["upToDate"], true);
        assert_eq!(up["newCommits"], 0);

        let frozen = serde_json::to_value(GitPullSummary::from(PullReport::Frozen)).unwrap();
        assert_eq!(frozen["frozen"], true);
        assert_eq!(frozen["upToDate"], false);

        let updated = serde_json::to_value(GitPullSummary::from(PullReport::Updated {
            new_commits: 3,
            branches_added: vec![],
        }))
        .unwrap();
        assert_eq!(updated["newCommits"], 3);
        assert_eq!(updated["upToDate"], false);
    }

    /// A `desktop_folders.json` written before the git-bridge slice (no `git` key)
    /// must still deserialize — `git` defaults to `false` — so upgrading never drops
    /// a user's existing vaults.
    #[test]
    fn folder_cfg_git_defaults_false_for_old_configs() {
        let legacy = r#"[{"path":"/vaults/a"},{"path":"/vaults/b","peer":"tkt"}]"#;
        let cfgs: Vec<FolderCfg> = serde_json::from_str(legacy).unwrap();
        assert_eq!(cfgs.len(), 2);
        assert!(!cfgs[0].git);
        assert!(!cfgs[1].git);
        assert_eq!(cfgs[1].peer.as_deref(), Some("tkt"));
        // A git folder round-trips its flag.
        let git_cfg = FolderCfg { path: "/v/g".into(), peer: None, git: true };
        let s = serde_json::to_string(&git_cfg).unwrap();
        let back: FolderCfg = serde_json::from_str(&s).unwrap();
        assert!(back.git);
    }
}
