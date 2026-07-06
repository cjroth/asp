//! The sans-IO replication `Session` (§Sync protocol, §Security). One protocol
//! state machine — handshake, anti-entropy (version-vector catch-up), and
//! integrate — driven identically by the native socket driver and any future
//! wasm/SDK node. It consumes inbound [`Msg`] bytes and emits outbound [`Step`]s;
//! it owns no sockets, fs, or clock. Local I/O (the store) goes through `Engine`.
//!
//! Handshake: the transport (iroh's QUIC key handshake) authenticates both node
//! keys before any frame. Each side sends a `Hello` binding proto/vault/identity
//! (+ the optional auth-key); on the peer's `Hello` the receiver cross-checks
//! `Hello.node_id` against the transport-verified key, the **listener** applies
//! `authorized_keys` admission, and both are authenticated — no nonce/signature
//! round-trip. Then they exchange version vectors and send exactly what's missing.

use crate::authkeys::AdmitCtx;
use crate::error::{AspError, AspResult};
use crate::order::NodeId;
use crate::wire::{Msg, WireRow, PROTO};
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
    /// The peer's `NodeId` as authenticated by the transport — iroh's QUIC key
    /// handshake proved the remote holds the private key for it *before* any ASP
    /// frame. The peer's `Hello.node_id` MUST equal this; a mismatch can only be a
    /// bug (the transport key can't be spoofed) and aborts.
    verified_peer: NodeId,
    admit: AdmitCtx,
    /// Connector: the enrollment secret to present in `Hello`. Listener: unused.
    present_auth_key: Option<String>,
    /// Listener: the configured enrollment secrets to validate a presented key
    /// against. Connector: unused.
    configured_auth_keys: Vec<String>,
    peer_node: Option<NodeId>,
    authed: bool,
    sent_vector: bool,
}

/// Per-frame catch-up budget. A `Msg::Rows` frame accumulates rows until it
/// crosses ~4 MiB of blob bytes (or 512 rows), then flushes — so serving a
/// full clone never holds more than one budget's worth of the history in a
/// serialized frame at a time. A single row whose blobs exceed the budget
/// still ships alone (never split a row from its blobs — the fold needs them
/// together).
// Smaller frames (1 MiB / 256 rows) than a bulk transfer would want, deliberately:
// the browser can only reach a listener over a RELAY (no direct UDP), where one
// large frame streams slowly and stalls the "Receiving notes…" bar. Smaller frames
// keep bytes flowing, advance progress smoothly, and stay under the connector's
// per-frame idle timeout on a big clone.
const CATCHUP_CHUNK_BYTES: usize = 1024 * 1024;
const CATCHUP_CHUNK_ROWS: usize = 256;

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

