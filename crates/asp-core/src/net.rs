//! Shared native driver helpers (§Sync protocol). The transport itself is iroh
//! (see [`crate::iroh_net`]); this module holds the transport-agnostic glue the
//! iroh driver reuses: the admission options, the engine handle, the live
//! connection registry + fan-out, and the debounced filesystem watcher.
//!
//! `rusqlite::Connection` is `Send` but `!Sync`, so the `Engine` is shared as
//! `Arc<Mutex<Engine>>` and locked **briefly around each synchronous call**
//! (never across an `.await`).

use crate::authkeys::PeerPolicy;
use crate::log::Kind;
use crate::wire::WireRow;
use crate::AdmitCtx;
use crate::{Engine, Msg};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};

/// Engine shared across async tasks (locked briefly per sync call).
pub type EngineRef = Arc<StdMutex<Engine>>;

#[derive(Clone, Default)]
pub struct AuthOpts {
    /// Listener: accepted enrollment secrets. Connector: the secret to present.
    pub auth_keys: Vec<String>,
    pub no_tofu: bool,
    pub default_ttl_days: u64,
}

impl AuthOpts {
    pub(crate) fn admit_ctx(&self, auth_key_ok: bool) -> AdmitCtx {
        AdmitCtx {
            no_tofu: self.no_tofu,
            auth_key_ok,
            auth_key_configured: !self.auth_keys.is_empty(),
            default_ttl_days: self.default_ttl_days,
            now_unix: now_unix(),
        }
    }
}

/// A live peer connection: its outbound queue plus the replication grant the
/// listener admitted it with (scoped-sync §3.5). The grant governs the realtime
/// send-filter; it is filled in after the handshake authenticates (default
/// full/read-write until then).
pub struct ConnEntry {
    pub tx: mpsc::UnboundedSender<Msg>,
    pub policy: PeerPolicy,
}

/// Registry of live peer connections for real-time fan-out (hub forward-then-
/// merge + the watcher's live push). Transport-agnostic — shared by the iroh
/// driver and the desktop engine. Keyed by a process-unique connection id.
pub type Conns = Arc<Mutex<HashMap<u64, ConnEntry>>>;
pub(crate) static CONN_SEQ: AtomicU64 = AtomicU64::new(1);

pub fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Fan out a newly-integrated file row to every live peer except `except`,
/// applying each peer's scope grant (A — scoped-sync §3.5). This is the realtime
/// twin of the catch-up filter and MUST live here, not only in catch-up: the hub
/// re-forward is the leak path a catch-up-only filter would miss.
///
/// - **Unscoped peer** → the lone `Push` (today's behavior, no engine work).
/// - **Non-file row** (Branch/Tag/Merge/Git*) → always shipped as a lone `Push`.
/// - **Scoped peer, in-scope file** → normally the lone `Push`; but a **`Rename`**
///   that brings a file into scope ships the file's WHOLE `file_id` chain as
///   `Msg::Rows` — a lone Push of the Rename would arrive without the below-
///   watermark `Create` and orphan the fold (§3.3 the subtle realtime bug).
///   Idempotent (INSERT-OR-IGNORE), so it is also safe for rename-within-scope.
/// - **Scoped peer, out-of-scope file** → skipped.
pub(crate) async fn fanout_row(conns: &Conns, except: u64, engine: &EngineRef, wr: &WireRow) {
    let map = conns.lock().await;
    for (id, e) in map.iter() {
        if *id == except {
            continue;
        }
        let allowed = match &e.policy.allowed_paths {
            None => {
                let _ = e.tx.send(Msg::Push { row: Box::new(wr.clone()) });
                continue;
            }
            Some(a) => a,
        };
        if !crate::session::is_file_mutation(&wr.row) {
            let _ = e.tx.send(Msg::Push { row: Box::new(wr.clone()) });
            continue;
        }
        // Compute membership (and, for a Rename, the reship chain) under one brief
        // engine lock — no await is held across it.
        let (member, chain) = {
            let eng = engine.lock().unwrap();
            let member = eng.file_in_scope(&wr.row.file_id, allowed).unwrap_or(false);
            let chain = if member && wr.row.kind == Kind::Rename { eng.wire_chain(&wr.row.file_id).ok() } else { None };
            (member, chain)
        };
        if !member {
            continue;
        }
        match chain {
            Some(rows) => {
                let _ = e.tx.send(Msg::Rows { rows });
            }
            None => {
                let _ = e.tx.send(Msg::Push { row: Box::new(wr.clone()) });
            }
        }
    }
}

// ---------------- watch (file watcher) ----------------

