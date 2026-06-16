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

use crate::net::{now_unix, AuthOpts, EngineRef};
use crate::session::Step;
use crate::{Msg, NodeId, Role, Session};
use anyhow::{anyhow, Result};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, TransportAddr};
use std::net::SocketAddr;
use tokio::sync::mpsc;

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

/// Drive one connection's `Session` to completion over an iroh bi-stream. A
/// oneshot connector (`sync`/`clone`) finishes when the peer signals `Synced`; a
/// listener stays until the peer closes. `on_auth` surfaces the authenticated
/// peer `NodeId` (used by `clone` to pin the listener).
async fn drive(
    mut send: SendStream,
    mut recv: RecvStream,
    engine: EngineRef,
    mut session: Session,
    oneshot: bool,
    on_auth: Option<mpsc::UnboundedSender<NodeId>>,
) -> Result<()> {
    for step in session.start() {
        if let Step::Send(m) = step {
            send_msg(&mut send, &m).await?;
        }
    }

    // A oneshot is "done" only when the peer signals it sent everything
    // (`Synced`). A denied connector authenticates the listener (so `authed()` is
    // true) but never reaches `Synced`, and the QUIC teardown can race the
    // `Denied` frame — so completion, not a clean close, is the success gate.
    let mut completed = false;
    let result: Result<()> = loop {
        let frame = match read_frame(&mut recv).await? {
            Some(f) => f,
            None => break Ok(()),
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
                Step::Send(m) => send_msg(&mut send, &m).await?,
                Step::Authenticated(node) => {
                    tracing::info!(peer = %&node.to_hex()[..12], "iroh handshake ok");
                    if let Some(s) = &on_auth {
                        let _ = s.send(node);
                    }
                }
                Step::Integrated(_rows) => {
                    // Live fan-out to other peers (hub forward-then-merge) is added
                    // with the multi-connection watch host; a single sync/clone
                    // connection has no siblings to forward to.
                }
                Step::Closed(reason) => {
                    if reason.contains("denied") || reason.contains("different vault") {
                        let hint = if reason.contains("different vault") {
                            " (separate vaults — `asp clone <ticket>` to follow the peer)"
                        } else {
                            ""
                        };
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

/// Accept inbound iroh connections forever, driving each as a listener `Session`
/// gated by the `authorized_keys` admission set.
pub async fn serve(engine: EngineRef, ep: Endpoint, auth: AuthOpts) -> Result<()> {
    tracing::info!(endpoint = %ep.id().fmt_short(), "iroh listening");
    while let Some(incoming) = ep.accept().await {
        let engine = engine.clone();
        let auth = auth.clone();
        tokio::spawn(async move {
            if let Err(e) = accept_one(incoming, engine, auth).await {
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
    drive(send, recv, engine, session, false, None).await
}

// ---------------- connector (sync / clone / watch) ----------------

/// Dial a peer by address and drive a connector `Session`. `oneshot` ends on the
/// peer's `Synced`; a persistent `watch` passes `false` and stays connected.
pub async fn connect(
    engine: EngineRef,
    ep: &Endpoint,
    addr: EndpointAddr,
    auth: &AuthOpts,
    oneshot: bool,
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
    drive(send, recv, engine, session, oneshot, on_auth).await
}

/// Clone bootstrap: connect with an empty local vault, adopt the peer's vault,
/// pull everything, then pin the listener as a peer.
pub async fn clone_bootstrap(
    engine: EngineRef,
    ep: &Endpoint,
    addr: EndpointAddr,
    auth: &AuthOpts,
) -> Result<()> {
    let (auth_tx, mut auth_rx) = mpsc::unbounded_channel::<NodeId>();
    let ticket_id = addr.id;
    connect(engine.clone(), ep, addr, auth, true, Some(auth_tx)).await?;
    if let Ok(node) = auth_rx.try_recv() {
        let eng = engine.lock().unwrap();
        let _ = eng.store.add_peer(&node.to_hex(), &node.to_hex(), now_unix());
    }
    let _ = ticket_id;
    Ok(())
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
        let server = tokio::spawn(serve(srv_engine, srv_ep, AuthOpts::default()));

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
        let server = tokio::spawn(serve(srv_engine, srv_ep, opts));

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
        async fn attempt(secret: &str, cli_seed: u8) -> (Result<()>, tempfile::TempDir) {
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
            let server = tokio::spawn(serve(srv_engine, srv_ep, srv_opts));
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
}
