//! The **wasm/browser** iroh transport driver (§Transport: iroh, browser nodes).
//!
//! A browser sandbox can't send UDP, so iroh relays the QUIC traffic over a
//! WebSocket to a relay — still end-to-end encrypted, still dial-by-key. The
//! whole connect+drive lives in one owned async future (no `tokio`, no spawned
//! reader task), so it satisfies wasm-bindgen's `'static` requirement when a
//! `WasmEngine` method `.await`s it: the future owns the endpoint, the streams,
//! the `Session`, and an `Rc<MemEngine>` clone — nothing is borrowed across the
//! boundary.
//!
//! Only the **connector** role runs here (a thin/browser node dials a hub and
//! never listens), so the driver is a simple sequential loop — no fan-out, no
//! paged catch-up (the connector's own catch-up is built inline by the Session).

use crate::authkeys::AdmitCtx;
use crate::memengine::MemEngine;
use crate::session::Step;
use crate::{Msg, Role, Session};
use iroh::endpoint::{RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, RelayMap, RelayMode, RelayUrl, SecretKey};
use iroh_tickets::endpoint::EndpointTicket;
use std::rc::Rc;
use std::str::FromStr;

/// Application protocol — identical to the native driver, so a browser node and a
/// native node speak the same ALPN.
pub const ALPN: &[u8] = b"asp/sync/1";

async fn write_frame(send: &mut SendStream, bytes: &[u8]) -> Result<(), String> {
    let len = (bytes.len() as u32).to_be_bytes();
    send.write_all(&len).await.map_err(|e| format!("write: {e}"))?;
    send.write_all(bytes).await.map_err(|e| format!("write: {e}"))?;
    Ok(())
}

/// Idle cap for a single catch-up frame. A large clone streams many frames, but no
/// single frame should take this long to arrive over the relay — if one does, the
/// transfer has stalled (dropped relay, dead listener), so fail loudly instead of
/// hanging the "Receiving notes…" spinner forever. Generous so a slow-but-alive
/// relay isn't killed mid-frame.
const FRAME_IDLE_SECS: u64 = 45;

async fn read_frame(recv: &mut RecvStream) -> Result<Option<Vec<u8>>, String> {
    use n0_future::time::{timeout, Duration};
    let mut len = [0u8; 4];
    // The gap BETWEEN frames is unbounded (the peer may pause), so a plain read on
    // the length prefix is fine — a clean end-of-stream here just ends catch-up.
    if recv.read_exact(&mut len).await.is_err() {
        return Ok(None); // clean end-of-stream at a frame boundary
    }
    let n = u32::from_be_bytes(len) as usize;
    let mut buf = vec![0u8; n];
    // Once a frame's length has arrived its body must follow promptly; bound it so a
    // mid-frame stall surfaces as an error the UI can show rather than an infinite hang.
    match timeout(Duration::from_secs(FRAME_IDLE_SECS), recv.read_exact(&mut buf)).await {
        Ok(Ok(())) => Ok(Some(buf)),
        Ok(Err(e)) => Err(format!("read body: {e}")),
        Err(_) => Err(format!("clone stalled — no data for {FRAME_IDLE_SECS}s (relay or peer dropped)")),
    }
}

/// Bind a browser iroh endpoint under the device key. With a `relay_url` it uses
/// exactly that relay (hermetic tests / a private relay); otherwise the public n0
/// relays + browser pkarr discovery.
async fn bind(seed: &[u8; 32], relay_url: Option<String>) -> Result<Endpoint, String> {
    let sk = SecretKey::from_bytes(seed);
    let builder = match relay_url {
        Some(u) => {
            let url = RelayUrl::from_str(u.trim()).map_err(|e| format!("bad relay url: {e}"))?;
            let map: RelayMap = [url].into_iter().collect();
            Endpoint::builder(iroh::endpoint::presets::Empty).relay_mode(RelayMode::Custom(map))
        }
        None => Endpoint::builder(iroh::endpoint::presets::N0),
    };
    builder
        .crypto_provider(iroh::tls::default_provider())
        .secret_key(sk)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .map_err(|e| format!("bind: {e}"))
}