/// Spawn a debounced filesystem watcher: an OS fs event → debounced
/// `capture_rescan` into the log → fan-out of the new rows to every live peer.
pub fn spawn_watcher(engine: EngineRef, conns: Conns, debounce_ms: u64) -> Result<notify::RecommendedWatcher> {
    use notify::{RecursiveMode, Watcher};
    let root = { engine.lock().unwrap().root.clone() };
    let (raw_tx, mut raw_rx) = mpsc::unbounded_channel::<()>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            let _ = raw_tx.send(());
        }
    })
    .context("creating watcher")?;
    watcher.watch(&root, RecursiveMode::Recursive).context("watching root")?;

    tokio::spawn(async move {
        loop {
            if raw_rx.recv().await.is_none() {
                break;
            }
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(debounce_ms.max(50))) => break,
                    e = raw_rx.recv() => { if e.is_none() { return; } }
                }
            }
            let rows = {
                let eng = engine.lock().unwrap();
                eng.capture_rescan()
            };
            match rows {
                Ok(rows) => {
                    for wr in rows {
                        fanout_row(&conns, 0, &engine, &wr).await;
                    }
                }
                Err(e) => tracing::warn!("capture error: {e}"),
            }
        }
    });
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::identity::Identity;
    use tempfile::tempdir;

    /// A — realtime rename-into-scope reships the whole chain (scoped-sync §3.3,
    /// §10 risk 3). A lone Push of a boundary-crossing Rename would arrive at a
    /// scoped peer without the below-watermark Create and orphan the fold; the
    /// fan-out must instead ship the file's whole `file_id` chain. An unscoped peer
    /// keeps getting lone Pushes; an out-of-scope file never reaches a scoped peer.
    #[tokio::test]
    async fn fanout_reships_whole_chain_on_rename_into_scope() {
        let dir = tempdir().unwrap();
        let e = Engine::init(dir.path(), Identity::from_seed(&[5; 32])).unwrap();
        let engine: EngineRef = Arc::new(StdMutex::new(e));

        let (stx, mut srx) = mpsc::unbounded_channel::<Msg>();
        let (utx, mut urx) = mpsc::unbounded_channel::<Msg>();
        let conns: Conns = Arc::new(Mutex::new(HashMap::new()));
        conns.lock().await.insert(
            1,
            ConnEntry { tx: stx, policy: PeerPolicy { allowed_paths: Some(vec!["work".into()]), read_only: false } },
        );
        conns.lock().await.insert(2, ConnEntry { tx: utx, policy: PeerPolicy::default() });

        // Create OUT of scope: the scoped peer must NOT receive it; the unscoped one does.
        let create = { engine.lock().unwrap().record_write("personal/x.md", b"hi\n").unwrap().unwrap() };
        fanout_row(&conns, 0, &engine, &create).await;
        assert!(srx.try_recv().is_err(), "out-of-scope create is not sent to the scoped peer");
        assert!(matches!(urx.try_recv(), Ok(Msg::Push { .. })), "unscoped peer gets the create");

        // Rename INTO scope: the scoped peer must get the WHOLE chain as Msg::Rows.
        let rename = { engine.lock().unwrap().record_rename("personal/x.md", "work/x.md").unwrap().unwrap() };
        fanout_row(&conns, 0, &engine, &rename).await;
        match srx.try_recv() {
            Ok(Msg::Rows { rows }) => {
                assert!(rows.iter().any(|w| w.row.kind == Kind::Create), "chain includes the below-watermark Create");
                assert!(rows.iter().any(|w| w.row.kind == Kind::Rename), "chain includes the Rename");
            }
            other => panic!("expected Msg::Rows whole-chain reship, got {other:?}"),
        }
        assert!(matches!(urx.try_recv(), Ok(Msg::Push { .. })), "unscoped peer gets the lone rename push");
    }

    #[tokio::test]
    async fn watcher_captures_a_disk_write() {
        // The `asp watch` glue: an OS fs event → debounced capture into the log.
        let dir = tempdir().unwrap();
        let e = Engine::init(dir.path(), Identity::from_seed(&[9; 32])).unwrap();
        let engine: EngineRef = Arc::new(StdMutex::new(e));
        let conns: Conns = Arc::new(Mutex::new(HashMap::new()));
        let _watcher = spawn_watcher(engine.clone(), conns, 50).expect("spawn watcher");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await; // let it register

        std::fs::write(dir.path().join("live.md"), b"typed by a human\n").unwrap();
        let mut captured = false;
        for _ in 0..80 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if engine.lock().unwrap().materialize().unwrap().contains_key("live.md") {
                captured = true;
                break;
            }
        }
        assert!(captured, "watcher should capture a disk write within the timeout");
    }
}
