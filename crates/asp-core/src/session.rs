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

use crate::authkeys::{AdmitCtx, PeerPolicy};
use crate::error::{AspError, AspResult};
use crate::log::{Kind, LogRow};
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
    /// Admit `peer` and return its retained replication grant ([`PeerPolicy`],
    /// scoped-sync §3.1) — `Err` denies the connection. The listener threads the
    /// returned policy onto the `Session` (catch-up filter + read-only reject).
    fn admit(&self, peer: &NodeId, ctx: &AdmitCtx) -> AspResult<PeerPolicy>;
    /// The set of `file_id`s that EVER resolved under `allowed` (scoped-sync §3.3
    /// SYNC membership). The catch-up / fanout send-filter ships exactly these file
    /// rows (plus all non-file rows). Includes tombstoned files, so an in-scope
    /// Delete still ships. Only called when a peer carries a scope grant. The
    /// default is **fail-closed** (empty = nothing in scope); the real engines
    /// override it — a vault that forgets to must never leak the whole log.
    fn scope_members(&self, _allowed: &[String]) -> AspResult<std::collections::HashSet<String>> {
        Ok(std::collections::HashSet::new())
    }
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
    CatchUp { peer_vv: std::collections::BTreeMap<String, i64>, policy: PeerPolicy },
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
    /// The admitted peer's replication grant (scoped-sync §3.1), set on the
    /// listener when admission passes. Governs the catch-up send-filter (A —
    /// `allowed_paths`) and the read-only push reject (B — `read_only`). Default
    /// (full / read-write) on the connector and before admission — unchanged behavior.
    policy: PeerPolicy,
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

/// A row admitted to a scoped peer's feed (scoped-sync §3.2/§3.4): every non-file
/// row ships wholesale (Branch/Tag/Merge/Git* — few, load-bearing, path-overloaded),
/// and a file row ships iff its `file_id` is a scope member (its whole chain is
/// present, so the fold never orphans, §3.3). `None` members = no scoping (full).
pub(crate) fn scope_admits(wr: &WireRow, members: Option<&std::collections::HashSet<String>>) -> bool {
    match members {
        None => true,
        Some(m) => !is_file_mutation(&wr.row) || m.contains(&wr.row.file_id),
    }
}