/// One-shot sync: dial `ticket` over iroh, run the handshake + bidirectional
/// version-vector catch-up, converge, and close. Returns the number of rows
/// integrated FROM the peer this pass. The future owns everything → `'static`.
pub async fn sync_oneshot(
    eng: Rc<MemEngine>,
    ticket: String,
    auth_keys: Vec<String>,
    relay_url: Option<String>,
    on_progress: &dyn Fn(usize, usize),
) -> Result<usize, String> {
    // Parse the ticket first (cheap, fails fast on a malformed/empty ticket
    // before we bind an endpoint or touch the network).
    let addr: EndpointAddr = EndpointTicket::from_str(ticket.trim())
        .map(EndpointAddr::from)
        .map_err(|e| format!("bad ticket: {e}"))?;
    let seed = eng.device_seed();
    let ep = bind(&seed, relay_url).await?;

    let result = drive(&ep, &eng, addr, auth_keys, on_progress).await;
    ep.close().await;
    result
}

/// **Live** connect: dial `ticket`, run the same handshake + catch-up as
/// `sync_oneshot`, but then *stay connected* and stream rows both ways in
/// realtime — the browser can't accept inbound connections, but once it dials
/// out the link is bidirectional. Inbound peer pushes are integrated and reported
/// via `on_change(rows)`; locally-authored rows arriving on `local_rx` are pushed
/// to the peer over the same connection. Returns when the connection closes (the
/// caller reconnects). The future owns everything → `'static`.
#[cfg(target_arch = "wasm32")]
pub async fn connect_live(
    eng: Rc<MemEngine>,
    ticket: String,
    auth_keys: Vec<String>,
    relay_url: Option<String>,
    local_rx: futures_channel::mpsc::UnboundedReceiver<crate::wire::WireRow>,
    mut on_change: impl FnMut(usize),
) -> Result<(), String> {
    use futures_util::StreamExt;

    let addr: EndpointAddr = EndpointTicket::from_str(ticket.trim())
        .map(EndpointAddr::from)
        .map_err(|e| format!("bad ticket: {e}"))?;
    let seed = eng.device_seed();
    let ep = bind(&seed, relay_url).await?;
    let conn = ep.connect(addr, ALPN).await.map_err(|e| format!("connect: {e}"))?;
    let verified_peer = crate::NodeId(*conn.remote_id().as_bytes());
    let (mut send, recv) = conn.open_bi().await.map_err(|e| format!("open_bi: {e}"))?;

    let admit = AdmitCtx {
        no_tofu: false,
        auth_key_ok: false,
        auth_key_configured: false,
        default_ttl_days: 90,
        now_unix: 0,
    };
    let mut session = Session::new(Role::Connector, &*eng, admit, verified_peer, auth_keys);
    for step in session.start() {
        if let Step::Send(m) = step {
            write_frame(&mut send, &m.to_bytes().map_err(|e| e.to_string())?).await?;
        }
    }

    // A half-read frame must never be dropped by stream selection, so a dedicated
    // reader future owns `recv` and forwards whole frames over a channel; the main
    // loop only selects over cancel-safe stream items.
    let (frame_tx, frame_rx) = futures_channel::mpsc::unbounded::<Vec<u8>>();
    wasm_bindgen_futures::spawn_local(async move {
        let mut recv = recv;
        loop {
            match read_frame(&mut recv).await {
                Ok(Some(f)) => {
                    if frame_tx.unbounded_send(f).is_err() {
                        break;
                    }
                }
                _ => break, // EOF or read error → drop frame_tx → frame_rx ends
            }
        }
    });

    enum Ev {
        Frame(Vec<u8>),
        Local(crate::wire::WireRow),
        Closed,
    }
    let frames = frame_rx.map(Ev::Frame);
    let locals = local_rx.map(Ev::Local);
    let closed = futures_util::stream::once(async move {
        conn.closed().await;
        Ev::Closed
    });
    // Box::pin so the combined stream is Unpin (the `once(async …)` arm isn't).
    let mut events = Box::pin(futures_util::stream::select(
        futures_util::stream::select(frames, locals),
        closed,
    ));

    let mut result: Result<(), String> = Ok(());
    'live: while let Some(ev) = events.next().await {
        match ev {
            Ev::Closed => break,
            Ev::Local(row) => {
                // A row the host just authored — push it to the peer immediately.
                let frame = Msg::Push { row: Box::new(row) }.to_bytes().map_err(|e| e.to_string())?;
                if write_frame(&mut send, &frame).await.is_err() {
                    break; // peer went away; caller reconnects
                }
            }
            Ev::Frame(frame) => {
                let msg = match Msg::from_bytes(&frame) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let steps = session.on_msg(&*eng, msg).map_err(|e| e.to_string())?;
                let mut integrated = 0usize;
                for step in steps {
                    match step {
                        Step::Send(m) => {
                            write_frame(&mut send, &m.to_bytes().map_err(|e| e.to_string())?).await?
                        }
                        Step::Integrated(rows) => integrated += rows.len(),
                        Step::Closed(reason) => {
                            result = Err(reason);
                            break 'live;
                        }
                        // Initial catch-up done — refresh the UI, then keep the
                        // connection open for live pushes.
                        Step::PeerSynced => on_change(0),
                        Step::Authenticated(_) | Step::CatchUp { .. } => {}
                    }
                }
                if integrated > 0 {
                    on_change(integrated);
                }
            }
        }
    }

    let _ = send.finish();
    ep.close().await;
    result
}

