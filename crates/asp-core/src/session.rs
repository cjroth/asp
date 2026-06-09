//! The sans-IO replication `Session` (§Sync protocol, §Security). One protocol
//! state machine — handshake, anti-entropy (version-vector catch-up), and
//! integrate — driven identically by the native socket driver and any future
//! wasm/SDK node. It consumes inbound [`Msg`] bytes and emits outbound [`Step`]s;
//! it owns no sockets, fs, or clock. Local I/O (the store) goes through `Engine`.
//!
//! Handshake: both sides send `Hello`; on the peer's `Hello` each signs the
//! mutual-auth transcript (`Auth`); on the peer's valid `Auth` the **listener**
//! applies `authorized_keys` admission and the **connector** enforces the
//! advertised channel binding and pins the listener. Then both exchange version
//! vectors and send exactly the rows the other lacks.

use crate::authkeys::AdmitCtx;
use crate::error::{AspError, AspResult};
use crate::identity::verify_detached;
use crate::order::NodeId;
use crate::wire::{transcript, Msg, WireRow, PROTO};
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Connector,
    Listener,
}

/// What the sans-IO `Session` needs from its host engine — implemented by both
/// the native `Engine` (SQLite, on-disk) and the wasm `MemEngine` (in-memory), so
/// the *identical* handshake + catch-up + integrate runs on every surface
/// (§Implementation: one engine, thin bindings). Methods take `&self`; both
/// engines are interior-mutable.
pub trait SessionVault {
    fn node_id(&self) -> NodeId;
    fn sign(&self, msg: &[u8]) -> Vec<u8>;
    fn vault_id(&self) -> String;
    /// A fresh node (empty vault id) adopts the peer's vault id on clone.
    fn adopt_vault_id(&self, vault_id: &str) -> AspResult<()>;
    fn version_vector(&self) -> AspResult<BTreeMap<String, i64>>;
    /// Rows authored by `site` after `seq`, bundled with their blobs.
    fn rows_after_wire(&self, site: &str, after: i64) -> AspResult<Vec<WireRow>>;
    fn integrate(&self, wr: &WireRow) -> AspResult<bool>;
    /// Integrate a batch, returning a per-row flag (true = newly added). The
    /// default integrates one-by-one; engines that re-fold on each `integrate`
    /// should override this to fold once — the per-row path is O(n²) over a
    /// large catch-up.
    fn integrate_many(&self, rows: &[WireRow]) -> AspResult<Vec<bool>> {
        rows.iter().map(|wr| self.integrate(wr)).collect()
    }
    fn admit(&self, peer: &NodeId, ctx: &AdmitCtx) -> AspResult<()>;
    /// No authored rows yet — the vault has nothing of its own to lose, so it
    /// adopts a peer's vault on connect (like `clone`). A freshly-`init`'d folder
    /// that hasn't committed content is pristine.
    fn is_pristine(&self) -> bool;
}

/// An effect the driver must perform.
pub enum Step {
    /// Send this frame to the peer.
    Send(Msg),
    /// Handshake complete; peer authenticated as this node.
    Authenticated(NodeId),
    /// These rows were newly integrated — the driver re-materializes and (if a
    /// relay) forwards them to other peers (hub forward-then-merge).
    Integrated(Vec<WireRow>),
    /// Close the connection with a reason (handshake/auth failure or `Bye`).
    Closed(String),
    /// Listener-side catch-up: send the peer (whose version vector is `peer_vv`)
    /// every row it lacks. The driver STREAMS these in pages rather than building
    /// the whole set up front — a full-vault build of a large vault is slow on a
    /// constrained host, and if the first byte doesn't reach the peer before its
    /// idle timeout the peer closes with 0 rows. Streaming keeps frames flowing
    /// (idle stays reset) and memory bounded. Only the native driver (net.rs)
    /// produces real streaming; see also `catchup_rows` for the inline fallback.
    CatchUp { peer_vv: std::collections::BTreeMap<String, i64> },
    /// The peer signalled it has sent all our missing rows (`Msg::Synced`). A
    /// oneshot driver closes on this; a persistent `watch` ignores it and stays
    /// connected for live pushes.
    PeerSynced,
}

pub struct Session {
    role: Role,
    our_node: NodeId,
    vault_id: String,
    our_nonce: Vec<u8>,
    /// The listener's advertised channel binding (its served-cert SHA-256, or
    /// empty = binding-disabled). For a listener this is its own; for a connector
    /// it is filled from the listener's `Hello`.
    advertised_binding: Vec<u8>,
    /// The connector's actually-observed cert binding (`None` if unobservable,
    /// e.g. plaintext `ws://` or a WebView that can't read the peer cert).
    observed_binding: Option<Vec<u8>>,
    admit: AdmitCtx,
    peer_node: Option<NodeId>,
    peer_nonce: Option<Vec<u8>>,
    sent_auth: bool,
    authed: bool,
    sent_vector: bool,
}