/// Every row the peer (`peer_vv`) is missing, built up front, **filtered to the
/// peer's `policy`** (scoped-sync §3.2). Used by the connector (small push-back)
/// and by non-streaming drivers (the in-process test); the native listener streams
/// via `Engine::rows_after_wire_page` + the same filter in `stream_catchup` instead.
pub(crate) fn catchup_rows(
    vault: &dyn SessionVault,
    peer_vv: &std::collections::BTreeMap<String, i64>,
    policy: &PeerPolicy,
) -> AspResult<Vec<WireRow>> {
    let members = match &policy.allowed_paths {
        Some(allowed) => Some(vault.scope_members(allowed)?),
        None => None,
    };
    let mut rows = Vec::new();
    for (site, _max) in vault.version_vector()? {
        let peer_seq = peer_vv.get(&site).copied().unwrap_or(-1);
        for wr in vault.rows_after_wire(&site, peer_seq)? {
            if scope_admits(&wr, members.as_ref()) {
                rows.push(wr);
            }
        }
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

/// A row that mutates file CONTENT — the kinds a read-only peer (B) may not push
/// (scoped-sync §4.2). Metadata rows (`Branch`/`Tag`/`Merge`/`GitCommit`/
/// `GitIngest`/`GitPlan`) are not file mutations and are never gated by read-only.
pub(crate) fn is_file_mutation(row: &LogRow) -> bool {
    matches!(
        row.kind,
        Kind::Create | Kind::Edit | Kind::Rename | Kind::Delete | Kind::Reclass
    )
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
            policy: PeerPolicy::default(),
        }
    }

    /// The admitted peer's replication grant (scoped-sync §3.1) — full/read-write
    /// until a listener admits a scoped peer. Exposed for the drivers/tests.
    pub fn policy(&self) -> &PeerPolicy {
        &self.policy
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
                    match vault.admit(&peer, &self.admit) {
                        // Retain the admitted grant (scoped-sync §3.1): the catch-up
                        // filter (A) and read-only reject (B) read `self.policy`.
                        Ok(policy) => self.policy = policy,
                        Err(e) => {
                            let reason = format!("admission denied: {e}");
                            return Ok(vec![
                                Step::Send(Msg::Denied { reason: reason.clone() }),
                                Step::Closed(reason),
                            ]);
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
                    // The listener streams the catch-up in pages (Step::CatchUp),
                    // carrying the admitted grant so the driver applies the scope
                    // send-filter (A, scoped-sync §3.2). The connector's own push-back
                    // is unscoped (its policy is default) and built inline.
                    Role::Listener => out.push(Step::CatchUp { peer_vv: vv, policy: self.policy.clone() }),
                    Role::Connector => push_rows_chunked(&mut out, catchup_rows(vault, &vv, &self.policy)?),
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
        // Read-only enforcement (B — scoped-sync §4). A peer the listener admitted as
        // `read_only` may PULL but not push file mutations: drop its inbound
        // Create/Edit/Rename/Delete/Reclass rows here, at the single integrator,
        // BEFORE they enter the log — so they neither fold locally nor fan out to
        // other peers (the leak path a fold-time filter would miss, §4.1 regime 1).
        // This lives in the sans-IO Session, so the native Engine and the wasm
        // MemEngine enforce it identically (mandatory — else the browser is the laxer
        // node, §10 risk 5). Metadata rows (Branch/Tag/Merge/Git*) are not file
        // mutations and still integrate.
        //
        // We DROP silently rather than replying `Msg::Denied`: our own `Msg::Denied`
        // handler closes the connection (below), so denying a live push would tear
        // down the read-only peer's ongoing read pull. The source simply refuses to
        // integrate the peer's writes — the Trust-mode topological boundary (§4.4).
        let rows: Vec<WireRow> = if self.policy.read_only {
            rows.into_iter().filter(|wr| !is_file_mutation(&wr.row)).collect()
        } else {
            rows
        };
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

    /// B — read-only enforcement at the single integrator (scoped-sync §4, §9). The
    /// listener admits the connector as `read_only`; the connector's authored file
    /// row must NEVER enter the listener's log, while the listener's rows must still
    /// reach the connector (one-way sync from a hub), and the connection stays up.
    #[test]
    fn read_only_peer_cannot_push_but_still_pulls_memengine() {
        use crate::MemEngine;
        let a = MemEngine::create(Identity::from_seed(&[1; 32]), "v"); // listener / source
        let b = MemEngine::create(Identity::from_seed(&[2; 32]), "v"); // connector / read-only replica
        a.authorize_with_policy(&Identity::from_seed(&[2; 32]).to_ssh_string(), None, true, "test", None, true)
            .unwrap();
        a.record_write("from_source.md", b"source note\n").unwrap();
        b.record_write("from_readonly.md", b"local edit\n").unwrap();

        let mut la = Session::new(Role::Listener, &a, ctx(), nid(2), Vec::new());
        let mut cb = Session::new(Role::Connector, &b, ctx(), nid(1), Vec::new());
        pump(&a, &mut la, &b, &mut cb);

        assert!(la.authed() && cb.authed(), "connection stays up — no Denied/close");
        assert!(la.policy().read_only, "listener retained the read-only grant");
        assert!(b.files_map().unwrap().contains_key("from_source.md"), "read-only peer still PULLS the source's rows");
        assert!(
            !a.files_map().unwrap().contains_key("from_readonly.md"),
            "the read-only peer's authored row must never enter the source",
        );
    }

    /// Native-Engine mirror of the read-only enforcement (parity — the reject lives
    /// in the sans-IO Session, so both surfaces must behave identically, §10 risk 5).
    #[test]
    fn read_only_peer_cannot_push_but_still_pulls_native() {
        let da = tempdir().unwrap();
        let db = tempdir().unwrap();
        let a = Engine::init(da.path(), Identity::from_seed(&[1; 32])).unwrap();
        let b = Engine::init(db.path(), Identity::from_seed(&[2; 32])).unwrap();
        let vid = a.store.get_config("vault_id").unwrap().unwrap();
        b.store.set_config("vault_id", &vid).unwrap();
        a.authorize_with_policy(&Identity::from_seed(&[2; 32]).to_ssh_string(), None, true, "test", None, true)
            .unwrap();
        a.record_write("from_source.md", b"source note\n").unwrap();
        b.record_write("from_readonly.md", b"local edit\n").unwrap();

        let mut la = Session::new(Role::Listener, &a, ctx(), nid(2), Vec::new());
        let mut cb = Session::new(Role::Connector, &b, ctx(), nid(1), Vec::new());
        pump(&a, &mut la, &b, &mut cb);

        assert!(la.authed() && cb.authed(), "connection stays up");
        assert!(std::fs::read(db.path().join("from_source.md")).is_ok(), "read-only peer pulls the source's file");
        assert!(
            a.store.file_id_for_path("from_readonly.md").unwrap().is_none(),
            "the read-only peer's push never materializes at the source",
        );
        // And it is not merely hidden — the row itself never entered the source's log.
        let a_has_readonly_row = a.store.all_rows().unwrap().iter().any(|r| r.site_id == b.site_id());
        assert!(!a_has_readonly_row, "no row authored by the read-only peer reached the source's log");
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

    // ---------------- Feature A: partial subdir sync (scoped-sync §3, §9) ----------------

    /// Drive a full catch-up from `source` (listener) to a fresh `replica`
    /// (connector) that the source admitted with a `--subdir` grant of `allowed`
    /// (empty = full). Exercises the real `catchup_rows` scope filter via the
    /// in-process pump (Step::CatchUp → msgs_of). Re-runnable (authorize is
    /// idempotent) to simulate a reconnect.
    fn scoped_pump(source: &crate::MemEngine, replica: &crate::MemEngine, allowed: &[&str]) {
        let paths: Vec<String> = allowed.iter().map(|s| s.to_string()).collect();
        let grant = if paths.is_empty() { None } else { Some(paths) };
        source
            .authorize_with_policy(&Identity::from_seed(&[2; 32]).to_ssh_string(), None, true, "test", grant, false)
            .unwrap();
        let mut ls = Session::new(Role::Listener, source, ctx(), nid(2), Vec::new());
        let mut cr = Session::new(Role::Connector, replica, ctx(), nid(1), Vec::new());
        pump(source, &mut ls, replica, &mut cr);
        assert!(ls.authed() && cr.authed(), "scoped session authenticated");
    }

    /// A — ground-truth invariant (load-bearing, §9). Over a deterministic LCG
    /// history of create/edit/within-scope-rename/delete across in-scope (`work/`)
    /// and out-of-scope (`personal/`) files, a subdir-scoped replica's fold equals
    /// the source's fold restricted to the in-scope files — byte-for-byte, nothing
    /// out of scope. (Cross-boundary renames are covered by the dedicated monotonic
    /// test below, where membership ≠ current path.)
    #[test]
    fn scoped_replica_fold_equals_source_restricted_to_scope() {
        use crate::MemEngine;
        let source = MemEngine::create(Identity::from_seed(&[1; 32]), "v");
        let mut lcg: u64 = 0x1234_5678_9abc_def0;
        let mut rnd = || {
            lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (lcg >> 33) as u32
        };
        for i in 0..40u32 {
            let dir = if rnd() % 2 == 0 { "work" } else { "personal" };
            source.record_write(&format!("{dir}/f{i:02}.md"), format!("v0/{i}\n").as_bytes()).unwrap();
        }
        let mut ren = 0u32;
        for _ in 0..150 {
            let files: Vec<String> = source.files_map().unwrap().keys().cloned().collect();
            if files.is_empty() {
                break;
            }
            let f = files[(rnd() as usize) % files.len()].clone();
            let top = f.split('/').next().unwrap().to_string();
            match rnd() % 4 {
                0 | 1 => {
                    source.record_write(&f, format!("edit-{}\n", rnd()).as_bytes()).unwrap();
                }
                2 => {
                    // within-scope rename (unique target keeps the top dir, no collision)
                    ren += 1;
                    source.record_rename(&f, &format!("{top}/r{ren}.md")).unwrap();
                }
                _ => {
                    source.record_remove(&f).unwrap();
                }
            }
        }

        let replica = MemEngine::create(Identity::from_seed(&[2; 32]), "v");
        scoped_pump(&source, &replica, &["work"]);

        let src_scoped: std::collections::BTreeMap<String, Vec<u8>> =
            source.files_map().unwrap().into_iter().filter(|(p, _)| p.starts_with("work/")).collect();
        assert!(!src_scoped.is_empty(), "fixture must produce in-scope files");
        assert_eq!(replica.files_map().unwrap(), src_scoped, "scoped fold == source restricted to scope");
    }

    /// A — rename across the scope boundary is monotonic (§3.3). SYNC membership is
    /// "ever resolved under X", so a file that entered X ships its WHOLE chain
    /// (incl. the out-of-scope Create) and a file that LEFT X still ships (the
    /// replica learns it left — no stale ghost). A file that never touched X never
    /// ships.
    #[test]
    fn scoped_rename_across_boundary_is_monotonic() {
        use crate::MemEngine;
        let source = MemEngine::create(Identity::from_seed(&[1; 32]), "v");
        source.record_write("personal/a.md", b"A body\n").unwrap();
        source.record_rename("personal/a.md", "work/a.md").unwrap(); // INTO scope
        source.record_write("work/b.md", b"B body\n").unwrap();
        source.record_rename("work/b.md", "personal/b.md").unwrap(); // OUT of scope
        source.record_write("personal/c.md", b"C body\n").unwrap(); // never in scope

        let replica = MemEngine::create(Identity::from_seed(&[2; 32]), "v");
        scoped_pump(&source, &replica, &["work"]);

        let files = replica.files_map().unwrap();
        assert_eq!(files.get("work/a.md").map(|v| v.as_slice()), Some(&b"A body\n"[..]), "rename-into-scope ships the whole chain");
        assert!(!files.contains_key("work/b.md"), "no stale ghost at the old in-scope path");
        assert_eq!(files.get("personal/b.md").map(|v| v.as_slice()), Some(&b"B body\n"[..]), "renamed-out file folds at its new path (monotonic membership)");
        assert!(!files.contains_key("personal/c.md"), "a file that never touched the scope never ships");
    }

    /// A — dense-seq regression (§3.2). The filter drops a mid-sequence slice, so the
    /// scoped replica holds a gapped seq set ({0,2,4} with 1,3 out of scope). It must
    /// converge within scope AND, on reconnect, never re-request the dropped seqs
    /// (its `MAX(seq)` watermark advertises 4; nothing below is requestable).
    #[test]
    fn scoped_dense_seq_holes_converge_and_stay_converged() {
        use crate::MemEngine;
        let source = MemEngine::create(Identity::from_seed(&[1; 32]), "v");
        source.record_write("work/0.md", b"0\n").unwrap(); // seq 0
        source.record_write("personal/1.md", b"1\n").unwrap(); // seq 1 (out)
        source.record_write("work/2.md", b"2\n").unwrap(); // seq 2
        source.record_write("personal/3.md", b"3\n").unwrap(); // seq 3 (out)
        source.record_write("work/4.md", b"4\n").unwrap(); // seq 4

        let replica = MemEngine::create(Identity::from_seed(&[2; 32]), "v");
        scoped_pump(&source, &replica, &["work"]);
        assert_eq!(
            replica.files_map().unwrap().keys().cloned().collect::<Vec<_>>(),
            vec!["work/0.md".to_string(), "work/2.md".to_string(), "work/4.md".to_string()],
        );
        let before = replica.row_count();
        scoped_pump(&source, &replica, &["work"]); // reconnect
        assert_eq!(replica.row_count(), before, "no re-request loop for the dropped out-of-scope seqs");
    }

    /// A — tombstone membership (§3.3). An in-scope Delete must ship (membership is
    /// computed over the log, not the `deleted=0` live view), so the replica shows no
    /// ghost of a deleted in-scope file.
    #[test]
    fn scoped_in_scope_delete_ships_no_ghost() {
        use crate::MemEngine;
        let source = MemEngine::create(Identity::from_seed(&[1; 32]), "v");
        source.record_write("work/keep.md", b"keep\n").unwrap();
        source.record_write("work/gone.md", b"gone\n").unwrap();
        source.record_remove("work/gone.md").unwrap();

        let replica = MemEngine::create(Identity::from_seed(&[2; 32]), "v");
        scoped_pump(&source, &replica, &["work"]);
        let files = replica.files_map().unwrap();
        assert!(files.contains_key("work/keep.md"));
        assert!(!files.contains_key("work/gone.md"), "in-scope delete ships → no ghost on the replica");
    }

    /// A — N-vs-2N scaling (§9). A scoped clone of N in-scope + N out-of-scope files
    /// transfers ~N rows/blobs, not ~2N.
    #[test]
    fn scoped_clone_transfers_only_in_scope_rows() {
        use crate::MemEngine;
        const N: usize = 50;
        let source = MemEngine::create(Identity::from_seed(&[1; 32]), "v");
        for i in 0..N {
            source.record_write(&format!("work/w{i}.md"), format!("w{i}\n").as_bytes()).unwrap();
        }
        for i in 0..N {
            source.record_write(&format!("personal/p{i}.md"), format!("p{i}\n").as_bytes()).unwrap();
        }
        assert_eq!(source.row_count(), 2 * N, "source holds 2N rows");

        let replica = MemEngine::create(Identity::from_seed(&[2; 32]), "v");
        scoped_pump(&source, &replica, &["work"]);
        assert_eq!(replica.row_count(), N, "scoped clone transfers exactly the in-scope rows");
        assert_eq!(replica.files_map().unwrap().len(), N);
    }

    /// A — native-Engine parity for the ground-truth invariant (§10 risk 5). The
    /// filter lives in the shared sans-IO path, so the native SQLite engine must
    /// produce the identical scoped fold.
    #[test]
    fn scoped_replica_native_engine_parity() {
        let ds = tempdir().unwrap();
        let dr = tempdir().unwrap();
        let source = Engine::init(ds.path(), Identity::from_seed(&[1; 32])).unwrap();
        let replica = Engine::init(dr.path(), Identity::from_seed(&[2; 32])).unwrap();
        // Same vault (clone scenario).
        let vid = source.store.get_config("vault_id").unwrap().unwrap();
        replica.store.set_config("vault_id", &vid).unwrap();
        for i in 0..12 {
            source.record_write(&format!("work/w{i}.md"), format!("w{i}\n").as_bytes()).unwrap();
            source.record_write(&format!("personal/p{i}.md"), format!("p{i}\n").as_bytes()).unwrap();
        }
        source.record_remove("work/w3.md").unwrap();

        source
            .authorize_with_policy(&Identity::from_seed(&[2; 32]).to_ssh_string(), None, true, "test", Some(vec!["work".into()]), false)
            .unwrap();
        let mut ls = Session::new(Role::Listener, &source, ctx(), nid(2), Vec::new());
        let mut cr = Session::new(Role::Connector, &replica, ctx(), nid(1), Vec::new());
        pump(&source, &mut ls, &replica, &mut cr);

        // The replica materialized exactly the live in-scope files, nothing else.
        for i in 0..12 {
            let w = dr.path().join(format!("work/w{i}.md"));
            let p = dr.path().join(format!("personal/p{i}.md"));
            assert!(!p.exists(), "out-of-scope file must not materialize");
            if i == 3 {
                assert!(!w.exists(), "deleted in-scope file must not materialize");
            } else {
                assert_eq!(std::fs::read(&w).unwrap(), format!("w{i}\n").as_bytes(), "in-scope file materialized");
            }
        }
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
            Step::CatchUp { peer_vv, policy } => {
                let mut out = Vec::new();
                push_rows_chunked(&mut out, catchup_rows(vault, &peer_vv, &policy).unwrap());
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
