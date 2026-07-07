//! Thin remote-view client (C) — a source node serves read/query + write + subscribe
//! over a **separate iroh ALPN** (`asp/query/1`), alongside and independent of the
//! row-streaming sync ALPN (scoped-sync §5). The thin client keeps NO local log or
//! blobs: every read is a *server-side fold*, every write is *authored by the
//! source*. A separate ALPN leaves `Msg`/`PROTO` untouched (no bump) and reuses A's
//! `allowed_paths` and B's `read_only` grants directly — the QUIC handshake already
//! proves the client's `node_id`, so there is no separate bearer-token→policy table.
//!
//! This module is the native, server-side protocol core: the request/response
//! frames and the [`ThinSession`] handler. The reads fold the source's full store
//! (filtered to the client's grant); the writes are authored by the source on the
//! client's behalf, with per-user attribution recorded OUTSIDE the row (the client's
//! signed envelope in `remote_edits`, since `canonical_fields` is frozen — history
//! legitimately says "the source authored it", §5.3). The star is naturally
//! enforced: a thin client speaks ONLY this ALPN, never the sync ALPN, so it is not
//! a sync participant.

use crate::authkeys::PeerPolicy;
use crate::engine::Engine;
use crate::error::AspResult;
use crate::identity::verify_detached;
use crate::order::NodeId;
use crate::store::BlobStore;
use serde::{Deserialize, Serialize};

/// The thin-client query ALPN — distinct from the sync ALPN, so `Msg`/`PROTO` are
/// untouched (scoped-sync §5.1, §7).
pub const QUERY_ALPN: &[u8] = b"asp/query/1";

/// A read/query against the source's HEAD (or a point in history).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum QueryOp {
    /// Immediate children (files + subdirs) under `path` (`""` = the root).
    ListDir { path: String },
    /// The current bytes of `path` (`None` if it doesn't exist / is out of scope).
    ReadFile { path: String },
    /// The bytes of `path` as of wall-clock `ts` (history slider).
    ReadFileAt { path: String, ts: i64 },
    /// Existence + content hash of `path`.
    Stat { path: String },
}

/// A write-through: the client requests it; the SOURCE authors the row (§5.3).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SubmitOp {
    /// Write `bytes` to `path`. `base_hash` is the content hash the client read
    /// (optimistic concurrency: the source rejects if the tip moved).
    Write { path: String, bytes: Vec<u8>, base_hash: Option<String> },
    Rename { from: String, to: String },
    Delete { path: String },
}

/// A thin-client request frame (its own protocol, not `wire::Msg`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ThinReq {
    Query { id: u64, op: QueryOp },
    /// A write-through; `nonce` + `envelope_sig` (the client's ed25519 signature over
    /// the op + nonce) attribute it to the client without a synced row field (§5.3).
    Submit { id: u64, op: SubmitOp, nonce: u64, #[serde(with = "serde_bytes")] envelope_sig: Vec<u8> },
    /// Subscribe to changes under `path_prefix` (signal-then-pull, §5.4).
    Subscribe { sub_id: u64, path_prefix: String },
    Unsubscribe { sub_id: u64 },
}

/// One entry of a `ListDir` result.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatInfo {
    pub exists: bool,
    pub hash: Option<String>,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum QueryResult {
    Dir(Vec<DirEntry>),
    File(Option<Vec<u8>>),
    Stat(StatInfo),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SubmitResult {
    /// Authored — the source-authored row id (attributed in `remote_edits`).
    Ok { row_id: String },
    /// The optimistic `base_hash` guard fired (the tip moved) — the client re-reads.
    Conflict,
    /// The write was a no-op or the path is `.aspignore`d (explicit, never silent
    /// success — scoped-sync §5.3 `.aspignore` no-op trap).
    NoOp,
}

/// A thin-client response frame.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ThinResp {
    QueryResp { id: u64, result: QueryResult },
    SubmitResp { id: u64, result: SubmitResult },
    /// A subscribed subtree changed — the client re-queries it (signal-then-pull).
    Event { sub_id: u64 },
    /// The request was refused (out of grant, read-only, or a bad envelope sig).
    Denied { id: u64, reason: String },
}

