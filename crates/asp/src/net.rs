//! The native socket driver (§Sync protocol, §Implementation: sans-IO Session).
//! tokio + WebSockets move frame bytes; all protocol/merge logic stays in
//! `asp_core::Session`. Provides: a listener (relay/hub) that admits via the
//! `authorized_keys` table with `AUTH_KEY` enrollment at the WS upgrade; an
//! outbound connector for one-shot `sync`/`clone` and persistent `watch`; and a
//! debounced file watcher that captures FS changes into rows and pushes them.
//!
//! `rusqlite::Connection` is `Send` but `!Sync`, so the `Engine` is shared as
//! `Arc<Mutex<Engine>>` and locked **briefly around each synchronous call**
//! (never across an `.await`) — this also serializes fold/materialize so the
//! `files` table never interleaves between connection tasks.

use anyhow::{anyhow, Context, Result};
use asp_core::engine::AdmitCtx;
use asp_core::session::Step;
use asp_core::{Engine, Msg, NodeId, Role, Session};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_hdr_async, client_async, WebSocketStream};

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
    fn admit_ctx(&self, auth_key_ok: bool) -> AdmitCtx {
        AdmitCtx {
            no_tofu: self.no_tofu,
            auth_key_ok,
            auth_key_configured: !self.auth_keys.is_empty(),
            default_ttl_days: self.default_ttl_days,
            now_unix: now_unix(),
        }
    }
}

type Conns = Arc<Mutex<HashMap<u64, mpsc::UnboundedSender<Msg>>>>;
static CONN_SEQ: AtomicU64 = AtomicU64::new(1);

pub fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

async fn fanout(conns: &Conns, except: u64, msg: &Msg) {
    let map = conns.lock().await;
    for (id, tx) in map.iter() {
        if *id != except {
            let _ = tx.send(msg.clone());
        }
    }
}

async fn run_connection<S>(
    ws: WebSocketStream<S>,
    engine: EngineRef,
    mut session: Session,
    conns: Conns,
    oneshot: bool,
    on_auth: Option<mpsc::UnboundedSender<NodeId>>,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let conn_id = CONN_SEQ.fetch_add(1, Ordering::SeqCst);
    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
    conns.lock().await.insert(conn_id, tx);

    let (mut sink, mut stream) = ws.split();

    for step in session.start() {
        if let Step::Send(m) = step {
            sink.send(Message::Binary(m.to_bytes()?)).await?;
        }
    }

    let idle = Duration::from_millis(700);
    let mut announced_auth = false;
    let result: Result<()> = loop {
        let idle_fut = async {
            if oneshot {
                tokio::time::sleep(idle).await
            } else {
                std::future::pending::<()>().await
            }
        };
        tokio::select! {
            biased;
            outbound = rx.recv() => {
                if let Some(m) = outbound {
                    if sink.send(Message::Binary(m.to_bytes()?)).await.is_err() { break Ok(()); }
                }
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Binary(b))) => {
                        let msg = match Msg::from_bytes(&b) { Ok(m) => m, Err(_) => continue };
                        // Lock the engine only for the synchronous protocol step.
                        let steps = {
                            let eng = engine.lock().unwrap();
                            session.on_msg(&eng, msg)
                        };
                        let steps = match steps {
                            Ok(s) => s,
                            Err(e) => break Err(anyhow!("session error: {e}")),
                        };
                        let mut closing = false;
                        for step in steps {
                            match step {
                                Step::Send(m) => { let _ = sink.send(Message::Binary(m.to_bytes()?)).await; }
                                Step::Authenticated(node) => {
                                    tracing::info!(peer = %&node.to_hex()[..12], "handshake ok");
                                    if let Some(s) = &on_auth { if !announced_auth { let _ = s.send(node); announced_auth = true; } }
                                }
                                Step::Integrated(rows) => {
                                    for wr in rows {
                                        fanout(&conns, conn_id, &Msg::Push { row: Box::new(wr) }).await;
                                    }
                                }
                                Step::Closed(reason) => { tracing::info!(reason, "closing"); closing = true; }
                            }
                        }
                        if closing { break Ok(()); }
                    }
                    Some(Ok(Message::Close(_))) | None => break Ok(()),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => break Err(anyhow!("ws error: {e}")),
                }
            }
            _ = idle_fut => {
                if oneshot && session.authed() {
                    let _ = sink.send(Message::Binary(Msg::Bye.to_bytes()?)).await;
                    let _ = sink.send(Message::Close(None)).await;
                    break Ok(());
                }
            }
        }
    };
    conns.lock().await.remove(&conn_id);
    result
}

// ---------------- listener ----------------

pub async fn serve(
    engine: EngineRef,
    bind: &str,
    auth: AuthOpts,
    conns: Conns,
    port_tx: Option<tokio::sync::oneshot::Sender<u16>>,
) -> Result<()> {
    let listener = TcpListener::bind(bind).await.with_context(|| format!("binding {bind}"))?;
    let port = listener.local_addr()?.port();
    tracing::info!(%bind, port, "listening (ws)");
    if let Some(tx) = port_tx {
        let _ = tx.send(port);
    }
    let auth = Arc::new(auth);
    loop {
        let (tcp, _addr) = match listener.accept().await {
            Ok(x) => x,
            Err(_) => continue,
        };
        let engine = engine.clone();
        let auth = auth.clone();
        let conns = conns.clone();
        tokio::spawn(async move {
            if let Err(e) = accept_one(tcp, engine, auth, conns).await {
                tracing::debug!("accept error: {e}");
            }
        });
    }
}

