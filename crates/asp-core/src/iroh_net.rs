//! The native **iroh** transport driver (§Sync protocol, §Transport: iroh).
//!
//! iroh moves the frame bytes; all protocol/merge logic stays in the sans-IO
//! [`crate::Session`]. A node's ed25519 device identity *is* its iroh
//! `EndpointId` (both are the same 32-byte ed25519 key), so dial-by-key and the
//! `authorized_keys` admission set speak the same identity with no translation.
//!
//! The connection is QUIC: mutually key-authenticated by iroh before any ASP
//! frame is read, and always end-to-end encrypted (direct hole-punched path when
//! the network allows, relay fallback otherwise). One bi-directional stream
//! carries the length-delimited [`Msg`] frames the `Session` already speaks, so
//! the handshake / catch-up / integrate state machine is byte-for-byte the same
//! as every other surface — only the bytes' carrier changed.

use crate::net::{fanout, AuthOpts, Conns, EngineRef, CONN_SEQ};
use crate::session::Step;
use crate::{Msg, NodeId, Role, Session};
use anyhow::{anyhow, Result};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::{PublicKey, RelayMode, SecretKey, TransportAddr};
use iroh_tickets::endpoint::EndpointTicket;
// Re-exported so thin native drivers (the CLI) can name the endpoint/address
// types without depending on the iroh crate directly.
pub use iroh::{Endpoint, EndpointAddr};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::Mutex;

/// Application-layer protocol negotiated on the QUIC connection. A bump here is a
/// hard, visible transport-incompatibility boundary (distinct from the in-band
/// `wire::PROTO`, which versions the frame/handshake content).
pub const ALPN: &[u8] = b"asp/sync/1";

/// Rows fetched per page when streaming a listener catch-up — bounded so
/// per-connection memory stays flat under many concurrent clients (mirrors the
/// former WebSocket driver).
const CATCHUP_PAGE_ROWS: i64 = 256;

/// The device identity as an iroh secret key — the *same* ed25519 key, no second
/// keypair (§Identity is the iroh key).
fn secret_key(seed: &[u8; 32]) -> SecretKey {
    SecretKey::from_bytes(seed)
}

/// Map an iroh `EndpointId` (ed25519 pubkey) to our `NodeId` — the identity iroh
/// already authenticated for this connection.
fn node_id_of(conn: &Connection) -> NodeId {
    NodeId(*conn.remote_id().as_bytes())
}

/// Bind an iroh endpoint under this node's device key. `relays` selects the
/// public n0 relays + discovery (production: reachable across NATs and from
/// browser nodes) or a relay-less endpoint (LAN / loopback tests).
pub async fn bind_endpoint(seed: &[u8; 32], relays: bool) -> Result<Endpoint> {
    let sk = secret_key(seed);
    let builder = if relays {
        Endpoint::builder(iroh::endpoint::presets::N0)
    } else {
        Endpoint::builder(iroh::endpoint::presets::Empty).relay_mode(RelayMode::Disabled)
    };
    builder
        // iroh's QUIC/TLS needs an explicit rustls crypto provider (ring, the one
        // the workspace links) rather than relying on the process-global default.
        .crypto_provider(iroh::tls::default_provider())
        .secret_key(sk)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .map_err(|e| anyhow!("binding iroh endpoint: {e}"))
}

/// This endpoint's loopback dial address (its bound UDP port on `127.0.0.1`).
/// Used by the relay-less tests; production dials by ticket / discovery instead.
pub fn loopback_addr(ep: &Endpoint) -> EndpointAddr {
    let mut addr = EndpointAddr::new(ep.id());
    for sa in ep.bound_sockets() {
        let port = sa.port();
        addr.addrs
            .insert(TransportAddr::Ip(SocketAddr::from(([127, 0, 0, 1], port))));
    }
    addr
}

// ---------------- addressing (tickets / node ids) ----------------

/// This node's shareable connection **ticket** — its `NodeId` plus relay/direct
/// address hints, base32-encoded for copy-paste / QR. With relays on, waits for
/// the home relay so the ticket is dialable from anywhere; relay-less, it carries
/// whatever direct addresses are known.
pub async fn ticket(ep: &Endpoint, relays: bool) -> Result<String> {
    if relays {
        // Ensure the home relay (and thus a globally-dialable address) is present.
        ep.online().await;
    }
    Ok(EndpointTicket::new(ep.addr()).to_string())
}

