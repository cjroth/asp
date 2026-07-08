//! Storage abstraction (§Implementation: I/O injected via traits). The
//! convergence-critical fold needs only **blob access**, so that is the one
//! storage seam shared across surfaces: the native [`crate::sqlite::SqliteStore`]
//! (SQLite, on-disk) and the wasm-safe [`MemBlobStore`] (in-memory / OPFS-backed
//! by the host) both implement [`BlobStore`], and the *same* `fold::compute_files`
//! runs over either. Everything else native (the full SQLite schema, on-disk
//! materialize) lives in `sqlite.rs`; the wasm engine keeps its log/config/auth in
//! memory ([`crate::memengine`]).

use crate::error::AspResult;
use crate::log::MergeClass;
use std::cell::RefCell;
use std::collections::HashMap;

/// Content-addressed blob storage — the one seam the deterministic fold needs.
pub trait BlobStore {
    fn put_blob(&self, bytes: &[u8]) -> AspResult<String>;
    fn get_blob(&self, hash: &str) -> AspResult<Option<Vec<u8>>>;
    fn has_blob(&self, hash: &str) -> AspResult<bool>;

    /// Insert `bytes` under an **already-computed** content hash, skipping the
    /// SHA-256 that `put_blob` would recompute. `hash` MUST equal
    /// `content_hash(bytes)` — the caller computed it (e.g. in a parallel pre-pass).
    /// The default re-hashes and debug-asserts equality (correct, but no speedup);
    /// [`MemBlobStore`] overrides it to insert directly. Used by the parallel genesis
    /// pre-pass to move blob hashing off the single-threaded emission loop.
    fn put_blob_with_hash(&self, hash: &str, bytes: &[u8]) -> AspResult<()> {
        let h = self.put_blob(bytes)?;
        debug_assert_eq!(h, hash, "put_blob_with_hash: precomputed hash mismatch");
        Ok(())
    }

    /// Like [`put_blob_with_hash`](BlobStore::put_blob_with_hash) but takes the bytes by
    /// value, letting an in-memory store **move** them in instead of copying — saving a
    /// full memcpy per blob on the (memory-bandwidth-bound) clone path. Default falls
    /// back to the borrowing form; [`MemBlobStore`] overrides it to move.
    fn put_blob_with_hash_owned(&self, hash: &str, bytes: Vec<u8>) -> AspResult<()> {
        self.put_blob_with_hash(hash, &bytes)
    }

    /// Insert a **batch** of already-hashed blobs, moving the bytes in. The clone's
    /// pack-decode spill hands blobs over in batches so a disk-backed store can commit
    /// one transaction per batch instead of one per blob (millions of autocommits
    /// otherwise dominate a full-history clone). Each `hash` MUST equal
    /// `content_hash(bytes)` — the caller (decode) already computed it, so no re-hash.
    /// Default loops the owned single insert; [`crate::sqlite::SqliteStore`] overrides
    /// it with a single transaction.
    fn put_blobs_with_hash_owned(&self, batch: Vec<(String, Vec<u8>)>) -> AspResult<()> {
        for (h, b) in batch {
            self.put_blob_with_hash_owned(&h, b)?;
        }
        Ok(())
    }
}

/// A materialized file row (§files) — the fold's output, surface-independent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRow {
    pub file_id: String,
    pub path: String,
    pub result_hash: Option<String>,
    pub merge_class: MergeClass,
    pub deleted: bool,
    pub lamport: u64,
    pub site_id: String,
    pub conflict: bool,
}

/// In-memory content-addressed blob store (wasm / tests). Interior-mutable so its
/// `put_blob` matches the `&self` `BlobStore` contract.
#[derive(Default)]
pub struct MemBlobStore {
    blobs: RefCell<HashMap<String, Vec<u8>>>,
}

impl MemBlobStore {
    pub fn new() -> MemBlobStore {
        MemBlobStore::default()
    }
}

impl BlobStore for MemBlobStore {
    fn put_blob(&self, bytes: &[u8]) -> AspResult<String> {
        let h = crate::oid::content_hash(bytes);
        self.blobs.borrow_mut().entry(h.clone()).or_insert_with(|| bytes.to_vec());
        Ok(h)
    }
    fn get_blob(&self, hash: &str) -> AspResult<Option<Vec<u8>>> {
        Ok(self.blobs.borrow().get(hash).cloned())
    }
    fn has_blob(&self, hash: &str) -> AspResult<bool> {
        Ok(self.blobs.borrow().contains_key(hash))
    }
    fn put_blob_with_hash(&self, hash: &str, bytes: &[u8]) -> AspResult<()> {
        self.blobs
            .borrow_mut()
            .entry(hash.to_string())
            .or_insert_with(|| bytes.to_vec());
        Ok(())
    }
    fn put_blob_with_hash_owned(&self, hash: &str, bytes: Vec<u8>) -> AspResult<()> {
        // Move the owned Vec straight in (no memcpy); on a dedup hit `bytes` is dropped.
        self.blobs.borrow_mut().entry(hash.to_string()).or_insert(bytes);
        Ok(())
    }
}