async fn accept_one(
    tcp: TcpStream,
    engine: EngineRef,
    auth: Arc<AuthOpts>,
    conns: Conns,
) -> Result<()> {
    let auth_state = Arc::new(StdMutex::new(false));
    let as2 = auth_state.clone();
    let auth2 = auth.clone();
    let callback = move |req: &Request, mut resp: Response| -> std::result::Result<Response, ErrorResponse> {
        let presented = extract_auth_key(req);
        if auth2.auth_keys.is_empty() {
            return Ok(resp);
        }
        match presented {
            None => Ok(resp),
            Some(k) => {
                if auth2.auth_keys.iter().any(|x| x == &k) {
                    *as2.lock().unwrap() = true;
                    if let Some(p) = req.headers().get("sec-websocket-protocol") {
                        if p.to_str().unwrap_or("").starts_with("bearer.") {
                            resp.headers_mut().insert("sec-websocket-protocol", p.clone());
                        }
                    }
                    Ok(resp)
                } else {
                    let mut err = ErrorResponse::new(Some("invalid auth key".into()));
                    *err.status_mut() = StatusCode::UNAUTHORIZED;
                    Err(err)
                }
            }
        }
    };

    let ws = accept_hdr_async(tcp, callback).await.context("ws upgrade")?;
    let auth_key_ok = *auth_state.lock().unwrap();
    let admit = auth.admit_ctx(auth_key_ok);
    let session = {
        let eng = engine.lock().unwrap();
        Session::new(Role::Listener, &eng, Vec::new(), None, admit)
    };
    run_connection(ws, engine, session, conns, false, None).await
}

fn extract_auth_key(req: &Request) -> Option<String> {
    if let Some(h) = req.headers().get("authorization") {
        if let Ok(s) = h.to_str() {
            if let Some(rest) = s.strip_prefix("Bearer ") {
                return Some(rest.trim().to_string());
            }
        }
    }
    if let Some(q) = req.uri().query() {
        for pair in q.split('&') {
            if let Some(v) = pair.strip_prefix("auth_key=") {
                return Some(v.to_string());
            }
        }
    }
    if let Some(p) = req.headers().get("sec-websocket-protocol") {
        if let Ok(s) = p.to_str() {
            for proto in s.split(',') {
                if let Some(k) = proto.trim().strip_prefix("bearer.") {
                    return Some(k.to_string());
                }
            }
        }
    }
    None
}

// ---------------- connector ----------------

pub async fn connect(
    engine: EngineRef,
    url: &str,
    auth: &AuthOpts,
    conns: Conns,
    oneshot: bool,
    on_auth: Option<mpsc::UnboundedSender<NodeId>>,
) -> Result<()> {
    let (host, port, secure) = parse_ws_url(url)?;
    if secure {
        return Err(anyhow!("wss:// is not supported in this build; use ws:// (with --no-tls on the listener)"));
    }
    let tcp = TcpStream::connect((host.as_str(), port)).await.with_context(|| format!("connecting {host}:{port}"))?;
    let request = build_request(url, auth)?;
    let (ws, _resp) = client_async(request, tcp).await.context("ws client handshake")?;
    let session = {
        let eng = engine.lock().unwrap();
        Session::new(Role::Connector, &eng, Vec::new(), None, auth.admit_ctx(false))
    };
    run_connection(ws, engine, session, conns, oneshot, on_auth).await
}

fn build_request(url: &str, auth: &AuthOpts) -> Result<Request> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = url.into_client_request().context("bad url")?;
    if let Some(k) = auth.auth_keys.first() {
        req.headers_mut().insert("authorization", format!("Bearer {k}").parse().unwrap());
    }
    Ok(req)
}

fn parse_ws_url(url: &str) -> Result<(String, u16, bool)> {
    let (secure, rest) = if let Some(r) = url.strip_prefix("wss://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("ws://") {
        (false, r)
    } else {
        return Err(anyhow!("url must start with ws:// or wss://"));
    };
    let hostport = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(if secure { 443 } else { 80 })),
        None => (hostport.to_string(), if secure { 443 } else { 80 }),
    };
    Ok((host, port, secure))
}

// ---------------- watch (file watcher) ----------------

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

/// One-shot sync: capture local disk changes, connect, exchange, exit.
pub async fn sync_oneshot(engine: EngineRef, url: &str, auth: &AuthOpts) -> Result<()> {
    {
        let eng = engine.lock().unwrap();
        eng.capture_rescan()?;
    }
    let conns: Conns = Arc::new(Mutex::new(HashMap::new()));
    connect(engine, url, auth, conns, true, None).await
}

/// Clone bootstrap: connect with an empty local vault, adopt the peer's vault id,
/// pull everything, materialize, and pin the listener as a peer.
pub async fn clone_bootstrap(engine: EngineRef, url: &str, auth: &AuthOpts) -> Result<()> {
    let conns: Conns = Arc::new(Mutex::new(HashMap::new()));
    let (auth_tx, mut auth_rx) = mpsc::unbounded_channel::<NodeId>();
    connect(engine.clone(), url, auth, conns, true, Some(auth_tx)).await?;
    if let Ok(node) = auth_rx.try_recv() {
        let eng = engine.lock().unwrap();
        let _ = eng.store.add_peer(url, &node.to_hex(), now_unix());
    }
    Ok(())
}
