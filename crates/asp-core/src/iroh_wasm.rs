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

async fn read_frame(recv: &mut RecvStream) -> Result<Option<Vec<u8>>, String> {
    let mut len = [0u8; 4];
    if recv.read_exact(&mut len).await.is_err() {
        return Ok(None); // clean end-of-stream at a frame boundary
    }
    let n = u32::from_be_bytes(len) as usize;
    let mut buf = vec![0u8; n];
    recv.read_exact(&mut buf).await.map_err(|e| format!("read body: {e}"))?;
    Ok(Some(buf))
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
) -> Result<usize, String> {
    let seed = eng.device_seed();
    let ep = bind(&seed, relay_url).await?;
    let addr: EndpointAddr = EndpointTicket::from_str(ticket.trim())
        .map(EndpointAddr::from)
        .map_err(|e| format!("bad ticket: {e}"))?;

    let result = drive(&ep, &eng, addr, auth_keys).await;
    ep.close().await;
    result
}

async fn drive(
    ep: &Endpoint,
    eng: &Rc<MemEngine>,
    addr: EndpointAddr,
    auth_keys: Vec<String>,
) -> Result<usize, String> {
    let conn = ep.connect(addr, ALPN).await.map_err(|e| format!("connect: {e}"))?;
    let (mut send, mut recv) = conn.open_bi().await.map_err(|e| format!("open_bi: {e}"))?;

    let admit = AdmitCtx {
        no_tofu: false,
        auth_key_ok: false,
        auth_key_configured: false,
        default_ttl_days: 90,
        now_unix: 0,
    };
    let mut session = Session::with_auth(Role::Connector, &**eng, Vec::new(), None, admit, auth_keys);

    for step in session.start() {
        if let Step::Send(m) = step {
            write_frame(&mut send, &m.to_bytes().map_err(|e| e.to_string())?).await?;
        }
    }

    let mut integrated = 0usize;
    let mut synced = false;
    loop {
        let frame = match read_frame(&mut recv).await? {
            Some(f) => f,
            None => break,
        };
        let msg = match Msg::from_bytes(&frame) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let steps = session.on_msg(&**eng, msg).map_err(|e| e.to_string())?;
        let mut done = false;
        for step in steps {
            match step {
                Step::Send(m) => {
                    write_frame(&mut send, &m.to_bytes().map_err(|e| e.to_string())?).await?
                }
                Step::Integrated(rows) => integrated += rows.len(),
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
            break;
        }
    }
    let _ = send.finish();
    if !synced {
        return Err("sync closed before completion — catch-up incomplete".into());
    }
    Ok(integrated)
}
