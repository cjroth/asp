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

use crate::net::{fanout_row, AuthOpts, ConnEntry, Conns, EngineRef, CONN_SEQ};
use crate::session::Step;
use crate::wire::WireRow;
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
    bind_endpoint_relay(seed, relays, None).await
}

/// Bind with an explicit relay choice: `relay_url` pins one relay (a self-hosted
/// `asp relay`, or a local relay in tests) regardless of `relays`; otherwise
/// `relays` selects the public n0 relays vs a relay-less endpoint.
pub async fn bind_endpoint_relay(
    seed: &[u8; 32],
    relays: bool,
    relay_url: Option<&str>,
) -> Result<Endpoint> {
    use std::str::FromStr;
    let sk = secret_key(seed);
    let builder = if let Some(u) = relay_url {
        let url = iroh::RelayUrl::from_str(u.trim()).map_err(|e| anyhow!("bad relay url: {e}"))?;
        let map: iroh::RelayMap = [url].into_iter().collect();
        Endpoint::builder(iroh::endpoint::presets::Empty).relay_mode(RelayMode::Custom(map))
    } else if relays {
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
    ticket_with_relay(ep, relays, None).await
}

/// Like [`ticket`], but also injects an explicit relay URL into the ticket. Used
/// by the co-hosted all-in-one box (`watch --listen --relay`): it registers with
/// its OWN relay, reached via a cloud hairpin (e.g. the box dialing its own
/// `https://app.fly.dev`). The relay *connection* comes up fine, but iroh's
/// home-relay *selection* (netcheck) can stall over that hairpin and never
/// complete — so we don't block on it, and we embed the known relay URL directly
/// so the ticket is dialable regardless.
pub async fn ticket_with_relay(
    ep: &Endpoint,
    relays: bool,
    explicit_relay: Option<&str>,
) -> Result<String> {
    use std::str::FromStr;
    if relays {
        // A reachable home relay is normally selected in ~1-2s, but the wait can
        // hang indefinitely if selection stalls, so cap it. With an explicit
        // relay we inject it below regardless, so a short cap is fine.
        let secs = if explicit_relay.is_some() { 3 } else { 12 };
        let _ = tokio::time::timeout(std::time::Duration::from_secs(secs), ep.online()).await;
    }
    // Wait (bounded) for at least one dialable address — direct addresses are
    // discovered asynchronously just after bind, so a ticket minted too eagerly
    // could carry no address (undialable under `--no-relay`).
    for _ in 0..200 {
        if !ep.addr().addrs.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let mut addr = ep.addr();
    if let Some(u) = explicit_relay {
        let url = iroh::RelayUrl::from_str(u.trim()).map_err(|e| anyhow!("bad relay url: {e}"))?;
        addr.addrs.insert(TransportAddr::Relay(url));
    }
    Ok(EndpointTicket::new(addr).to_string())
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

/// Spawn a co-hosted relay and return its `http://addr` URL plus a task that owns
/// it (aborting the task stops the relay). Used by the desktop "faster local
/// syncing" toggle to route peers through this machine instead of the public n0
/// relays. Bind `127.0.0.1:0` for a free localhost port.
pub async fn spawn_relay(http_bind: SocketAddr) -> Result<(String, tokio::task::JoinHandle<()>)> {
    use iroh_relay::server::{RelayConfig, Server, ServerConfig};
    let mut config = ServerConfig::default();
    config.relay = Some(RelayConfig::new(http_bind));
    config.quic = None;
    let server = Server::spawn(config).await.map_err(|e| anyhow!("starting relay: {e}"))?;
    let addr = server.http_addr().ok_or_else(|| anyhow!("relay bound no http address"))?;
    let url = format!("http://{addr}");
    tracing::info!(%addr, "co-hosted relay up");
    // The spawned task owns `server`, keeping it alive until the task is aborted.
    let handle = tokio::spawn(async move {
        std::future::pending::<()>().await;
        drop(server);
    });
    Ok((url, handle))
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
    conns.lock().await.insert(conn_id, ConnEntry { tx, policy: crate::authkeys::PeerPolicy::default() });

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
                    // Retain the admitted grant on this live connection so the
                    // realtime fan-out applies the peer's scope filter (A, §3.5).
                    if let Some(entry) = conns.lock().await.get_mut(&conn_id) {
                        entry.policy = session.policy().clone();
                    }
                    if let Some(s) = &on_auth {
                        let _ = s.send(node);
                    }
                }
                Step::Integrated(rows) => {
                    // Hub forward-then-merge: push newly-integrated rows to every
                    // other live peer so a relay propagates without re-folding —
                    // each peer's scope grant applied (fanout_row, §3.5).
                    for wr in rows {
                        fanout_row(&conns, conn_id, &engine, &wr).await;
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
                Step::CatchUp { peer_vv, policy } => {
                    if stream_catchup(&mut send, &engine, &peer_vv, &policy).await.is_err() {
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
///
/// Blobs are deduplicated across the whole catch-up: a row bundles its content
/// blob, but a vault with lots of repeated content (e.g. 28k files sharing 3k
/// unique blobs) would otherwise ship each blob once per referencing row — ~5x
/// its real size, which the receiver then has to parse and hash. We send each
/// blob only on its first occurrence (causal/seq order guarantees that's at or
/// before any later reference); the receiver accumulates blobs in its content
/// store, so a later row referencing an already-sent hash folds fine without the
/// bytes. Convergence is unchanged — the receiver's state is byte-identical.
async fn stream_catchup(
    send: &mut SendStream,
    engine: &EngineRef,
    peer_vv: &std::collections::BTreeMap<String, i64>,
    policy: &crate::authkeys::PeerPolicy,
) -> Result<()> {
    // Scope send-filter (A, scoped-sync §3.2): resolve the peer's whole-`file_id`
    // membership ONCE up front (SYNC membership over the full history), then retain
    // only in-scope rows in each page — CRUCIALLY between the cursor advance and the
    // blob dedup (below), so the examined frontier still moves across dropped seqs
    // (the dense-seq story) and a dropped out-of-scope row can't mark a shared blob
    // "sent" and starve a later in-scope row of its bytes.
    let (our_vv, members) = {
        let eng = engine.lock().unwrap();
        let members = match &policy.allowed_paths {
            Some(allowed) => Some(eng.scope_members(allowed)?),
            None => None,
        };
        (eng.store.version_vector()?, members)
    };
    let mut sent_blobs: std::collections::HashSet<String> = std::collections::HashSet::new();
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
            // Scope filter THEN blob dedup, in that order (§3.2) — see the helper.
            let page = scope_and_dedup_page(page, members.as_ref(), &mut sent_blobs);
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

/// Apply the scope send-filter, then the cross-catch-up blob dedup, to one already
/// cursor-advanced page — **in that order** (scoped-sync §3.2). Filtering AFTER the
/// dedup would let a dropped out-of-scope row mark a *shared* content blob "sent",
/// so a later in-scope row referencing that hash would ship blob-less and the
/// receiver would fold empty bytes and silently diverge. `members = None` disables
/// scoping (full replica). Pure, so the ordering is unit-tested without a socket.
fn scope_and_dedup_page(
    mut page: Vec<WireRow>,
    members: Option<&std::collections::HashSet<String>>,
    sent_blobs: &mut std::collections::HashSet<String>,
) -> Vec<WireRow> {
    if members.is_some() {
        page.retain(|wr| crate::session::scope_admits(wr, members));
    }
    for wr in &mut page {
        wr.blobs.retain(|b| sent_blobs.insert(b.hash.clone()));
    }
    page
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
    // iroh's QUIC handshake already authenticated the remote key; the Session
    // cross-checks the peer's `Hello.node_id` against it. Admission proper is the
    // `authorized_keys` table inside the Session.
    let verified_peer = node_id_of(&conn);
    let (send, recv) = conn.accept_bi().await.map_err(|e| anyhow!("accept_bi: {e}"))?;
    let admit = auth.admit_ctx(false);
    let session = {
        let eng = engine.lock().unwrap();
        Session::new(Role::Listener, &*eng, admit, verified_peer, auth.auth_keys.clone())
    };
    drive(send, recv, engine, session, false, conns, None).await
}

// ---------------- thin remote-view query server (C, scoped-sync §5) ----------------

/// Bind an endpoint that serves the thin-client QUERY ALPN (`asp/query/1`) —
/// `asp serve`. Distinct ALPN from sync, so `Msg`/`PROTO` are untouched.
pub async fn bind_query_endpoint(seed: &[u8; 32], relays: bool, relay_url: Option<&str>) -> Result<Endpoint> {
    use std::str::FromStr;
    let sk = secret_key(seed);
    let builder = if let Some(u) = relay_url {
        let url = iroh::RelayUrl::from_str(u.trim()).map_err(|e| anyhow!("bad relay url: {e}"))?;
        let map: iroh::RelayMap = [url].into_iter().collect();
        Endpoint::builder(iroh::endpoint::presets::Empty).relay_mode(RelayMode::Custom(map))
    } else if relays {
        Endpoint::builder(iroh::endpoint::presets::N0)
    } else {
        Endpoint::builder(iroh::endpoint::presets::Empty).relay_mode(RelayMode::Disabled)
    };
    builder
        .crypto_provider(iroh::tls::default_provider())
        .secret_key(sk)
        .alpns(vec![crate::thin::QUERY_ALPN.to_vec()])
        .bind()
        .await
        .map_err(|e| anyhow!("binding query endpoint: {e}"))
}

/// Serve the thin-client query protocol (scoped-sync §5): each connection is a
/// request/response stream driving a [`crate::thin::ThinSession`] against the
/// source engine, filtered to the client's `authorized_keys` grant. One broadcast
/// change-signal feeds every subscriber (signal-then-pull), so multiple clients'
/// subscriptions all fire.
pub async fn serve_queries(engine: EngineRef, ep: Endpoint, auth: AuthOpts) -> Result<()> {
    use tokio::sync::broadcast;
    let (chg_tx, _) = broadcast::channel::<String>(256);
    {
        // The engine's single change listener fans out to the broadcast. `notify`
        // fires per integrate/author; we signal all subscribers (they re-query).
        let tx = chg_tx.clone();
        engine.lock().unwrap().set_change_listener(Arc::new(move || {
            let _ = tx.send(String::new());
        }));
    }
    tracing::info!(endpoint = %ep.id().fmt_short(), "asp serve: thin query ALPN listening");
    while let Some(incoming) = ep.accept().await {
        let engine = engine.clone();
        let auth = auth.clone();
        let chg_rx = chg_tx.subscribe();
        tokio::spawn(async move {
            if let Err(e) = accept_query(incoming, engine, auth, chg_rx).await {
                tracing::debug!("query accept error: {e}");
            }
        });
    }
    Ok(())
}

async fn accept_query(
    incoming: iroh::endpoint::Incoming,
    engine: EngineRef,
    auth: AuthOpts,
    mut chg_rx: tokio::sync::broadcast::Receiver<String>,
) -> Result<()> {
    use crate::thin::{ThinReq, ThinResp, ThinSession};
    let conn = incoming.await.map_err(|e| anyhow!("accept: {e}"))?;
    let client = node_id_of(&conn);
    // The query ALPN reuses A's/B's grant directly: admit the client and retain its
    // policy (allowed_paths / read_only). Denied → close.
    let policy = {
        let eng = engine.lock().unwrap();
        eng.admit(&client, &auth.admit_ctx(false)).map_err(|e| anyhow!("admission denied: {e}"))?
    };
    let session = ThinSession::new(client, policy);
    let (mut send, mut recv) = conn.accept_bi().await.map_err(|e| anyhow!("accept_bi: {e}"))?;

    let mut subs: Vec<u64> = Vec::new();
    loop {
        tokio::select! {
            biased;
            // A vault change → signal every active subscriber (signal-then-pull).
            chg = chg_rx.recv() => {
                if chg.is_err() { continue; }
                for sub_id in &subs {
                    let ev = ThinResp::Event { sub_id: *sub_id };
                    if write_frame(&mut send, &ev.to_bytes()?).await.is_err() { return Ok(()); }
                }
            }
            frame = read_frame(&mut recv) => {
                let frame = match frame? { Some(f) => f, None => break };
                let req = match ThinReq::from_bytes(&frame) { Ok(r) => r, Err(_) => continue };
                if let ThinReq::Subscribe { sub_id, .. } = &req { if !subs.contains(sub_id) { subs.push(*sub_id); } }
                if let ThinReq::Unsubscribe { sub_id } = &req { subs.retain(|s| s != sub_id); }
                let resp = { let eng = engine.lock().unwrap(); session.on_req(&eng, req)? };
                if write_frame(&mut send, &resp.to_bytes()?).await.is_err() { break; }
            }
        }
    }
    let _ = send.finish();
    Ok(())
}

/// A thin-client connection (`asp view`): dial the source's query ALPN and drive a
/// request/response stream. Holds the bi-stream so multiple requests reuse it.
pub struct QueryClient {
    conn: Connection,
    send: SendStream,
    recv: RecvStream,
}

impl QueryClient {
    /// Dial `addr`'s query ALPN and open the request stream.
    pub async fn connect(ep: &Endpoint, addr: EndpointAddr) -> Result<QueryClient> {
        let conn = ep
            .connect(addr, crate::thin::QUERY_ALPN)
            .await
            .map_err(|e| anyhow!("query connect: {e}"))?;
        let (send, recv) = conn.open_bi().await.map_err(|e| anyhow!("open_bi: {e}"))?;
        Ok(QueryClient { conn, send, recv })
    }

    /// Send one request, await its response.
    pub async fn request(&mut self, req: &crate::thin::ThinReq) -> Result<crate::thin::ThinResp> {
        write_frame(&mut self.send, &req.to_bytes()?).await?;
        let frame = read_frame(&mut self.recv)
            .await?
            .ok_or_else(|| anyhow!("query stream closed before a response"))?;
        crate::thin::ThinResp::from_bytes(&frame).map_err(|e| anyhow!("{e}"))
    }

    pub async fn close(self) {
        let mut send = self.send;
        let _ = send.finish();
        self.conn.close(0u32.into(), b"bye");
    }
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
    // The listener's key, as authenticated by iroh's QUIC handshake.
    let verified_peer = node_id_of(&conn);
    let (send, recv) = conn.open_bi().await.map_err(|e| anyhow!("open_bi: {e}"))?;
    let session = {
        let eng = engine.lock().unwrap();
        Session::new(Role::Connector, &*eng, auth.admit_ctx(false), verified_peer, auth.auth_keys.clone())
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

    /// A — the scope filter MUST precede the blob dedup in `stream_catchup`
    /// (scoped-sync §3.2, §9). Two files share one content blob; the OUT-of-scope
    /// one sorts first. If we deduped before filtering, the dropped out-of-scope row
    /// would mark the shared blob "sent" and the surviving in-scope row would ship
    /// blob-less — the receiver would fold empty bytes and diverge. `scope_and_dedup_page`
    /// does it in the safe order; this pins that the in-scope row keeps its blob.
    #[test]
    fn scope_filter_precedes_blob_dedup() {
        use crate::log::{Kind, LogRow, MergeClass};
        use crate::wire::WireBlob;
        let h = crate::oid::content_hash(b"shared body\n");
        let blob = WireBlob { hash: h.clone(), bytes: b"shared body\n".to_vec() };
        let mk = |file_id: &str, seq: u64, path: &str| WireRow {
            row: LogRow {
                site_id: "s".into(),
                seq,
                lamport: seq + 1,
                file_id: file_id.into(),
                kind: Kind::Create,
                merge_class: MergeClass::Text,
                result_hash: Some(h.clone()),
                path: Some(path.into()),
                ..LogRow::default()
            }
            .seal(),
            blobs: vec![blob.clone()],
        };
        // Page order: the OUT-of-scope file (seq 0) precedes the in-scope one (seq 1).
        let page = vec![mk("fB", 0, "personal/b.md"), mk("fA", 1, "work/a.md")];
        let members: std::collections::HashSet<String> = ["fA".to_string()].into_iter().collect();
        let mut sent = std::collections::HashSet::new();
        let out = scope_and_dedup_page(page, Some(&members), &mut sent);
        assert_eq!(out.len(), 1, "only the in-scope row survives");
        assert_eq!(out[0].row.file_id, "fA");
        assert!(out[0].blobs.iter().any(|b| b.hash == h), "the in-scope row keeps its shared blob (not stolen by the dropped out-of-scope row)");
    }

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

    /// A — a scoped (`--subdir`) clone over REAL iroh QUIC receives only in-scope
    /// files; out-of-scope rows never cross the wire (scoped-sync §3, §9). Exercises
    /// the real `stream_catchup` scope filter end-to-end, not just the in-process
    /// pump. The grant is also read-only (the recommended "read-only single-subdir
    /// clone from a hub" slice).
    #[tokio::test]
    async fn iroh_scoped_clone_only_receives_in_scope_files() {
        let srv_dir = tempdir().unwrap();
        let cli_dir = tempdir().unwrap();
        let srv_id = Identity::from_seed(&[31; 32]);
        let cli_id = Identity::from_seed(&[32; 32]);
        let srv = Engine::init(srv_dir.path(), srv_id.clone()).unwrap();
        srv.record_write("work/a.md", b"in scope A\n").unwrap();
        srv.record_write("work/sub/b.md", b"in scope B\n").unwrap();
        srv.record_write("personal/secret.md", b"OUT of scope\n").unwrap();
        // Grant the client a work/-scoped, read-only replica.
        srv.authorize_with_policy(&cli_id.to_ssh_string(), None, true, "test", Some(vec!["work".into()]), true).unwrap();

        let srv_ep = bind_endpoint(&srv_id.seed(), false).await.unwrap();
        let cli_ep = bind_endpoint(&cli_id.seed(), false).await.unwrap();
        let dial = loopback_addr(&srv_ep);
        let srv_engine: EngineRef = Arc::new(StdMutex::new(srv));
        let server = tokio::spawn(serve(srv_engine, srv_ep, AuthOpts::default(), new_conns()));

        let cli = Engine::init(cli_dir.path(), cli_id).unwrap();
        cli.set_partial_scope(&["work".to_string()]).unwrap();
        let cli_engine: EngineRef = Arc::new(StdMutex::new(cli));
        clone_bootstrap(cli_engine.clone(), &cli_ep, dial, &AuthOpts::default())
            .await
            .expect("scoped clone over iroh should succeed");

        assert_eq!(std::fs::read(cli_dir.path().join("work/a.md")).unwrap(), b"in scope A\n");
        assert_eq!(std::fs::read(cli_dir.path().join("work/sub/b.md")).unwrap(), b"in scope B\n");
        assert!(!cli_dir.path().join("personal/secret.md").exists(), "out-of-scope file must never cross the wire");
        let has_out = cli_engine
            .lock()
            .unwrap()
            .store
            .all_rows()
            .unwrap()
            .iter()
            .any(|r| r.path.as_deref() == Some("personal/secret.md"));
        assert!(!has_out, "no out-of-scope row reached the scoped replica's log");
        server.abort();
    }

    /// C — the thin remote-view client over the REAL query ALPN (scoped-sync §5, §9).
    /// A source serves reads + a write-through over `asp/query/1`; the client reads a
    /// file, submits a signed write, and the source authors it (attributed in
    /// `remote_edits`). Exercises the whole native query stack end to end.
    #[tokio::test]
    async fn iroh_thin_query_and_submit_over_query_alpn() {
        use crate::thin::{QueryOp, QueryResult, SubmitOp, SubmitResult, ThinReq, ThinResp};
        let srv_dir = tempdir().unwrap();
        let srv_id = Identity::from_seed(&[41; 32]);
        let cli_id = Identity::from_seed(&[42; 32]);
        let srv = Engine::init(srv_dir.path(), srv_id.clone()).unwrap();
        srv.record_write("notes/hello.md", b"hi from source\n").unwrap();
        srv.authorize(&cli_id.to_ssh_string(), None, true, "test").unwrap();

        let srv_ep = bind_query_endpoint(&srv_id.seed(), false, None).await.unwrap();
        let cli_ep = bind_endpoint(&cli_id.seed(), false).await.unwrap();
        let dial = loopback_addr(&srv_ep);
        let srv_engine: EngineRef = Arc::new(StdMutex::new(srv));
        let server = tokio::spawn(serve_queries(srv_engine.clone(), srv_ep, AuthOpts::default()));

        let mut client = QueryClient::connect(&cli_ep, dial).await.expect("connect over query ALPN");

        // Read a file server-side (no local log on the client).
        let r = client.request(&ThinReq::Query { id: 1, op: QueryOp::ReadFile { path: "notes/hello.md".into() } }).await.unwrap();
        assert!(matches!(r, ThinResp::QueryResp { result: QueryResult::File(Some(ref b)), .. } if b == b"hi from source\n"), "got {r:?}");

        // Submit a signed write-through; the SOURCE authors it.
        let op = SubmitOp::Write { path: "notes/new.md".into(), bytes: b"authored via thin\n".to_vec(), base_hash: None };
        let nonce = 3;
        let sig = cli_id.sign(&crate::thin::submit_envelope(&op, nonce));
        let r = client.request(&ThinReq::Submit { id: 2, op, nonce, envelope_sig: sig }).await.unwrap();
        let ThinResp::SubmitResp { result: SubmitResult::Ok { row_id }, .. } = r else { panic!("expected Ok, got {r:?}") };

        {
            let eng = srv_engine.lock().unwrap();
            assert_eq!(
                eng.store.live_file_by_path("notes/new.md").unwrap().unwrap().result_hash,
                Some(crate::oid::content_hash(b"authored via thin\n")),
                "the write-through materialized on the source",
            );
            let attr = eng.store.remote_edit(&row_id).unwrap().expect("attribution recorded");
            assert_eq!(attr.0, cli_id.node_id().to_hex(), "attributed to the thin client");
        }
        client.close().await;
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
                fanout_row(&srv_conns, 0, &srv_engine, &wr).await;
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