impl ThinReq {
    pub fn to_bytes(&self) -> AspResult<Vec<u8>> {
        rmp_serde::to_vec_named(self).map_err(|e| crate::error::AspError::Protocol(e.to_string()))
    }
    pub fn from_bytes(b: &[u8]) -> AspResult<ThinReq> {
        rmp_serde::from_slice(b).map_err(|e| crate::error::AspError::Protocol(e.to_string()))
    }
}

impl ThinResp {
    pub fn to_bytes(&self) -> AspResult<Vec<u8>> {
        rmp_serde::to_vec_named(self).map_err(|e| crate::error::AspError::Protocol(e.to_string()))
    }
    pub fn from_bytes(b: &[u8]) -> AspResult<ThinResp> {
        rmp_serde::from_slice(b).map_err(|e| crate::error::AspError::Protocol(e.to_string()))
    }
}

/// Does a change to `changed_path` fall under a subscription's `prefix`? (§5.4)
pub fn prefix_intersects(prefix: &str, changed_path: &str) -> bool {
    let p = prefix.trim_end_matches('/');
    p.is_empty() || changed_path == p || changed_path.starts_with(&format!("{p}/"))
}

/// The canonical bytes a client signs to attribute a `Submit` (scoped-sync §5.3):
/// the op + nonce, so the source can verify authorship with the client's key.
pub fn submit_envelope(op: &SubmitOp, nonce: u64) -> Vec<u8> {
    let mut b = Vec::new();
    let field = |b: &mut Vec<u8>, tag: u8, s: &[u8]| {
        b.push(tag);
        b.extend_from_slice(&(s.len() as u64).to_be_bytes());
        b.extend_from_slice(s);
    };
    match op {
        SubmitOp::Write { path, bytes, .. } => {
            b.extend_from_slice(b"write\0");
            field(&mut b, b'p', path.as_bytes());
            field(&mut b, b'b', bytes);
        }
        SubmitOp::Rename { from, to } => {
            b.extend_from_slice(b"rename\0");
            field(&mut b, b'f', from.as_bytes());
            field(&mut b, b't', to.as_bytes());
        }
        SubmitOp::Delete { path } => {
            b.extend_from_slice(b"delete\0");
            field(&mut b, b'p', path.as_bytes());
        }
    }
    b.extend_from_slice(&nonce.to_be_bytes());
    b
}

/// A server-side thin-client session: the transport-verified client key + the
/// grant the source admitted it with (A's `allowed_paths`, B's `read_only`). Reads
/// fold the source's store filtered to the grant; writes are source-authored and
/// attributed to the client in `remote_edits`.
pub struct ThinSession {
    client: NodeId,
    policy: PeerPolicy,
}

impl ThinSession {
    pub fn new(client: NodeId, policy: PeerPolicy) -> ThinSession {
        ThinSession { client, policy }
    }

    /// Is `path` within the client's read/write scope (A)? `None` grant = full vault.
    fn in_scope(&self, path: &str) -> bool {
        match &self.policy.allowed_paths {
            None => true,
            Some(allowed) => crate::scope::allows(allowed, path),
        }
    }

    /// Handle one request against the source `engine`, returning one response.
    pub fn on_req(&self, engine: &Engine, req: ThinReq) -> AspResult<ThinResp> {
        match req {
            ThinReq::Query { id, op } => self.on_query(engine, id, op),
            ThinReq::Submit { id, op, nonce, envelope_sig } => self.on_submit(engine, id, op, nonce, &envelope_sig),
            // Subscribe/Unsubscribe are tracked by the DRIVER (which owns the change
            // listener + the outbound stream); the sans-IO session just acknowledges
            // by echoing an Event on the initial subscribe so the client can pull once.
            ThinReq::Subscribe { sub_id, .. } => Ok(ThinResp::Event { sub_id }),
            ThinReq::Unsubscribe { sub_id } => Ok(ThinResp::Event { sub_id }),
        }
    }