/// Parse a peer spec into a dial address: an iroh **ticket** (preferred — carries
/// relay/address hints) or a bare 64-hex **`NodeId`** (resolved via discovery).
pub fn parse_peer(s: &str) -> Result<EndpointAddr> {
    let s = s.trim();
    if let Ok(t) = s.parse::<EndpointTicket>() {
        return Ok(EndpointAddr::from(t));
    }
    if let Some(node) = NodeId::from_hex(s) {
        let pk = PublicKey::from_bytes(&node.0).map_err(|e| anyhow!("bad node id: {e}"))?;
        return Ok(EndpointAddr::from(pk));
    }
    Err(anyhow!("not an iroh ticket or 64-hex node id: {s}"))
}

// ---------------- relay server (`asp relay`) ----------------

/// Run a **pure iroh relay** on `http_bind`: a stateless packet-forwarder for
/// connection setup / NAT traversal / browser nodes. It holds no vault, stores
/// nothing, and only ever forwards ciphertext (it cannot decrypt peer traffic).
/// Lets an operator self-host relay infrastructure with the same binary instead
/// of depending on the public n0 relays. Runs until cancelled.
pub async fn run_relay(http_bind: SocketAddr) -> Result<()> {
    use iroh_relay::server::{RelayConfig, Server, ServerConfig};
    // ServerConfig is #[non_exhaustive]: build via Default, then set public fields.
    let mut config = ServerConfig::default();
    config.relay = Some(RelayConfig::new(http_bind));
    config.quic = None;
    let server = Server::spawn(config).await.map_err(|e| anyhow!("starting relay: {e}"))?;
    if let Some(addr) = server.http_addr() {
        println!("relay listening on http://{addr}");
        tracing::info!(%addr, "iroh relay (http) up — forwards ciphertext, stores nothing");
    }
    // Hold the server alive until the process is cancelled.
    std::future::pending::<()>().await;
    drop(server);
    Ok(())
}

// ---------------- framing ----------------

/// Write one length-delimited frame: a `u32` big-endian length, then the bytes.
async fn write_frame(send: &mut SendStream, bytes: &[u8]) -> Result<()> {
    let len = u32::try_from(bytes.len()).map_err(|_| anyhow!("frame too large"))?;
    send.write_all(&len.to_be_bytes())
        .await
        .map_err(|e| anyhow!("iroh write: {e}"))?;
    send.write_all(bytes)
        .await
        .map_err(|e| anyhow!("iroh write: {e}"))?;
    Ok(())
}

/// Read one length-delimited frame. `Ok(None)` on a clean end-of-stream.
async fn read_frame(recv: &mut RecvStream) -> Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match recv.read_exact(&mut len_buf).await {
        Ok(()) => {}
        // A clean FIN at a frame boundary is a normal close, not an error.
        Err(_) => return Ok(None),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf)
        .await
        .map_err(|e| anyhow!("iroh read body: {e}"))?;
    Ok(Some(buf))
}

async fn send_msg(send: &mut SendStream, msg: &Msg) -> Result<()> {
    write_frame(send, &msg.to_bytes()?).await
}

// ---------------- session driver ----------------

