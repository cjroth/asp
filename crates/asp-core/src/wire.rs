//! Wire framing (§Sync protocol). Each [`Msg`] is one transport frame
//! (one WebSocket binary message), msgpack-encoded. The log is the synced unit;
//! a row travels with its result blob bundled so the receiver can fold it without
//! a separate blob round-trip (causal ordering guarantees the *base* blob is
//! already present from the parent).

use crate::error::{AspError, AspResult};
use crate::log::LogRow;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Wire protocol version. A handshake-transcript or framing change bumps this so
/// skew surfaces as a clear version mismatch, not an opaque signature error.
pub const PROTO: u32 = 1;

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
    /// First frame each side sends. The listener's Hello additionally carries the
    /// advertised channel-binding value (SHA-256 of its served cert, or empty =
    /// binding-disabled) which both sides sign.
    Hello {
        proto: u32,
        node_id: String,
        #[serde(with = "serde_bytes")]
        nonce: Vec<u8>,
        #[serde(with = "serde_bytes")]
        channel_binding: Vec<u8>,
        vault_id: String,
        is_listener: bool,
        /// AUTH_KEY enrollment secret a connector presents (§Security). Carried
        /// here over the authenticated iroh stream — iroh has no HTTP upgrade
        /// header. `None` from a listener and from already-enrolled connectors.
        #[serde(default)]
        auth_key: Option<String>,
    },
    /// ed25519 signature over the mutual-auth transcript.
    Auth {
        #[serde(with = "serde_bytes")]
        sig: Vec<u8>,
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

/// The signed handshake transcript binding both nonces, both identities, the
/// advertised channel binding, the vault, and the proto version — so a captured
/// handshake cannot be replayed onto another channel (§Security).
pub fn transcript(
    proto: u32,
    listener_node: &str,
    connector_node: &str,
    listener_nonce: &[u8],
    connector_nonce: &[u8],
    channel_binding: &[u8],
    vault_id: &str,
) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(b"asp-handshake-v1");
    t.extend_from_slice(&proto.to_be_bytes());
    for part in [
        listener_node.as_bytes(),
        connector_node.as_bytes(),
        listener_nonce,
        connector_nonce,
        channel_binding,
        vault_id.as_bytes(),
    ] {
        t.extend_from_slice(&(part.len() as u64).to_be_bytes());
        t.extend_from_slice(part);
    }
    t
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
}