    fn on_query(&self, engine: &Engine, id: u64, op: QueryOp) -> AspResult<ThinResp> {
        match op {
            QueryOp::ListDir { path } => {
                // Immediate children of `path`, filtered to the client's scope.
                let prefix = if path.is_empty() { String::new() } else { format!("{}/", path.trim_end_matches('/')) };
                let mut seen = std::collections::BTreeMap::<String, bool>::new();
                for f in engine.store.live_files()? {
                    if !self.in_scope(&f.path) {
                        continue;
                    }
                    let Some(rest) = f.path.strip_prefix(&prefix) else { continue };
                    if rest.is_empty() {
                        continue;
                    }
                    match rest.find('/') {
                        Some(i) => {
                            seen.entry(rest[..i].to_string()).or_insert(true);
                        } // subdir
                        None => {
                            seen.insert(rest.to_string(), false);
                        } // file
                    }
                }
                let entries = seen.into_iter().map(|(name, is_dir)| DirEntry { name, is_dir }).collect();
                Ok(ThinResp::QueryResp { id, result: QueryResult::Dir(entries) })
            }
            QueryOp::ReadFile { path } => {
                if !self.in_scope(&path) {
                    return Ok(ThinResp::Denied { id, reason: "path out of grant".into() });
                }
                let bytes = match engine.store.live_file_by_path(&path)? {
                    Some(f) => match f.result_hash {
                        Some(h) => engine.store.get_blob(&h)?,
                        None => None,
                    },
                    None => None,
                };
                Ok(ThinResp::QueryResp { id, result: QueryResult::File(bytes) })
            }
            QueryOp::ReadFileAt { path, ts } => {
                if !self.in_scope(&path) {
                    return Ok(ThinResp::Denied { id, reason: "path out of grant".into() });
                }
                Ok(ThinResp::QueryResp { id, result: QueryResult::File(engine.file_at(&path, ts)?) })
            }
            QueryOp::Stat { path } => {
                if !self.in_scope(&path) {
                    return Ok(ThinResp::Denied { id, reason: "path out of grant".into() });
                }
                let info = match engine.store.live_file_by_path(&path)? {
                    Some(f) => {
                        let (hash, size) = match &f.result_hash {
                            Some(h) => (Some(h.clone()), engine.store.get_blob(h)?.map(|b| b.len() as u64).unwrap_or(0)),
                            None => (None, 0),
                        };
                        StatInfo { exists: true, hash, size }
                    }
                    None => StatInfo { exists: false, hash: None, size: 0 },
                };
                Ok(ThinResp::QueryResp { id, result: QueryResult::Stat(info) })
            }
        }
    }