/// Per-frame catch-up budget. A `Msg::Rows` frame accumulates rows until it
/// crosses ~4 MiB of blob bytes (or 512 rows), then flushes — so serving a
/// full clone never holds more than one budget's worth of the history in a
/// serialized frame at a time. A single row whose blobs exceed the budget
/// still ships alone (never split a row from its blobs — the fold needs them
/// together).
const CATCHUP_CHUNK_BYTES: usize = 4 * 1024 * 1024;
const CATCHUP_CHUNK_ROWS: usize = 512;

/// Every row the peer (`peer_vv`) is missing, built up front. Used by the
/// connector (small push-back) and by non-streaming drivers (the in-process
/// test); the native listener streams via `Engine::rows_after_wire_page` instead.
pub(crate) fn catchup_rows(
    vault: &dyn SessionVault,
    peer_vv: &std::collections::BTreeMap<String, i64>,
) -> AspResult<Vec<WireRow>> {
    let mut rows = Vec::new();
    for (site, _max) in vault.version_vector()? {
        let peer_seq = peer_vv.get(&site).copied().unwrap_or(-1);
        rows.extend(vault.rows_after_wire(&site, peer_seq)?);
    }
    Ok(rows)
}

/// Drain `rows` into byte-budgeted `Msg::Rows` send steps appended to `out`.
pub(crate) fn push_rows_chunked(out: &mut Vec<Step>, rows: Vec<WireRow>) {
    let mut chunk: Vec<WireRow> = Vec::new();
    let mut chunk_bytes = 0usize;
    for wr in rows {
        let row_bytes: usize = wr.blobs.iter().map(|b| b.bytes.len()).sum();
        if !chunk.is_empty()
            && (chunk.len() >= CATCHUP_CHUNK_ROWS || chunk_bytes + row_bytes > CATCHUP_CHUNK_BYTES)
        {
            out.push(Step::Send(Msg::Rows { rows: std::mem::take(&mut chunk) }));
            chunk_bytes = 0;
        }
        chunk_bytes += row_bytes;
        chunk.push(wr);
    }
    if !chunk.is_empty() {
        out.push(Step::Send(Msg::Rows { rows: chunk }));
    }
}

fn nonce() -> Vec<u8> {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b.to_vec()
}

impl Session {
    pub fn new(
        role: Role,
        vault: &dyn SessionVault,
        advertised_binding: Vec<u8>,
        observed_binding: Option<Vec<u8>>,
        admit: AdmitCtx,
    ) -> Session {
        // A pristine vault (no authored rows) advertises an EMPTY vault id so it
        // adopts the peer's vault on connect — exactly like `clone`. This makes
        // "init then `watch --peer`" with no local content Just Work, and is how a
        // fresh hub/relay adopts the first connector's vault. A populated vault
        // advertises its real id, so two unrelated vaults never silently merge.
        let vault_id = if vault.is_pristine() { String::new() } else { vault.vault_id() };
        Session {
            role,
            our_node: vault.node_id(),
            vault_id,
            our_nonce: nonce(),
            advertised_binding,
            observed_binding,
            admit,
            peer_node: None,
            peer_nonce: None,
            sent_auth: false,
            authed: false,
            sent_vector: false,
        }
    }

    /// The opening frame each side sends.
    pub fn start(&self) -> Vec<Step> {
        vec![Step::Send(Msg::Hello {
            proto: PROTO,
            node_id: self.our_node.to_hex(),
            nonce: self.our_nonce.clone(),
            channel_binding: if self.role == Role::Listener {
                self.advertised_binding.clone()
            } else {
                Vec::new()
            },
            vault_id: self.vault_id.clone(),
            is_listener: self.role == Role::Listener,
        })]
    }

    /// Compute the signed transcript once both Hellos are in.
    fn transcript(&self) -> Option<Vec<u8>> {
        let peer = self.peer_node?;
        let peer_nonce = self.peer_nonce.as_ref()?;
        let (listener_node, connector_node, listener_nonce, connector_nonce) = match self.role {
            Role::Listener => (self.our_node, peer, &self.our_nonce, peer_nonce),
            Role::Connector => (peer, self.our_node, peer_nonce, &self.our_nonce),
        };
        Some(transcript(
            PROTO,
            &listener_node.to_hex(),
            &connector_node.to_hex(),
            listener_nonce,
            connector_nonce,
            &self.advertised_binding,
            &self.vault_id,
        ))
    }

