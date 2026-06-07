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

use crate::engine::{AdmitCtx, Engine};
use crate::error::{AspError, AspResult};
use crate::identity::verify_detached;
use crate::order::NodeId;
use crate::wire::{transcript, Msg, WireRow, PROTO};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Connector,
    Listener,
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

fn nonce() -> Vec<u8> {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b.to_vec()
}

impl Session {
    pub fn new(
        role: Role,
        engine: &Engine,
        advertised_binding: Vec<u8>,
        observed_binding: Option<Vec<u8>>,
        admit: AdmitCtx,
    ) -> Session {
        // Read the current vault id from config so a hub that *adopted* a vault
        // advertises it to later peers (and a fresh clone advertises empty).
        let vault_id = engine.store.get_config("vault_id").ok().flatten().unwrap_or_default();
        Session {
            role,
            our_node: engine.identity.node_id(),
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

    pub fn on_msg(&mut self, engine: &Engine, msg: Msg) -> AspResult<Vec<Step>> {
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
                    engine.store.set_config("vault_id", &vault_id)?;
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
                        let sig = engine.identity.sign(&t);
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
                        if let Err(e) = engine.admit(&peer, &self.admit) {
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
                    out.push(Step::Send(Msg::Vector { vv: engine.store.version_vector()? }));
                }
                Ok(out)
            }
            Msg::Vector { vv } => {
                if !self.authed {
                    return Ok(vec![Step::Closed("Vector before auth".into())]);
                }
                // Send exactly what the peer is missing.
                let ours = engine.store.version_vector()?;
                let mut rows = Vec::new();
                for (site, _max) in ours {
                    let peer_seq = vv.get(&site).copied().unwrap_or(-1);
                    for r in engine.store.rows_after(&site, peer_seq)? {
                        rows.push(engine.wire(r)?);
                    }
                }
                let mut out = Vec::new();
                if !self.sent_vector {
                    self.sent_vector = true;
                    out.push(Step::Send(Msg::Vector { vv: engine.store.version_vector()? }));
                }
                if !rows.is_empty() {
                    out.push(Step::Send(Msg::Rows { rows }));
                }
                Ok(out)
            }
            Msg::Rows { rows } => {
                if !self.authed {
                    return Ok(vec![Step::Closed("Rows before auth".into())]);
                }
                let integrated = self.integrate_batch(engine, rows)?;
                Ok(vec![Step::Integrated(integrated)])
            }
            Msg::Push { row } => {
                if !self.authed {
                    return Ok(vec![Step::Closed("Push before auth".into())]);
                }
                let integrated = self.integrate_batch(engine, vec![*row])?;
                Ok(vec![Step::Integrated(integrated)])
            }
            Msg::Denied { reason } => Ok(vec![Step::Closed(format!("denied by peer: {reason}"))]),
            Msg::Bye => Ok(vec![Step::Closed("bye".into())]),
        }
    }

    fn integrate_batch(&self, engine: &Engine, rows: Vec<WireRow>) -> AspResult<Vec<WireRow>> {
        let mut added = Vec::new();
        for wr in rows {
            if engine.integrate(&wr)? {
                added.push(wr);
            }
        }
        Ok(added)
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
                    if let Some(m) = send_of(s) {
                        n_to_b.push(m);
                    }
                }
            }
            for m in to_b.drain(..) {
                for s in cb.on_msg(&b, m).unwrap() {
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
        assert!(la.authed() && cb.authed(), "both authenticated");
        assert_eq!(std::fs::read(db.path().join("a.md")).unwrap(), b"hello from A\n");
    }

    fn send_of(s: Step) -> Option<Msg> {
        match s {
            Step::Send(m) => Some(m),
            _ => None,
        }
    }
}
