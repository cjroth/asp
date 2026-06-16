//! Shared native driver helpers (§Sync protocol). The transport itself is iroh
//! (see [`crate::iroh_net`]); this module holds the transport-agnostic glue the
//! iroh driver reuses: the admission options, the engine handle, the live
//! connection registry + fan-out, and the debounced filesystem watcher.
//!
//! `rusqlite::Connection` is `Send` but `!Sync`, so the `Engine` is shared as
//! `Arc<Mutex<Engine>>` and locked **briefly around each synchronous call**
//! (never across an `.await`).

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

/// Registry of live peer connections for real-time fan-out (hub forward-then-
/// merge + the watcher's live push). Transport-agnostic — shared by the iroh
/// driver. Keyed by a process-unique connection id.
pub(crate) type Conns = Arc<Mutex<HashMap<u64, mpsc::UnboundedSender<Msg>>>>;
pub(crate) static CONN_SEQ: AtomicU64 = AtomicU64::new(1);

pub fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Push `msg` to every live connection except `except` (real-time fan-out).
pub(crate) async fn fanout(conns: &Conns, except: u64, msg: &Msg) {
    let map = conns.lock().await;
    for (id, tx) in map.iter() {
        if *id != except {
            let _ = tx.send(msg.clone());
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
                        fanout(&conns, 0, &Msg::Push { row: Box::new(wr) }).await;
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