    pub fn on_msg(&mut self, vault: &dyn SessionVault, msg: Msg) -> AspResult<Vec<Step>> {
        match msg {
            Msg::Hello { proto, node_id, nonce, channel_binding, vault_id, is_listener } => {
                if proto != PROTO {
                    return Ok(vec![Step::Closed(format!("proto mismatch: {proto} != {PROTO}"))]);
                }
                // Vault matching, with clone adoption: a fresh node (empty
                // vault_id) adopts the peer's; an empty peer vault_id means the
                // peer is cloning from us. Two populated, differing ids never sync.
                if self.vault_id.is_empty() && !vault_id.is_empty() {
                    self.vault_id = vault_id.clone();
                    vault.adopt_vault_id(&vault_id)?;
                } else if !vault_id.is_empty() && !self.vault_id.is_empty() && vault_id != self.vault_id {
                    return Ok(vec![Step::Closed("different vault".into())]);
                }
                let peer = NodeId::from_hex(&node_id)
                    .ok_or_else(|| AspError::Protocol("bad peer node id".into()))?;
                self.peer_node = Some(peer);
                self.peer_nonce = Some(nonce);
                if is_listener {
                    self.advertised_binding = channel_binding;
                }
                // We now have both Hellos → sign and send Auth (once).
                if !self.sent_auth {
                    if let Some(t) = self.transcript() {
                        self.sent_auth = true;
                        let sig = vault.sign(&t);
                        return Ok(vec![Step::Send(Msg::Auth { sig })]);
                    }
                }
                Ok(vec![])
            }
            Msg::Auth { sig } => {
                let peer = self
                    .peer_node
                    .ok_or_else(|| AspError::Protocol("Auth before Hello".into()))?;
                let t = self
                    .transcript()
                    .ok_or_else(|| AspError::Protocol("no transcript".into()))?;
                if verify_detached(&peer, &t, &sig).is_err() {
                    return Ok(vec![Step::Closed("bad handshake signature".into())]);
                }
                // Role-specific gate.
                match self.role {
                    Role::Listener => {
                        if let Err(e) = vault.admit(&peer, &self.admit) {
                            let reason = format!("admission denied: {e}");
                            return Ok(vec![
                                Step::Send(Msg::Denied { reason: reason.clone() }),
                                Step::Closed(reason),
                            ]);
                        }
                    }
                    Role::Connector => {
                        if let Err(e) = self.check_channel_binding() {
                            return Ok(vec![Step::Closed(e)]);
                        }
                    }
                }
                self.authed = true;
                let mut out = vec![Step::Authenticated(peer)];
                if !self.sent_vector {
                    self.sent_vector = true;
                    out.push(Step::Send(Msg::Vector { vv: vault.version_vector()? }));
                }
                Ok(out)
            }
            Msg::Vector { vv } => {
                if !self.authed {
                    return Ok(vec![Step::Closed("Vector before auth".into())]);
                }
                let mut out = Vec::new();
                if !self.sent_vector {
                    self.sent_vector = true;
                    out.push(Step::Send(Msg::Vector { vv: vault.version_vector()? }));
                }
                // Send the peer what it's missing. The LISTENER (hub / `asp watch`)
                // may hold a large vault, so it STREAMS the catch-up in pages (see
                // Step::CatchUp) — building the whole set up front is slow enough on
                // a small host that the peer idles out before the first byte arrives
                // and closes with 0 rows. The CONNECTOR's own catch-up (its local
                // edits pushed back) is small, and the wasm/browser connector can't
                // stream across its feed() boundary, so it builds inline.
                match self.role {
                    Role::Listener => out.push(Step::CatchUp { peer_vv: vv }),
                    Role::Connector => push_rows_chunked(&mut out, catchup_rows(vault, &vv)?),
                }
                Ok(out)
            }
            Msg::Rows { rows } => {
                if !self.authed {
                    return Ok(vec![Step::Closed("Rows before auth".into())]);
                }
                let integrated = self.integrate_batch(vault, rows)?;
                Ok(vec![Step::Integrated(integrated)])
            }
            Msg::Push { row } => {
                if !self.authed {
                    return Ok(vec![Step::Closed("Push before auth".into())]);
                }
                let integrated = self.integrate_batch(vault, vec![*row])?;
                Ok(vec![Step::Integrated(integrated)])
            }
            Msg::Denied { reason } => Ok(vec![Step::Closed(format!("denied by peer: {reason}"))]),
            Msg::Bye => Ok(vec![Step::Closed("bye".into())]),
            // Peer finished its catch-up. The driver decides what to do (oneshot
            // closes; persistent watch stays connected).
            Msg::Synced => Ok(vec![Step::PeerSynced]),
        }
    }