async fn drive(
    ep: &Endpoint,
    eng: &Rc<MemEngine>,
    addr: EndpointAddr,
    auth_keys: Vec<String>,
    on_progress: &dyn Fn(usize, usize),
) -> Result<usize, String> {
    let conn = ep.connect(addr, ALPN).await.map_err(|e| format!("connect: {e}"))?;
    // The listener's key, authenticated by iroh's QUIC handshake — the Session
    // cross-checks the peer's `Hello.node_id` against it.
    let verified_peer = crate::NodeId(*conn.remote_id().as_bytes());
    let (mut send, mut recv) = conn.open_bi().await.map_err(|e| format!("open_bi: {e}"))?;
    use n0_future::time::{timeout, Duration};

    let admit = AdmitCtx {
        no_tofu: false,
        auth_key_ok: false,
        auth_key_configured: false,
        default_ttl_days: 90,
        now_unix: 0,
    };
    let mut session = Session::new(Role::Connector, &**eng, admit, verified_peer, auth_keys);

    for step in session.start() {
        if let Step::Send(m) = step {
            write_frame(&mut send, &m.to_bytes().map_err(|e| e.to_string())?).await?;
        }
    }

    // Defer the fold across the whole paged catch-up: integrate every page into
    // the log, then fold ONCE below — not once per page (O(N·pages) → O(N), the
    // difference between a multi-minute and a few-second clone of a big vault).
    eng.set_batch(true);
    let result: Result<usize, String> = async {
        let mut integrated = 0usize;
        // The listener's version vector (its first frame) tells us how many rows
        // exist, so the UI can show a real "x of N" progress bar during catch-up.
        let mut total = 0usize;
        let mut synced = false;
        on_progress(0, 0);
        loop {
            let frame = match read_frame(&mut recv).await? {
                Some(f) => f,
                None => break,
            };
            let msg = match Msg::from_bytes(&frame) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if let Msg::Vector { vv } = &msg {
                // seqs are 0-based per site, so a site's row count is max_seq + 1.
                total = vv.values().map(|v| (*v + 1).max(0) as usize).sum();
            }
            let steps = session.on_msg(&**eng, msg).map_err(|e| e.to_string())?;
            let mut done = false;
            for step in steps {
                match step {
                    Step::Send(m) => {
                        write_frame(&mut send, &m.to_bytes().map_err(|e| e.to_string())?).await?
                    }
                    Step::Integrated(rows) => {
                        integrated += rows.len();
                        on_progress(integrated, total.max(integrated));
                    }
                    Step::Closed(reason) => return Err(reason),
                    Step::PeerSynced => {
                        synced = true;
                        done = true;
                    }
                    // A browser connector never lists, so it never streams a catch-up.
                    Step::Authenticated(_) | Step::CatchUp { .. } => {}
                }
            }
            if done {
                // Send a graceful `Bye` end-marker, then wait for the listener to
                // close the connection — it does so only after draining our catch-up
                // rows (QUIC orders our `Rows` before this `Bye`). This guarantees our
                // pushed edits were received before we tear down; a fixed delay would
                // race the relay RTT and silently drop the push.
                let _ = write_frame(&mut send, &Msg::Bye.to_bytes().map_err(|e| e.to_string())?).await;
                let _ = send.finish();
                let _ = timeout(Duration::from_secs(10), conn.closed()).await;
                break;
            }
        }
        if !synced {
            let _ = send.finish();
            return Err("sync closed before completion — catch-up incomplete".into());
        }
        Ok(integrated)
    }
    .await;
    // Always clear batch and fold once — on success the full history, on an early
    // exit whatever we integrated — so the engine is never left deferred (`drive`
    // also backs `syncNow` on a long-lived engine, not just a throwaway clone).
    eng.set_batch(false);
    eng.materialize().map_err(|e| e.to_string())?;
    result
}
