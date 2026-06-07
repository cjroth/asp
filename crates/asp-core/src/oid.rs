//! Content addressing. Blobs and log rows are addressed by the hex SHA-256 of
//! their canonical bytes. The log row's hash is its **Merkle id** — any change
//! to any field yields a different id, so a substituted/corrupted row cannot
//! masquerade as another and dedup is free (§Core model).

use sha2::{Digest, Sha256};

/// Hex SHA-256 of arbitrary bytes — the content hash used for `blobs` and as
/// the building block of a row's Merkle id.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// Hash a sequence of length-prefixed fields into one canonical digest. Used to
/// compute a log row's Merkle id deterministically and unambiguously (no field
/// boundary can be confused with another).
pub fn merkle_id(fields: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for f in fields {
        h.update((f.len() as u64).to_be_bytes());
        h.update(f);
    }
    hex::encode(h.finalize())
}