    fn integrate_batch(&self, vault: &dyn SessionVault, rows: Vec<WireRow>) -> AspResult<Vec<WireRow>> {
        // Fold once for the whole batch (see SessionVault::integrate_many) — the
        // old per-row loop re-folded the log on every row, O(n²) over a catch-up.
        let flags = vault.integrate_many(&rows)?;
        Ok(rows.into_iter().zip(flags).filter_map(|(wr, is_new)| is_new.then_some(wr)).collect())
    }

    /// Connector-side channel-binding enforcement (§Security, advertised binding).
    fn check_channel_binding(&self) -> Result<(), String> {
        if self.advertised_binding.is_empty() {
            return Ok(()); // disabled marker → degraded, trust pinned identity
        }
        match &self.observed_binding {
            None => Ok(()), // advertised but unobservable → degraded (should warn)
            Some(obs) => {
                if obs == &self.advertised_binding {
                    Ok(())
                } else {
                    Err("channel binding mismatch (possible MITM)".into())
                }
            }
        }
    }

    pub fn authed(&self) -> bool {
        self.authed
    }
    pub fn role(&self) -> Role {
        self.role
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::identity::Identity;
    use tempfile::tempdir;

    /// Drive two in-process sessions to convergence over a simulated wire.
    #[test]
    fn handshake_and_catchup_converges() {
        let da = tempdir().unwrap();
        let db = tempdir().unwrap();
        let a = Engine::init(da.path(), Identity::from_seed(&[1; 32])).unwrap();
        let b = Engine::init(db.path(), Identity::from_seed(&[2; 32])).unwrap();
        // Same vault id on both (clone scenario): copy A's vault id into B.
        let vid = a.store.get_config("vault_id").unwrap().unwrap();
        b.store.set_config("vault_id", &vid).unwrap();
        // Pre-authorize each other so admission passes without TOFU races.
        a.authorize(&Identity::from_seed(&[2; 32]).to_ssh_string(), None, true, "test").unwrap();
        b.authorize(&Identity::from_seed(&[1; 32]).to_ssh_string(), None, true, "test").unwrap();

        a.record_write("a.md", b"hello from A\n").unwrap();

        let mkctx = || AdmitCtx { no_tofu: false, auth_key_ok: false, auth_key_configured: false, default_ttl_days: 90, now_unix: 1_700_000_000 };

        let mut la = Session::new(Role::Listener, &a, Vec::new(), None, mkctx());
        let mut cb = Session::new(Role::Connector, &b, Vec::new(), None, mkctx());

        // Message pump: `to_a` feeds the listener (A), `to_b` feeds connector (B).
        let mut to_a: Vec<Msg> = cb.start().into_iter().filter_map(send_of).collect();
        let mut to_b: Vec<Msg> = la.start().into_iter().filter_map(send_of).collect();

        for _ in 0..30 {
            let mut n_to_a = Vec::new();
            let mut n_to_b = Vec::new();
            for m in to_a.drain(..) {
                for s in la.on_msg(&a, m).unwrap() {
                    n_to_b.extend(msgs_of(&a, s));
                }
            }
            for m in to_b.drain(..) {
                for s in cb.on_msg(&b, m).unwrap() {
                    n_to_a.extend(msgs_of(&b, s));
                }
            }
            to_a = n_to_a;
            to_b = n_to_b;
            if to_a.is_empty() && to_b.is_empty() {
                break;
            }
        }
        assert!(la.authed() && cb.authed(), "both authenticated");
        assert_eq!(std::fs::read(db.path().join("a.md")).unwrap(), b"hello from A\n");
    }

    /// A full clone (peer missing everything) must NOT be serialized as one
    /// giant `Msg::Rows` — that buffers the whole history (every blob) at once,
    /// the OOM on a small hub VM. It is split into byte-budgeted frames, each
    /// sent + dropped before the next, and every row ships exactly once.
    #[test]
    fn catchup_rows_are_chunked_by_byte_budget() {
        let da = tempdir().unwrap();
        let a = Engine::init(da.path(), Identity::from_seed(&[7; 32])).unwrap();
        let big = vec![b'x'; 1024 * 1024]; // 1 MiB per file
        for i in 0..6 {
            a.record_write(&format!("f{i}.bin"), &big).unwrap();
        }
        // The catch-up the listener would assemble for a fresh peer.
        let mut rows = Vec::new();
        for (site, _) in a.version_vector().unwrap() {
            rows.extend(a.rows_after_wire(&site, -1).unwrap());
        }
        let total = rows.len();
        assert!(total >= 6, "expected ≥6 rows, got {total}");

        let mut out = Vec::new();
        push_rows_chunked(&mut out, rows);

        assert!(out.len() > 1, "≈6 MiB over a 4 MiB budget must span >1 frame, got {}", out.len());
        let mut shipped = 0;
        for step in &out {
            let Step::Send(Msg::Rows { rows }) = step else { panic!("non-Rows step") };
            let bytes: usize = rows.iter().flat_map(|r| &r.blobs).map(|b| b.bytes.len()).sum();
            assert!(rows.len() == 1 || bytes <= CATCHUP_CHUNK_BYTES, "frame over budget: {bytes} B");
            shipped += rows.len();
        }
        assert_eq!(shipped, total, "every catch-up row ships exactly once");
    }

    /// Expand a step into the frames a real driver would send — including
    /// streaming the listener's `Step::CatchUp` (which net.rs pages over a socket;
    /// the in-process pump expands it whole).
    fn msgs_of(vault: &dyn SessionVault, s: Step) -> Vec<Msg> {
        match s {
            Step::Send(m) => vec![m],
            Step::CatchUp { peer_vv } => {
                let mut out = Vec::new();
                push_rows_chunked(&mut out, catchup_rows(vault, &peer_vv).unwrap());
                out.into_iter().filter_map(send_of).collect()
            }
            _ => vec![],
        }
    }

    fn send_of(s: Step) -> Option<Msg> {
        match s {
            Step::Send(m) => Some(m),
            _ => None,
        }
    }

    /// §Security advertised channel binding: a connector that *observes* a cert
    /// fingerprint different from the one the listener *advertised* (a live MITM /
    /// cert substitution) MUST abort with a distinct channel-binding error.
    #[test]
    fn channel_binding_mismatch_aborts_connector() {
        let da = tempdir().unwrap();
        let db = tempdir().unwrap();
        let a = Engine::init(da.path(), Identity::from_seed(&[1; 32])).unwrap();
        let b = Engine::init(db.path(), Identity::from_seed(&[2; 32])).unwrap();
        let vid = a.store.get_config("vault_id").unwrap().unwrap();
        b.store.set_config("vault_id", &vid).unwrap();
        a.authorize(&Identity::from_seed(&[2; 32]).to_ssh_string(), None, true, "t").unwrap();
        b.authorize(&Identity::from_seed(&[1; 32]).to_ssh_string(), None, true, "t").unwrap();
        let mkctx = || AdmitCtx { no_tofu: false, auth_key_ok: false, auth_key_configured: false, default_ttl_days: 90, now_unix: 1_700_000_000 };

        // Listener advertises fingerprint [1,2,3]; the connector OBSERVES [9,9,9]
        // (a MITM re-terminated TLS with a different cert).
        let mut la = Session::new(Role::Listener, &a, vec![1, 2, 3], None, mkctx());
        let mut cb = Session::new(Role::Connector, &b, Vec::new(), Some(vec![9, 9, 9]), mkctx());

        let mut to_a: Vec<Msg> = cb.start().into_iter().filter_map(send_of).collect();
        let mut to_b: Vec<Msg> = la.start().into_iter().filter_map(send_of).collect();
        let mut aborted = false;
        for _ in 0..30 {
            let mut n_to_a = Vec::new();
            let mut n_to_b = Vec::new();
            for m in to_a.drain(..) {
                for s in la.on_msg(&a, m).unwrap() {
                    if let Some(m) = send_of(s) {
                        n_to_b.push(m);
                    }
                }
            }
            for m in to_b.drain(..) {
                for s in cb.on_msg(&b, m).unwrap() {
                    // The connector rejects the listener with a *distinct* channel-
                    // binding error (the live MITM / cert-substitution defense).
                    if let Step::Closed(reason) = &s {
                        if reason.contains("channel binding") {
                            aborted = true;
                        }
                    }
                    if let Some(m) = send_of(s) {
                        n_to_a.push(m);
                    }
                }
            }
            to_a = n_to_a;
            to_b = n_to_b;
            if to_a.is_empty() && to_b.is_empty() {
                break;
            }
        }
        assert!(aborted, "connector must abort on a channel-binding mismatch");
        assert!(!cb.authed(), "and must not authenticate");
    }
}