impl Session {
    /// Build a session for a connection whose remote key the transport has
    /// already authenticated (`verified_peer`). `auth_keys` is the AUTH_KEY
    /// enrollment set: a connector presents `auth_keys.first()` in its `Hello`; a
    /// listener validates a presented key against the whole set.
    pub fn new(
        role: Role,
        vault: &dyn SessionVault,
        admit: AdmitCtx,
        verified_peer: NodeId,
        auth_keys: Vec<String>,
    ) -> Session {
        let (present_auth_key, configured_auth_keys) = match role {
            Role::Connector => (auth_keys.into_iter().next(), Vec::new()),
            Role::Listener => (None, auth_keys),
        };
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
            verified_peer,
            admit,
            present_auth_key,
            configured_auth_keys,
            peer_node: None,
            authed: false,
            sent_vector: false,
        }
    }

    /// The opening frame each side sends.
    pub fn start(&self) -> Vec<Step> {
        vec![Step::Send(Msg::Hello {
            proto: PROTO,
            node_id: self.our_node.to_hex(),
            vault_id: self.vault_id.clone(),
            is_listener: self.role == Role::Listener,
            auth_key: self.present_auth_key.clone(),
        })]
    }

    pub fn on_msg(&mut self, vault: &dyn SessionVault, msg: Msg) -> AspResult<Vec<Step>> {
        match msg {
            Msg::Hello { proto, node_id, vault_id, is_listener: _, auth_key } => {
                if proto != PROTO {
                    // Both numbers, plus a direction + action hint. A proto-3 peer
                    // meeting a proto-4 (git-bridge) node lands here and gets a clear
                    // "upgrade" message rather than corrupt rows (git-bridge §6.2).
                    let action = if proto < PROTO { "upgrade this peer" } else { "upgrade the other peer" };
                    return Ok(vec![Step::Closed(format!(
                        "proto mismatch: peer speaks proto {proto}, we speak {PROTO} — {action} to the same version"
                    ))]);
                }
                // Listener: validate a presented AUTH_KEY against the configured
                // enrollment set (§Security). A mismatch denies loudly with no
                // fall-through; a match flips `auth_key_ok` so admission enrolls
                // this peer; an absent key proceeds to normal admission (an
                // already-enrolled peer needs no secret).
                if self.role == Role::Listener && !self.configured_auth_keys.is_empty() {
                    if let Some(presented) = &auth_key {
                        if self.configured_auth_keys.iter().any(|k| k == presented) {
                            self.admit.auth_key_ok = true;
                        } else {
                            let reason = "invalid auth key".to_string();
                            return Ok(vec![
                                Step::Send(Msg::Denied { reason: reason.clone() }),
                                Step::Closed(reason),
                            ]);
                        }
                    }
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
                // Cross-check the claimed identity against the key the transport
                // already authenticated (iroh's QUIC handshake). This can only fail
                // on a bug — the remote can't present a `NodeId` it didn't prove —
                // so it's a hard abort, not a trust decision.
                if peer != self.verified_peer {
                    return Ok(vec![Step::Closed(
                        "peer identity mismatch (Hello node id != transport-verified key)".into(),
                    )]);
                }
                self.peer_node = Some(peer);
                // iroh authenticated the connection, so there is no signature
                // round-trip: authenticate + admit on the `Hello` directly. The
                // listener applies the `authorized_keys` gate; the connector simply
                // records the verified listener (which `clone` pins).
                if self.role == Role::Listener {
                    if let Err(e) = vault.admit(&peer, &self.admit) {
                        let reason = format!("admission denied: {e}");
                        return Ok(vec![
                            Step::Send(Msg::Denied { reason: reason.clone() }),
                            Step::Closed(reason),
                        ]);
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

    /// A node id from a deterministic seed — the transport-verified peer key the
    /// driver would supply (here the in-process pump knows both sides' keys).
    fn nid(n: u8) -> NodeId {
        Identity::from_seed(&[n; 32]).node_id()
    }

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

        // Each side's `verified_peer` is the *other* node's key (what iroh would
        // have authenticated): A's listener verifies B, B's connector verifies A.
        let mut la = Session::new(Role::Listener, &a, mkctx(), nid(2), Vec::new());
        let mut cb = Session::new(Role::Connector, &b, mkctx(), nid(1), Vec::new());

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

    fn ctx() -> AdmitCtx {
        AdmitCtx { no_tofu: false, auth_key_ok: false, auth_key_configured: false, default_ttl_days: 90, now_unix: 1_700_000_000 }
    }

    // Drive the SAME pump over two MemEngines — the wasm/browser SessionVault impl
    // (rows_after_wire / integrate / admit / adopt_vault_id), which the disk-engine
    // tests never exercise. Each session is told the other's key as `verified_peer`.
    fn pump(a: &dyn SessionVault, la: &mut Session, b: &dyn SessionVault, cb: &mut Session) {
        let mut to_a: Vec<Msg> = cb.start().into_iter().filter_map(send_of).collect();
        let mut to_b: Vec<Msg> = la.start().into_iter().filter_map(send_of).collect();
        for _ in 0..40 {
            let (mut na, mut nb) = (Vec::new(), Vec::new());
            for m in to_a.drain(..) {
                for s in la.on_msg(a, m).unwrap() {
                    nb.extend(msgs_of(a, s));
                }
            }
            for m in to_b.drain(..) {
                for s in cb.on_msg(b, m).unwrap() {
                    na.extend(msgs_of(b, s));
                }
            }
            to_a = na;
            to_b = nb;
            if to_a.is_empty() && to_b.is_empty() {
                break;
            }
        }
    }

    #[test]
    fn memengine_sessions_handshake_and_converge() {
        use crate::MemEngine;
        let a = MemEngine::create(Identity::from_seed(&[1; 32]), "v");
        let b = MemEngine::create(Identity::from_seed(&[2; 32]), "v");
        a.authorize(&Identity::from_seed(&[2; 32]).to_ssh_string(), None, true, "test").unwrap();
        b.authorize(&Identity::from_seed(&[1; 32]).to_ssh_string(), None, true, "test").unwrap();
        a.record_write("m.md", b"from mem A\n").unwrap();

        let mut la = Session::new(Role::Listener, &a, ctx(), nid(2), Vec::new());
        let mut cb = Session::new(Role::Connector, &b, ctx(), nid(1), Vec::new());
        pump(&a, &mut la, &b, &mut cb);

        assert!(la.authed() && cb.authed(), "both mem sessions authenticate");
        assert_eq!(b.files_map().unwrap().get("m.md").map(|v| v.as_slice()), Some(&b"from mem A\n"[..]), "B pulled A's file");
    }

    #[test]
    fn tofu_admits_an_unknown_peer_but_no_tofu_denies_it() {
        use crate::MemEngine;
        let mk = || {
            let a = MemEngine::create(Identity::from_seed(&[1; 32]), "v");
            let b = MemEngine::create(Identity::from_seed(&[2; 32]), "v");
            a.record_write("m.md", b"hi\n").unwrap(); // NO authorize() — rely on admission policy
            (a, b)
        };

        // TOFU (no_tofu=false): an unknown peer is admitted on first use → converge.
        let (a, b) = mk();
        let mut la = Session::new(Role::Listener, &a, ctx(), nid(2), Vec::new());
        let mut cb = Session::new(Role::Connector, &b, ctx(), nid(1), Vec::new());
        pump(&a, &mut la, &b, &mut cb);
        assert!(la.authed() && cb.authed(), "TOFU admits an unknown peer");
        assert!(b.files_map().unwrap().contains_key("m.md"));

        // no_tofu: an unknown, un-authorized peer is denied → no data crosses.
        let deny = || AdmitCtx { no_tofu: true, ..ctx() };
        let (a, b) = mk();
        let mut la = Session::new(Role::Listener, &a, deny(), nid(2), Vec::new());
        let mut cb = Session::new(Role::Connector, &b, deny(), nid(1), Vec::new());
        pump(&a, &mut la, &b, &mut cb);
        assert!(!(la.authed() && cb.authed()), "no_tofu denies an unknown peer");
        assert!(!b.files_map().unwrap().contains_key("m.md"), "no data leaks to a denied peer");
    }

    #[test]
    fn proto_mismatch_closes_the_session() {
        let d = tempdir().unwrap();
        let e = Engine::init(d.path(), Identity::from_seed(&[1; 32])).unwrap();
        let mut s = Session::new(Role::Listener, &e, ctx(), nid(2), Vec::new());
        let hello = Msg::Hello {
            proto: crate::wire::PROTO + 99,
            node_id: nid(2).to_hex(),
            vault_id: "whatever".into(),
            is_listener: false,
            auth_key: None,
        };
        let steps = s.on_msg(&e, hello).unwrap();
        assert!(steps.iter().any(|st| matches!(st, Step::Closed(m) if m.contains("proto"))), "proto mismatch must close");
    }

    #[test]
    fn hello_node_id_must_match_transport_verified_key() {
        // The transport authenticated peer [3], but the Hello claims to be [2] —
        // an impossible-without-a-bug claim that must abort (iroh already proved
        // the remote holds [3]'s key).
        let d = tempdir().unwrap();
        let e = Engine::init(d.path(), Identity::from_seed(&[1; 32])).unwrap();
        let mut s = Session::new(Role::Listener, &e, ctx(), nid(3), Vec::new());
        let hello = Msg::Hello {
            proto: crate::wire::PROTO,
            node_id: nid(2).to_hex(),
            vault_id: String::new(),
            is_listener: false,
            auth_key: None,
        };
        let steps = s.on_msg(&e, hello).unwrap();
        assert!(
            steps.iter().any(|st| matches!(st, Step::Closed(m) if m.contains("identity mismatch"))),
            "a Hello node id that doesn't match the transport-verified key must abort",
        );
        assert!(!s.authed(), "and must not authenticate");
    }

    #[test]
    fn differing_populated_vaults_refuse_to_sync() {
        let (da, db) = (tempdir().unwrap(), tempdir().unwrap());
        let a = Engine::init(da.path(), Identity::from_seed(&[1; 32])).unwrap();
        let b = Engine::init(db.path(), Identity::from_seed(&[2; 32])).unwrap();
        // Both populated → each advertises its OWN (different) vault id.
        a.record_write("a.md", b"secret A\n").unwrap();
        b.record_write("b.md", b"secret B\n").unwrap();
        let la = Session::new(Role::Listener, &a, ctx(), nid(2), Vec::new());
        let mut cb = Session::new(Role::Connector, &b, ctx(), nid(1), Vec::new());
        let a_hello = la.start().into_iter().filter_map(send_of).next().expect("A hello");
        let steps = cb.on_msg(&b, a_hello).unwrap();
        assert!(
            steps.iter().any(|st| matches!(st, Step::Closed(m) if m.contains("different vault"))),
            "two populated vaults with different ids must refuse to sync",
        );
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
        let n = (CATCHUP_CHUNK_BYTES / (1024 * 1024)) + 4; // a few MiB over one frame's budget
        for i in 0..n {
            a.record_write(&format!("f{i}.bin"), &big).unwrap();
        }
        // The catch-up the listener would assemble for a fresh peer.
        let mut rows = Vec::new();
        for (site, _) in a.version_vector().unwrap() {
            rows.extend(a.rows_after_wire(&site, -1).unwrap());
        }
        let total = rows.len();
        assert!(total >= n, "expected ≥{n} rows, got {total}");

        let mut out = Vec::new();
        push_rows_chunked(&mut out, rows);

        assert!(out.len() > 1, "data over one frame's byte budget must span >1 frame, got {}", out.len());
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
}