/// Drive one connection's `Session` over an iroh bi-stream. A oneshot connector
/// (`sync`/`clone`) finishes when the peer signals `Synced`; a listener / `watch`
/// connector (`oneshot=false`) stays open for live push. Registers itself in
/// `conns` so the watcher and other peers can fan out new rows to it (hub
/// forward-then-merge). `on_auth` surfaces the authenticated peer `NodeId`.
///
/// A QUIC `RecvStream` read is **not** cancel-safe (a half-read length-delimited
/// frame would lose bytes if dropped by `select!`), so a dedicated reader task
/// owns `recv` and forwards whole frames over a channel; the main loop only
/// selects over cancel-safe channel receives.
async fn drive(
    mut send: SendStream,
    recv: RecvStream,
    engine: EngineRef,
    mut session: Session,
    oneshot: bool,
    conns: Conns,
    on_auth: Option<mpsc::UnboundedSender<NodeId>>,
) -> Result<()> {
    let conn_id = CONN_SEQ.fetch_add(1, Ordering::SeqCst);
    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
    conns.lock().await.insert(conn_id, tx);

    // Reader task: owns `recv`, ships each whole frame over `inbound`.
    let (inbound_tx, mut inbound_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let reader = tokio::spawn(async move {
        let mut recv = recv;
        while let Ok(Some(frame)) = read_frame(&mut recv).await {
            if inbound_tx.send(frame).is_err() {
                break;
            }
        }
    });

    for step in session.start() {
        if let Step::Send(m) = step {
            if send_msg(&mut send, &m).await.is_err() {
                break;
            }
        }
    }

    // A oneshot is "done" only when the peer signals it sent everything
    // (`Synced`). A denied connector authenticates the listener (so `authed()` is
    // true) but never reaches `Synced`, and the QUIC teardown can race the
    // `Denied` frame — so completion, not a clean close, is the success gate.
    let mut completed = false;
    let result: Result<()> = loop {
        let frame = tokio::select! {
            biased;
            // Outbound live push (watcher / fan-out from a sibling connection).
            outbound = rx.recv() => {
                match outbound {
                    Some(m) => {
                        if send_msg(&mut send, &m).await.is_err() { break Ok(()); }
                        continue;
                    }
                    None => break Ok(()),
                }
            }
            inbound = inbound_rx.recv() => {
                match inbound {
                    Some(f) => f,
                    None => break Ok(()), // peer closed
                }
            }
        };
        let msg = match Msg::from_bytes(&frame) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let steps = {
            let eng = engine.lock().unwrap();
            session.on_msg(&*eng, msg)
        };
        let steps = match steps {
            Ok(s) => s,
            Err(e) => break Err(anyhow!("session error: {e}")),
        };
        let mut closing = false;
        for step in steps {
            match step {
                Step::Send(m) => {
                    if send_msg(&mut send, &m).await.is_err() {
                        closing = true;
                        break;
                    }
                }
                Step::Authenticated(node) => {
                    tracing::info!(peer = %&node.to_hex()[..12], "iroh handshake ok");
                    if let Some(s) = &on_auth {
                        let _ = s.send(node);
                    }
                }
                Step::Integrated(rows) => {
                    // Hub forward-then-merge: push newly-integrated rows to every
                    // other live peer so a relay propagates without re-folding.
                    for wr in rows {
                        fanout(&conns, conn_id, &Msg::Push { row: Box::new(wr) }).await;
                    }
                }
                Step::Closed(reason) => {
                    if reason.contains("denied") || reason.contains("different vault") {
                        let hint = if reason.contains("different vault") {
                            " (separate vaults — `asp clone <ticket>` to follow the peer)"
                        } else {
                            ""
                        };
                        conns.lock().await.remove(&conn_id);
                        reader.abort();
                        return Err(anyhow!("{reason}{hint}"));
                    }
                    tracing::info!(reason, "closing");
                    closing = true;
                }
                Step::CatchUp { peer_vv } => {
                    if stream_catchup(&mut send, &engine, &peer_vv).await.is_err() {
                        closing = true;
                        break;
                    }
                }
                Step::PeerSynced => {
                    completed = true;
                    if oneshot {
                        closing = true;
                    }
                }
            }
        }
        if closing {
            break Ok(());
        }
    };

    conns.lock().await.remove(&conn_id);
    reader.abort();
    let _ = send.finish();
    // A oneshot that never completed catch-up was rejected/unreachable/denied.
    if oneshot && result.is_ok() && (!session.authed() || !completed) {
        return Err(anyhow!("handshake did not complete (rejected or vault mismatch)"));
    }
    result
}

/// Stream the listener's catch-up to a connector in bounded pages, then a final
/// `Synced` so the connector finishes promptly (no idle wait).
async fn stream_catchup(
    send: &mut SendStream,
    engine: &EngineRef,
    peer_vv: &std::collections::BTreeMap<String, i64>,
) -> Result<()> {
    let our_vv = { engine.lock().unwrap().store.version_vector()? };
    for (site, _max) in our_vv {
        let mut cursor = peer_vv.get(&site).copied().unwrap_or(-1);
        loop {
            let page = {
                let eng = engine.lock().unwrap();
                eng.rows_after_wire_page(&site, cursor, CATCHUP_PAGE_ROWS)?
            };
            if page.is_empty() {
                break;
            }
            cursor = page.last().map(|w| w.row.seq as i64).unwrap_or(cursor);
            let mut sends = Vec::new();
            crate::session::push_rows_chunked(&mut sends, page);
            for s in sends {
                if let Step::Send(m) = s {
                    send_msg(send, &m).await?;
                }
            }
        }
    }
    send_msg(send, &Msg::Synced).await
}

// ---------------- listener (hub) ----------------

/// A fresh, empty connection registry (no live siblings to fan out to). Used by
/// oneshot `sync`/`clone`; `watch` shares one registry across all peers.
pub fn new_conns() -> Conns {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Accept inbound iroh connections forever, driving each as a listener `Session`
/// gated by the `authorized_keys` admission set. `conns` is the shared registry
/// so inbound peers receive the watcher's live pushes and each other's rows.
pub async fn serve(engine: EngineRef, ep: Endpoint, auth: AuthOpts, conns: Conns) -> Result<()> {
    tracing::info!(endpoint = %ep.id().fmt_short(), "iroh listening");
    while let Some(incoming) = ep.accept().await {
        let engine = engine.clone();
        let auth = auth.clone();
        let conns = conns.clone();
        tokio::spawn(async move {
            if let Err(e) = accept_one(incoming, engine, auth, conns).await {
                tracing::debug!("iroh accept error: {e}");
            }
        });
    }
    Ok(())
}

async fn accept_one(
    incoming: iroh::endpoint::Incoming,
    engine: EngineRef,
    auth: AuthOpts,
    conns: Conns,
) -> Result<()> {
    let conn = incoming.await.map_err(|e| anyhow!("accept: {e}"))?;
    let (send, recv) = conn.accept_bi().await.map_err(|e| anyhow!("accept_bi: {e}"))?;
    // The connection's verified remote key gates whether an auth-key was even
    // needed; admission proper is the `authorized_keys` table inside the Session.
    let admit = auth.admit_ctx(false);
    let session = {
        let eng = engine.lock().unwrap();
        Session::with_auth(Role::Listener, &*eng, Vec::new(), None, admit, auth.auth_keys.clone())
    };
    let _peer = node_id_of(&conn);
    drive(send, recv, engine, session, false, conns, None).await
}

// ---------------- connector (sync / clone / watch) ----------------

/// Dial a peer by address and drive a connector `Session`. `oneshot` ends on the
/// peer's `Synced`; a persistent `watch` passes `false` and stays connected so
/// the watcher's live pushes (over `conns`) reach it.
pub async fn connect(
    engine: EngineRef,
    ep: &Endpoint,
    addr: EndpointAddr,
    auth: &AuthOpts,
    oneshot: bool,
    conns: Conns,
    on_auth: Option<mpsc::UnboundedSender<NodeId>>,
) -> Result<()> {
    let conn = ep
        .connect(addr, ALPN)
        .await
        .map_err(|e| anyhow!("iroh connect: {e}"))?;
    let (send, recv) = conn.open_bi().await.map_err(|e| anyhow!("open_bi: {e}"))?;
    let session = {
        let eng = engine.lock().unwrap();
        Session::with_auth(
            Role::Connector,
            &*eng,
            Vec::new(),
            None,
            auth.admit_ctx(false),
            auth.auth_keys.clone(),
        )
    };
    drive(send, recv, engine, session, oneshot, conns, on_auth).await
}

/// One-shot sync: capture local disk changes, connect, exchange, exit.
pub async fn sync_oneshot(
    engine: EngineRef,
    ep: &Endpoint,
    addr: EndpointAddr,
    auth: &AuthOpts,
) -> Result<()> {
    {
        let eng = engine.lock().unwrap();
        eng.capture_rescan()?;
    }
    connect(engine, ep, addr, auth, true, new_conns(), None).await
}

/// Clone bootstrap: connect with an empty local vault, adopt the peer's vault,
/// and pull everything. Returns the listener's iroh-verified `NodeId` so the
/// caller can pin it as the default peer (the CLI saves the source ticket too).
pub async fn clone_bootstrap(
    engine: EngineRef,
    ep: &Endpoint,
    addr: EndpointAddr,
    auth: &AuthOpts,
) -> Result<Option<NodeId>> {
    let (auth_tx, mut auth_rx) = mpsc::unbounded_channel::<NodeId>();
    connect(engine, ep, addr, auth, true, new_conns(), Some(auth_tx)).await?;
    Ok(auth_rx.try_recv().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::identity::Identity;
    use std::sync::{Arc, Mutex as StdMutex};
    use tempfile::tempdir;

    /// REAL iroh loopback: two engines, two endpoints keyed by their device
    /// identities, a clone over a live QUIC connection (relays disabled — direct
    /// loopback). Proves the whole stack — dial-by-key, ALPN, framed `Msg` over a
    /// bi-stream, the unchanged `Session` handshake/catch-up — converges over
    /// iroh, including an oversized blob that must span multiple frames.
    #[tokio::test]
    async fn iroh_loopback_clone_converges() {
        let srv_dir = tempdir().unwrap();
        let cli_dir = tempdir().unwrap();
        let srv_id = Identity::from_seed(&[11; 32]);
        let cli_id = Identity::from_seed(&[12; 32]);
        let srv = Engine::init(srv_dir.path(), srv_id.clone()).unwrap();
        let big = vec![0xCDu8; 8 * 1024 * 1024];
        srv.record_write("notes/hello.md", b"over iroh\n").unwrap();
        srv.record_write("assets/big.bin", &big).unwrap();
        srv.authorize(&cli_id.to_ssh_string(), None, true, "test").unwrap();

        let srv_ep = bind_endpoint(&srv_id.seed(), false).await.unwrap();
        let cli_ep = bind_endpoint(&cli_id.seed(), false).await.unwrap();
        let dial = loopback_addr(&srv_ep);

        let srv_engine: EngineRef = Arc::new(StdMutex::new(srv));
        let server = tokio::spawn(serve(srv_engine, srv_ep, AuthOpts::default(), new_conns()));

        let cli = Engine::init(cli_dir.path(), cli_id).unwrap();
        let cli_engine: EngineRef = Arc::new(StdMutex::new(cli));
        clone_bootstrap(cli_engine, &cli_ep, dial, &AuthOpts::default())
            .await
            .expect("clone over iroh should succeed");

        assert_eq!(
            std::fs::read(cli_dir.path().join("notes/hello.md")).unwrap(),
            b"over iroh\n"
        );
        let got = std::fs::read(cli_dir.path().join("assets/big.bin")).unwrap();
        assert_eq!(got, big, "oversized blob must survive multi-frame transfer");
        server.abort();
    }

    /// An unauthorized, keyless peer is denied admission over iroh and no data
    /// leaks — the `authorized_keys` trust gate is enforced on the QUIC stream.
    #[tokio::test]
    async fn iroh_unauthorized_peer_is_denied() {
        let srv_dir = tempdir().unwrap();
        let cli_dir = tempdir().unwrap();
        let srv_id = Identity::from_seed(&[13; 32]);
        let cli_id = Identity::from_seed(&[14; 32]);
        let srv = Engine::init(srv_dir.path(), srv_id.clone()).unwrap();
        srv.record_write("private.md", b"secret\n").unwrap();

        let srv_ep = bind_endpoint(&srv_id.seed(), false).await.unwrap();
        let cli_ep = bind_endpoint(&cli_id.seed(), false).await.unwrap();
        let dial = loopback_addr(&srv_ep);

        let srv_engine: EngineRef = Arc::new(StdMutex::new(srv));
        let opts = AuthOpts { no_tofu: true, ..Default::default() };
        let server = tokio::spawn(serve(srv_engine, srv_ep, opts, new_conns()));

        let cli = Engine::init(cli_dir.path(), cli_id).unwrap();
        let cli_engine: EngineRef = Arc::new(StdMutex::new(cli));
        let r = clone_bootstrap(cli_engine, &cli_ep, dial, &AuthOpts::default()).await;
        assert!(r.is_err(), "an unauthorized keyless peer must be denied");
        assert!(!cli_dir.path().join("private.md").exists(), "no data may leak");
        server.abort();
    }

    /// AUTH_KEY enrollment over iroh: the secret rides in the `Hello` handshake
    /// (no WS header). A connector presenting the right secret is enrolled and
    /// syncs; a wrong secret is denied loudly and leaks nothing.
    #[tokio::test]
    async fn iroh_auth_key_enrollment_and_mismatch() {
        async fn attempt(secret: &str, cli_seed: u8) -> (Result<Option<NodeId>>, tempfile::TempDir) {
            let srv_dir = tempdir().unwrap();
            let cli_dir = tempdir().unwrap();
            let srv_id = Identity::from_seed(&[21; 32]);
            let cli_id = Identity::from_seed(&[cli_seed; 32]);
            let srv = Engine::init(srv_dir.path(), srv_id.clone()).unwrap();
            srv.record_write("enrolled.md", b"members only\n").unwrap();
            let srv_ep = bind_endpoint(&srv_id.seed(), false).await.unwrap();
            let cli_ep = bind_endpoint(&cli_id.seed(), false).await.unwrap();
            let dial = loopback_addr(&srv_ep);
            let srv_engine: EngineRef = Arc::new(StdMutex::new(srv));
            // Listener configures the enrollment secret "S3CRET" (no pre-authorized
            // keys; auth-key configured implicitly disables TOFU).
            let srv_opts = AuthOpts { auth_keys: vec!["S3CRET".into()], ..Default::default() };
            let server = tokio::spawn(serve(srv_engine, srv_ep, srv_opts, new_conns()));
            let cli = Engine::init(cli_dir.path(), cli_id).unwrap();
            let cli_engine: EngineRef = Arc::new(StdMutex::new(cli));
            let cli_opts = AuthOpts { auth_keys: vec![secret.into()], ..Default::default() };
            let r = clone_bootstrap(cli_engine, &cli_ep, dial, &cli_opts).await;
            server.abort();
            (r, cli_dir)
        }

        // Right secret → enrolled and synced.
        let (ok, dir) = attempt("S3CRET", 22).await;
        ok.expect("correct auth key must enroll and sync over iroh");
        assert_eq!(std::fs::read(dir.path().join("enrolled.md")).unwrap(), b"members only\n");

        // Wrong secret → denied, nothing leaks.
        let (bad, dir) = attempt("WRONG", 23).await;
        assert!(bad.is_err(), "a wrong auth key must be denied");
        assert!(!dir.path().join("enrolled.md").exists(), "no data may leak on a bad key");
    }

    /// Persistent `watch` over iroh: a connector stays connected after catch-up
    /// and receives a **live push** when the hub authors a new row — the
    /// real-time path (conns registry + fan-out), not just one-shot catch-up.
    #[tokio::test]
    async fn iroh_live_push_reaches_a_watching_peer() {
        let srv_dir = tempdir().unwrap();
        let cli_dir = tempdir().unwrap();
        let srv_id = Identity::from_seed(&[31; 32]);
        let cli_id = Identity::from_seed(&[32; 32]);
        let srv = Engine::init(srv_dir.path(), srv_id.clone()).unwrap();
        srv.record_write("base.md", b"start\n").unwrap();
        srv.authorize(&cli_id.to_ssh_string(), None, true, "test").unwrap();

        let srv_ep = bind_endpoint(&srv_id.seed(), false).await.unwrap();
        let cli_ep = bind_endpoint(&cli_id.seed(), false).await.unwrap();
        let dial = loopback_addr(&srv_ep);
        let srv_engine: EngineRef = Arc::new(StdMutex::new(srv));
        let srv_conns = new_conns();
        let server = tokio::spawn(serve(srv_engine.clone(), srv_ep, AuthOpts::default(), srv_conns.clone()));

        // The client clones to adopt the vault, then connects a persistent watch.
        let cli = Engine::init(cli_dir.path(), cli_id).unwrap();
        let cli_engine: EngineRef = Arc::new(StdMutex::new(cli));
        clone_bootstrap(cli_engine.clone(), &cli_ep, dial.clone(), &AuthOpts::default())
            .await
            .unwrap();
        let watcher = {
            let (e, a) = (cli_engine.clone(), AuthOpts::default());
            let ep = cli_ep.clone();
            let dial = dial.clone();
            tokio::spawn(async move { connect(e, &ep, dial, &a, false, new_conns(), None).await })
        };

        // Give the watch connection time to handshake + register.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        // The hub authors a new row and fans it out live to connected peers.
        let pushed = {
            let eng = srv_engine.lock().unwrap();
            eng.record_write("live.md", b"pushed live\n").unwrap();
            let site = eng.site_id();
            crate::SessionVault::rows_after_wire(&*eng, &site, -1).unwrap()
        };
        for wr in pushed {
            if wr.row.path.as_deref() == Some("live.md") {
                fanout(&srv_conns, 0, &Msg::Push { row: Box::new(wr) }).await;
            }
        }

        // The watching client should materialize the pushed file within a bound.
        let mut got = false;
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if cli_dir.path().join("live.md").exists() {
                got = true;
                break;
            }
        }
        assert!(got, "a live push must reach and materialize on a watching peer");
        assert_eq!(std::fs::read(cli_dir.path().join("live.md")).unwrap(), b"pushed live\n");
        watcher.abort();
        server.abort();
    }
}
