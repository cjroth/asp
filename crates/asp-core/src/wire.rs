//! Wire framing (§Sync protocol). Each [`Msg`] is one transport frame
//! (one WebSocket binary message), msgpack-encoded. The log is the synced unit;
//! a row travels with its result blob bundled so the receiver can fold it without
//! a separate blob round-trip (causal ordering guarantees the *base* blob is
//! already present from the parent).

use crate::error::{AspError, AspResult};
use crate::log::LogRow;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Wire protocol version. A handshake or framing change bumps this so skew
/// surfaces as a clear version mismatch. v2 dropped the app-level nonce/signature
/// handshake — iroh's QUIC connection authenticates both node keys, so the
/// `Hello` just binds proto/vault/identity and carries the auth-key. v3 added the
/// `branch_id`/`merge_parent` fields to every `LogRow` (§9): they extend the
/// Merkle-id payload, so a v3 row id is computed differently — new↔new speak
/// branches; an old peer would reject v3 rows on the id check. v4 (git-bridge
/// §6.2) added three `Kind` variants — `GitCommit`/`GitIngest`/`GitPlan`. Those
/// serialize as new msgpack enum strings, so a proto-3 peer's `Kind` decoder
/// **fails** on any git row rather than silently mishandling it; the `Hello`
/// proto check (session.rs) refuses the mismatch up front. Decided 2026-07-06:
/// coordinated same-day fleet upgrade, no two-step understand-then-author release.
pub const PROTO: u32 = 4;

/// One content blob shipped alongside a row.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WireBlob {
    pub hash: String,
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
}

/// A log row plus the content blobs it references that the peer may lack — its
/// `base_hash` (the author's content at authoring time, needed as the 3-way LCA)
/// and its `result_hash`. Causal ordering means ancestors arrived first, but the
/// base of a locally-merged edit is derived (never an ancestor's result), so it
/// is bundled explicitly to keep the fold self-contained.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WireRow {
    pub row: LogRow,
    pub blobs: Vec<WireBlob>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Msg {
    /// First frame each side sends over the already-key-authenticated iroh
    /// connection. It binds the wire `proto`, the `vault_id`, and the claimed
    /// `node_id` (which the receiver cross-checks against the transport-verified
    /// remote key), and carries the optional AUTH_KEY enrollment secret. There is
    /// no nonce/signature: iroh's QUIC handshake already proved each side holds
    /// the private key for the `NodeId` it claims (§Security).
    Hello {
        proto: u32,
        node_id: String,
        vault_id: String,
        is_listener: bool,
        /// AUTH_KEY enrollment secret a connector presents (§Security). `None`
        /// from a listener and from already-enrolled connectors.
        #[serde(default)]
        auth_key: Option<String>,
    },
    /// Version vector: site_id -> highest seq held.
    Vector { vv: BTreeMap<String, i64> },
    /// Batch of rows (catch-up: exactly what the peer was missing).
    Rows { rows: Vec<WireRow> },
    /// Optimistic real-time push of a single new row.
    Push { row: Box<WireRow> },
    /// The listener refused admission (or another fatal handshake error). Sent
    /// before close so the connector learns it was rejected rather than silently
    /// seeing the socket drop.
    Denied { reason: String },
    /// Graceful close.
    Bye,
    /// Listener → connector: "I've sent every row you were missing." Lets a
    /// oneshot connector finish the instant the catch-up stream ends instead of
    /// waiting out an idle timeout — so completion is explicit (no per-sync tail)
    /// and independent of how long any single (large) frame took to arrive.
    Synced,
}

impl Msg {
    pub fn to_bytes(&self) -> AspResult<Vec<u8>> {
        rmp_serde::to_vec_named(self).map_err(|e| AspError::Protocol(e.to_string()))
    }
    pub fn from_bytes(b: &[u8]) -> AspResult<Msg> {
        rmp_serde::from_slice(b).map_err(|e| AspError::Protocol(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msg_roundtrip() {
        let m = Msg::Vector { vv: BTreeMap::from([("aa".to_string(), 3i64)]) };
        let b = m.to_bytes().unwrap();
        match Msg::from_bytes(&b).unwrap() {
            Msg::Vector { vv } => assert_eq!(vv["aa"], 3),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn git_kind_rows_survive_push_wire_roundtrip() {
        // git-bridge §6.1/§6.2: a proto-4 LogRow of each new Kind must survive
        // seal()/id_valid() and a full Msg::Push (WireRow) msgpack round-trip — the
        // path every synced row travels. (An old proto-3 peer instead fails to
        // decode the Kind, which is why the Hello proto check refuses it.)
        use crate::gitrecord::{
            build_commit_marker_row, build_ingest_row, build_plan_row, GitCommitMarker, GitIngestRecord,
            GitPlanRecord, GitRowIdentity,
        };
        use crate::store::{BlobStore, MemBlobStore};

        let store = MemBlobStore::new();
        let ident = GitRowIdentity { site_id: "repo-site".into(), lamport: 1, seq: 0, ts: 42, parent: None };
        let marker = GitCommitMarker {
            sha: "a1b2c3".into(),
            author_name: "Ada".into(),
            author_email: "ada@x".into(),
            committer_ts: 42,
            message: "m".into(),
            parents: vec![],
            branch_id: "main".into(),
        };
        let ingest = GitIngestRecord {
            commit_sha: "a1b2c3".into(),
            upto_site: "repo-site".into(),
            upto_seq: 0,
            modes: vec![("f".into(), 0o100644)],
            symlinks: vec![],
            gitlinks: vec![],
            remote_ref: "refs/heads/main".into(),
            rebaselined: false,
        };
        let plan = GitPlanRecord {
            frontier: BTreeMap::from([("repo-site".to_string(), 0i64)]),
            message: "asp: 1 file".into(),
            author: "Bot <b@x>".into(),
            planned_ts: 42,
        };

        let rows = vec![
            build_commit_marker_row(&store, &ident, &marker).unwrap(),
            build_ingest_row(&store, &ident, &ingest).unwrap(),
            build_plan_row(&store, &ident, &plan).unwrap(),
        ];
        for row in rows {
            assert!(row.id_valid(), "git-kind row must be sealed");
            let blob = store.get_blob(row.result_hash.as_ref().unwrap()).unwrap().unwrap();
            let hash = row.result_hash.clone().unwrap();
            let push = Msg::Push { row: Box::new(WireRow { row: row.clone(), blobs: vec![WireBlob { hash, bytes: blob }] }) };
            let bytes = push.to_bytes().unwrap();
            match Msg::from_bytes(&bytes).unwrap() {
                Msg::Push { row: got } => {
                    assert_eq!(got.row, row, "row survives msgpack round-trip byte-identical");
                    assert!(got.row.id_valid(), "id still valid after the wire trip");
                }
                _ => panic!("wrong variant"),
            }
        }
    }
}