    fn on_submit(&self, engine: &Engine, id: u64, op: SubmitOp, nonce: u64, sig: &[u8]) -> AspResult<ThinResp> {
        // B — a read-only client may not write.
        if self.policy.read_only {
            return Ok(ThinResp::Denied { id, reason: "read-only".into() });
        }
        // The target path must be in the client's grant (A).
        let target = match &op {
            SubmitOp::Write { path, .. } | SubmitOp::Delete { path } => path.clone(),
            SubmitOp::Rename { to, .. } => to.clone(),
        };
        if !self.in_scope(&target) {
            return Ok(ThinResp::Denied { id, reason: "path out of grant".into() });
        }
        // Attribution: the client signed (op + nonce) with its key; the source
        // verifies before authoring (the first real use of verify_detached for
        // thin-client submits, §5.3). A bad envelope is refused.
        let envelope = submit_envelope(&op, nonce);
        if verify_detached(&self.client, &envelope, sig).is_err() {
            return Ok(ThinResp::Denied { id, reason: "bad envelope signature".into() });
        }
        // Optimistic concurrency: if the client sent the base_hash it read and the
        // tip has since moved, reject with Conflict rather than silently clobbering.
        if let SubmitOp::Write { path, base_hash: Some(base), .. } = &op {
            let cur = engine.store.live_file_by_path(path)?.and_then(|f| f.result_hash);
            if cur.as_deref() != Some(base.as_str()) {
                return Ok(ThinResp::SubmitResp { id, result: SubmitResult::Conflict });
            }
        }
        // The SOURCE authors the row (§5.3): causally valid, convergent, fans out to
        // full peers normally. `record_*` returns None for an ignored path or a
        // no-op — surfaced explicitly as NoOp, never silent success (§5.3 trap).
        let authored = match &op {
            SubmitOp::Write { path, bytes, .. } => engine.record_write(path, bytes)?,
            SubmitOp::Rename { from, to } => engine.record_rename(from, to)?,
            SubmitOp::Delete { path } => engine.record_remove(path)?,
        };
        match authored {
            Some(wr) => {
                // Record who really submitted it — node-local, never synced (§6).
                let ts = crate::net::now_unix() as i64;
                engine.store.record_remote_edit(&wr.row.id, &self.client.to_hex(), sig, ts)?;
                Ok(ThinResp::SubmitResp { id, result: SubmitResult::Ok { row_id: wr.row.id } })
            }
            None => Ok(ThinResp::SubmitResp { id, result: SubmitResult::NoOp }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use tempfile::tempdir;

    fn full_policy() -> PeerPolicy {
        PeerPolicy::default()
    }

    fn client_id() -> Identity {
        Identity::from_seed(&[42; 32])
    }

    fn engine_with(files: &[(&str, &[u8])]) -> (tempfile::TempDir, Engine) {
        let d = tempdir().unwrap();
        let e = Engine::init(d.path(), Identity::from_seed(&[1; 32])).unwrap();
        for (p, b) in files {
            e.record_write(p, b).unwrap();
        }
        (d, e)
    }

    #[test]
    fn read_query_serves_head_and_lists_dirs() {
        let (_d, e) = engine_with(&[("work/a.md", b"A"), ("work/sub/b.md", b"B"), ("personal/c.md", b"C")]);
        let s = ThinSession::new(client_id().node_id(), full_policy());

        // ReadFile
        let r = s.on_req(&e, ThinReq::Query { id: 1, op: QueryOp::ReadFile { path: "work/a.md".into() } }).unwrap();
        assert_eq!(r, ThinResp::QueryResp { id: 1, result: QueryResult::File(Some(b"A".to_vec())) });

        // ListDir root → immediate children (work/, personal/ as dirs).
        let r = s.on_req(&e, ThinReq::Query { id: 2, op: QueryOp::ListDir { path: "".into() } }).unwrap();
        let ThinResp::QueryResp { result: QueryResult::Dir(entries), .. } = r else { panic!() };
        assert!(entries.iter().any(|e| e.name == "work" && e.is_dir));
        assert!(entries.iter().any(|e| e.name == "personal" && e.is_dir));

        // ListDir work/ → a.md (file) + sub (dir).
        let r = s.on_req(&e, ThinReq::Query { id: 3, op: QueryOp::ListDir { path: "work".into() } }).unwrap();
        let ThinResp::QueryResp { result: QueryResult::Dir(entries), .. } = r else { panic!() };
        assert!(entries.iter().any(|e| e.name == "a.md" && !e.is_dir));
        assert!(entries.iter().any(|e| e.name == "sub" && e.is_dir));

        // Stat a missing file.
        let r = s.on_req(&e, ThinReq::Query { id: 4, op: QueryOp::Stat { path: "nope.md".into() } }).unwrap();
        assert_eq!(r, ThinResp::QueryResp { id: 4, result: QueryResult::Stat(StatInfo { exists: false, hash: None, size: 0 }) });
    }

    #[test]
    fn queries_are_filtered_by_the_client_grant() {
        let (_d, e) = engine_with(&[("work/a.md", b"A"), ("personal/secret.md", b"S")]);
        let scoped = PeerPolicy { allowed_paths: Some(vec!["work".into()]), read_only: false };
        let s = ThinSession::new(client_id().node_id(), scoped);

        // Out-of-scope read is refused.
        let r = s.on_req(&e, ThinReq::Query { id: 1, op: QueryOp::ReadFile { path: "personal/secret.md".into() } }).unwrap();
        assert!(matches!(r, ThinResp::Denied { .. }), "out-of-grant read refused");
        // ListDir root hides the out-of-scope subtree.
        let r = s.on_req(&e, ThinReq::Query { id: 2, op: QueryOp::ListDir { path: "".into() } }).unwrap();
        let ThinResp::QueryResp { result: QueryResult::Dir(entries), .. } = r else { panic!() };
        assert!(entries.iter().any(|e| e.name == "work"));
        assert!(!entries.iter().any(|e| e.name == "personal"), "out-of-scope subtree hidden");
    }

    #[test]
    fn submit_authors_a_source_row_and_records_attribution() {
        let (_d, e) = engine_with(&[]);
        let client = client_id();
        let s = ThinSession::new(client.node_id(), full_policy());

        let op = SubmitOp::Write { path: "notes/hi.md".into(), bytes: b"hello".to_vec(), base_hash: None };
        let nonce = 7;
        let sig = client.sign(&submit_envelope(&op, nonce));
        let r = s.on_req(&e, ThinReq::Submit { id: 1, op, nonce, envelope_sig: sig.clone() }).unwrap();

        let ThinResp::SubmitResp { result: SubmitResult::Ok { row_id }, .. } = r else { panic!("expected Ok, got {r:?}") };
        // Exactly one source-authored row + one remote_edits attribution row.
        assert_eq!(e.store.live_file_by_path("notes/hi.md").unwrap().unwrap().result_hash, Some(crate::oid::content_hash(b"hello")));
        let attr = e.store.remote_edit(&row_id).unwrap().expect("attribution recorded");
        assert_eq!(attr.0, client.node_id().to_hex(), "attributed to the client");
        assert_eq!(attr.1, sig, "envelope sig stored");
        // The row itself is authored by the SOURCE (history says so), not the client.
        let row = e.store.rows_by_ids(&[row_id]).unwrap().pop().unwrap();
        assert_eq!(row.site_id, e.site_id(), "the source authored the row");
        assert_ne!(row.site_id, client.node_id().to_hex());
    }

    #[test]
    fn submit_rejects_a_bad_envelope_signature() {
        let (_d, e) = engine_with(&[]);
        let client = client_id();
        let s = ThinSession::new(client.node_id(), full_policy());
        let op = SubmitOp::Write { path: "x.md".into(), bytes: b"x".to_vec(), base_hash: None };
        // Signed by the WRONG key.
        let sig = Identity::from_seed(&[99; 32]).sign(&submit_envelope(&op, 1));
        let r = s.on_req(&e, ThinReq::Submit { id: 1, op, nonce: 1, envelope_sig: sig }).unwrap();
        assert!(matches!(r, ThinResp::Denied { .. }), "a forged envelope must be refused");
        assert!(e.store.live_file_by_path("x.md").unwrap().is_none(), "nothing authored");
    }

    #[test]
    fn submit_is_refused_when_read_only_or_out_of_scope() {
        let (_d, e) = engine_with(&[]);
        let client = client_id();
        // read-only
        let ro = ThinSession::new(client.node_id(), PeerPolicy { allowed_paths: None, read_only: true });
        let op = SubmitOp::Write { path: "a.md".into(), bytes: b"a".to_vec(), base_hash: None };
        let sig = client.sign(&submit_envelope(&op, 1));
        let r = ro.on_req(&e, ThinReq::Submit { id: 1, op: op.clone(), nonce: 1, envelope_sig: sig.clone() }).unwrap();
        assert!(matches!(r, ThinResp::Denied { reason, .. } if reason.contains("read-only")));

        // out-of-scope
        let scoped = ThinSession::new(client.node_id(), PeerPolicy { allowed_paths: Some(vec!["work".into()]), read_only: false });
        let out = SubmitOp::Write { path: "personal/x.md".into(), bytes: b"x".to_vec(), base_hash: None };
        let sig = client.sign(&submit_envelope(&out, 2));
        let r = scoped.on_req(&e, ThinReq::Submit { id: 2, op: out, nonce: 2, envelope_sig: sig }).unwrap();
        assert!(matches!(r, ThinResp::Denied { .. }), "out-of-scope write refused");
    }

    #[test]
    fn submit_conflict_guard_and_aspignore_noop() {
        let (_d, e) = engine_with(&[("doc.md", b"v1")]);
        let client = client_id();
        let s = ThinSession::new(client.node_id(), full_policy());

        // Optimistic base_hash guard: a stale base is a Conflict.
        let op = SubmitOp::Write { path: "doc.md".into(), bytes: b"v2".to_vec(), base_hash: Some(crate::oid::content_hash(b"STALE")) };
        let sig = client.sign(&submit_envelope(&op, 1));
        let r = s.on_req(&e, ThinReq::Submit { id: 1, op, nonce: 1, envelope_sig: sig }).unwrap();
        assert_eq!(r, ThinResp::SubmitResp { id: 1, result: SubmitResult::Conflict });

        // A no-op write (identical content) is surfaced as NoOp, not silent success.
        let op = SubmitOp::Write { path: "doc.md".into(), bytes: b"v1".to_vec(), base_hash: Some(crate::oid::content_hash(b"v1")) };
        let sig = client.sign(&submit_envelope(&op, 2));
        let r = s.on_req(&e, ThinReq::Submit { id: 2, op, nonce: 2, envelope_sig: sig }).unwrap();
        assert_eq!(r, ThinResp::SubmitResp { id: 2, result: SubmitResult::NoOp });
    }
}
