//! Node identity and the concurrent-tiebreak key.
//!
//! A node's stable identity is its 32-byte ed25519 public key — the same key
//! that authenticates its connections (§Security) and stamps every row it
//! authors as `site_id`. The fold orders rows causally (parent before child);
//! among **concurrent** rows it breaks ties by `(lamport, site_id, id)`
//! (§Clocks & ordering). `tiebreak_key` is genesis-immutable and fixed to
//! `lamport` in v1.

use serde::{Deserialize, Serialize};

/// 32-byte ed25519 public key.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(pub [u8; 32]);

impl NodeId {
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
    pub fn from_hex(s: &str) -> Option<NodeId> {
        let v = hex::decode(s).ok()?;
        if v.len() != 32 {
            return None;
        }
        let mut a = [0u8; 32];
        a.copy_from_slice(&v);
        Some(NodeId(a))
    }
}

impl std::fmt::Debug for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NodeId({}…)", &self.to_hex()[..12.min(self.to_hex().len())])
    }
}

/// The fold tiebreak key among concurrent rows: `(lamport, site_id, id)`.
/// Ascending — the later (higher) key folds in last and so "wins" a same-region
/// contention, identically on every node (§The merge model).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OrderKey {
    pub lamport: u64,
    pub site_id: String,
    pub id: String,
}

impl PartialOrd for OrderKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.lamport
            .cmp(&other.lamport)
            .then_with(|| self.site_id.cmp(&other.site_id))
            .then_with(|| self.id.cmp(&other.id))
    }
}
